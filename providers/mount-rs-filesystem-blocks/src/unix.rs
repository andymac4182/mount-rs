use crate::MAX_BLOCK_BYTES;
use async_trait::async_trait;
use mount_rs_core::diagnostics::filesystem_blocks::{
    Observer, Operation as ObservedOperation, Path as PutPath, Span as ObservationSpan,
    Stage as MetricStage,
};
use mount_rs_core::storage::{BlockId, BlockStore, ConcurrentBackingId};
use mount_rs_core::{ErrorCode, FsError, Result};
use sha2::{Digest, Sha256};
use std::ffi::{CStr, CString};
use std::fs::{File, Metadata};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};
use std::sync::Arc;

const MARKER: &CStr = c"_mount-rs-backing-id-v1";
const MARKER_MAGIC: &[u8; 4] = b"MFB1";
const DIR_FLAGS: i32 = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
const FILE_FLAGS: i32 = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenBarrier {
    StagingFile,
    MarkerFile,
    RootDirectory,
    ParentDirectory,
    FinalDevice,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
}

impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
        }
    }
}

#[derive(Debug)]
struct Inner {
    root: File,
    root_components: Vec<CString>,
    root_identity: Identity,
    marker_identity: Identity,
    backing: ConcurrentBackingId,
    durable: bool,
    observer: Observer,
    #[cfg(test)]
    faults: test_support::Faults,
}

/// Immutable blocks shared by independent contexts using the same local root.
///
/// `open` requires an existing, effective-user-owned directory with mode 0700.
/// It synchronously establishes and durably reads its backing marker. Async
/// operations own their input and descriptors in a Tokio blocking task; dropping
/// an awaiting future cannot interrupt an admitted publication or its barriers.
#[derive(Clone, Debug)]
pub struct FilesystemBlockStore {
    inner: Arc<Inner>,
}

impl FilesystemBlockStore {
    pub fn open(root: impl AsRef<Path>, durable: bool) -> Result<Self> {
        Self::open_with_barriers(root.as_ref(), durable, |point, file| match point {
            OpenBarrier::StagingFile | OpenBarrier::MarkerFile => file_barrier(file),
            OpenBarrier::RootDirectory | OpenBarrier::ParentDirectory => {
                file.sync_all().map_err(io_error)
            }
            OpenBarrier::FinalDevice => final_device_barrier(file),
        })
    }

    fn open_with_barriers(
        root: &Path,
        durable: bool,
        mut barrier: impl FnMut(OpenBarrier, &File) -> Result<()>,
    ) -> Result<Self> {
        let components = root_components(root)?;
        let directory = walk_root(&components)?;
        let root_identity = checked_directory(&directory)?;
        let parent_components = &components[..components.len() - 1];
        let parent = walk_root(parent_components)?;
        let parent_identity = Identity::of(&parent.metadata().map_err(io_error)?);
        if parent_identity.dev != root_identity.dev {
            return Err(FsError::enotsup(
                "filesystem block root and its parent must share one filesystem",
            ));
        }
        let _initialization_lock = DirectoryLock::acquire(&directory)?;
        let marker = match open_at(&directory, MARKER, FILE_FLAGS, 0) {
            Ok(marker) => marker,
            Err(error) if error.is(ErrorCode::Enoent) => {
                if !directory_is_empty(&directory)? {
                    return Err(stale("filesystem backing marker is missing"));
                }
                let backing = ConcurrentBackingId::from_bytes(*uuid::Uuid::new_v4().as_bytes())?;
                let mut bytes = [0_u8; 20];
                bytes[..4].copy_from_slice(MARKER_MAGIC);
                bytes[4..].copy_from_slice(&backing.as_bytes());
                let mut stage = Stage::create(&directory)?;
                stage.file.write_all(&bytes).map_err(io_error)?;
                barrier(OpenBarrier::StagingFile, &stage.file)?;
                stage.verify_named()?;
                match rename_create_only(&directory, &stage.name, &directory, MARKER) {
                    Ok(()) => stage.published(),
                    Err(error) if error.is(ErrorCode::Eexist) => stage.remove()?,
                    Err(error) => return Err(error),
                }
                open_at(&directory, MARKER, FILE_FLAGS, 0)
                    .map_err(|_| stale("filesystem backing marker is unavailable"))?
            }
            Err(_) => {
                return Err(stale(
                    "filesystem backing marker is not a regular private file",
                ));
            }
        };
        let (backing, marker_identity) = read_marker(&marker)?;
        // A second opener completes the marker's barriers too: observing a
        // name is insufficient evidence that its creator finished its syncs.
        barrier(OpenBarrier::MarkerFile, &marker)?;
        barrier(OpenBarrier::RootDirectory, &directory)?;
        barrier(OpenBarrier::ParentDirectory, &parent)?;
        barrier(OpenBarrier::FinalDevice, &marker)?;
        if checked_directory(&walk_root(&components)?)? != root_identity {
            return Err(stale("filesystem block root changed during initialization"));
        }
        if Identity::of(&walk_root(parent_components)?.metadata().map_err(io_error)?)
            != parent_identity
        {
            return Err(stale(
                "filesystem block root parent changed during initialization",
            ));
        }
        let current_marker = open_at(&directory, MARKER, FILE_FLAGS, 0)
            .map_err(|_| stale("filesystem backing marker changed during initialization"))?;
        if read_marker(&current_marker)? != (backing, marker_identity) {
            return Err(stale(
                "filesystem backing marker changed during initialization",
            ));
        }
        if checked_directory(&walk_root(&components)?)? != root_identity {
            return Err(stale("filesystem block root changed during initialization"));
        }
        drop(_initialization_lock);
        Ok(Self {
            inner: Arc::new(Inner {
                root: directory,
                root_components: components,
                root_identity,
                marker_identity,
                backing,
                durable,
                observer: if cfg!(feature = "io-profiling") {
                    Observer::enabled()
                } else {
                    Observer::disabled()
                },
                #[cfg(test)]
                faults: test_support::Faults::default(),
            }),
        })
    }

    async fn owned<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Inner) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.owned_observed(None, work).await
    }

    async fn owned_observed<T: Send + 'static>(
        &self,
        observation: Option<(ObservedOperation, u64)>,
        work: impl FnOnce(&Inner) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let waiter =
            observation.map(|(operation, bytes)| self.inner.observer.waiter(operation, bytes));
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle,
            Err(_) => {
                return finish_observation(
                    waiter,
                    Err(FsError::enotsup(
                        "filesystem block operations require a Tokio runtime",
                    )),
                );
            }
        };
        let inner = self.inner.clone();
        // The queue token belongs to the admitted closure, not its async waiter.
        let queue = observation.map(|(operation, bytes)| inner.observer.queue(operation, bytes));
        let result = handle
            .spawn_blocking(move || {
                if let Some(queue) = queue {
                    queue.finish_success();
                }
                let worker =
                    observation.map(|(operation, bytes)| inner.observer.worker(operation, bytes));
                finish_observation(worker, work(&inner))
            })
            .await
            .map_err(|_| FsError::backend("filesystem block operation worker failed"))
            .and_then(|result| result);
        finish_observation(waiter, result)
    }
}

fn finish_observation<T>(span: Option<ObservationSpan>, result: Result<T>) -> Result<T> {
    if let Some(span) = span {
        if result.is_ok() {
            span.finish_success();
        } else {
            span.finish_error();
        }
    }
    result
}

impl Inner {
    fn measured<T>(
        &self,
        stage: MetricStage,
        offered_bytes: u64,
        work: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let span = self.observer.stage(stage, offered_bytes);
        finish_observation(Some(span), work())
    }

    fn verify(&self) -> Result<()> {
        let current = walk_root(&self.root_components)
            .map_err(|_| stale("filesystem block root is unavailable"))?;
        if checked_directory(&current)
            .map_err(|_| stale("filesystem block root is no longer private"))?
            != self.root_identity
        {
            return Err(stale("filesystem block root was replaced"));
        }
        if checked_directory(&self.root)
            .map_err(|_| stale("filesystem block root is no longer private"))?
            != self.root_identity
        {
            return Err(stale("filesystem block root changed"));
        }
        let marker = open_at(&self.root, MARKER, FILE_FLAGS, 0)
            .map_err(|_| stale("filesystem backing marker is unavailable"))?;
        if read_marker(&marker)? != (self.backing, self.marker_identity) {
            return Err(stale("filesystem backing marker was changed or replaced"));
        }
        let after = walk_root(&self.root_components)
            .map_err(|_| stale("filesystem block root is unavailable"))?;
        if checked_directory(&after)
            .map_err(|_| stale("filesystem block root is no longer private"))?
            != self.root_identity
        {
            return Err(stale(
                "filesystem block root was replaced during authority verification",
            ));
        }
        Ok(())
    }

    fn shard(&self, id: &BlockId, create: bool) -> Result<(File, CString, Identity)> {
        validate_id(id)?;
        let name = CString::new(&id.0.as_bytes()[1..3]).expect("validated ASCII shard");
        let directory = match open_at(&self.root, &name, DIR_FLAGS, 0) {
            Ok(directory) => directory,
            Err(error) if create && error.is(ErrorCode::Enoent) => {
                // SAFETY: owned descriptors and a NUL-terminated single component.
                let status = unsafe { libc::mkdirat(self.root.as_raw_fd(), name.as_ptr(), 0o700) };
                if status != 0 {
                    let error = io_error(std::io::Error::last_os_error());
                    if !error.is(ErrorCode::Eexist) {
                        return Err(error);
                    }
                }
                open_at(&self.root, &name, DIR_FLAGS, 0)?
            }
            Err(error) => return Err(error),
        };
        let identity = checked_directory(&directory)?;
        Ok((directory, name, identity))
    }

    fn verify_shard(&self, name: &CStr, expected: Identity) -> Result<()> {
        let current = open_at(&self.root, name, DIR_FLAGS, 0)
            .map_err(|_| stale("filesystem block shard is unavailable"))?;
        if checked_directory(&current)
            .map_err(|_| stale("filesystem block shard is no longer private"))?
            != expected
        {
            return Err(stale("filesystem block shard was replaced"));
        }
        Ok(())
    }

    fn put(&self, bytes: Vec<u8>) -> Result<BlockId> {
        self.measured(MetricStage::InitialAuthority, 0, || self.verify())?;
        #[cfg(test)]
        let mut _completion = None;
        let id = self.measured(MetricStage::ContentId, bytes.len() as u64, || {
            Ok(content_id(&bytes))
        })?;
        let (shard, shard_name, shard_identity) =
            self.measured(MetricStage::ShardOpen, 0, || self.shard(&id, true))?;
        let final_name = CString::new(id.0.as_bytes()).expect("generated ASCII ID");
        let acknowledged_file = match open_at(&shard, &final_name, FILE_FLAGS, 0) {
            Ok(existing) => {
                self.observer.put_path(PutPath::Existing);
                self.measured(MetricStage::ExistingVerify, bytes.len() as u64, || {
                    verify_existing(&existing, &id, &bytes)
                })?;
                self.file_barrier(&existing)?;
                existing
            }
            Err(error) if error.is(ErrorCode::Enoent) => {
                let mut stage =
                    self.measured(MetricStage::StageCreateWrite, bytes.len() as u64, || {
                        let mut stage = Stage::create(&shard)?;
                        stage.file.write_all(&bytes).map_err(io_error)?;
                        Ok(stage)
                    })?;
                self.file_barrier(&stage.file)?;
                #[cfg(test)]
                {
                    _completion = self.faults.pause_before_publication()?;
                }
                self.measured(MetricStage::BeforePublishAuthority, 0, || {
                    self.verify()?;
                    self.verify_shard(&shard_name, shard_identity)?;
                    stage.verify_named()
                })?;
                match self.measured(MetricStage::PublishName, 0, || {
                    rename_create_only(&shard, &stage.name, &shard, &final_name)
                }) {
                    Ok(()) => {
                        self.observer.put_path(PutPath::Created);
                        stage.published();
                        stage.file.try_clone().map_err(io_error)?
                    }
                    Err(error) if error.is(ErrorCode::Eexist) => {
                        self.observer.put_path(PutPath::RaceExisting);
                        let existing = open_at(&shard, &final_name, FILE_FLAGS, 0)?;
                        self.measured(MetricStage::ExistingVerify, bytes.len() as u64, || {
                            verify_existing(&existing, &id, &bytes)
                        })?;
                        self.file_barrier(&existing)?;
                        stage.remove()?;
                        existing
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        // The winner's file and both directory entries receive barriers before
        // any duplicate caller acknowledges the immutable object too.
        self.directory_barrier(&shard)?;
        self.root_barrier()?;
        self.final_device_barrier(&acknowledged_file)?;
        self.measured(MetricStage::FinalAuthority, 0, || {
            self.verify_shard(&shard_name, shard_identity)?;
            self.verify()
        })?;
        Ok(id)
    }

    fn get(&self, id: BlockId) -> Result<Vec<u8>> {
        validate_id(&id)?;
        self.verify()?;
        let (shard, shard_name, shard_identity) = self.shard(&id, false)?;
        let name = CString::new(id.0.as_bytes()).expect("validated ASCII ID");
        let object = open_at(&shard, &name, FILE_FLAGS, 0)?;
        let bytes = read_object(&object, &id)?;
        self.verify_shard(&shard_name, shard_identity)?;
        self.verify()?;
        Ok(bytes)
    }

    fn file_barrier(&self, file: &File) -> Result<()> {
        self.measured(MetricStage::FileSync, 0, || {
            #[cfg(test)]
            self.faults.check(test_support::Point::File)?;
            file.sync_all().map_err(io_error)
        })?;
        self.measured(MetricStage::FileDeviceSync, 0, || {
            final_device_barrier(file)
        })
    }

    fn directory_barrier(&self, directory: &File) -> Result<()> {
        self.measured(MetricStage::ShardSync, 0, || {
            #[cfg(test)]
            self.faults.check(test_support::Point::Directory)?;
            directory.sync_all().map_err(io_error)
        })
    }

    fn root_barrier(&self) -> Result<()> {
        self.measured(MetricStage::RootSync, 0, || {
            #[cfg(test)]
            self.faults.check(test_support::Point::Root)?;
            self.root.sync_all().map_err(io_error)
        })
    }

    fn final_device_barrier(&self, file: &File) -> Result<()> {
        self.measured(MetricStage::PostDirectoryDeviceSync, 0, || {
            #[cfg(test)]
            self.faults.check(test_support::Point::Publication)?;
            final_device_barrier(file)
        })
    }
}

#[async_trait]
impl BlockStore for FilesystemBlockStore {
    fn durable(&self) -> bool {
        self.inner.durable
    }

    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        self.owned(|inner| {
            inner.verify()?;
            Ok(inner.backing)
        })
        .await
    }

    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        self.owned(move |inner| {
            inner.verify()?;
            if inner.backing != expected {
                return Err(stale("filesystem backing identity differs"));
            }
            Ok(())
        })
        .await
    }

    async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.get(id).await
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        if bytes.len() > MAX_BLOCK_BYTES {
            return Err(FsError::new(ErrorCode::Efbig));
        }
        let owned = self
            .inner
            .measured(MetricStage::InputCopy, bytes.len() as u64, || {
                let mut owned = Vec::new();
                owned
                    .try_reserve_exact(bytes.len())
                    .map_err(|_| FsError::new(ErrorCode::Enomem))?;
                owned.extend_from_slice(bytes);
                Ok(owned)
            })?;
        self.owned_observed(
            Some((ObservedOperation::Put, bytes.len() as u64)),
            move |inner| inner.put(owned),
        )
        .await
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        validate_id(id)?;
        let id = id.clone();
        self.owned(move |inner| inner.get(id)).await
    }

    async fn flush(&self) -> Result<()> {
        self.owned_observed(Some((ObservedOperation::Flush, 0)), |inner| {
            // Every acknowledged durable put already completed the file/shard/
            // root barriers. There is no detached successful-put write buffer.
            inner.verify()?;
            #[cfg(test)]
            let _completion = inner.faults.pause_after_flush_first_verification()?;
            inner.verify()
        })
        .await
    }

    async fn delete(&self, _id: &BlockId) -> Result<()> {
        Err(FsError::enotsup(
            "filesystem block deletion requires scoped reclamation",
        ))
    }
}

fn root_components(root: &Path) -> Result<Vec<CString>> {
    let absolute = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir().map_err(io_error)?.join(root)
    };
    let mut names = Vec::new();
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => names
                .push(CString::new(name.as_bytes()).map_err(|_| FsError::new(ErrorCode::Einval))?),
            _ => {
                return Err(FsError::new(ErrorCode::Einval)
                    .with_message("filesystem root must not contain parent traversal"));
            }
        }
    }
    if names.is_empty() {
        return Err(FsError::new(ErrorCode::Einval)
            .with_message("filesystem block root must not be the host root directory"));
    }
    Ok(names)
}

fn walk_root(components: &[CString]) -> Result<File> {
    // SAFETY: a static NUL-terminated root path and constant flags; ownership of
    // the returned descriptor is transferred immediately into File.
    let descriptor = unsafe { libc::open(c"/".as_ptr(), DIR_FLAGS) };
    if descriptor < 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    let mut directory = unsafe { File::from_raw_fd(descriptor) };
    for component in components {
        directory = open_at(&directory, component, DIR_FLAGS, 0)?;
    }
    Ok(directory)
}

fn checked_directory(directory: &File) -> Result<Identity> {
    let metadata = directory.metadata().map_err(io_error)?;
    // SAFETY: geteuid has no preconditions and returns a scalar user ID.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() {
        return Err(FsError::new(ErrorCode::Enotdir));
    }
    if metadata.uid() != uid || metadata.mode() & 0o7777 != 0o700 {
        return Err(FsError::new(ErrorCode::Eacces).with_message(
            "filesystem block directory must be owned by the effective user with mode 0700",
        ));
    }
    Ok(Identity::of(&metadata))
}

fn checked_regular(file: &File) -> Result<Metadata> {
    let metadata = file.metadata().map_err(io_error)?;
    // SAFETY: geteuid has no preconditions and returns a scalar user ID.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != uid
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(FsError::backend(
            "filesystem block is not a private, single-link regular file",
        ));
    }
    Ok(metadata)
}

fn read_marker(file: &File) -> Result<(ConcurrentBackingId, Identity)> {
    let metadata = checked_regular(file).map_err(|_| {
        stale("filesystem backing marker is not a private, single-link regular file")
    })?;
    if metadata.len() != 20 {
        return Err(stale("filesystem backing marker has invalid length"));
    }
    let mut source = file;
    let mut bytes = [0_u8; 20];
    source
        .read_exact(&mut bytes)
        .map_err(|_| stale("filesystem backing marker cannot be read"))?;
    let mut trailing = [0_u8; 1];
    if source
        .read(&mut trailing)
        .map_err(|_| stale("filesystem backing marker cannot be read"))?
        != 0
        || &bytes[..4] != MARKER_MAGIC
    {
        return Err(stale("filesystem backing marker has invalid content"));
    }
    let backing =
        ConcurrentBackingId::from_bytes(bytes[4..].try_into().expect("fixed marker length"))
            .map_err(|_| stale("filesystem backing marker has invalid identity"))?;
    Ok((backing, Identity::of(&metadata)))
}

fn validate_id(id: &BlockId) -> Result<()> {
    let bytes = id.0.as_bytes();
    if bytes.len() != 65
        || bytes[0] != b'b'
        || !bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(FsError::new(ErrorCode::Einval)
            .with_message("invalid filesystem SHA256 block identity"));
    }
    Ok(())
}

fn content_id(bytes: &[u8]) -> BlockId {
    let digest = Sha256::digest(bytes);
    let mut text = String::with_capacity(65);
    text.push('b');
    for byte in digest {
        text.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        text.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
    }
    BlockId(text)
}

fn read_object(file: &File, id: &BlockId) -> Result<Vec<u8>> {
    let metadata = checked_regular(file)?;
    if metadata.len() > MAX_BLOCK_BYTES as u64 {
        return Err(FsError::new(ErrorCode::Efbig));
    }
    let length = usize::try_from(metadata.len()).map_err(|_| FsError::new(ErrorCode::Efbig))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| FsError::new(ErrorCode::Enomem))?;
    let mut source = file;
    source
        .take(metadata.len())
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    let mut trailing = [0_u8; 1];
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    if bytes.len() != length
        || source.read(&mut trailing).map_err(io_error)? != 0
        || digest != id_digest(id)?
    {
        return Err(FsError::backend(
            "filesystem immutable block digest mismatch",
        ));
    }
    checked_regular(file)?;
    Ok(bytes)
}

fn verify_existing(file: &File, id: &BlockId, expected: &[u8]) -> Result<()> {
    let metadata = checked_regular(file)?;
    if metadata.len() != expected.len() as u64 {
        return Err(FsError::backend(
            "filesystem immutable object differs from requested bytes",
        ));
    }
    let mut source = file;
    let mut buffer = [0_u8; 16 * 1024];
    let mut hasher = Sha256::new();
    let mut offset = 0_usize;
    loop {
        let count = source.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        let end = offset
            .checked_add(count)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        if end > expected.len() || buffer[..count] != expected[offset..end] {
            return Err(FsError::backend(
                "filesystem immutable object differs from requested bytes",
            ));
        }
        hasher.update(&buffer[..count]);
        offset = end;
    }
    let digest: [u8; 32] = hasher.finalize().into();
    if offset != expected.len() || digest != id_digest(id)? {
        return Err(FsError::backend(
            "filesystem immutable object digest mismatch",
        ));
    }
    checked_regular(file)?;
    Ok(())
}

fn id_digest(id: &BlockId) -> Result<[u8; 32]> {
    validate_id(id)?;
    fn nibble(byte: u8) -> u8 {
        if byte.is_ascii_digit() {
            byte - b'0'
        } else {
            byte - b'a' + 10
        }
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in id.0.as_bytes()[1..].chunks_exact(2).enumerate() {
        digest[index] = nibble(pair[0]) << 4 | nibble(pair[1]);
    }
    Ok(digest)
}

fn file_barrier(file: &File) -> Result<()> {
    file.sync_all().map_err(io_error)?;
    final_device_barrier(file)
}

fn final_device_barrier(_file: &File) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: a live owned regular-file descriptor and scalar fcntl command.
        if unsafe { libc::fcntl(_file.as_raw_fd(), libc::F_FULLFSYNC) } != 0 {
            return Err(io_error(std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

#[allow(clippy::unnecessary_cast)] // Darwin mode_t needs C variadic integer promotion.
fn open_at(directory: &File, name: &CStr, flags: i32, mode: libc::mode_t) -> Result<File> {
    // SAFETY: a live directory descriptor, a NUL-terminated component, and
    // caller-controlled flags/mode. A successful descriptor has one File owner.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags,
            mode as libc::c_uint,
        )
    };
    if descriptor < 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn rename_create_only(source: &File, from: &CStr, target: &File, to: &CStr) -> Result<()> {
    // SAFETY: owned directory descriptors and NUL-terminated single names.
    // The closed platform flags forbid replacing an existing final object.
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            source.as_raw_fd(),
            from.as_ptr(),
            target.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let status = unsafe {
        libc::renameatx_np(
            source.as_raw_fd(),
            from.as_ptr(),
            target.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if status == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(libc::ENOSYS | libc::EINVAL | libc::ENOTSUP)
    ) {
        return Err(FsError::enotsup(
            "filesystem requires atomic no-replace rename",
        ));
    }
    Err(io_error(error))
}

struct Stage {
    directory: File,
    name: CString,
    file: File,
    identity: Identity,
    armed: bool,
}

impl Stage {
    fn create(directory: &File) -> Result<Self> {
        let owned_directory = directory.try_clone().map_err(io_error)?;
        let name = CString::new(format!("_mount-rs-stage-{}", uuid::Uuid::new_v4().simple()))
            .expect("generated ASCII stage");
        let file = open_at(
            directory,
            &name,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )?;
        let identity = Identity::of(&file.metadata().map_err(io_error)?);
        Ok(Self {
            directory: owned_directory,
            name,
            file,
            identity,
            armed: true,
        })
    }

    fn published(&mut self) {
        self.armed = false;
    }

    fn verify_named(&self) -> Result<()> {
        let current = open_at(&self.directory, &self.name, FILE_FLAGS, 0)?;
        if Identity::of(&checked_regular(&current)?) != self.identity {
            return Err(stale("filesystem temporary object was replaced"));
        }
        Ok(())
    }

    fn remove(&mut self) -> Result<()> {
        if !self.armed {
            return Ok(());
        }
        self.verify_named()?;
        // SAFETY: descriptor-relative unlink of the generated single component;
        // the just-opened regular file was proved to be this stage's inode.
        if unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) } != 0 {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        self.armed = false;
        Ok(())
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

struct DirectoryLock<'a>(&'a File);

impl<'a> DirectoryLock<'a> {
    fn acquire(directory: &'a File) -> Result<Self> {
        // SAFETY: flock accepts this live descriptor and a closed scalar flag.
        if unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        Ok(Self(directory))
    }
}

impl Drop for DirectoryLock<'_> {
    fn drop(&mut self) {
        // SAFETY: the borrowed descriptor outlives this guard.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn directory_is_empty(directory: &File) -> Result<bool> {
    let duplicate = directory.try_clone().map_err(io_error)?;
    let descriptor = duplicate.into_raw_fd();
    // SAFETY: fdopendir takes ownership only on success; error path closes the
    // exact descriptor. The resulting DIR is local to this blocking operation.
    let pointer = unsafe { libc::fdopendir(descriptor) };
    if pointer.is_null() {
        let error = std::io::Error::last_os_error();
        drop(unsafe { File::from_raw_fd(descriptor) });
        return Err(io_error(error));
    }
    struct DirectoryStream(*mut libc::DIR);
    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            // SAFETY: this guard is the unique owner of a live DIR.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let stream = DirectoryStream(pointer);
    loop {
        // SAFETY: errno is thread-local, this DIR is owned here, and a returned
        // dirent is only borrowed before the next readdir call.
        let entry = unsafe {
            *errno_pointer() = 0;
            libc::readdir(stream.0)
        };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(0) {
                return Err(io_error(error));
            }
            return Ok(true);
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name != c"." && name != c".." {
            return Ok(false);
        }
    }
}

#[cfg(target_os = "linux")]
unsafe fn errno_pointer() -> *mut libc::c_int {
    // SAFETY: platform ABI returns this thread's live errno slot.
    unsafe { libc::__errno_location() }
}
#[cfg(target_os = "macos")]
unsafe fn errno_pointer() -> *mut libc::c_int {
    // SAFETY: platform ABI returns this thread's live errno slot.
    unsafe { libc::__error() }
}

fn stale(message: &'static str) -> FsError {
    FsError::new(ErrorCode::Estale).with_message(message)
}

fn io_error(error: std::io::Error) -> FsError {
    let code = match error.raw_os_error() {
        Some(libc::ENOENT) => ErrorCode::Enoent,
        Some(libc::EEXIST) => ErrorCode::Eexist,
        Some(libc::EACCES) | Some(libc::EPERM) => ErrorCode::Eacces,
        Some(libc::ENOTDIR) => ErrorCode::Enotdir,
        Some(libc::ELOOP) => ErrorCode::Eloop,
        Some(libc::ENOSPC) => ErrorCode::Enospc,
        Some(libc::EDQUOT) => ErrorCode::Edquot,
        Some(libc::EROFS) => ErrorCode::Erofs,
        Some(libc::EMFILE) => ErrorCode::Emfile,
        Some(libc::ENFILE) => ErrorCode::Enfile,
        Some(libc::EFBIG) => ErrorCode::Efbig,
        Some(libc::EINVAL) => ErrorCode::Einval,
        Some(libc::EINTR) => ErrorCode::Eintr,
        Some(libc::ENOTSUP) | Some(libc::ENOSYS) => ErrorCode::Enotsup,
        _ => ErrorCode::Eio,
    };
    FsError::new(code).with_message(format!("filesystem block I/O failed ({})", code.as_str()))
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
