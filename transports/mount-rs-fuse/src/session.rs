//! Driver-backed requests for modern FUSE (7.9+). Negotiation and mount
//! lifecycle are separate and remain under implementation.
use crate::{
    Request,
    constants::{
        FUSE_BATCH_FORGET, FUSE_COPY_FILE_RANGE, FUSE_FALLOCATE, FUSE_FORGET, FUSE_INTERRUPT,
        FUSE_KERNEL_MINOR_VERSION, FUSE_LSEEK, FUSE_NOTIFY_REPLY, FUSE_POLL, FUSE_READLINK,
        FUSE_RENAME2, FUSE_SETXATTR_EXT, FUSE_STATFS,
    },
    error_reply,
    inodes::InodeTable,
    open_flags,
    protocol::{
        FuseKstatfs, FuseReadlinkOut, FuseReplyBody, FuseRequestBody, ProtocolContext,
        decode_request_body,
    },
};
use mount_rs_core::{ErrorCode, FileHandle, FsDriver, FsError, MkdirOptions, Result, Stats};
use std::{collections::HashMap, sync::Arc};

type DirectorySnapshot = Option<Vec<(String, u32)>>;

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
    directories: HashMap<u64, (u64, DirectorySnapshot)>,
    next_handle: u64,
    pub max_request: usize,
    pub negotiated: Option<crate::init::InitReply>,
    destroyed: bool,
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
fn validate_body(opcode: u32, body: &[u8]) -> Result<()> {
    let exact = match opcode {
        2 => Some(8),
        3 => Some(16),
        4 => Some(88),
        5 | 17 | 38 => Some(0),
        14 | 27 | 34 => Some(8),
        15 | 28 | 44 => Some(40),
        18 | 25 | 29 => Some(24),
        20 | 30 => Some(16),
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
        1 | 10 | 11 => Some((0, 1)),
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
        body[16..24].copy_from_slice(&10u64.to_le_bytes());
        body[24..32].copy_from_slice(&10u64.to_le_bytes());
        body.extend(attr(id, &stats));
        Ok(body)
    }
    pub fn new(driver: Arc<dyn FsDriver>) -> Self {
        Self {
            driver,
            inodes: InodeTable::default(),
            handles: HashMap::new(),
            handle_nodes: HashMap::new(),
            inode_handles: HashMap::new(),
            directories: HashMap::new(),
            next_handle: 1,
            max_request: 1024 * 1024,
            negotiated: None,
            destroyed: false,
        }
    }
    /// Returns no frame for FORGET and BATCH_FORGET. Malformed no-reply frames
    /// return a protocol error before changing inode state.
    pub async fn handle(
        &mut self,
        bytes: &[u8],
    ) -> std::result::Result<Option<Vec<u8>>, crate::ProtocolError> {
        let request = Request::decode(bytes, self.max_request)?;
        if matches!(request.header.opcode, FUSE_FORGET | FUSE_BATCH_FORGET) {
            // These opcodes are explicitly no-reply operations. A malformed
            // frame cannot be answered without violating that contract, so
            // reject it before applying a partial or guessed forget list.
            validate_body(request.header.opcode, request.body).map_err(|error| {
                crate::ProtocolError::new(format!(
                    "{} body validation failed: {}",
                    crate::constants::opcode_name(request.header.opcode),
                    error.code.as_str()
                ))
            })?;
            let decoded = decode_request_body(request.header.opcode, request.body, None)?;
            match decoded {
                FuseRequestBody::Forget(value) => {
                    self.inodes.forget(request.header.nodeid, value.nlookup);
                }
                FuseRequestBody::BatchForget(value) => {
                    for forget in value.forgets {
                        self.inodes.forget(forget.nodeid, forget.nlookup);
                    }
                }
                _ => unreachable!("no-reply forget opcode decoded to another body"),
            }
            return Ok(None);
        }
        if request.header.unique == 0 || request.header.opcode == FUSE_NOTIFY_REPLY {
            return Ok(None);
        }
        if let Err(error) = validate_body(request.header.opcode, request.body) {
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
            Err(error) => Ok(Some(error_reply(unique, error.code).to_vec())),
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
            let preferences = crate::init::Preferences {
                // Advertise only operations the dispatcher currently supports.
                flags: (1 << 0)
                    | (1 << 3)
                    | (1 << 5)
                    | (1 << 12)
                    | (1 << 13)
                    | (1 << 14)
                    | (1 << 22),
                max_write: self.max_request.saturating_sub(80).min(u32::MAX as usize) as u32,
                ..Default::default()
            };
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
                    // FUSE_FLUSH is a per-open-handle close/flush point. It
                    // is intentionally distinct from FSYNC: durable drivers
                    // must make the handle's pending writes observable before
                    // acknowledging it, while volatile drivers may complete
                    // the protocol operation without backend work.
                    handle.sync().await?;
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
                body[..8].copy_from_slice(&10u64.to_le_bytes());
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
                                    entry[16..24].copy_from_slice(&10u64.to_le_bytes());
                                    entry[24..32].copy_from_slice(&10u64.to_le_bytes());
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
                        namelen: 255,
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
            FUSE_FALLOCATE | FUSE_RENAME2 | FUSE_LSEEK | FUSE_COPY_FILE_RANGE => {
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
                body.extend(2u32.to_le_bytes()); // FOPEN_KEEP_CACHE
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
            12 => {
                let parent = u64_at(r.body, 0)?;
                let (old, rest) = string(&r.body[8..])?;
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
                self.entry(&path).await
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
                body[..8].copy_from_slice(&10u64.to_le_bytes());
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
                body[8..12].copy_from_slice(&2u32.to_le_bytes()); // FOPEN_KEEP_CACHE
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
                let handle = self
                    .handles
                    .remove(&id)
                    .ok_or_else(|| FsError::new(ErrorCode::Ebadf))?;
                if let Some(node) = self.handle_nodes.remove(&id)
                    && let Some(ids) = self.inode_handles.get_mut(&node)
                {
                    ids.retain(|entry| *entry != id);
                    if ids.is_empty() {
                        self.inode_handles.remove(&node);
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
        self.directories.clear();
        self.handle_nodes.clear();
        self.inode_handles.clear();
        for (_, handle) in self.handles.drain() {
            let _ = handle.close().await;
        }
        self.inodes = InodeTable::default();
    }
}
