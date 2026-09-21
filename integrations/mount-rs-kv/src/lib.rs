//! Filesystem semantics over a flat asynchronous key-value store.
//!
//! This is the Rust counterpart of mountx's `unstorage` driver. The store
//! remains deliberately small: it owns raw bytes, point existence, listing,
//! mutation, and optional native metadata. The adapter supplies the tree,
//! handles, buffering, and process-local metadata overlay. It is not a
//! replacement for the independent metadata/block-store integrations.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use mount_rs_core::driver::{FileHandle, FsDriver};
use mount_rs_core::error::{ErrorCode, FsError, Result, backend_error};
use mount_rs_core::handle::{OpenFlags, checked_position};
use mount_rs_core::path::{basename, dirname, is_path_inside, normalize_path, split_path};
use mount_rs_core::types::{
    Capabilities, DirEntry, FileType, MkdirOptions, S_IFDIR, S_IFMT, S_IFREG, Stats, now_ms,
};

const BLOCK_SIZE: u64 = 4096;
const KEY_SEPARATOR: &str = ":";

/// Optional native metadata supplied by a key-value backend.
///
/// Metadata is never serialized by this crate. Backends that cannot provide a
/// field leave it as `None`; the adapter uses its process-local overlay or a
/// stable creation timestamp instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyValueMetadata {
    pub size: Option<u64>,
    pub atime_ms: Option<i64>,
    pub mtime_ms: Option<i64>,
    pub ctime_ms: Option<i64>,
    pub birthtime_ms: Option<i64>,
}

/// The minimal asynchronous store surface required by [`KeyValueFs`].
///
/// Keys are the store's native strings. `get_keys(prefix)` returns every
/// visible key below that prefix, including the prefix itself when a backend
/// chooses to do so; the adapter filters metadata keys ending in `$` and
/// interprets the remaining flat key space as a directory tree.
#[async_trait]
pub trait KeyValueStore: Send + Sync + 'static {
    type Error: fmt::Display + Send + Sync + 'static;

    async fn has_item(&self, key: &str) -> std::result::Result<bool, Self::Error>;

    async fn get_item_raw(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error>;

    async fn set_item_raw(&self, key: &str, value: Vec<u8>)
    -> std::result::Result<(), Self::Error>;

    /// Remove a value and any metadata associated with the same key.
    ///
    /// The upstream unstorage driver uses `removeItem(key, { removeMeta: true
    /// })` for unlink and rename-source cleanup, so a recreated key must not
    /// inherit the removed file's native metadata.
    async fn remove_item(&self, key: &str) -> std::result::Result<(), Self::Error>;

    async fn get_keys(&self, prefix: &str) -> std::result::Result<Vec<String>, Self::Error>;

    /// Return a provider-bounded key listing when the backend can enforce one
    /// before materializing the result. `None` means the provider has no safe
    /// bounded-listing operation and callers must fail closed. A `Some` list
    /// may contain at most `max_keys + 1` keys; a result longer than
    /// `max_keys` is treated as an overflow signal by the filesystem adapter.
    async fn get_keys_bounded(
        &self,
        _prefix: &str,
        _max_keys: usize,
    ) -> std::result::Result<Option<Vec<String>>, Self::Error> {
        Ok(None)
    }

    /// Return backend-native metadata without reading the value when possible.
    async fn get_meta(&self, _key: &str) -> std::result::Result<KeyValueMetadata, Self::Error> {
        Ok(KeyValueMetadata::default())
    }
}

/// Options for the unstorage-compatible adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnstorageOptions {
    /// Owner used when the backend does not supply an overlay value.
    pub uid: u32,
    pub gid: u32,
    /// Permission bits reported for files without an explicit `chmod`.
    pub file_mode: u32,
    /// Permission bits reported for directories without an explicit `chmod`.
    pub dir_mode: u32,
    /// Reject every operation that could mutate the store with `EROFS`.
    pub read_only: bool,
}

impl Default for UnstorageOptions {
    fn default() -> Self {
        Self {
            uid: process_uid(),
            gid: process_gid(),
            file_mode: 0o644,
            dir_mode: 0o755,
            read_only: false,
        }
    }
}

#[derive(Debug, Clone)]
struct Attributes {
    ino: u64,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    atime_ms: Option<i64>,
    mtime_ms: Option<i64>,
    ctime_ms: Option<i64>,
}

struct OpenFile {
    path: String,
    data: Vec<u8>,
    refs: u64,
    dirty: bool,
    /// An unlinked/replaced file remains readable but must never reappear.
    orphan: bool,
}

struct State {
    next_ino: u64,
    next_fd: u64,
    attributes: HashMap<String, Attributes>,
    empty_directories: HashSet<String>,
    open_files: HashMap<String, Arc<Mutex<OpenFile>>>,
}

struct Inner<S> {
    store: S,
    options: UnstorageOptions,
    created_ms: i64,
    state: Mutex<State>,
}

/// A cloneable generic key-value filesystem driver.
pub struct KeyValueFs<S> {
    inner: Arc<Inner<S>>,
}

/// Alias matching the upstream driver name in Rust APIs.
pub type UnstorageFs<S> = KeyValueFs<S>;

#[cfg(unix)]
fn process_uid() -> u32 {
    // SAFETY: getuid has no preconditions and returns a value by copy.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn process_uid() -> u32 {
    0
}

#[cfg(unix)]
fn process_gid() -> u32 {
    // SAFETY: getgid has no preconditions and returns a value by copy.
    unsafe { libc::getgid() }
}

#[cfg(not(unix))]
fn process_gid() -> u32 {
    0
}

/// Construct an unstorage-compatible filesystem driver over `store`.
pub fn create_unstorage_driver<S>(store: S, options: UnstorageOptions) -> KeyValueFs<S>
where
    S: KeyValueStore,
{
    KeyValueFs {
        inner: Arc::new(Inner {
            store,
            options,
            created_ms: now_ms(),
            state: Mutex::new(State {
                next_ino: 1,
                next_fd: 3,
                attributes: HashMap::new(),
                empty_directories: HashSet::new(),
                open_files: HashMap::new(),
            }),
        }),
    }
}

impl<S> Clone for KeyValueFs<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[derive(Default)]
struct Scope {
    items: HashMap<String, bool>,
    keys: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    Missing,
}

struct HandleState {
    position: u64,
    closed: bool,
}

struct KvFileHandle<S> {
    inner: Arc<Inner<S>>,
    entry: Arc<Mutex<OpenFile>>,
    flags: OpenFlags,
    fd: u64,
    state: Mutex<HandleState>,
}

struct KvDirectoryHandle<S> {
    inner: Arc<Inner<S>>,
    path: String,
    fd: u64,
}

fn key_of(path: &str, syscall: &str) -> Result<String> {
    let normalized = normalize_path(path);
    let segments = split_path(&normalized);
    for segment in &segments {
        if segment.contains(KEY_SEPARATOR) || segment.contains('?') || segment.ends_with('$') {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall(syscall)
                .with_path(normalized)
                .with_message(format!(
                    "Name is not representable as an unstorage key: '{segment}'"
                )));
        }
    }
    Ok(segments.join(KEY_SEPARATOR))
}

fn path_of(key: &str) -> String {
    let path = format!("/{}/", key.replace(KEY_SEPARATOR, "/"));
    normalize_path(path.trim_end_matches('/'))
}

fn prefix_of(key: &str) -> String {
    if key.is_empty() {
        String::new()
    } else {
        format!("{key}{KEY_SEPARATOR}")
    }
}

fn resize_bytes(data: &mut Vec<u8>, length: u64, syscall: &str) -> Result<()> {
    let length = usize::try_from(length)
        .map_err(|_| FsError::new(ErrorCode::Efbig).with_syscall(syscall))?;
    if length > data.len() {
        data.try_reserve(length - data.len())
            .map_err(|_| FsError::new(ErrorCode::Efbig).with_syscall(syscall))?;
    }
    data.resize(length, 0);
    Ok(())
}

impl<S> Inner<S>
where
    S: KeyValueStore,
{
    fn lock_state(&self) -> Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("key-value state lock poisoned"))
    }

    fn store_error<E: fmt::Display>(&self, error: E, syscall: &str, path: &str) -> FsError {
        backend_error(error)
            .with_syscall(syscall)
            .with_path(normalize_path(path))
    }

    fn attributes_of(&self, path: &str) -> Result<Attributes> {
        let mut state = self.lock_state()?;
        if let Some(attributes) = state.attributes.get(path) {
            return Ok(attributes.clone());
        }
        let ino = state.next_ino;
        state.next_ino = ino
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow).with_syscall("stat"))?;
        let attributes = Attributes {
            ino,
            mode: None,
            uid: None,
            gid: None,
            atime_ms: None,
            mtime_ms: None,
            ctime_ms: None,
        };
        state.attributes.insert(path.to_owned(), attributes.clone());
        Ok(attributes)
    }

    fn update_attributes<F>(&self, path: &str, syscall: &str, update: F) -> Result<()>
    where
        F: FnOnce(&mut Attributes),
    {
        let mut state = self.lock_state()?;
        if !state.attributes.contains_key(path) {
            let ino = state.next_ino;
            state.next_ino = ino
                .checked_add(1)
                .ok_or_else(|| FsError::new(ErrorCode::Eoverflow).with_syscall(syscall))?;
            state.attributes.insert(
                path.to_owned(),
                Attributes {
                    ino,
                    mode: None,
                    uid: None,
                    gid: None,
                    atime_ms: None,
                    mtime_ms: None,
                    ctime_ms: None,
                },
            );
        }
        if let Some(attributes) = state.attributes.get_mut(path) {
            update(attributes);
        }
        Ok(())
    }

    fn remove_attributes(&self, path: &str) -> Result<()> {
        self.lock_state()?.attributes.remove(path);
        Ok(())
    }

    fn set_empty_directory(&self, path: &str, present: bool) -> Result<()> {
        let mut state = self.lock_state()?;
        if present {
            state.empty_directories.insert(path.to_owned());
        } else {
            state.empty_directories.remove(path);
        }
        Ok(())
    }

    fn has_empty_directory(&self, path: &str) -> Result<bool> {
        Ok(self.lock_state()?.empty_directories.contains(path))
    }

    fn has_open_file(&self, path: &str) -> Result<bool> {
        Ok(self.lock_state()?.open_files.contains_key(path))
    }

    fn open_file(&self, path: &str) -> Result<Option<Arc<Mutex<OpenFile>>>> {
        Ok(self.lock_state()?.open_files.get(path).cloned())
    }

    fn allocate_fd(&self) -> Result<u64> {
        let mut state = self.lock_state()?;
        let fd = state.next_fd;
        state.next_fd = fd
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow).with_syscall("open"))?;
        Ok(fd)
    }

    fn remove_open_file_if(&self, path: &str, entry: &Arc<Mutex<OpenFile>>) -> Result<()> {
        let mut state = self.lock_state()?;
        if state
            .open_files
            .get(path)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            state.open_files.remove(path);
        }
        Ok(())
    }

    fn mark_orphan(&self, path: &str) -> Result<()> {
        let entry = self.lock_state()?.open_files.remove(path);
        if let Some(entry) = entry {
            entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio))?
                .orphan = true;
        }
        Ok(())
    }

    fn remove_closed(&self, entry: &Arc<Mutex<OpenFile>>) -> Result<()> {
        let (path, refs, dirty, orphan) = {
            let file = entry.lock().map_err(|_| FsError::new(ErrorCode::Eio))?;
            (file.path.clone(), file.refs, file.dirty, file.orphan)
        };
        if refs == 0 && (!dirty || orphan) {
            self.remove_open_file_if(&path, entry)?;
        }
        Ok(())
    }

    fn move_paths(&self, from: &str, to: &str) -> Result<()> {
        let mut moved_open = Vec::new();
        {
            let mut state = self.lock_state()?;

            let attributes = state
                .attributes
                .keys()
                .filter(|path| is_path_inside(path, from))
                .cloned()
                .collect::<Vec<_>>();
            for path in attributes {
                if let Some(value) = state.attributes.remove(&path) {
                    let destination = remap_path(from, to, &path);
                    state.attributes.insert(destination, value);
                }
            }

            let directories = state
                .empty_directories
                .iter()
                .filter(|path| is_path_inside(path, from))
                .cloned()
                .collect::<Vec<_>>();
            for path in directories {
                state.empty_directories.remove(&path);
                state.empty_directories.insert(remap_path(from, to, &path));
            }

            let open = state
                .open_files
                .keys()
                .filter(|path| is_path_inside(path, from))
                .cloned()
                .collect::<Vec<_>>();
            for path in open {
                if let Some(entry) = state.open_files.remove(&path) {
                    let destination = remap_path(from, to, &path);
                    state
                        .open_files
                        .insert(destination.clone(), Arc::clone(&entry));
                    moved_open.push((entry, destination));
                }
            }
        }
        for (entry, destination) in moved_open {
            entry.lock().map_err(|_| FsError::new(ErrorCode::Eio))?.path = destination;
        }
        Ok(())
    }

    async fn has_key(
        &self,
        key: &str,
        scope: &mut Scope,
        syscall: &str,
        path: &str,
    ) -> Result<bool> {
        if let Some(value) = scope.items.get(key) {
            return Ok(*value);
        }
        let value = self
            .store
            .has_item(key)
            .await
            .map_err(|error| self.store_error(error, syscall, path))?;
        scope.items.insert(key.to_owned(), value);
        Ok(value)
    }

    async fn keys_under(
        &self,
        key: &str,
        scope: &mut Scope,
        syscall: &str,
        path: &str,
    ) -> Result<Vec<String>> {
        if let Some(value) = scope.keys.get(key) {
            return Ok(value.clone());
        }
        let value = self
            .store
            .get_keys(key)
            .await
            .map_err(|error| self.store_error(error, syscall, path))?
            .into_iter()
            .filter(|entry| !entry.ends_with('$'))
            .collect::<Vec<_>>();
        scope.keys.insert(key.to_owned(), value.clone());
        Ok(value)
    }

    async fn keys_under_bounded(
        &self,
        key: &str,
        max_keys: usize,
        syscall: &str,
        path: &str,
    ) -> Result<Option<Vec<String>>> {
        self.store
            .get_keys_bounded(key, max_keys)
            .await
            .map(|value| {
                value.map(|keys| {
                    keys.into_iter()
                        .filter(|entry| !entry.ends_with('$'))
                        .collect::<Vec<_>>()
                })
            })
            .map_err(|error| self.store_error(error, syscall, path))
    }

    async fn read_value(&self, path: &str, syscall: &str) -> Result<Vec<u8>> {
        let key = key_of(path, syscall)?;
        Ok(self
            .store
            .get_item_raw(&key)
            .await
            .map_err(|error| self.store_error(error, syscall, path))?
            .unwrap_or_default())
    }

    async fn write_value(&self, path: &str, data: &[u8], syscall: &str) -> Result<()> {
        let key = key_of(path, syscall)?;
        self.store
            .set_item_raw(&key, data.to_vec())
            .await
            .map_err(|error| self.store_error(error, syscall, path))?;
        self.touch(path, syscall)
    }

    async fn remove_value(&self, path: &str, syscall: &str) -> Result<()> {
        let key = key_of(path, syscall)?;
        self.store
            .remove_item(&key)
            .await
            .map_err(|error| self.store_error(error, syscall, path))
    }

    async fn move_value(&self, from: &str, to: &str) -> Result<()> {
        let source_key = key_of(from, "rename")?;
        let destination_key = key_of(to, "rename")?;
        let value = self
            .store
            .get_item_raw(&source_key)
            .await
            .map_err(|error| self.store_error(error, "rename", from))?
            .unwrap_or_default();
        self.store
            .set_item_raw(&destination_key, value)
            .await
            .map_err(|error| self.store_error(error, "rename", to))?;
        self.store
            .remove_item(&source_key)
            .await
            .map_err(|error| self.store_error(error, "rename", from))
    }

    fn touch(&self, path: &str, syscall: &str) -> Result<()> {
        let timestamp = now_ms();
        self.update_attributes(path, syscall, |attributes| {
            attributes.mtime_ms = Some(timestamp);
            attributes.ctime_ms = Some(timestamp);
        })
    }
}

impl<S> KvFileHandle<S>
where
    S: KeyValueStore,
{
    fn path(&self) -> Result<String> {
        Ok(self
            .entry
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio))?
            .path
            .clone())
    }

    fn begin(&self, write: bool, syscall: &str) -> Result<String> {
        let path = self.path()?;
        let state = self
            .state
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall(syscall))?;
        if state.closed || (write && !self.flags.write) || (!write && !self.flags.read) {
            return Err(FsError::new(ErrorCode::Ebadf)
                .with_syscall(syscall)
                .with_path(path));
        }
        Ok(path)
    }

    fn ensure_open(&self, syscall: &str) -> Result<String> {
        let path = self.path()?;
        let state = self
            .state
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall(syscall))?;
        if state.closed {
            return Err(FsError::new(ErrorCode::Ebadf)
                .with_syscall(syscall)
                .with_path(path));
        }
        Ok(path)
    }
}

#[async_trait]
impl<S> FileHandle for KvFileHandle<S>
where
    S: KeyValueStore,
{
    fn fd(&self) -> Option<u64> {
        Some(self.fd)
    }

    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        let _path = self.begin(false, "read")?;
        let mut handle = self
            .state
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("read"))?;
        let from = position.unwrap_or(handle.position);
        let from = usize::try_from(from).unwrap_or(usize::MAX);
        let file = self
            .entry
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("read"))?;
        let count = if from >= file.data.len() {
            0
        } else {
            buffer.len().min(file.data.len() - from)
        };
        if count > 0 {
            buffer[..count].copy_from_slice(&file.data[from..from + count]);
        }
        if position.is_none() {
            handle.position = handle.position.saturating_add(count as u64);
        }
        drop(file);
        drop(handle);
        Ok(count)
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        let path = self.begin(true, "write")?;
        let mut handle = self
            .state
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("write"))?;
        let mut file = self
            .entry
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("write"))?;
        let from = if self.flags.append {
            file.data.len() as u64
        } else {
            position.unwrap_or(handle.position)
        };
        let end = from.checked_add(buffer.len() as u64).ok_or_else(|| {
            FsError::new(ErrorCode::Efbig)
                .with_syscall("write")
                .with_path(&path)
        })?;
        if end > file.data.len() as u64 {
            resize_bytes(&mut file.data, end, "write")?;
        }
        if !buffer.is_empty() {
            let start = checked_position(from)?;
            file.data[start..start + buffer.len()].copy_from_slice(buffer);
        }
        if self.flags.append || position.is_none() {
            handle.position = end;
        }
        file.dirty = true;
        drop(file);
        drop(handle);
        self.inner.touch(&path, "write")?;
        Ok(buffer.len())
    }

    async fn stat(&self) -> Result<Stats> {
        let path = self.ensure_open("fstat")?;
        self.inner
            .file_stats(&path, "fstat", Some(Arc::clone(&self.entry)))
            .await
    }

    async fn truncate(&self, length: u64) -> Result<()> {
        let path = self.begin(true, "ftruncate")?;
        {
            let mut file = self
                .entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("ftruncate"))?;
            resize_bytes(&mut file.data, length, "ftruncate")?;
            file.dirty = true;
        }
        self.inner.touch(&path, "ftruncate")?;
        Ok(())
    }

    async fn sync(&self) -> Result<()> {
        self.inner.flush(&self.entry).await
    }

    async fn datasync(&self) -> Result<()> {
        self.inner.flush(&self.entry).await
    }

    async fn close(&self) -> Result<()> {
        let should_release = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("close"))?;
            if state.closed {
                false
            } else {
                state.closed = true;
                true
            }
        };
        if should_release {
            self.inner.release(&self.entry).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl<S> FileHandle for KvDirectoryHandle<S>
where
    S: KeyValueStore,
{
    fn fd(&self) -> Option<u64> {
        Some(self.fd)
    }

    async fn read(&self, _buffer: &mut [u8], _position: Option<u64>) -> Result<usize> {
        Err(FsError::new(ErrorCode::Eisdir)
            .with_syscall("read")
            .with_path(&self.path))
    }

    async fn write(&self, _buffer: &[u8], _position: Option<u64>) -> Result<usize> {
        Err(FsError::new(ErrorCode::Eisdir)
            .with_syscall("write")
            .with_path(&self.path))
    }

    async fn stat(&self) -> Result<Stats> {
        self.inner.directory_stats(&self.path)
    }

    async fn truncate(&self, _length: u64) -> Result<()> {
        Err(FsError::new(ErrorCode::Eisdir)
            .with_syscall("ftruncate")
            .with_path(&self.path))
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl<S> FsDriver for KeyValueFs<S>
where
    S: KeyValueStore,
{
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            handles: true,
            hardlinks: false,
            symlinks: false,
            permissions: true,
            times: true,
            truncate: true,
            atomic_rename: false,
            case_sensitive: true,
            statfs: false,
            read_only: self.inner.options.read_only,
            durable_writes: false,
            mknod: false,
        }
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        let path = normalize_path(path);
        self.inner
            .stat_of(&path, "stat", &mut Scope::default())
            .await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        let path = normalize_path(path);
        self.inner
            .lstat_of(&path, "lstat", &mut Scope::default())
            .await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        let parent_path = normalize_path(path);
        let mut scope = Scope::default();
        match self
            .inner
            .lookup(&parent_path, "scandir", &mut scope)
            .await?
        {
            Kind::Missing => {
                return Err(FsError::new(ErrorCode::Enoent)
                    .with_syscall("scandir")
                    .with_path(&parent_path));
            }
            Kind::File => {
                return Err(FsError::new(ErrorCode::Enotdir)
                    .with_syscall("scandir")
                    .with_path(&parent_path));
            }
            Kind::Directory => {}
        }

        let base = key_of(&parent_path, "scandir")?;
        let prefix = prefix_of(&base);
        let keys = self
            .inner
            .keys_under(&base, &mut scope, "scandir", &parent_path)
            .await?;
        let mut entries: Vec<(String, bool)> = Vec::new();
        for key in keys {
            if !key.starts_with(&prefix) {
                continue;
            }
            let relative = &key[prefix.len()..];
            if relative.is_empty() {
                continue;
            }
            let (name, is_directory) = match relative.find(KEY_SEPARATOR) {
                Some(index) => (&relative[..index], true),
                None => (relative, false),
            };
            if let Some((_, current_directory)) =
                entries.iter_mut().find(|(entry, _)| entry == name)
            {
                if !is_directory {
                    *current_directory = false;
                }
            } else {
                entries.push((name.to_owned(), is_directory));
            }
        }

        let empty_directories = self
            .inner
            .lock_state()?
            .empty_directories
            .iter()
            .filter(|directory| dirname(directory) == parent_path && **directory != parent_path)
            .cloned()
            .collect::<Vec<_>>();
        for directory in empty_directories {
            let name = basename(&directory);
            if !entries.iter().any(|(entry, _)| entry == &name) {
                entries.push((name, true));
            }
        }

        Ok(entries
            .into_iter()
            .map(|(name, directory)| DirEntry {
                name,
                parent_path: parent_path.clone(),
                file_type: if directory {
                    FileType::Directory
                } else {
                    FileType::File
                },
            })
            .collect())
    }

    async fn readdir_bounded(&self, path: &str, max_entries: usize) -> Result<Vec<DirEntry>> {
        let parent_path = normalize_path(path);
        if max_entries == 0 {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("scandir")
                .with_path(&parent_path)
                .with_message("directory entry limit must be positive"));
        }
        let mut scope = Scope::default();
        match self
            .inner
            .lookup(&parent_path, "scandir", &mut scope)
            .await?
        {
            Kind::Missing => {
                return Err(FsError::new(ErrorCode::Enoent)
                    .with_syscall("scandir")
                    .with_path(&parent_path));
            }
            Kind::File => {
                return Err(FsError::new(ErrorCode::Enotdir)
                    .with_syscall("scandir")
                    .with_path(&parent_path));
            }
            Kind::Directory => {}
        }

        let base = key_of(&parent_path, "scandir")?;
        let prefix = prefix_of(&base);
        let Some(keys) = self
            .inner
            .keys_under_bounded(
                &base,
                max_entries.saturating_add(1),
                "scandir",
                &parent_path,
            )
            .await?
        else {
            return Err(FsError::enotsup("scandir")
                .with_path(&parent_path)
                .with_message("provider does not expose bounded key enumeration"));
        };
        if keys.len() > max_entries {
            return Err(FsError::new(ErrorCode::Eoverflow)
                .with_syscall("scandir")
                .with_path(&parent_path)
                .with_message("directory exceeds the configured entry limit"));
        }

        let mut entries: Vec<(String, bool)> = Vec::new();
        for key in keys {
            if !key.starts_with(&prefix) {
                continue;
            }
            let relative = &key[prefix.len()..];
            if relative.is_empty() {
                continue;
            }
            let (name, is_directory) = match relative.find(KEY_SEPARATOR) {
                Some(index) => (&relative[..index], true),
                None => (relative, false),
            };
            if let Some((_, current_directory)) =
                entries.iter_mut().find(|(entry, _)| entry == name)
            {
                if !is_directory {
                    *current_directory = false;
                }
            } else {
                if entries.len() == max_entries {
                    return Err(FsError::new(ErrorCode::Eoverflow)
                        .with_syscall("scandir")
                        .with_path(&parent_path)
                        .with_message("directory exceeds the configured entry limit"));
                }
                entries.push((name.to_owned(), is_directory));
            }
        }

        let state = self.inner.lock_state()?;
        for directory in state
            .empty_directories
            .iter()
            .filter(|directory| dirname(directory) == parent_path && **directory != parent_path)
        {
            let name = basename(directory);
            if entries.iter().any(|(entry, _)| entry == &name) {
                continue;
            }
            if entries.len() == max_entries {
                return Err(FsError::new(ErrorCode::Eoverflow)
                    .with_syscall("scandir")
                    .with_path(&parent_path)
                    .with_message("directory exceeds the configured entry limit"));
            }
            entries.push((name, true));
        }

        Ok(entries
            .into_iter()
            .map(|(name, directory)| DirEntry {
                name,
                parent_path: parent_path.clone(),
                file_type: if directory {
                    FileType::Directory
                } else {
                    FileType::File
                },
            })
            .collect())
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        let path = normalize_path(path);
        let flags = OpenFlags::parse(flags, &path)?;
        self.open_flags(&path, flags, mode).await
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        let path = normalize_path(path);
        if flags.write || flags.create {
            self.inner.mutable("open", &path)?;
        }
        let mut scope = Scope::default();
        let kind = self.inner.lookup(&path, "open", &mut scope).await?;
        if kind == Kind::Missing {
            if !flags.create {
                return Err(FsError::new(ErrorCode::Enoent)
                    .with_syscall("open")
                    .with_path(&path));
            }
            self.inner.update_attributes(&path, "open", |attributes| {
                attributes.mode = Some(mode & 0o7777);
            })?;
            self.inner.write_value(&path, &[], "open").await?;
            let entry = self.inner.acquire(&path, Some(Vec::new()), "open").await?;
            return Ok(Arc::new(KvFileHandle {
                inner: Arc::clone(&self.inner),
                entry,
                flags,
                fd: self.inner.allocate_fd()?,
                state: Mutex::new(HandleState {
                    position: 0,
                    closed: false,
                }),
            }));
        }
        if flags.exclusive {
            return Err(FsError::new(ErrorCode::Eexist)
                .with_syscall("open")
                .with_path(&path));
        }
        if kind == Kind::Directory {
            if flags.write {
                return Err(FsError::new(ErrorCode::Eisdir)
                    .with_syscall("open")
                    .with_path(&path));
            }
            return Ok(Arc::new(KvDirectoryHandle {
                inner: Arc::clone(&self.inner),
                path,
                fd: self.inner.allocate_fd()?,
            }));
        }

        let known = flags.truncate.then(Vec::new);
        let entry = self.inner.acquire(&path, known, "open").await?;
        if flags.truncate {
            {
                let mut file = entry
                    .lock()
                    .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("truncate"))?;
                resize_bytes(&mut file.data, 0, "truncate")?;
                file.dirty = true;
            }
            self.inner.touch(&path, "truncate")?;
        }
        Ok(Arc::new(KvFileHandle {
            inner: Arc::clone(&self.inner),
            entry,
            flags,
            fd: self.inner.allocate_fd()?,
            state: Mutex::new(HandleState {
                position: 0,
                closed: false,
            }),
        }))
    }

    async fn mkdir(&self, path: &str, options: MkdirOptions) -> Result<Option<String>> {
        let path = normalize_path(path);
        self.inner.mutable("mkdir", &path)?;
        let mode = options.mode.unwrap_or(0o777) & 0o7777;
        let mut scope = Scope::default();
        if options.recursive {
            let mut current = String::new();
            let mut first_created = None;
            let segments = split_path(&path);
            for (index, segment) in segments.iter().enumerate() {
                current.push('/');
                current.push_str(segment);
                match self.inner.classify(&current, "mkdir", &mut scope).await? {
                    Kind::Missing => {
                        self.inner.set_empty_directory(&current, true)?;
                        self.inner
                            .update_attributes(&current, "mkdir", |attributes| {
                                attributes.mode = Some(mode);
                            })?;
                        if first_created.is_none() {
                            first_created = Some(current.clone());
                        }
                    }
                    Kind::File => {
                        return Err(FsError::new(if index + 1 == segments.len() {
                            ErrorCode::Eexist
                        } else {
                            ErrorCode::Enotdir
                        })
                        .with_syscall("mkdir")
                        .with_path(&current));
                    }
                    Kind::Directory => {}
                }
            }
            return Ok(first_created);
        }
        if self.inner.lookup(&path, "mkdir", &mut scope).await? != Kind::Missing {
            return Err(FsError::new(ErrorCode::Eexist)
                .with_syscall("mkdir")
                .with_path(&path));
        }
        self.inner.set_empty_directory(&path, true)?;
        self.inner.update_attributes(&path, "mkdir", |attributes| {
            attributes.mode = Some(mode);
        })?;
        Ok(None)
    }

    async fn rmdir(&self, path: &str) -> Result<()> {
        let path = normalize_path(path);
        self.inner.mutable("rmdir", &path)?;
        let mut scope = Scope::default();
        match self.inner.lookup(&path, "rmdir", &mut scope).await? {
            Kind::Missing => {
                return Err(FsError::new(ErrorCode::Enoent)
                    .with_syscall("rmdir")
                    .with_path(&path));
            }
            Kind::File => {
                return Err(FsError::new(ErrorCode::Enotdir)
                    .with_syscall("rmdir")
                    .with_path(&path));
            }
            Kind::Directory => {}
        }
        if path == "/" {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("rmdir")
                .with_path(&path));
        }
        if self.inner.has_entries(&path, "rmdir", &mut scope).await? {
            return Err(FsError::new(ErrorCode::Enotempty)
                .with_syscall("rmdir")
                .with_path(&path));
        }
        self.inner.set_empty_directory(&path, false)?;
        self.inner.remove_attributes(&path)
    }

    async fn unlink(&self, path: &str) -> Result<()> {
        let path = normalize_path(path);
        self.inner.mutable("unlink", &path)?;
        let mut scope = Scope::default();
        match self.inner.lookup(&path, "unlink", &mut scope).await? {
            Kind::Missing => {
                return Err(FsError::new(ErrorCode::Enoent)
                    .with_syscall("unlink")
                    .with_path(&path));
            }
            Kind::Directory => {
                return Err(FsError::new(ErrorCode::Eisdir)
                    .with_syscall("unlink")
                    .with_path(&path));
            }
            Kind::File => {}
        }
        self.inner.remove_value(&path, "unlink").await?;
        self.inner.mark_orphan(&path)?;
        self.inner.set_empty_directory(&path, false)?;
        self.inner.remove_attributes(&path)
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        let from = normalize_path(old_path);
        let to = normalize_path(new_path);
        self.inner.mutable("rename", &from)?;
        let mut scope = Scope::default();
        let source = self.inner.lookup(&from, "rename", &mut scope).await?;
        if source == Kind::Missing {
            return Err(FsError::new(ErrorCode::Enoent)
                .with_syscall("rename")
                .with_path(&from)
                .with_dest(&to));
        }
        if from == to {
            return Ok(());
        }
        let directory = source == Kind::Directory;
        if directory && is_path_inside(&to, &from) {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("rename")
                .with_path(&from)
                .with_dest(&to));
        }

        let destination = self.inner.lookup(&to, "rename", &mut scope).await?;
        if directory
            && destination == Kind::Directory
            && self.inner.has_entries(&to, "rename", &mut scope).await?
        {
            return Err(FsError::new(ErrorCode::Enotempty)
                .with_syscall("rename")
                .with_path(&from)
                .with_dest(&to));
        }
        match (directory, destination) {
            (true, Kind::File) => {
                return Err(FsError::new(ErrorCode::Enotdir)
                    .with_syscall("rename")
                    .with_path(&from)
                    .with_dest(&to));
            }
            (true, Kind::Directory) => {}
            (false, Kind::Directory) => {
                return Err(FsError::new(ErrorCode::Eisdir)
                    .with_syscall("rename")
                    .with_path(&from)
                    .with_dest(&to));
            }
            _ => {}
        }

        if destination != Kind::Missing {
            self.inner.mark_orphan(&to)?;
            self.inner.remove_attributes(&to)?;
            self.inner.set_empty_directory(&to, false)?;
        }

        if directory {
            let base = key_of(&from, "rename")?;
            let prefix = prefix_of(&base);
            let keys = self
                .inner
                .keys_under(&base, &mut scope, "rename", &from)
                .await?;
            for key in keys {
                if !key.starts_with(&prefix) {
                    continue;
                }
                let child = path_of(&key);
                let destination = normalize_path(&format!("{to}{}", &child[from.len()..]));
                self.inner.move_value(&child, &destination).await?;
            }
        } else {
            self.inner.move_value(&from, &to).await?;
        }
        self.inner.move_paths(&from, &to)?;
        if directory {
            self.inner.set_empty_directory(&to, true)?;
        }
        self.inner.touch(&to, "rename")
    }

    async fn truncate(&self, path: &str, length: u64) -> Result<()> {
        let path = normalize_path(path);
        self.inner.mutable("truncate", &path)?;
        let mut scope = Scope::default();
        self.inner
            .resolve_file(&path, "truncate", &mut scope)
            .await?;
        if let Some(entry) = self.inner.open_file(&path)? {
            {
                let mut file = entry
                    .lock()
                    .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("truncate"))?;
                resize_bytes(&mut file.data, length, "truncate")?;
                file.dirty = true;
            }
            self.inner.touch(&path, "truncate")?;
            let refs = entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("truncate"))?
                .refs;
            if refs == 0 {
                let result = self.inner.flush(&entry).await;
                let cleanup = self.inner.remove_closed(&entry);
                return result.and(cleanup);
            }
            return Ok(());
        }
        if length == 0 {
            return self.inner.write_value(&path, &[], "truncate").await;
        }
        let mut data = self.inner.read_value(&path, "truncate").await?;
        resize_bytes(&mut data, length, "truncate")?;
        self.inner.write_value(&path, &data, "truncate").await
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        let path = normalize_path(path);
        self.inner.mutable("chmod", &path)?;
        self.inner
            .stat_of(&path, "chmod", &mut Scope::default())
            .await?;
        let timestamp = now_ms();
        self.inner.update_attributes(&path, "chmod", |attributes| {
            attributes.mode = Some(mode & 0o7777);
            attributes.ctime_ms = Some(timestamp);
        })
    }

    async fn chown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        let path = normalize_path(path);
        self.inner.mutable("chown", &path)?;
        self.inner
            .stat_of(&path, "chown", &mut Scope::default())
            .await?;
        let timestamp = now_ms();
        self.inner.update_attributes(&path, "chown", |attributes| {
            if uid != u32::MAX {
                attributes.uid = Some(uid);
            }
            if gid != u32::MAX {
                attributes.gid = Some(gid);
            }
            attributes.ctime_ms = Some(timestamp);
        })
    }

    async fn lchown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.chown(path, uid, gid).await
    }

    async fn utimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        let path = normalize_path(path);
        self.inner.mutable("utime", &path)?;
        self.inner
            .stat_of(&path, "utime", &mut Scope::default())
            .await?;
        let timestamp = now_ms();
        self.inner.update_attributes(&path, "utime", |attributes| {
            attributes.atime_ms = Some(atime_ms);
            attributes.mtime_ms = Some(mtime_ms);
            attributes.ctime_ms = Some(timestamp);
        })
    }

    async fn lutimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.utimes(path, atime_ms, mtime_ms).await
    }
}

fn remap_path(from: &str, to: &str, path: &str) -> String {
    normalize_path(&format!("{to}{}", &path[from.len()..]))
}

impl<S> Inner<S>
where
    S: KeyValueStore,
{
    async fn classify(&self, path: &str, syscall: &str, scope: &mut Scope) -> Result<Kind> {
        if path == "/" {
            return Ok(Kind::Directory);
        }
        if self.has_open_file(path)? {
            return Ok(Kind::File);
        }
        let key = key_of(path, syscall)?;
        if self.has_key(&key, scope, syscall, path).await? {
            return Ok(Kind::File);
        }
        if self.has_empty_directory(path)? {
            return Ok(Kind::Directory);
        }
        Ok(
            if self
                .keys_under(&key, scope, syscall, path)
                .await?
                .is_empty()
            {
                Kind::Missing
            } else {
                Kind::Directory
            },
        )
    }

    async fn lookup(&self, path: &str, syscall: &str, scope: &mut Scope) -> Result<Kind> {
        let kind = self.classify(path, syscall, scope).await?;
        if kind == Kind::Missing {
            self.require_directory(&dirname(path), syscall, path, scope)
                .await?;
            return Ok(kind);
        }
        let parent = dirname(path);
        if parent != "/"
            && self
                .has_key(&key_of(&parent, syscall)?, scope, syscall, path)
                .await?
        {
            return Err(FsError::new(ErrorCode::Enotdir)
                .with_syscall(syscall)
                .with_path(path));
        }
        Ok(kind)
    }

    async fn require_directory(
        &self,
        path: &str,
        syscall: &str,
        reported: &str,
        scope: &mut Scope,
    ) -> Result<()> {
        if path == "/" {
            return Ok(());
        }
        let key = key_of(path, syscall)?;
        let segments = key.split(KEY_SEPARATOR).collect::<Vec<_>>();
        let mut current_path = String::new();
        let mut current_key = String::new();
        let mut paths = Vec::with_capacity(segments.len());
        let mut keys = Vec::with_capacity(segments.len());
        for segment in segments {
            current_path.push('/');
            current_path.push_str(segment);
            current_key = if current_key.is_empty() {
                segment.to_owned()
            } else {
                format!("{current_key}{KEY_SEPARATOR}{segment}")
            };
            paths.push(current_path.clone());
            keys.push(current_key.clone());
        }

        for (component_path, component_key) in paths.iter().zip(keys.iter()) {
            if self.has_open_file(component_path)?
                || self
                    .has_key(component_key, scope, syscall, reported)
                    .await?
            {
                return Err(FsError::new(ErrorCode::Enotdir)
                    .with_syscall(syscall)
                    .with_path(normalize_path(reported)));
            }
        }
        let deepest = paths.last().expect("non-root path has a component");
        if self.has_empty_directory(deepest)? {
            return Ok(());
        }
        if self
            .keys_under(
                keys.last().expect("non-root path has a key"),
                scope,
                syscall,
                reported,
            )
            .await?
            .is_empty()
        {
            return Err(FsError::new(ErrorCode::Enoent)
                .with_syscall(syscall)
                .with_path(normalize_path(reported)));
        }
        Ok(())
    }

    async fn resolve_file(&self, path: &str, syscall: &str, scope: &mut Scope) -> Result<()> {
        match self.lookup(path, syscall, scope).await? {
            Kind::File => Ok(()),
            Kind::Directory => Err(FsError::new(ErrorCode::Eisdir)
                .with_syscall(syscall)
                .with_path(path)),
            Kind::Missing => Err(FsError::new(ErrorCode::Enoent)
                .with_syscall(syscall)
                .with_path(path)),
        }
    }

    async fn has_entries(&self, path: &str, syscall: &str, scope: &mut Scope) -> Result<bool> {
        if !self
            .keys_under(&key_of(path, syscall)?, scope, syscall, path)
            .await?
            .is_empty()
        {
            return Ok(true);
        }
        let directories = self
            .lock_state()?
            .empty_directories
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        Ok(directories
            .into_iter()
            .any(|directory| directory != path && is_path_inside(&directory, path)))
    }

    fn mutable(&self, syscall: &str, path: &str) -> Result<()> {
        if self.options.read_only {
            return Err(FsError::new(ErrorCode::Erofs)
                .with_syscall(syscall)
                .with_path(normalize_path(path)));
        }
        Ok(())
    }

    fn make_stats(
        &self,
        path: &str,
        mode: u32,
        size: u64,
        metadata: KeyValueMetadata,
    ) -> Result<Stats> {
        let attributes = self.attributes_of(path)?;
        let kind = mode & S_IFMT;
        let mtime_ms = attributes
            .mtime_ms
            .or(metadata.mtime_ms)
            .unwrap_or(self.created_ms);
        Ok(Stats {
            dev: 0,
            ino: attributes.ino,
            mode,
            nlink: if kind == S_IFDIR { 2 } else { 1 },
            uid: attributes.uid.unwrap_or(self.options.uid),
            gid: attributes.gid.unwrap_or(self.options.gid),
            rdev: 0,
            size,
            blksize: BLOCK_SIZE,
            blocks: size.div_ceil(512),
            atime_ms: attributes
                .atime_ms
                .or(metadata.atime_ms)
                .unwrap_or(mtime_ms),
            mtime_ms,
            ctime_ms: attributes
                .ctime_ms
                .or(metadata.ctime_ms)
                .unwrap_or(mtime_ms),
            birthtime_ms: metadata.birthtime_ms.unwrap_or(mtime_ms),
        })
    }

    fn directory_stats(&self, path: &str) -> Result<Stats> {
        let attributes = self.attributes_of(path)?;
        let mode = S_IFDIR | (attributes.mode.unwrap_or(self.options.dir_mode) & 0o7777);
        self.make_stats(path, mode, BLOCK_SIZE, KeyValueMetadata::default())
    }

    async fn file_stats(
        &self,
        path: &str,
        syscall: &str,
        entry: Option<Arc<Mutex<OpenFile>>>,
    ) -> Result<Stats> {
        let attributes = self.attributes_of(path)?;
        let mode = S_IFREG | (attributes.mode.unwrap_or(self.options.file_mode) & 0o7777);
        let (orphan, open_size) = if let Some(entry) = &entry {
            let file = entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("fstat"))?;
            (file.orphan, Some(file.data.len() as u64))
        } else {
            (false, None)
        };
        if orphan {
            return self.make_stats(
                path,
                mode,
                open_size.unwrap_or(0),
                KeyValueMetadata::default(),
            );
        }
        let key = key_of(path, syscall)?;
        let metadata = self
            .store
            .get_meta(&key)
            .await
            .map_err(|error| self.store_error(error, syscall, path))?;
        let size = if let Some(size) = open_size {
            size
        } else if let Some(size) = metadata.size {
            size
        } else {
            self.read_value(path, syscall).await?.len() as u64
        };
        self.make_stats(path, mode, size, metadata)
    }

    async fn stat_of(&self, path: &str, syscall: &str, scope: &mut Scope) -> Result<Stats> {
        match self.lookup(path, syscall, scope).await? {
            Kind::Directory => self.directory_stats(path),
            Kind::File => {
                // An open buffer is the file's contents until its last close,
                // including writes that have not reached the store yet.
                self.file_stats(path, syscall, self.open_file(path)?).await
            }
            Kind::Missing => Err(FsError::new(ErrorCode::Enoent)
                .with_syscall(syscall)
                .with_path(path)),
        }
    }

    async fn lstat_of(&self, path: &str, syscall: &str, scope: &mut Scope) -> Result<Stats> {
        self.stat_of(path, syscall, scope).await
    }

    async fn acquire(
        &self,
        path: &str,
        known: Option<Vec<u8>>,
        syscall: &str,
    ) -> Result<Arc<Mutex<OpenFile>>> {
        if let Some(entry) = self.open_file(path)? {
            entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall(syscall))?
                .refs += 1;
            return Ok(entry);
        }
        let data = match known {
            Some(data) => data,
            None => self.read_value(path, syscall).await?,
        };
        let candidate = Arc::new(Mutex::new(OpenFile {
            path: path.to_owned(),
            data,
            refs: 1,
            dirty: false,
            orphan: false,
        }));
        let mut state = self.lock_state()?;
        if let Some(entry) = state.open_files.get(path).cloned() {
            drop(state);
            entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall(syscall))?
                .refs += 1;
            return Ok(entry);
        }
        state
            .open_files
            .insert(path.to_owned(), Arc::clone(&candidate));
        Ok(candidate)
    }

    async fn flush(&self, entry: &Arc<Mutex<OpenFile>>) -> Result<()> {
        let snapshot = {
            let mut file = entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("fsync"))?;
            if !file.dirty || file.orphan {
                return Ok(());
            }
            file.dirty = false;
            (file.path.clone(), file.data.clone())
        };
        if let Err(error) = self.write_value(&snapshot.0, &snapshot.1, "fsync").await {
            if let Ok(mut file) = entry.lock() {
                file.dirty = true;
            }
            return Err(error);
        }
        Ok(())
    }

    async fn release(&self, entry: &Arc<Mutex<OpenFile>>) -> Result<()> {
        {
            let mut file = entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("close"))?;
            file.refs = file.refs.saturating_sub(1);
        }
        let result = self.flush(entry).await;
        let cleanup = self.remove_closed(entry);
        result.and(cleanup)
    }
}
