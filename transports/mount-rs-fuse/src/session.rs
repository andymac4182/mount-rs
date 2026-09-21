//! Driver-backed requests for modern FUSE (7.9+). Negotiation and mount
//! lifecycle are separate and remain under implementation.
#[cfg(target_os = "linux")]
use crate::constants::FUSE_READ;
use crate::{
    Request,
    constants::{
        FOPEN_KEEP_CACHE, FOPEN_NOFLUSH, FUSE_BATCH_FORGET, FUSE_BMAP, FUSE_COPY_FILE_RANGE,
        FUSE_FALLOCATE, FUSE_FORGET, FUSE_GETLK, FUSE_GETXATTR, FUSE_INTERRUPT, FUSE_IOCTL,
        FUSE_KERNEL_MINOR_VERSION, FUSE_LISTXATTR, FUSE_LK_FLOCK, FUSE_LSEEK, FUSE_NOTIFY_REPLY,
        FUSE_POLL, FUSE_READLINK, FUSE_REMOVEXATTR, FUSE_RENAME2, FUSE_SETLK, FUSE_SETLKW,
        FUSE_SETXATTR, FUSE_SETXATTR_EXT, FUSE_STATFS,
    },
    error_reply,
    inodes::InodeTable,
    open_flags,
    protocol::{
        FuseFileLock, FuseKstatfs, FuseLkIn, FuseLkOut, FuseReadlinkOut, FuseReplyBody,
        FuseRequestBody, ProtocolContext, decode_request_body,
    },
};
use mount_rs_core::{ErrorCode, FileHandle, FsDriver, FsError, MkdirOptions, Result, Stats};
use std::{collections::HashMap, sync::Arc};

type DirectorySnapshot = Option<Vec<(String, u32)>>;
const FUSE_IOCTL_IN_SIZE: usize = 32;
// Keep path-component validation aligned with the `namelen` in STATFS.
const NAME_MAX: usize = 255;
const F_RDLCK: u32 = 0;
const F_WRLCK: u32 = 1;
const F_UNLCK: u32 = 2;
// The driver contract currently supports plain rename only.  Match the
// pinned mountx session: unsupported RENAME2 flag bits get ENOSYS so the
// kernel can fall back to a plain rename where appropriate.
const RENAME2_UNSUPPORTED_FLAGS: u32 = 0b111;

/// A validated read that can run without holding the mutable session state.
/// Stateful requests continue through [`FuseSession::handle`] in order.
#[cfg(target_os = "linux")]
pub(crate) struct PreparedRead {
    unique: u64,
    handle: Arc<dyn FileHandle>,
    offset: u64,
    size: usize,
}

#[cfg(target_os = "linux")]
impl PreparedRead {
    pub(crate) async fn reply(self) -> Vec<u8> {
        let mut body = vec![0; self.size];
        match self.handle.read(&mut body, Some(self.offset)).await {
            Ok(count) if count <= self.size => {
                body.truncate(count);
                let mut reply = Vec::with_capacity(16 + body.len());
                reply.extend(((16 + body.len()) as u32).to_le_bytes());
                reply.extend(0i32.to_le_bytes());
                reply.extend(self.unique.to_le_bytes());
                reply.extend(body);
                reply
            }
            Ok(_) => error_reply(self.unique, ErrorCode::Eio).to_vec(),
            Err(error) => error_reply(self.unique, error.code).to_vec(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct HeldLock {
    start: u128,
    end: u128,
    owner: u64,
    pid: u32,
    type_: u32,
}

fn lock_range(lock: FuseFileLock) -> Result<(u128, u128)> {
    if !matches!(lock.type_, F_RDLCK | F_WRLCK | F_UNLCK) || lock.start > lock.end {
        return Err(FsError::new(ErrorCode::Einval).with_message("invalid FUSE lock range"));
    }
    Ok((u128::from(lock.start), u128::from(lock.end) + 1))
}

fn ranges_overlap(left_start: u128, left_end: u128, right_start: u128, right_end: u128) -> bool {
    left_start < right_end && right_start < left_end
}

fn lock_conflicts(held: HeldLock, request: FuseLkIn, start: u128, end: u128) -> bool {
    held.owner != request.owner
        && ranges_overlap(held.start, held.end, start, end)
        && (held.type_ == F_WRLCK || request.lk.type_ == F_WRLCK)
}

fn wire_lock(lock: HeldLock) -> FuseFileLock {
    FuseFileLock {
        start: lock.start as u64,
        end: (lock.end - 1) as u64,
        type_: lock.type_,
        pid: lock.pid,
    }
}

async fn stat_of(driver: &dyn FsDriver, path: &str) -> Result<Stats> {
    match driver.lstat(path).await {
        Err(error) if error.code == ErrorCode::Enosys => driver.stat(path).await,
        result => result,
    }
}

pub struct FuseSession {
    pub inodes: InodeTable,
    driver: Arc<dyn FsDriver>,
    handles: HashMap<u64, Arc<dyn FileHandle>>,
    handle_nodes: HashMap<u64, u64>,
    inode_handles: HashMap<u64, Vec<u64>>,
    locks: HashMap<u64, Vec<HeldLock>>,
    directories: HashMap<u64, (u64, DirectorySnapshot)>,
    next_handle: u64,
    pub max_request: usize,
    pub negotiated: Option<crate::init::InitReply>,
    session_options: FuseSessionOptions,
    destroyed: bool,
    last_error: Option<FsError>,
}

/// Construction-time controls for the mount-free FUSE dispatcher.
///
/// The options are owned by the Rust session so callers can qualify the
/// protocol boundary without opening a native device. Native mount adapters
/// may choose a subset of these values, while embedding facades can expose
/// the complete structured policy.
#[derive(Debug, Clone)]
pub struct FuseSessionOptions {
    pub max_request: usize,
    pub use_driver_ino: bool,
    pub init: crate::init::Preferences,
    pub attr_timeout: std::time::Duration,
    pub entry_timeout: std::time::Duration,
    pub negative_timeout: std::time::Duration,
    pub keep_cache: bool,
    pub flush_mechanism: FuseFlushMechanism,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuseFlushMechanism {
    /// Synchronize durable handles, preserving the established dispatcher
    /// contract.
    Sync,
    /// Return `ENOSYS` for durable-handle `FLUSH`.
    Enosys,
    /// Advertise `FOPEN_NOFLUSH` for durable drivers and treat `FLUSH` as a
    /// protocol-level no-op after validating the handle.
    Noflush,
}

impl Default for FuseSessionOptions {
    fn default() -> Self {
        // Keep the default advertisement aligned with the dispatcher. In
        // particular, do not claim POSIX locks, xattrs, or other operations
        // that still have an explicit ENOSYS session boundary.
        let init = crate::init::Preferences {
            flags: (1 << 0) | (1 << 3) | (1 << 5) | (1 << 12) | (1 << 13) | (1 << 14) | (1 << 22),
            ..crate::init::Preferences::default()
        };
        Self {
            max_request: 1024 * 1024,
            use_driver_ino: true,
            init,
            attr_timeout: std::time::Duration::from_secs(10),
            entry_timeout: std::time::Duration::from_secs(10),
            negative_timeout: std::time::Duration::ZERO,
            keep_cache: true,
            flush_mechanism: FuseFlushMechanism::Sync,
        }
    }
}
fn u32_at(b: &[u8], offset: usize) -> Result<u32> {
    b.get(offset..offset + 4)
        .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
        .ok_or_else(|| FsError::new(ErrorCode::Einval))
}
fn string(bytes: &[u8]) -> Result<(&str, &[u8])> {
    let end = bytes
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| FsError::new(ErrorCode::Einval))?;
    let value = std::str::from_utf8(&bytes[..end]).map_err(|_| FsError::new(ErrorCode::Einval))?;
    Ok((value, &bytes[end + 1..]))
}
fn u64_at(b: &[u8], offset: usize) -> Result<u64> {
    b.get(offset..offset + 8)
        .map(|v| u64::from_le_bytes(v.try_into().unwrap()))
        .ok_or_else(|| FsError::new(ErrorCode::Einval))
}
fn validate_body(opcode: u32, body: &[u8], context: Option<ProtocolContext>) -> Result<()> {
    let exact = match opcode {
        2 => Some(8),
        3 => Some(16),
        4 => Some(88),
        5 | 17 | 38 => Some(0),
        14 | 27 | 34 => Some(8),
        15 | 28 | 44 => Some(40),
        18 | 25 | 29 => Some(24),
        20 | 30 => Some(16),
        FUSE_GETLK | FUSE_SETLK | FUSE_SETLKW => Some(48),
        FUSE_INTERRUPT => Some(8),
        FUSE_POLL => Some(24),
        FUSE_FALLOCATE => Some(32),
        FUSE_LSEEK => Some(24),
        FUSE_COPY_FILE_RANGE => Some(56),
        _ => None,
    };
    if exact.is_some_and(|size| body.len() != size) {
        return Err(FsError::new(ErrorCode::Einval));
    }
    if opcode == FUSE_IOCTL {
        let input_size =
            usize::try_from(u32_at(body, 24)?).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let expected = FUSE_IOCTL_IN_SIZE
            .checked_add(input_size)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        if body.len() != expected {
            return Err(FsError::new(ErrorCode::Einval));
        }
    }
    if matches!(
        opcode,
        FUSE_BMAP | FUSE_GETXATTR | FUSE_LISTXATTR | FUSE_SETXATTR
    ) {
        decode_request_body(opcode, body, context)
            .map(|_| ())
            .map_err(|_| FsError::new(ErrorCode::Einval))?;
    }
    if opcode == FUSE_BATCH_FORGET {
        if body.len() < 8 {
            return Err(FsError::new(ErrorCode::Einval));
        }
        let count =
            usize::try_from(u32_at(body, 0)?).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let entries = count
            .checked_mul(16)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let expected = 8usize
            .checked_add(entries)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        if body.len() != expected {
            return Err(FsError::new(ErrorCode::Einval));
        }
    }
    if opcode == 16 && (body.len() < 40 || body.len() - 40 != u32_at(body, 16)? as usize) {
        return Err(FsError::new(ErrorCode::Einval));
    }
    let names = match opcode {
        1 | 10 | 11 | FUSE_REMOVEXATTR => Some((0, 1)),
        6 => Some((0, 2)),
        9 | 13 => Some((8, 1)),
        12 => Some((8, 2)),
        8 | 35 => Some((16, 1)),
        FUSE_RENAME2 => Some((16, 2)),
        _ => None,
    };
    if let Some((offset, count)) = names {
        let mut rest = body
            .get(offset..)
            .ok_or_else(|| FsError::new(ErrorCode::Einval))?;
        for _ in 0..count {
            rest = string(rest)?.1;
        }
        if !rest.is_empty() {
            return Err(FsError::new(ErrorCode::Einval));
        }
    }
    Ok(())
}

fn encode_wire_reply(
    opcode: u32,
    body: &FuseReplyBody,
    context: ProtocolContext,
) -> Result<Vec<u8>> {
    crate::protocol::encode_reply_body(opcode, body, Some(context))
        .map_err(|error| FsError::backend(format!("FUSE reply encoding failed: {error}")))
}

/// Apply the Linux `access(2)` permission check to driver metadata. FUSE
/// supplies the calling uid/gid in the request header, while the driver
/// supplies the file owner, group, and mode. Supplementary groups are not
/// guessed because this session does not negotiate or receive them.
fn check_access(stats: &Stats, uid: u32, gid: u32, mask: u32) -> Result<()> {
    const R_OK: u32 = 4;
    const W_OK: u32 = 2;
    const X_OK: u32 = 1;
    const ALLOWED: u32 = R_OK | W_OK | X_OK;

    if mask & !ALLOWED != 0 {
        return Err(FsError::new(ErrorCode::Einval));
    }
    if mask == 0 {
        return Ok(());
    }

    // POSIX root bypasses read/write permission bits. Execute still needs at
    // least one execute bit, matching access(2)'s root rule.
    if uid == 0 {
        if mask & X_OK != 0 && stats.mode & 0o111 == 0 {
            return Err(FsError::new(ErrorCode::Eacces));
        }
        return Ok(());
    }

    let permissions = if uid == stats.uid {
        (stats.mode >> 6) & 0o7
    } else if gid == stats.gid {
        (stats.mode >> 3) & 0o7
    } else {
        stats.mode & 0o7
    };
    if permissions & mask == mask {
        Ok(())
    } else {
        Err(FsError::new(ErrorCode::Eacces))
    }
}
fn attr(id: u64, s: &Stats) -> Vec<u8> {
    let mut b = Vec::with_capacity(88);
    for v in [
        if s.ino > 0 { s.ino } else { id },
        s.size,
        s.blocks,
        s.atime_ms.div_euclid(1000) as u64,
        s.mtime_ms.div_euclid(1000) as u64,
        s.ctime_ms.div_euclid(1000) as u64,
    ] {
        b.extend(v.to_le_bytes());
    }
    for v in [
        (s.atime_ms.rem_euclid(1000) * 1_000_000) as u32,
        (s.mtime_ms.rem_euclid(1000) * 1_000_000) as u32,
        (s.ctime_ms.rem_euclid(1000) * 1_000_000) as u32,
        s.mode,
        s.nlink as u32,
        s.uid,
        s.gid,
        s.rdev as u32,
        s.blksize as u32,
        0,
    ] {
        b.extend(v.to_le_bytes());
    }
    b
}
impl FuseSession {
    fn protocol_context(&self) -> ProtocolContext {
        let negotiated = self.negotiated.as_ref();
        ProtocolContext {
            minor: negotiated
                .map(|reply| reply.minor)
                .unwrap_or(FUSE_KERNEL_MINOR_VERSION),
            setxattr_ext: negotiated.is_some_and(|reply| reply.flags & FUSE_SETXATTR_EXT != 0),
        }
    }

    fn write_attr_timeout(body: &mut [u8], timeout: std::time::Duration) {
        body[..8].copy_from_slice(&timeout.as_secs().to_le_bytes());
        body[8..12].copy_from_slice(&timeout.subsec_nanos().to_le_bytes());
    }

    fn write_entry_timeouts(
        body: &mut [u8],
        entry_timeout: std::time::Duration,
        attr_timeout: std::time::Duration,
    ) {
        body[16..24].copy_from_slice(&entry_timeout.as_secs().to_le_bytes());
        body[24..32].copy_from_slice(&attr_timeout.as_secs().to_le_bytes());
        body[32..36].copy_from_slice(&entry_timeout.subsec_nanos().to_le_bytes());
        body[36..40].copy_from_slice(&attr_timeout.subsec_nanos().to_le_bytes());
    }

    fn write_negative_timeout(body: &mut [u8], timeout: std::time::Duration) {
        body[16..24].copy_from_slice(&timeout.as_secs().to_le_bytes());
        body[32..36].copy_from_slice(&timeout.subsec_nanos().to_le_bytes());
    }

    fn open_flags(&self) -> u32 {
        let mut flags = if self.session_options.keep_cache {
            FOPEN_KEEP_CACHE
        } else {
            0
        };
        if self.session_options.flush_mechanism == FuseFlushMechanism::Noflush
            && self.driver.capabilities().durable_writes
            && self
                .negotiated
                .as_ref()
                .is_some_and(|reply| reply.minor >= 35)
        {
            flags |= FOPEN_NOFLUSH;
        }
        flags
    }

    async fn created_entry(
        &mut self,
        path: &str,
        header: &crate::RequestHeader,
    ) -> Result<Vec<u8>> {
        match self.driver.lchown(path, header.uid, header.gid).await {
            Err(error)
                if matches!(
                    error.code,
                    ErrorCode::Enosys | ErrorCode::Eperm | ErrorCode::Enotsup
                ) => {}
            result => result?,
        }
        self.entry(path).await
    }
    fn register_handle(&mut self, id: u64, inode: u64, handle: Arc<dyn FileHandle>) {
        self.handles.insert(id, handle);
        self.handle_nodes.insert(id, inode);
        self.inode_handles.entry(inode).or_default().push(id);
    }

    fn lock_request(body: &[u8]) -> Result<FuseLkIn> {
        Ok(FuseLkIn {
            fh: u64_at(body, 0)?,
            owner: u64_at(body, 8)?,
            lk: FuseFileLock {
                start: u64_at(body, 16)?,
                end: u64_at(body, 24)?,
                type_: u32_at(body, 32)?,
                pid: u32_at(body, 36)?,
            },
            lk_flags: u32_at(body, 40)?,
        })
    }

    fn lock_node(&self, fh: u64) -> Result<u64> {
        self.handle_nodes
            .get(&fh)
            .copied()
            .ok_or_else(|| FsError::new(ErrorCode::Ebadf))
    }

    fn validate_lock_request(request: FuseLkIn) -> Result<(u128, u128)> {
        if request.lk_flags & !FUSE_LK_FLOCK != 0 {
            return Err(FsError::new(ErrorCode::Einval).with_message("unsupported FUSE lock flags"));
        }
        lock_range(request.lk)
    }

    fn get_lock(&self, request: FuseLkIn) -> Result<FuseFileLock> {
        let (start, end) = Self::validate_lock_request(request)?;
        let node = self.lock_node(request.fh)?;
        let conflict = self
            .locks
            .get(&node)
            .into_iter()
            .flatten()
            .copied()
            .find(|held| lock_conflicts(*held, request, start, end));
        Ok(conflict.map_or(
            FuseFileLock {
                start: request.lk.start,
                end: request.lk.end,
                type_: F_UNLCK,
                pid: 0,
            },
            wire_lock,
        ))
    }

    fn set_lock(&mut self, request: FuseLkIn, blocking: bool) -> Result<()> {
        let (start, end) = Self::validate_lock_request(request)?;
        let node = self.lock_node(request.fh)?;
        let existing = self.locks.get(&node).cloned().unwrap_or_default();
        if request.lk.type_ != F_UNLCK
            && existing
                .iter()
                .copied()
                .any(|held| lock_conflicts(held, request, start, end))
        {
            return Err(FsError::new(ErrorCode::Eagain).with_message(if blocking {
                "blocking FUSE lock cannot wait on a serialized session"
            } else {
                "FUSE lock conflicts with another owner"
            }));
        }

        let mut updated = Vec::with_capacity(existing.len() + 1);
        for held in existing {
            if held.owner != request.owner || !ranges_overlap(held.start, held.end, start, end) {
                updated.push(held);
                continue;
            }
            if held.start < start {
                updated.push(HeldLock { end: start, ..held });
            }
            if held.end > end {
                updated.push(HeldLock { start: end, ..held });
            }
        }
        if request.lk.type_ != F_UNLCK {
            updated.push(HeldLock {
                start,
                end,
                owner: request.owner,
                pid: request.lk.pid,
                type_: request.lk.type_,
            });
        }

        updated.sort_by_key(|held| (held.start, held.end, held.owner, held.type_));
        let mut coalesced: Vec<HeldLock> = Vec::with_capacity(updated.len());
        for held in updated {
            if let Some(previous) = coalesced.last_mut()
                && previous.end >= held.start
                && previous.owner == held.owner
                && previous.pid == held.pid
                && previous.type_ == held.type_
            {
                previous.end = previous.end.max(held.end);
            } else {
                coalesced.push(held);
            }
        }
        if coalesced.is_empty() {
            self.locks.remove(&node);
        } else {
            self.locks.insert(node, coalesced);
        }
        Ok(())
    }

    fn release_locks_for_node(&mut self, node: u64, owner: u64) {
        let empty = if let Some(locks) = self.locks.get_mut(&node) {
            locks.retain(|held| held.owner != owner);
            locks.is_empty()
        } else {
            false
        };
        if empty {
            self.locks.remove(&node);
        }
    }

    fn metadata_handle(
        &self,
        inode: u64,
        explicit: Option<u64>,
    ) -> Result<Option<Arc<dyn FileHandle>>> {
        let node = self
            .inodes
            .get(inode)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        if let Some(handle) = explicit.and_then(|fh| self.handles.get(&fh)) {
            return Ok(Some(handle.clone()));
        }
        if !node.paths.is_empty() {
            return Ok(None);
        }
        Ok(self
            .inode_handles
            .get(&inode)
            .and_then(|ids| ids.iter().find_map(|id| self.handles.get(id)))
            .cloned())
    }
    fn child(&self, parent: u64, name: &str) -> Result<String> {
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(FsError::new(ErrorCode::Einval));
        }
        // `str::len()` counts UTF-8 bytes, which is the POSIX limit here.
        if name.len() > NAME_MAX {
            return Err(FsError::new(ErrorCode::Enametoolong));
        }
        Ok(format!(
            "{}/{name}",
            self.inodes.require_path(parent)?.trim_end_matches('/')
        ))
    }
    async fn entry(&mut self, path: &str) -> Result<Vec<u8>> {
        let stats = stat_of(self.driver.as_ref(), path).await?;
        let id = self.inodes.bind(path, &stats);
        self.inodes.acquire(id)?;
        let mut body = vec![0; 40];
        body[..8].copy_from_slice(&id.to_le_bytes());
        Self::write_entry_timeouts(
            &mut body,
            self.session_options.entry_timeout,
            self.session_options.attr_timeout,
        );
        body.extend(attr(id, &stats));
        Ok(body)
    }
    pub fn new(driver: Arc<dyn FsDriver>) -> Self {
        Self::with_options(driver, FuseSessionOptions::default())
    }

    pub fn with_options(driver: Arc<dyn FsDriver>, options: FuseSessionOptions) -> Self {
        Self {
            driver,
            inodes: InodeTable::new(options.use_driver_ino),
            handles: HashMap::new(),
            handle_nodes: HashMap::new(),
            inode_handles: HashMap::new(),
            locks: HashMap::new(),
            directories: HashMap::new(),
            next_handle: 1,
            max_request: options.max_request,
            negotiated: None,
            session_options: options,
            destroyed: false,
            last_error: None,
        }
    }

    /// Return the immutable policy used by this session.
    pub fn options(&self) -> &FuseSessionOptions {
        &self.session_options
    }

    /// Take the structured error that produced the most recent negative
    /// reply, if any. The wire reply only carries a POSIX errno; this view
    /// preserves the driver-owned path/syscall context for embeddings.
    pub fn take_last_error(&mut self) -> Option<FsError> {
        self.last_error.take()
    }

    /// Whether the session has been torn down by `DESTROY` or its owner.
    pub fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    /// Number of live file and directory handles owned by this session.
    pub fn open_handles(&self) -> usize {
        self.handles.len() + self.directories.len()
    }

    /// Prepare a positional READ without retaining mutable session state over
    /// the backend future. Invalid or not-yet-ready reads return `None` so the
    /// normal serialized dispatcher can produce the canonical error reply.
    #[cfg(target_os = "linux")]
    pub(crate) fn prepare_read(
        &self,
        bytes: &[u8],
    ) -> std::result::Result<Option<PreparedRead>, crate::ProtocolError> {
        let request = Request::decode(bytes, self.max_request)?;
        if request.header.opcode != FUSE_READ
            || request.header.unique == 0
            || self.negotiated.is_none()
            || self.destroyed
            || validate_body(FUSE_READ, request.body, None).is_err()
        {
            return Ok(None);
        }
        let handle_id = match u64_at(request.body, 0) {
            Ok(handle_id) => handle_id,
            Err(_) => return Ok(None),
        };
        let offset = match u64_at(request.body, 8) {
            Ok(offset) => offset,
            Err(_) => return Ok(None),
        };
        let Ok(raw_size) = u32_at(request.body, 16) else {
            return Ok(None);
        };
        let Ok(size) = usize::try_from(raw_size) else {
            return Ok(None);
        };
        if size > self.max_request {
            return Ok(None);
        }
        let Some(handle) = self.handles.get(&handle_id).cloned() else {
            return Ok(None);
        };
        Ok(Some(PreparedRead {
            unique: request.header.unique,
            handle,
            offset,
            size,
        }))
    }

    /// Returns no frame for FORGET and BATCH_FORGET. Malformed FORGET bodies
    /// are ignored to match the pinned no-reply oracle; malformed
    /// BATCH_FORGET bodies remain rejected before changing inode state.
    pub async fn handle(
        &mut self,
        bytes: &[u8],
    ) -> std::result::Result<Option<Vec<u8>>, crate::ProtocolError> {
        self.last_error = None;
        let request = Request::decode(bytes, self.max_request)?;
        if request.header.opcode == FUSE_FORGET {
            // FORGET has no reply, and the pinned mountx session ignores a
            // malformed body rather than surfacing an unanswerable error.
            if let Ok(FuseRequestBody::Forget(value)) =
                decode_request_body(FUSE_FORGET, request.body, None)
            {
                self.inodes.forget(request.header.nodeid, value.nlookup);
            }
            return Ok(None);
        }
        if request.header.opcode == FUSE_BATCH_FORGET {
            // BATCH_FORGET is also no-reply, but its count and fixed-width
            // records must be validated before applying any inode changes.
            validate_body(
                FUSE_BATCH_FORGET,
                request.body,
                Some(self.protocol_context()),
            )
            .map_err(|error| {
                crate::ProtocolError::new(format!(
                    "BATCH_FORGET body validation failed: {}",
                    error.code.as_str()
                ))
            })?;
            let decoded = decode_request_body(FUSE_BATCH_FORGET, request.body, None)?;
            if let FuseRequestBody::BatchForget(value) = decoded {
                for forget in value.forgets {
                    self.inodes.forget(forget.nodeid, forget.nlookup);
                }
            } else {
                unreachable!("batch-forget opcode decoded to another body");
            }
            return Ok(None);
        }
        if request.header.unique == 0 || request.header.opcode == FUSE_NOTIFY_REPLY {
            return Ok(None);
        }
        if let Err(error) = validate_body(
            request.header.opcode,
            request.body,
            Some(self.protocol_context()),
        ) {
            self.last_error = Some(error.clone());
            return Ok(Some(
                error_reply(request.header.unique, error.code).to_vec(),
            ));
        }
        let unique = request.header.unique;
        match self.dispatch(request).await {
            Ok(body) => {
                let mut reply = Vec::with_capacity(16 + body.len());
                reply.extend(((16 + body.len()) as u32).to_le_bytes());
                reply.extend(0i32.to_le_bytes());
                reply.extend(unique.to_le_bytes());
                reply.extend(body);
                Ok(Some(reply))
            }
            Err(error) => {
                self.last_error = Some(error.clone());
                Ok(Some(error_reply(unique, error.code).to_vec()))
            }
        }
    }
    async fn dispatch(&mut self, r: Request<'_>) -> Result<Vec<u8>> {
        if r.header.opcode == 26 {
            if self.negotiated.is_some() {
                return Err(FsError::new(ErrorCode::Eio));
            }
            let kernel = crate::init::KernelInit {
                major: u32_at(r.body, 0)?,
                minor: u32_at(r.body, 4)?,
                max_readahead: u32_at(r.body, 8)?,
                flags: u32_at(r.body, 12)?,
                flags2: if r.body.len() >= 20 {
                    u32_at(r.body, 16)?
                } else {
                    0
                },
            };
            // Older layouts are still being ported. Never silently parse them
            // with the modern request offsets used by this dispatcher.
            if kernel.major == 7 && kernel.minor < 12 {
                return Err(FsError::new(ErrorCode::Eproto));
            }
            let mut preferences = self.session_options.init.clone();
            preferences.max_write = preferences
                .max_write
                .min(self.max_request.saturating_sub(80).min(u32::MAX as usize) as u32);
            return match crate::init::negotiate(kernel, &preferences) {
                crate::init::Negotiation::UnsupportedMajor => Err(FsError::new(ErrorCode::Eproto)),
                crate::init::Negotiation::Retry(reply) => Ok(reply.encode()),
                crate::init::Negotiation::Ready(reply) => {
                    let bytes = reply.encode();
                    self.negotiated = Some(reply);
                    Ok(bytes)
                }
            };
        }
        if self.negotiated.is_none() {
            return Err(FsError::new(ErrorCode::Eio));
        }
        if self.destroyed {
            return Err(FsError::new(ErrorCode::Enodev));
        }
        match r.header.opcode {
            8 => {
                let mode = u32_at(r.body, 0)?;
                let dev = u64::from(u32_at(r.body, 4)?);
                let (name, _) = string(&r.body[16..])?;
                let path = self.child(r.header.nodeid, name)?;
                match self.driver.mknod(&path, mode, dev).await {
                    Err(error)
                        if error.code == ErrorCode::Enosys
                            && (mode & 0o170000 == 0 || mode & 0o170000 == 0o100000) =>
                    {
                        self.driver
                            .open(&path, "wx", mode & 0o7777)
                            .await?
                            .close()
                            .await?;
                    }
                    result => result?,
                }
                self.created_entry(&path, &r.header).await
            }
            25 => {
                let handle = self
                    .handles
                    .get(&u64_at(r.body, 0)?)
                    .ok_or_else(|| FsError::new(ErrorCode::Ebadf))?;
                if self.driver.capabilities().durable_writes {
                    match self.session_options.flush_mechanism {
                        FuseFlushMechanism::Sync => handle.sync().await?,
                        FuseFlushMechanism::Enosys => return Err(FsError::enosys("flush")),
                        FuseFlushMechanism::Noflush => {}
                    }
                }
                Ok(vec![])
            }
            30 => {
                if !self.directories.contains_key(&u64_at(r.body, 0)?) {
                    return Err(FsError::new(ErrorCode::Ebadf));
                }
                self.driver.syncfs().await?;
                Ok(vec![])
            }
            4 => {
                if r.body.len() != 88 {
                    return Err(FsError::new(ErrorCode::Einval));
                }
                let valid = u32_at(r.body, 0)?;
                let handle = self.metadata_handle(
                    r.header.nodeid,
                    if valid & 64 != 0 {
                        Some(u64_at(r.body, 8)?)
                    } else {
                        None
                    },
                )?;
                if valid & 1 != 0 {
                    self.driver
                        .chmod(
                            self.inodes.require_path(r.header.nodeid)?,
                            u32_at(r.body, 68)? & 0o7777,
                        )
                        .await?;
                }
                if valid & 6 != 0 {
                    let path = self.inodes.require_path(r.header.nodeid)?;
                    let uid = if valid & 2 != 0 {
                        u32_at(r.body, 76)?
                    } else {
                        u32::MAX
                    };
                    let gid = if valid & 4 != 0 {
                        u32_at(r.body, 80)?
                    } else {
                        u32::MAX
                    };
                    match self.driver.lchown(path, uid, gid).await {
                        Err(error) if error.code == ErrorCode::Enosys => {
                            self.driver.chown(path, uid, gid).await?
                        }
                        result => result?,
                    }
                }
                if valid & 8 != 0 {
                    let size = u64_at(r.body, 16)?;
                    if let Some(handle) = &handle {
                        handle.truncate(size).await?;
                    } else {
                        self.driver
                            .truncate(self.inodes.require_path(r.header.nodeid)?, size)
                            .await?;
                    }
                }
                if valid & (16 | 32 | 128 | 256) != 0 {
                    let path = self.inodes.require_path(r.header.nodeid)?;
                    let current = if valid & (16 | 128) == 0 || valid & (32 | 256) == 0 {
                        Some(stat_of(self.driver.as_ref(), path).await?)
                    } else {
                        None
                    };
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|_| FsError::new(ErrorCode::Eoverflow))?
                        .as_millis() as i128
                        * 1_000_000;
                    let time =
                        |bit, now_bit, seconds_at, nanos_at, previous: i64| -> Result<i128> {
                            if valid & now_bit != 0 {
                                return Ok(now);
                            }
                            if valid & bit == 0 {
                                return Ok(i128::from(previous) * 1_000_000);
                            }
                            let seconds = u64_at(r.body, seconds_at)? as i64;
                            let nanos = u32_at(r.body, nanos_at)?;
                            if nanos >= 1_000_000_000 {
                                return Err(FsError::new(ErrorCode::Einval));
                            }
                            Ok(i128::from(seconds) * 1_000_000_000 + i128::from(nanos))
                        };
                    let atime_ns =
                        time(16, 128, 32, 56, current.as_ref().map_or(0, |s| s.atime_ms))?;
                    let mtime_ns =
                        time(32, 256, 40, 60, current.as_ref().map_or(0, |s| s.mtime_ms))?;
                    if self.driver.has_utimens() {
                        self.driver.utimens(path, atime_ns, mtime_ns, true).await?;
                    } else {
                        let atime = i64::try_from(atime_ns.div_euclid(1_000_000))
                            .map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
                        let mtime = i64::try_from(mtime_ns.div_euclid(1_000_000))
                            .map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
                        match self.driver.lutimes(path, atime, mtime).await {
                            Err(error) if error.code == ErrorCode::Enosys => {
                                self.driver.utimes(path, atime, mtime).await?
                            }
                            result => result?,
                        }
                    }
                }
                let stats = if let Some(handle) = handle {
                    handle.stat().await?
                } else {
                    stat_of(
                        self.driver.as_ref(),
                        self.inodes.require_path(r.header.nodeid)?,
                    )
                    .await?
                };
                let mut body = vec![0; 16];
                Self::write_attr_timeout(&mut body, self.session_options.attr_timeout);
                body.extend(attr(r.header.nodeid, &stats));
                Ok(body)
            }
            38 => {
                self.destroy().await;
                Ok(vec![])
            }
            27 => {
                u32_at(r.body, 0)?;
                self.inodes.require_path(r.header.nodeid)?;
                let id = self.next_handle;
                self.next_handle += 1;
                self.directories.insert(id, (r.header.nodeid, None));
                let mut body = id.to_le_bytes().to_vec();
                body.extend([0; 8]);
                Ok(body)
            }
            29 => {
                self.directories
                    .remove(&u64_at(r.body, 0)?)
                    .ok_or_else(|| FsError::new(ErrorCode::Ebadf))?;
                Ok(vec![])
            }
            28 | 44 => {
                let plus = r.header.opcode == 44;
                let fh = u64_at(r.body, 0)?;
                let start = usize::try_from(u64_at(r.body, 8)?)
                    .map_err(|_| FsError::new(ErrorCode::Einval))?;
                let budget = (u32_at(r.body, 16)? as usize).min(self.max_request);
                let entry_timeout = self.session_options.entry_timeout;
                let attr_timeout = self.session_options.attr_timeout;
                let (inode, snapshot) = self
                    .directories
                    .get_mut(&fh)
                    .ok_or_else(|| FsError::new(ErrorCode::Ebadf))?;
                let path = self.inodes.require_path(*inode)?.to_owned();
                if start == 0 || snapshot.is_none() {
                    let mut entries = vec![(".".to_owned(), 4), ("..".to_owned(), 4)];
                    entries.extend(
                        self.driver
                            .readdir(&path)
                            .await?
                            .into_iter()
                            .filter(|e| e.name != "." && e.name != "..")
                            .map(|e| (e.name, e.file_type.mode_bits() >> 12)),
                    );
                    *snapshot = Some(entries);
                }
                let mut body = Vec::new();
                for (index, (name, kind)) in
                    snapshot.as_ref().unwrap().iter().enumerate().skip(start)
                {
                    let size = ((24 + name.len() + 7) & !7) + if plus { 128 } else { 0 };
                    if size > budget - body.len() {
                        break;
                    }
                    let mut entry = vec![0; if plus { 128 } else { 0 }];
                    let (ino, kind) = if name == "." || name == ".." {
                        (*inode, *kind)
                    } else {
                        let child = format!("{}/{name}", path.trim_end_matches('/'));
                        match stat_of(self.driver.as_ref(), &child).await {
                            Ok(stats) => {
                                if plus {
                                    let id = self.inodes.bind(&child, &stats);
                                    entry[..8].copy_from_slice(&id.to_le_bytes());
                                    Self::write_entry_timeouts(
                                        &mut entry,
                                        entry_timeout,
                                        attr_timeout,
                                    );
                                    entry[40..].copy_from_slice(&attr(id, &stats));
                                    self.inodes.acquire(id)?;
                                }
                                (
                                    if stats.ino > 0 {
                                        stats.ino
                                    } else {
                                        self.inodes.bind(&child, &stats)
                                    },
                                    (stats.mode & 0o170000) >> 12,
                                )
                            }
                            Err(_) => (0, *kind),
                        }
                    };
                    let begin = body.len();
                    body.extend(entry);
                    body.extend(ino.to_le_bytes());
                    body.extend(((index + 1) as u64).to_le_bytes());
                    body.extend((name.len() as u32).to_le_bytes());
                    body.extend(kind.to_le_bytes());
                    body.extend(name.as_bytes());
                    body.resize(begin + size, 0);
                }
                Ok(body)
            }
            FUSE_STATFS => {
                let path = self
                    .inodes
                    .require_path(r.header.nodeid)
                    .unwrap_or("/")
                    .to_owned();
                let stats = self.driver.statfs(&path).await?;
                let block = if stats.block_size == 0 {
                    4096
                } else {
                    stats.block_size as u32
                };
                encode_wire_reply(
                    FUSE_STATFS,
                    &FuseReplyBody::Statfs(FuseKstatfs {
                        blocks: stats.blocks,
                        bfree: stats.blocks_free,
                        bavail: stats.blocks_available,
                        files: stats.files,
                        ffree: stats.files_free,
                        bsize: block,
                        namelen: NAME_MAX as u32,
                        frsize: block,
                    }),
                    self.protocol_context(),
                )
            }
            34 => {
                let path = self.inodes.require_path(r.header.nodeid)?;
                // ACCESS follows symbolic links. Drivers that only expose
                // lstat retain the explicit fallback used by this transport.
                let stats = match self.driver.stat(path).await {
                    Err(error) if error.code == ErrorCode::Enosys => {
                        self.driver.lstat(path).await?
                    }
                    result => result?,
                };
                check_access(&stats, r.header.uid, r.header.gid, u32_at(r.body, 0)?)?;
                Ok(vec![])
            }
            FUSE_GETLK => {
                let request = Self::lock_request(r.body)?;
                let lock = self.get_lock(request)?;
                encode_wire_reply(
                    FUSE_GETLK,
                    &FuseReplyBody::Lk(FuseLkOut { lk: lock }),
                    self.protocol_context(),
                )
            }
            FUSE_SETLK | FUSE_SETLKW => {
                let request = Self::lock_request(r.body)?;
                self.set_lock(request, r.header.opcode == FUSE_SETLKW)?;
                Ok(vec![])
            }
            FUSE_INTERRUPT => {
                let target_unique = u64_at(r.body, 0)?;
                // This request pump is deliberately serial and has no
                // in-flight registry. Never guess which operation an
                // interrupt refers to or cancel a reused unique ID. FUSE
                // permits EAGAIN when the original request cannot be found.
                Err(FsError::new(ErrorCode::Eagain).with_message(format!(
                    "FUSE_INTERRUPT target {target_unique} is not safely cancellable"
                )))
            }
            FUSE_POLL => {
                // FsDriver has no readiness/poll capability yet. Keep this
                // operation explicitly unsupported after validating its
                // fixed wire body; libfuse treats ENOSYS as a successful
                // default poll result and will not keep sending POLL calls.
                Err(FsError::enosys("poll"))
            }
            FUSE_IOCTL => Err(FsError::enosys(crate::constants::opcode_name(FUSE_IOCTL))),
            FUSE_FALLOCATE | FUSE_LSEEK | FUSE_COPY_FILE_RANGE => {
                // These operations have no corresponding FsDriver capability
                // yet. Their wire bodies were validated before dispatch, so a
                // well-formed request receives an explicit unsupported result
                // without mutating handles, inodes, or backend contents.
                Err(FsError::enosys(crate::constants::opcode_name(
                    r.header.opcode,
                )))
            }
            20 => {
                let handle = self
                    .handles
                    .get(&u64_at(r.body, 0)?)
                    .ok_or_else(|| FsError::new(ErrorCode::Ebadf))?;
                if u32_at(r.body, 8)? & 1 != 0 {
                    handle.datasync().await?;
                } else {
                    handle.sync().await?;
                }
                Ok(vec![])
            }
            35 => {
                let flags = open_flags(u32_at(r.body, 0)?);
                let mode = u32_at(r.body, 4)? & 0o7777;
                let (name, _) = string(
                    r.body
                        .get(16..)
                        .ok_or_else(|| FsError::new(ErrorCode::Einval))?,
                )?;
                let path = self.child(r.header.nodeid, name)?;
                let handle = self.driver.open_flags(&path, flags, mode).await?;
                let mut body = match self.created_entry(&path, &r.header).await {
                    Ok(body) => body,
                    Err(error) => {
                        let _ = handle.close().await;
                        return Err(error);
                    }
                };
                let id = self.next_handle;
                self.next_handle += 1;
                self.register_handle(id, u64_at(&body, 0)?, handle);
                body.extend(id.to_le_bytes());
                body.extend(self.open_flags().to_le_bytes());
                body.extend([0; 4]);
                Ok(body)
            }
            FUSE_READLINK => {
                let path = self.inodes.require_path(r.header.nodeid)?.to_owned();
                let target = self.driver.readlink(&path).await?;
                encode_wire_reply(
                    FUSE_READLINK,
                    &FuseReplyBody::Readlink(FuseReadlinkOut { target }),
                    self.protocol_context(),
                )
            }
            6 => {
                let (name, rest) = string(r.body)?;
                let (target, _) = string(rest)?;
                let path = self.child(r.header.nodeid, name)?;
                self.driver.symlink(target, &path).await?;
                self.created_entry(&path, &r.header).await
            }
            9 => {
                let mode = u32_at(r.body, 0)? & 0o7777;
                let (name, _) = string(
                    r.body
                        .get(8..)
                        .ok_or_else(|| FsError::new(ErrorCode::Einval))?,
                )?;
                let path = self.child(r.header.nodeid, name)?;
                self.driver
                    .mkdir(
                        &path,
                        MkdirOptions {
                            recursive: false,
                            mode: Some(mode),
                        },
                    )
                    .await?;
                self.created_entry(&path, &r.header).await
            }
            10 | 11 => {
                let (name, _) = string(r.body)?;
                let path = self.child(r.header.nodeid, name)?;
                if r.header.opcode == 10 {
                    self.driver.unlink(&path).await?;
                } else {
                    self.driver.rmdir(&path).await?;
                }
                self.inodes.unbind(&path);
                Ok(vec![])
            }
            12 | FUSE_RENAME2 => {
                if r.header.opcode == FUSE_RENAME2
                    && u32_at(r.body, 8)? & RENAME2_UNSUPPORTED_FLAGS != 0
                {
                    return Err(FsError::enosys("rename2"));
                }
                let parent = u64_at(r.body, 0)?;
                let names = if r.header.opcode == FUSE_RENAME2 {
                    &r.body[16..]
                } else {
                    &r.body[8..]
                };
                let (old, rest) = string(names)?;
                let (new, _) = string(rest)?;
                let from = self.child(r.header.nodeid, old)?;
                let to = self.child(parent, new)?;
                self.driver.rename(&from, &to).await?;
                self.inodes.remap(&from, &to);
                Ok(vec![])
            }
            13 => {
                let old = self.inodes.require_path(u64_at(r.body, 0)?)?.to_owned();
                let (name, _) = string(&r.body[8..])?;
                let path = self.child(r.header.nodeid, name)?;
                self.driver.link(&old, &path).await?;
                self.entry(&path).await
            }
            1 => {
                let (name, _) = string(r.body)?;
                let path = self.child(r.header.nodeid, name)?;
                match self.entry(&path).await {
                    Ok(body) => Ok(body),
                    Err(error)
                        if error.code == ErrorCode::Enoent
                            && !self.session_options.negative_timeout.is_zero() =>
                    {
                        let mut body = vec![0; 128];
                        Self::write_negative_timeout(
                            &mut body,
                            self.session_options.negative_timeout,
                        );
                        Ok(body)
                    }
                    Err(error) => Err(error),
                }
            }
            3 => {
                let handle = self.metadata_handle(
                    r.header.nodeid,
                    if u32_at(r.body, 0)? & 1 != 0 {
                        Some(u64_at(r.body, 8)?)
                    } else {
                        None
                    },
                )?;
                let stats = if let Some(handle) = handle {
                    handle.stat().await?
                } else {
                    stat_of(
                        self.driver.as_ref(),
                        self.inodes.require_path(r.header.nodeid)?,
                    )
                    .await?
                };
                let mut body = vec![0; 16];
                Self::write_attr_timeout(&mut body, self.session_options.attr_timeout);
                body.extend(attr(r.header.nodeid, &stats));
                Ok(body)
            }
            14 => {
                let path = self.inodes.require_path(r.header.nodeid)?;
                let handle = self
                    .driver
                    .open_flags(path, open_flags(u32_at(r.body, 0)?), 0o666)
                    .await?;
                let id = self.next_handle;
                self.next_handle += 1;
                self.register_handle(id, r.header.nodeid, handle);
                let mut body = vec![0; 16];
                body[..8].copy_from_slice(&id.to_le_bytes());
                body[8..12].copy_from_slice(&self.open_flags().to_le_bytes());
                Ok(body)
            }
            15 | 16 => {
                let handle = self
                    .handles
                    .get(&u64_at(r.body, 0)?)
                    .ok_or_else(|| FsError::new(ErrorCode::Ebadf))?;
                let offset = u64_at(r.body, 8)?;
                let size = u32_at(r.body, 16)? as usize;
                if size > self.max_request {
                    return Err(FsError::new(ErrorCode::Einval));
                }
                if r.header.opcode == 15 {
                    let mut body = vec![0; size];
                    let count = handle.read(&mut body, Some(offset)).await?;
                    if count > size {
                        return Err(FsError::new(ErrorCode::Eio));
                    }
                    body.truncate(count);
                    Ok(body)
                } else {
                    let data = r
                        .body
                        .get(40..40 + size)
                        .ok_or_else(|| FsError::new(ErrorCode::Einval))?;
                    let count = handle.write(data, Some(offset)).await?;
                    if count > size {
                        return Err(FsError::new(ErrorCode::Eio));
                    }
                    let mut body = vec![0; 8];
                    body[..4].copy_from_slice(&(count as u32).to_le_bytes());
                    Ok(body)
                }
            }
            18 => {
                let id = u64_at(r.body, 0)?;
                let lock_owner = u64_at(r.body, 16)?;
                let handle = self
                    .handles
                    .remove(&id)
                    .ok_or_else(|| FsError::new(ErrorCode::Ebadf))?;
                if let Some(node) = self.handle_nodes.remove(&id) {
                    self.release_locks_for_node(node, lock_owner);
                    if let Some(ids) = self.inode_handles.get_mut(&node) {
                        ids.retain(|entry| *entry != id);
                        if ids.is_empty() {
                            self.inode_handles.remove(&node);
                        }
                    }
                }
                handle.close().await?;
                Ok(vec![])
            }
            _ => Err(FsError::enosys("fuse")),
        }
    }

    pub async fn destroy(&mut self) {
        self.destroyed = true;
        self.locks.clear();
        self.directories.clear();
        self.handle_nodes.clear();
        self.inode_handles.clear();
        for (_, handle) in self.handles.drain() {
            let _ = handle.close().await;
        }
        self.inodes = InodeTable::new(self.session_options.use_driver_ino);
    }
}
