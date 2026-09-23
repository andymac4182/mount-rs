//! In-memory filesystem driver for mount-rs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use mount_rs_core::driver::{
    FileHandle, FsDriver, GuardedDirectoryEntry, GuardedMutation, GuardedMutationResult,
    GuardedRead, GuardedReadResult, GuardedSetattr, ObservedEntry, PathGuard, PathIdentity,
};
use mount_rs_core::error::{ErrorCode, FsError, Result};
use mount_rs_core::handle::{OpenFlags, checked_position};
use mount_rs_core::path::{is_path_inside, normalize_path, split_path};
use mount_rs_core::types::{
    Capabilities, DirEntry, FileType, MkdirOptions, S_IFDIR, S_IFMT, S_IFREG, Stats, StatsFs,
    now_ms,
};

const BLOCK_SIZE: u64 = 4096;
const MAX_SYMLINK_DEPTH: usize = 40;

/// Match JavaScript Map insertion order without a runtime dependency.
#[derive(Debug, Clone, Default)]
struct Children {
    values: HashMap<String, u64>,
    order: Vec<String>,
}
impl Children {
    fn new() -> Self {
        Self::default()
    }
    fn get(&self, name: &str) -> Option<&u64> {
        self.values.get(name)
    }
    fn insert(&mut self, name: String, inode: u64) {
        if !self.values.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.values.insert(name, inode);
    }
    fn remove(&mut self, name: &str) {
        self.values.remove(name);
        self.order.retain(|entry| entry != name);
    }
    fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    fn iter(&self) -> impl Iterator<Item = (&String, &u64)> {
        self.order.iter().map(|name| (name, &self.values[name]))
    }
}
impl FromIterator<(String, u64)> for Children {
    fn from_iter<T: IntoIterator<Item = (String, u64)>>(iter: T) -> Self {
        let mut children = Self::new();
        for (name, inode) in iter {
            children.insert(name, inode);
        }
        children
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryOptions {
    pub uid: u32,
    pub gid: u32,
    pub umask: u32,
    pub root_mode: u32,
}

impl Default for MemoryOptions {
    fn default() -> Self {
        Self {
            uid: 0,
            gid: 0,
            umask: 0,
            root_mode: 0o755,
        }
    }
}

#[derive(Debug, Clone)]
struct Node {
    ino: u64,
    mode: u32,
    nlink: u64,
    uid: u32,
    gid: u32,
    rdev: u64,
    atime_ms: i64,
    mtime_ms: i64,
    ctime_ms: i64,
    birthtime_ms: i64,
    data: Vec<u8>,
    target: Option<String>,
    children: Children,
    subdirs: u64,
}

impl Node {
    fn file_type(&self) -> FileType {
        FileType::from_mode(self.mode)
    }
    fn is_directory(&self) -> bool {
        self.file_type() == FileType::Directory
    }
    fn is_symlink(&self) -> bool {
        self.file_type() == FileType::Symlink
    }
    fn size(&self) -> u64 {
        if self.is_directory() {
            BLOCK_SIZE
        } else if let Some(target) = &self.target {
            target.len() as u64
        } else {
            self.data.len() as u64
        }
    }
    fn blocks(&self) -> u64 {
        self.size().div_ceil(BLOCK_SIZE)
    }
    fn stat(&self) -> Stats {
        Stats {
            dev: 0,
            ino: self.ino,
            mode: self.mode,
            nlink: if self.is_directory() {
                2 + self.subdirs
            } else {
                self.nlink
            },
            uid: self.uid,
            gid: self.gid,
            rdev: self.rdev,
            size: self.size(),
            blksize: BLOCK_SIZE,
            blocks: self.size().div_ceil(512),
            atime_ms: self.atime_ms,
            mtime_ms: self.mtime_ms,
            ctime_ms: self.ctime_ms,
            birthtime_ms: self.birthtime_ms,
        }
    }
}

struct MemoryState {
    root: u64,
    next_ino: u64,
    next_fd: u64,
    node_count: u64,
    used_blocks: u64,
    uid: u32,
    gid: u32,
    umask: u32,
    nodes: HashMap<u64, Node>,
}

#[derive(Debug, Clone)]
struct Entry {
    parent: u64,
    name: String,
    node: Option<u64>,
    path: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    root: u64,
    next_ino: u64,
    uid: u32,
    gid: u32,
    umask: u32,
    nodes: Vec<SnapshotNode>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SnapshotNode {
    ino: u64,
    mode: u32,
    nlink: u64,
    uid: u32,
    gid: u32,
    rdev: u64,
    atime_ms: i64,
    mtime_ms: i64,
    ctime_ms: i64,
    birthtime_ms: i64,
    data: Vec<u8>,
    target: Option<String>,
    children: Vec<(String, u64)>,
    subdirs: u64,
}

#[derive(Clone)]
pub struct MemoryFs {
    inner: Arc<Mutex<MemoryState>>,
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<()> {
    let invalid = || FsError::backend("inconsistent filesystem snapshot");
    let mut nodes = HashMap::new();
    for node in &snapshot.nodes {
        if node.ino == 0
            || node.ino >= snapshot.next_ino
            || node.nlink == 0
            || nodes.insert(node.ino, node).is_some()
        {
            return Err(invalid());
        }
        let kind = FileType::from_mode(node.mode);
        if kind.mode_bits() != node.mode & S_IFMT {
            return Err(invalid());
        }
        if (kind == FileType::Symlink) != node.target.is_some()
            || (kind != FileType::Directory && (!node.children.is_empty() || node.subdirs != 0))
            || (kind != FileType::File && !node.data.is_empty())
        {
            return Err(invalid());
        }
    }
    let root = nodes.get(&snapshot.root).ok_or_else(invalid)?;
    if root.mode & S_IFMT != S_IFDIR {
        return Err(invalid());
    }
    let mut references = HashMap::from([(snapshot.root, 1u64)]);
    for node in nodes.values() {
        let mut names = std::collections::HashSet::new();
        let mut subdirs = 0;
        for (name, child) in &node.children {
            if name.is_empty()
                || name == "."
                || name == ".."
                || name.contains('/')
                || name.contains('\0')
                || !names.insert(name)
            {
                return Err(invalid());
            }
            let child_node = nodes.get(child).ok_or_else(invalid)?;
            *references.entry(*child).or_insert(0) += 1;
            if child_node.mode & S_IFMT == S_IFDIR {
                subdirs += 1;
            }
        }
        if node.subdirs != subdirs {
            return Err(invalid());
        }
    }
    for node in nodes.values() {
        if references.get(&node.ino).copied().unwrap_or(0) != node.nlink
            || (node.mode & S_IFMT == S_IFDIR && node.nlink != 1)
        {
            return Err(invalid());
        }
    }
    // Iterative traversal avoids overflowing the stack on deeply nested trees.
    let mut visited = std::collections::HashSet::new();
    let mut pending = vec![snapshot.root];
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        pending.extend(nodes[&id].children.iter().map(|(_, child)| *child));
    }
    if visited.len() != nodes.len() {
        return Err(invalid());
    }
    Ok(())
}

impl MemoryFs {
    pub fn new(options: MemoryOptions) -> Self {
        let timestamp = now_ms();
        let root = Node {
            ino: 1,
            mode: S_IFDIR | (options.root_mode & 0o7777),
            nlink: 1,
            uid: options.uid,
            gid: options.gid,
            rdev: 0,
            atime_ms: timestamp,
            mtime_ms: timestamp,
            ctime_ms: timestamp,
            birthtime_ms: timestamp,
            data: Vec::new(),
            target: None,
            children: Children::new(),
            subdirs: 0,
        };
        let root_blocks = root.blocks();
        let mut nodes = HashMap::new();
        nodes.insert(1, root);
        Self {
            inner: Arc::new(Mutex::new(MemoryState {
                root: 1,
                next_ino: 2,
                next_fd: 3,
                node_count: 1,
                used_blocks: root_blocks,
                uid: options.uid,
                gid: options.gid,
                umask: options.umask,
                nodes,
            })),
        }
    }

    pub fn empty() -> Self {
        Self::new(MemoryOptions::default())
    }

    /// Serialize the linked filesystem state for a persistence integration.
    pub fn snapshot_bytes(&self) -> Result<Vec<u8>> {
        let state = self.lock()?;
        let snapshot = Snapshot {
            version: 1,
            root: state.root,
            next_ino: state.next_ino,
            uid: state.uid,
            gid: state.gid,
            umask: state.umask,
            nodes: state
                .nodes
                .values()
                .filter(|node| node.nlink > 0)
                .map(|node| SnapshotNode {
                    ino: node.ino,
                    mode: node.mode,
                    nlink: node.nlink,
                    uid: node.uid,
                    gid: node.gid,
                    rdev: node.rdev,
                    atime_ms: node.atime_ms,
                    mtime_ms: node.mtime_ms,
                    ctime_ms: node.ctime_ms,
                    birthtime_ms: node.birthtime_ms,
                    data: node.data.clone(),
                    target: node.target.clone(),
                    children: node
                        .children
                        .iter()
                        .map(|(name, id)| (name.clone(), *id))
                        .collect(),
                    subdirs: node.subdirs,
                })
                .collect(),
        };
        serde_json::to_vec(&snapshot).map_err(mount_rs_core::error::backend_error)
    }

    /// Restore a filesystem state produced by [`MemoryFs::snapshot_bytes`].
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self> {
        let snapshot: Snapshot = serde_json::from_slice(bytes)
            .map_err(|error| FsError::backend(format!("invalid filesystem snapshot: {error}")))?;
        if snapshot.version != 1 || !snapshot.nodes.iter().any(|node| node.ino == snapshot.root) {
            return Err(FsError::backend("unsupported filesystem snapshot"));
        }
        validate_snapshot(&snapshot)?;
        let mut nodes = HashMap::new();
        let mut used_blocks = 0;
        let mut node_count = 0;
        for node in snapshot.nodes {
            let restored = Node {
                ino: node.ino,
                mode: node.mode,
                nlink: node.nlink,
                uid: node.uid,
                gid: node.gid,
                rdev: node.rdev,
                atime_ms: node.atime_ms,
                mtime_ms: node.mtime_ms,
                ctime_ms: node.ctime_ms,
                birthtime_ms: node.birthtime_ms,
                data: node.data,
                target: node.target,
                children: node.children.into_iter().collect(),
                subdirs: node.subdirs,
            };
            if restored.nlink > 0 {
                node_count += 1;
                used_blocks += restored.blocks();
            }
            nodes.insert(restored.ino, restored);
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(MemoryState {
                root: snapshot.root,
                next_ino: snapshot.next_ino.max(2),
                next_fd: 3,
                node_count,
                used_blocks,
                uid: snapshot.uid,
                gid: snapshot.gid,
                umask: snapshot.umask,
                nodes,
            })),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, MemoryState>> {
        self.inner
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("filesystem lock poisoned"))
    }

    fn create_node(
        state: &mut MemoryState,
        mode: u32,
        rdev: u64,
        target: Option<String>,
    ) -> Result<u64> {
        let timestamp = now_ms();
        let ino = state.next_ino;
        state.next_ino = state.next_ino.checked_add(1).ok_or_else(|| {
            FsError::new(ErrorCode::Eoverflow)
                .with_syscall("create")
                .with_message("filesystem inode id overflow")
        })?;
        let node = Node {
            ino,
            mode,
            nlink: 1,
            uid: state.uid,
            gid: state.gid,
            rdev,
            atime_ms: timestamp,
            mtime_ms: timestamp,
            ctime_ms: timestamp,
            birthtime_ms: timestamp,
            data: Vec::new(),
            target,
            children: Children::new(),
            subdirs: 0,
        };
        state.used_blocks += node.blocks();
        state.node_count += 1;
        state.nodes.insert(ino, node);
        Ok(ino)
    }

    fn walk(
        state: &MemoryState,
        path: &str,
        follow_final: bool,
        syscall: &str,
        depth: usize,
    ) -> Result<Entry> {
        if path.contains('\0') {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall(syscall)
                .with_path(path));
        }
        if depth >= MAX_SYMLINK_DEPTH {
            return Err(FsError::new(ErrorCode::Eloop)
                .with_syscall(syscall)
                .with_path(normalize_path(path)));
        }
        let normalized = normalize_path(path);
        if normalized == "/" {
            return Ok(Entry {
                parent: state.root,
                name: String::new(),
                node: Some(state.root),
                path: normalized,
            });
        }
        let segments = split_path(&normalized);
        let mut current = state.root;
        for (index, name) in segments.iter().enumerate() {
            let current_node = state.nodes.get(&current).ok_or_else(|| {
                FsError::new(ErrorCode::Estale)
                    .with_syscall(syscall)
                    .with_path(&normalized)
            })?;
            if !current_node.is_directory() {
                return Err(FsError::new(ErrorCode::Enotdir)
                    .with_syscall(syscall)
                    .with_path(&normalized));
            }
            let Some(child) = current_node.children.get(name).copied() else {
                if index + 1 == segments.len() {
                    return Ok(Entry {
                        parent: current,
                        name: name.clone(),
                        node: None,
                        path: normalized,
                    });
                }
                return Err(FsError::new(ErrorCode::Enoent)
                    .with_syscall(syscall)
                    .with_path(&normalized));
            };
            let child_node = state.nodes.get(&child).ok_or_else(|| {
                FsError::new(ErrorCode::Estale)
                    .with_syscall(syscall)
                    .with_path(&normalized)
            })?;
            let last = index + 1 == segments.len();
            if child_node.is_symlink() && (!last || follow_final) {
                let target = child_node.target.clone().unwrap_or_default();
                if target.is_empty() {
                    return Err(FsError::new(ErrorCode::Enoent)
                        .with_syscall(syscall)
                        .with_path(&normalized));
                }
                let parent_path = if index == 0 {
                    "/".to_owned()
                } else {
                    format!("/{}", segments[..index].join("/"))
                };
                let mut rewritten = if target.starts_with('/') {
                    target
                } else {
                    format!("{parent_path}/{target}")
                };
                if index + 1 < segments.len() {
                    rewritten.push('/');
                    rewritten.push_str(&segments[index + 1..].join("/"));
                }
                return Self::walk(state, &rewritten, follow_final, syscall, depth + 1);
            }
            if last {
                return Ok(Entry {
                    parent: current,
                    name: name.clone(),
                    node: Some(child),
                    path: normalized,
                });
            }
            current = child;
        }
        unreachable!()
    }

    fn resolve(state: &MemoryState, path: &str, follow_final: bool, syscall: &str) -> Result<u64> {
        let entry = Self::walk(state, path, follow_final, syscall, 0)?;
        entry.node.ok_or_else(|| {
            FsError::new(ErrorCode::Enoent)
                .with_syscall(syscall)
                .with_path(entry.path)
        })
    }

    fn link_entry(state: &mut MemoryState, entry: &Entry, node: u64) -> Result<()> {
        let child_is_dir = state
            .nodes
            .get(&node)
            .map(Node::is_directory)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        let parent = state
            .nodes
            .get_mut(&entry.parent)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        if !parent.is_directory() {
            return Err(FsError::new(ErrorCode::Enotdir)
                .with_syscall("link")
                .with_path(&entry.path));
        }
        parent.children.insert(entry.name.clone(), node);
        if child_is_dir {
            parent.subdirs += 1;
        }
        parent.mtime_ms = now_ms();
        parent.ctime_ms = parent.mtime_ms;
        Ok(())
    }

    fn unlink_entry(state: &mut MemoryState, entry: &Entry, node: u64) -> Result<()> {
        let child_is_dir = state
            .nodes
            .get(&node)
            .map(Node::is_directory)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        let parent = state
            .nodes
            .get_mut(&entry.parent)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        parent.children.remove(&entry.name);
        if child_is_dir {
            parent.subdirs = parent.subdirs.saturating_sub(1);
        }
        parent.mtime_ms = now_ms();
        parent.ctime_ms = parent.mtime_ms;
        let child = state
            .nodes
            .get_mut(&node)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        child.nlink = child.nlink.saturating_sub(1);
        child.ctime_ms = now_ms();
        if child.nlink == 0 {
            state.node_count = state.node_count.saturating_sub(1);
            state.used_blocks = state.used_blocks.saturating_sub(child.blocks());
        }
        Ok(())
    }

    fn resize(state: &mut MemoryState, node_id: u64, length: u64, syscall: &str) -> Result<()> {
        let length = usize::try_from(length)
            .map_err(|_| FsError::new(ErrorCode::Efbig).with_syscall(syscall))?;
        let node = state
            .nodes
            .get_mut(&node_id)
            .ok_or_else(|| FsError::new(ErrorCode::Estale).with_syscall(syscall))?;
        let before = node.blocks();
        if length > node.data.len() {
            node.data
                .try_reserve(length - node.data.len())
                .map_err(|_| FsError::new(ErrorCode::Efbig).with_syscall(syscall))?;
        }
        node.data.resize(length, 0);
        if node.nlink > 0 {
            state.used_blocks = state.used_blocks + node.blocks() - before;
        }
        Ok(())
    }

    fn stat_locked(state: &MemoryState, node: u64, syscall: &str) -> Result<Stats> {
        state
            .nodes
            .get(&node)
            .map(Node::stat)
            .ok_or_else(|| FsError::new(ErrorCode::Estale).with_syscall(syscall))
    }

    fn stale_guard(path: &str, syscall: &str) -> FsError {
        FsError::new(ErrorCode::Estale)
            .with_syscall(syscall)
            .with_path(path)
            .with_message("opaque handle no longer names its original inode")
    }

    fn next_guarded_time(previous: i64, syscall: &str, path: &str) -> Result<i64> {
        previous
            .checked_add(1)
            .map(|next| next.max(now_ms()))
            .ok_or_else(|| {
                FsError::new(ErrorCode::Eoverflow)
                    .with_syscall(syscall)
                    .with_path(path)
                    .with_message("filesystem change time overflow")
            })
    }

    fn planned_touch(
        state: &MemoryState,
        inode: u64,
        syscall: &str,
        path: &str,
    ) -> Result<(i64, i64)> {
        let node = state
            .nodes
            .get(&inode)
            .ok_or_else(|| Self::stale_guard(path, syscall))?;
        Ok((
            Self::next_guarded_time(node.mtime_ms, syscall, path)?,
            Self::next_guarded_time(node.ctime_ms, syscall, path)?,
        ))
    }

    fn apply_touch(state: &mut MemoryState, inode: u64, touch: (i64, i64)) {
        let node = state
            .nodes
            .get_mut(&inode)
            .expect("inode retained through locked mutation");
        node.mtime_ms = touch.0;
        node.ctime_ms = touch.1;
    }

    fn apply_ctime(state: &mut MemoryState, inode: u64, ctime_ms: i64) {
        state
            .nodes
            .get_mut(&inode)
            .expect("inode retained through locked mutation")
            .ctime_ms = ctime_ms;
    }

    fn check_path_guard(state: &MemoryState, guard: &PathGuard, syscall: &str) -> Result<u64> {
        let path = normalize_path(&guard.path);
        if guard.identity.ino == 0 {
            return Err(Self::stale_guard(&path, syscall));
        }
        let inode = Self::resolve(state, &path, false, syscall)
            .map_err(|_| Self::stale_guard(&path, syscall))?;
        let node = state
            .nodes
            .get(&inode)
            .ok_or_else(|| Self::stale_guard(&path, syscall))?;
        if guard.identity.dev != 0 || node.ino != guard.identity.ino {
            return Err(Self::stale_guard(&path, syscall));
        }
        Ok(inode)
    }

    fn guarded_child_entry(
        state: &MemoryState,
        parent: &PathGuard,
        name: &str,
        observed: ObservedEntry,
        syscall: &str,
    ) -> Result<Entry> {
        if name.is_empty() || name.contains(['/', '\0']) || matches!(name, "." | "..") {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall(syscall)
                .with_path(name));
        }
        let parent_inode = Self::check_path_guard(state, parent, syscall)?;
        if !state
            .nodes
            .get(&parent_inode)
            .is_some_and(Node::is_directory)
        {
            return Err(Self::stale_guard(&parent.path, syscall));
        }
        let path = normalize_path(&format!("{}/{name}", parent.path));
        let entry = Self::walk(state, &path, false, syscall, 0)?;
        if entry.parent != parent_inode {
            return Err(Self::stale_guard(&parent.path, syscall));
        }
        let matches_observed = match observed {
            ObservedEntry::Any => true,
            ObservedEntry::Absent => entry.node.is_none(),
            ObservedEntry::Identity(identity) => {
                identity.ino != 0
                    && identity.dev == 0
                    && entry.node.is_some_and(|inode| inode == identity.ino)
            }
        };
        if !matches_observed {
            return Err(Self::stale_guard(&entry.path, syscall));
        }
        Ok(entry)
    }

    fn open_flags_locked(
        &self,
        state: &mut MemoryState,
        path: &str,
        parsed: OpenFlags,
        mode: u32,
    ) -> Result<(Arc<dyn FileHandle>, PathIdentity)> {
        let normalized = normalize_path(path);
        if !parsed.has_valid_truncate_access() {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("open")
                .with_path(&normalized)
                .with_message("truncate requires write access"));
        }
        let umask = state.umask;
        let entry = Self::walk(
            state,
            &normalized,
            !(parsed.create && parsed.exclusive),
            "open",
            0,
        )?;
        let node = if let Some(node) = entry.node {
            if parsed.exclusive {
                return Err(FsError::new(ErrorCode::Eexist)
                    .with_syscall("open")
                    .with_path(entry.path));
            }
            let kind = state
                .nodes
                .get(&node)
                .ok_or_else(|| FsError::new(ErrorCode::Estale))?
                .file_type();
            if kind == FileType::Directory && parsed.write {
                return Err(FsError::new(ErrorCode::Eisdir)
                    .with_syscall("open")
                    .with_path(entry.path));
            }
            if kind.is_special() {
                return Err(FsError::new(ErrorCode::Enxio)
                    .with_syscall("open")
                    .with_path(entry.path));
            }
            if parsed.truncate && kind != FileType::Directory {
                Self::resize(state, node, 0, "truncate")?;
                if let Some(node) = state.nodes.get_mut(&node) {
                    node.mtime_ms = now_ms();
                    node.ctime_ms = node.mtime_ms;
                }
            }
            node
        } else {
            if !parsed.create {
                return Err(FsError::new(ErrorCode::Enoent)
                    .with_syscall("open")
                    .with_path(entry.path));
            }
            let node = Self::create_node(state, S_IFREG | (mode & !umask & 0o7777), 0, None)?;
            Self::link_entry(state, &entry, node)?;
            node
        };
        let fd = state.next_fd;
        state.next_fd += 1;
        let handle = Arc::new(MemoryHandle {
            fs: self.clone(),
            node,
            flags: parsed,
            path: normalized,
            fd,
            state: Mutex::new(HandleState {
                position: 0,
                closed: false,
            }),
        });
        Ok((handle, PathIdentity { dev: 0, ino: node }))
    }

    fn apply_guarded_setattr(
        state: &mut MemoryState,
        target: &PathGuard,
        change: GuardedSetattr,
    ) -> Result<()> {
        let inode = Self::check_path_guard(state, target, "setattr")?;
        let node = state
            .nodes
            .get(&inode)
            .ok_or_else(|| Self::stale_guard(&target.path, "setattr"))?;
        if change
            .expected_ctime_ms
            .is_some_and(|expected| node.ctime_ms != expected)
        {
            return Err(FsError::new(ErrorCode::Eagain)
                .with_syscall("setattr")
                .with_path(&target.path)
                .with_message("change time guard does not match the current inode"));
        }
        if change.size.is_some() {
            match node.file_type() {
                FileType::Directory => {
                    return Err(FsError::new(ErrorCode::Eisdir)
                        .with_syscall("setattr")
                        .with_path(&target.path));
                }
                FileType::File => {}
                _ => {
                    return Err(FsError::new(ErrorCode::Einval)
                        .with_syscall("setattr")
                        .with_path(&target.path));
                }
            }
        }
        let changed = change.size.is_some()
            || change.mode.is_some()
            || change.uid.is_some_and(|uid| uid != u32::MAX)
            || change.gid.is_some_and(|gid| gid != u32::MAX)
            || change.atime_ms.is_some()
            || change.mtime_ms.is_some();
        let next_ctime = if changed {
            Some(Self::next_guarded_time(
                node.ctime_ms,
                "setattr",
                &target.path,
            )?)
        } else {
            None
        };
        let next_mtime = if change.size.is_some() && change.mtime_ms.is_none() {
            Some(Self::next_guarded_time(
                node.mtime_ms,
                "setattr",
                &target.path,
            )?)
        } else {
            None
        };
        // Reserve/resize before any metadata change, so an allocation failure
        // cannot leave half of a compound SETATTR applied.
        if let Some(length) = change.size {
            Self::resize(state, inode, length, "setattr")?;
        }
        let node = state
            .nodes
            .get_mut(&inode)
            .ok_or_else(|| Self::stale_guard(&target.path, "setattr"))?;
        if let Some(mode) = change.mode {
            node.mode = (node.mode & S_IFMT) | (mode & 0o7777);
        }
        if let Some(uid) = change.uid
            && uid != u32::MAX
        {
            node.uid = uid;
        }
        if let Some(gid) = change.gid
            && gid != u32::MAX
        {
            node.gid = gid;
        }
        if let Some(atime_ms) = change.atime_ms {
            node.atime_ms = atime_ms;
        }
        if let Some(mtime_ms) = change.mtime_ms {
            node.mtime_ms = mtime_ms;
        } else if let Some(next_mtime) = next_mtime {
            node.mtime_ms = next_mtime;
        }
        if let Some(next_ctime) = next_ctime {
            node.ctime_ms = next_ctime;
        }
        Ok(())
    }

    fn rename_entries(state: &mut MemoryState, from: Entry, to: Entry) -> Result<()> {
        let old_path = from.path.clone();
        let new_path = to.path.clone();
        let from_node = from.node.ok_or_else(|| {
            FsError::new(ErrorCode::Enoent)
                .with_syscall("rename")
                .with_path(old_path.clone())
                .with_dest(new_path.clone())
        })?;
        if from_node == to.node.unwrap_or(0) {
            return Ok(());
        }
        let is_directory = state
            .nodes
            .get(&from_node)
            .map(Node::is_directory)
            .unwrap_or(false);
        if is_directory && is_path_inside(&to.path, &from.path) {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("rename")
                .with_path(old_path)
                .with_dest(new_path));
        }
        if let Some(destination) = to.node {
            let destination_is_directory = state
                .nodes
                .get(&destination)
                .map(Node::is_directory)
                .unwrap_or(false);
            if is_directory {
                if !destination_is_directory {
                    return Err(FsError::new(ErrorCode::Enotdir)
                        .with_syscall("rename")
                        .with_path(old_path)
                        .with_dest(new_path));
                }
                if state
                    .nodes
                    .get(&destination)
                    .is_some_and(|node| !node.children.is_empty())
                {
                    return Err(FsError::new(ErrorCode::Enotempty)
                        .with_syscall("rename")
                        .with_path(old_path)
                        .with_dest(new_path));
                }
            } else if destination_is_directory {
                return Err(FsError::new(ErrorCode::Eisdir)
                    .with_syscall("rename")
                    .with_path(old_path)
                    .with_dest(new_path));
            }
            Self::unlink_entry(state, &to, destination)?;
        }
        let source_parent = state
            .nodes
            .get_mut(&from.parent)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        source_parent.children.remove(&from.name);
        if is_directory {
            source_parent.subdirs = source_parent.subdirs.saturating_sub(1);
        }
        source_parent.mtime_ms = now_ms();
        source_parent.ctime_ms = source_parent.mtime_ms;
        Self::link_entry(state, &to, from_node)?;
        if let Some(node) = state.nodes.get_mut(&from_node) {
            node.ctime_ms = now_ms();
        }
        Ok(())
    }
}

struct HandleState {
    position: u64,
    closed: bool,
}

struct MemoryHandle {
    fs: MemoryFs,
    node: u64,
    flags: OpenFlags,
    path: String,
    fd: u64,
    state: Mutex<HandleState>,
}

impl MemoryHandle {
    fn begin(&self, write: bool, syscall: &str) -> Result<std::sync::MutexGuard<'_, HandleState>> {
        let state = self
            .state
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio))?;
        if state.closed || (write && !self.flags.write) || (!write && !self.flags.read) {
            return Err(FsError::new(ErrorCode::Ebadf)
                .with_syscall(syscall)
                .with_path(&self.path));
        }
        let fs = self.fs.lock()?;
        let node = fs.nodes.get(&self.node).ok_or_else(|| {
            FsError::new(ErrorCode::Estale)
                .with_syscall(syscall)
                .with_path(&self.path)
        })?;
        if node.is_directory() {
            return Err(FsError::new(ErrorCode::Eisdir)
                .with_syscall(syscall)
                .with_path(&self.path));
        }
        if node.file_type().is_special() {
            return Err(FsError::new(ErrorCode::Enxio)
                .with_syscall(syscall)
                .with_path(&self.path));
        }
        drop(fs);
        Ok(state)
    }
}

#[async_trait]
impl FileHandle for MemoryHandle {
    fn fd(&self) -> Option<u64> {
        Some(self.fd)
    }

    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        let mut handle_state = self.begin(false, "read")?;
        let mut fs = self.fs.lock()?;
        let node = fs.nodes.get_mut(&self.node).ok_or_else(|| {
            FsError::new(ErrorCode::Estale)
                .with_syscall("read")
                .with_path(&self.path)
        })?;
        let from = position.unwrap_or(handle_state.position);
        let from = usize::try_from(from).unwrap_or(usize::MAX);
        let count = if from >= node.data.len() {
            0
        } else {
            buffer.len().min(node.data.len() - from)
        };
        if count > 0 {
            buffer[..count].copy_from_slice(&node.data[from..from + count]);
        }
        if position.is_none() {
            handle_state.position = handle_state.position.saturating_add(count as u64);
        }
        node.atime_ms = now_ms();
        Ok(count)
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        let mut handle_state = self.begin(true, "write")?;
        let mut fs = self.fs.lock()?;
        let from = if self.flags.append {
            fs.nodes
                .get(&self.node)
                .ok_or_else(|| FsError::new(ErrorCode::Estale))?
                .data
                .len() as u64
        } else {
            position.unwrap_or(handle_state.position)
        };
        let end = from.checked_add(buffer.len() as u64).ok_or_else(|| {
            FsError::new(ErrorCode::Efbig)
                .with_syscall("write")
                .with_path(&self.path)
        })?;
        let current_length = fs
            .nodes
            .get(&self.node)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?
            .data
            .len() as u64;
        if end > current_length {
            MemoryFs::resize(&mut fs, self.node, end, "write")?;
        }
        let node = fs
            .nodes
            .get_mut(&self.node)
            .ok_or_else(|| FsError::new(ErrorCode::Estale).with_syscall("write"))?;
        let start = checked_position(from)?;
        node.data[start..start + buffer.len()].copy_from_slice(buffer);
        if self.flags.append || position.is_none() {
            handle_state.position = end;
        }
        node.mtime_ms = now_ms();
        node.ctime_ms = node.mtime_ms;
        Ok(buffer.len())
    }

    async fn stat(&self) -> Result<Stats> {
        let state = self
            .state
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio))?;
        if state.closed {
            return Err(FsError::new(ErrorCode::Ebadf)
                .with_syscall("fstat")
                .with_path(&self.path));
        }
        let fs = self.fs.lock()?;
        MemoryFs::stat_locked(&fs, self.node, "fstat")
    }

    async fn truncate(&self, length: u64) -> Result<()> {
        let _state = self.begin(true, "ftruncate")?;
        let mut fs = self.fs.lock()?;
        MemoryFs::resize(&mut fs, self.node, length, "ftruncate")?;
        let node = fs
            .nodes
            .get_mut(&self.node)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        node.mtime_ms = now_ms();
        node.ctime_ms = node.mtime_ms;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio))?;
        state.closed = true;
        Ok(())
    }
}

#[async_trait]
impl FsDriver for MemoryFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            handles: true,
            hardlinks: true,
            symlinks: true,
            permissions: true,
            times: true,
            truncate: true,
            atomic_rename: true,
            case_sensitive: true,
            statfs: true,
            read_only: false,
            durable_writes: false,
            mknod: true,
        }
    }

    fn supports_guarded_mutations(&self) -> bool {
        true
    }

    fn supports_guarded_reads(&self) -> bool {
        true
    }

    fn stable_inode_ids(&self) -> bool {
        true
    }

    async fn guarded_read(&self, request: GuardedRead) -> Result<GuardedReadResult> {
        let mut state = self.lock()?;
        match request {
            GuardedRead::Stat { target } => {
                let inode = Self::check_path_guard(&state, &target, "stat")?;
                Ok(GuardedReadResult::Stat(Self::stat_locked(
                    &state, inode, "stat",
                )?))
            }
            GuardedRead::Lookup { parent, name } => {
                let inode = Self::check_path_guard(&state, &parent, "lookup")?;
                let directory = state
                    .nodes
                    .get(&inode)
                    .ok_or_else(|| Self::stale_guard(&parent.path, "lookup"))?;
                if !directory.is_directory() {
                    return Err(FsError::new(ErrorCode::Enotdir)
                        .with_syscall("lookup")
                        .with_path(parent.path));
                }
                let parent_stats = directory.stat();
                if name.is_empty() || name.contains(['/', '\0']) {
                    return Err(FsError::new(ErrorCode::Einval)
                        .with_syscall("lookup")
                        .with_path(name));
                }
                let child = match name.as_str() {
                    "." => inode,
                    ".." => {
                        Self::walk(&state, &parent.path, false, "lookup", 0)
                            .map_err(|_| Self::stale_guard(&parent.path, "lookup"))?
                            .parent
                    }
                    _ => directory.children.get(&name).copied().ok_or_else(|| {
                        FsError::new(ErrorCode::Enoent)
                            .with_syscall("lookup")
                            .with_path(normalize_path(&format!("{}/{name}", parent.path)))
                    })?,
                };
                Ok(GuardedReadResult::Lookup {
                    parent: parent_stats,
                    child: Self::stat_locked(&state, child, "lookup")?,
                })
            }
            GuardedRead::Readdir {
                directory,
                max_entries,
            } => {
                let inode = Self::check_path_guard(&state, &directory, "scandir")?;
                if max_entries == 0 {
                    return Err(FsError::new(ErrorCode::Einval)
                        .with_syscall("scandir")
                        .with_path(directory.path)
                        .with_message("directory entry limit must be positive"));
                }
                let (stats, children) = {
                    let node = state
                        .nodes
                        .get_mut(&inode)
                        .ok_or_else(|| Self::stale_guard(&directory.path, "scandir"))?;
                    if !node.is_directory() {
                        return Err(FsError::new(ErrorCode::Enotdir)
                            .with_syscall("scandir")
                            .with_path(directory.path));
                    }
                    if node.children.order.len() > max_entries {
                        return Err(FsError::new(ErrorCode::Eoverflow)
                            .with_syscall("scandir")
                            .with_path(directory.path)
                            .with_message("directory exceeds the configured entry limit"));
                    }
                    node.atime_ms = now_ms();
                    (
                        node.stat(),
                        node.children
                            .iter()
                            .map(|(name, child)| (name.clone(), *child))
                            .collect::<Vec<_>>(),
                    )
                };
                let entries = children
                    .into_iter()
                    .map(|(name, child)| {
                        Ok(GuardedDirectoryEntry {
                            name,
                            stats: Self::stat_locked(&state, child, "scandir")?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(GuardedReadResult::Directory { stats, entries })
            }
            GuardedRead::Readlink { target } => {
                let inode = Self::check_path_guard(&state, &target, "readlink")?;
                let node = state
                    .nodes
                    .get(&inode)
                    .ok_or_else(|| Self::stale_guard(&target.path, "readlink"))?;
                if !node.is_symlink() {
                    return Err(FsError::new(ErrorCode::Einval)
                        .with_syscall("readlink")
                        .with_path(target.path));
                }
                Ok(GuardedReadResult::Readlink {
                    stats: node.stat(),
                    target: node.target.clone().unwrap_or_default(),
                })
            }
        }
    }

    async fn guarded_mutation(&self, request: GuardedMutation) -> Result<GuardedMutationResult> {
        let mut state = self.lock()?;
        match request {
            GuardedMutation::Setattr { target, change } => {
                Self::apply_guarded_setattr(&mut state, &target, change)?;
                Ok(GuardedMutationResult::Applied)
            }
            GuardedMutation::Open {
                parent,
                name,
                observed,
                flags,
                mode,
            } => {
                let entry = Self::guarded_child_entry(&state, &parent, &name, observed, "open")?;
                if let Some(inode) = entry.node {
                    let node = state
                        .nodes
                        .get(&inode)
                        .ok_or_else(|| Self::stale_guard(&entry.path, "open"))?;
                    if node.is_symlink() {
                        return Err(FsError::new(ErrorCode::Eexist)
                            .with_syscall("open")
                            .with_path(entry.path));
                    }
                    if flags.truncate
                        && node.file_type() == FileType::File
                        && !matches!(observed, ObservedEntry::Identity(_))
                    {
                        return Err(Self::stale_guard(&entry.path, "open"));
                    }
                }
                let parent_touch = if entry.node.is_none() && flags.create {
                    Some(Self::planned_touch(
                        &state,
                        entry.parent,
                        "open",
                        &entry.path,
                    )?)
                } else {
                    None
                };
                let target_touch = if entry.node.is_some() && flags.truncate && !flags.exclusive {
                    let inode = Self::resolve(&state, &entry.path, true, "open")?;
                    Some(Self::planned_touch(&state, inode, "open", &entry.path)?)
                } else {
                    None
                };
                let (handle, identity) =
                    self.open_flags_locked(&mut state, &entry.path, flags, mode)?;
                if let Some(touch) = parent_touch {
                    Self::apply_touch(&mut state, entry.parent, touch);
                }
                if let Some(touch) = target_touch {
                    Self::apply_touch(&mut state, identity.ino, touch);
                }
                Ok(GuardedMutationResult::Opened { handle, identity })
            }
            GuardedMutation::Mkdir { parent, name, mode } => {
                let entry =
                    Self::guarded_child_entry(&state, &parent, &name, ObservedEntry::Any, "mkdir")?;
                if entry.node.is_some() {
                    return Err(FsError::new(ErrorCode::Eexist)
                        .with_syscall("mkdir")
                        .with_path(entry.path));
                }
                let touch = Self::planned_touch(&state, entry.parent, "mkdir", &entry.path)?;
                let mode = S_IFDIR | (mode & !state.umask & 0o7777);
                let inode = Self::create_node(&mut state, mode, 0, None)?;
                Self::link_entry(&mut state, &entry, inode)?;
                Self::apply_touch(&mut state, entry.parent, touch);
                Ok(GuardedMutationResult::Created(PathIdentity {
                    dev: 0,
                    ino: inode,
                }))
            }
            GuardedMutation::Symlink {
                parent,
                name,
                target,
            } => {
                let entry = Self::guarded_child_entry(
                    &state,
                    &parent,
                    &name,
                    ObservedEntry::Any,
                    "symlink",
                )?;
                if target.is_empty() {
                    return Err(FsError::new(ErrorCode::Enoent)
                        .with_syscall("symlink")
                        .with_path(target)
                        .with_dest(entry.path));
                }
                if entry.node.is_some() {
                    return Err(FsError::new(ErrorCode::Eexist)
                        .with_syscall("symlink")
                        .with_path(target)
                        .with_dest(entry.path));
                }
                let touch = Self::planned_touch(&state, entry.parent, "symlink", &entry.path)?;
                let inode = Self::create_node(
                    &mut state,
                    mount_rs_core::types::S_IFLNK | 0o777,
                    0,
                    Some(target),
                )?;
                Self::link_entry(&mut state, &entry, inode)?;
                Self::apply_touch(&mut state, entry.parent, touch);
                Ok(GuardedMutationResult::Created(PathIdentity {
                    dev: 0,
                    ino: inode,
                }))
            }
            GuardedMutation::Mknod {
                parent,
                name,
                mode,
                dev,
            } => {
                let entry =
                    Self::guarded_child_entry(&state, &parent, &name, ObservedEntry::Any, "mknod")?;
                if entry.node.is_some() {
                    return Err(FsError::new(ErrorCode::Eexist)
                        .with_syscall("mknod")
                        .with_path(entry.path));
                }
                let kind = FileType::from_mode(mode);
                if !kind.is_special() && kind != FileType::File {
                    return Err(FsError::new(ErrorCode::Eperm)
                        .with_syscall("mknod")
                        .with_path(entry.path));
                }
                let mode = kind.mode_bits() | (mode & !S_IFMT & !state.umask & 0o7777);
                let dev = if matches!(kind, FileType::CharacterDevice | FileType::BlockDevice) {
                    dev
                } else {
                    0
                };
                let touch = Self::planned_touch(&state, entry.parent, "mknod", &entry.path)?;
                let inode = Self::create_node(&mut state, mode, dev, None)?;
                Self::link_entry(&mut state, &entry, inode)?;
                Self::apply_touch(&mut state, entry.parent, touch);
                Ok(GuardedMutationResult::Created(PathIdentity {
                    dev: 0,
                    ino: inode,
                }))
            }
            GuardedMutation::Unlink {
                parent,
                name,
                entry,
            } => {
                let child = Self::guarded_child_entry(&state, &parent, &name, entry, "unlink")?;
                let inode = child.node.ok_or_else(|| {
                    FsError::new(ErrorCode::Enoent)
                        .with_syscall("unlink")
                        .with_path(child.path.clone())
                })?;
                if state.nodes.get(&inode).is_some_and(Node::is_directory) {
                    return Err(FsError::new(ErrorCode::Eisdir)
                        .with_syscall("unlink")
                        .with_path(child.path));
                }
                let parent_touch =
                    Self::planned_touch(&state, child.parent, "unlink", &child.path)?;
                let child_ctime =
                    Self::next_guarded_time(state.nodes[&inode].ctime_ms, "unlink", &child.path)?;
                Self::unlink_entry(&mut state, &child, inode)?;
                Self::apply_touch(&mut state, child.parent, parent_touch);
                Self::apply_ctime(&mut state, inode, child_ctime);
                Ok(GuardedMutationResult::Applied)
            }
            GuardedMutation::Rmdir {
                parent,
                name,
                entry,
            } => {
                let child = Self::guarded_child_entry(&state, &parent, &name, entry, "rmdir")?;
                let inode = child.node.ok_or_else(|| {
                    FsError::new(ErrorCode::Enoent)
                        .with_syscall("rmdir")
                        .with_path(child.path.clone())
                })?;
                if inode == state.root {
                    return Err(FsError::new(ErrorCode::Ebusy)
                        .with_syscall("rmdir")
                        .with_path(child.path));
                }
                let target = state
                    .nodes
                    .get(&inode)
                    .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
                if !target.is_directory() {
                    return Err(FsError::new(ErrorCode::Enotdir)
                        .with_syscall("rmdir")
                        .with_path(child.path));
                }
                if !target.children.is_empty() {
                    return Err(FsError::new(ErrorCode::Enotempty)
                        .with_syscall("rmdir")
                        .with_path(child.path));
                }
                let parent_touch = Self::planned_touch(&state, child.parent, "rmdir", &child.path)?;
                let child_ctime = Self::next_guarded_time(target.ctime_ms, "rmdir", &child.path)?;
                Self::unlink_entry(&mut state, &child, inode)?;
                Self::apply_touch(&mut state, child.parent, parent_touch);
                Self::apply_ctime(&mut state, inode, child_ctime);
                Ok(GuardedMutationResult::Applied)
            }
            GuardedMutation::Rename {
                from_parent,
                from_name,
                source,
                to_parent,
                to_name,
                destination,
            } => {
                let from =
                    Self::guarded_child_entry(&state, &from_parent, &from_name, source, "rename")?;
                let to =
                    Self::guarded_child_entry(&state, &to_parent, &to_name, destination, "rename")?;
                if let Some(source_inode) = from.node
                    && Some(source_inode) != to.node
                {
                    let from_touch =
                        Self::planned_touch(&state, from.parent, "rename", &from.path)?;
                    let to_touch = if from.parent == to.parent {
                        from_touch
                    } else {
                        Self::planned_touch(&state, to.parent, "rename", &to.path)?
                    };
                    let source_ctime = Self::next_guarded_time(
                        state.nodes[&source_inode].ctime_ms,
                        "rename",
                        &from.path,
                    )?;
                    let destination_ctime = to
                        .node
                        .map(|inode| {
                            Self::next_guarded_time(
                                state.nodes[&inode].ctime_ms,
                                "rename",
                                &to.path,
                            )
                            .map(|ctime| (inode, ctime))
                        })
                        .transpose()?;
                    let from_parent_inode = from.parent;
                    let to_parent_inode = to.parent;
                    Self::rename_entries(&mut state, from, to)?;
                    Self::apply_touch(&mut state, from_parent_inode, from_touch);
                    if from_parent_inode != to_parent_inode {
                        Self::apply_touch(&mut state, to_parent_inode, to_touch);
                    }
                    Self::apply_ctime(&mut state, source_inode, source_ctime);
                    if let Some((inode, ctime)) = destination_ctime {
                        Self::apply_ctime(&mut state, inode, ctime);
                    }
                    return Ok(GuardedMutationResult::Applied);
                }
                Self::rename_entries(&mut state, from, to)?;
                Ok(GuardedMutationResult::Applied)
            }
            GuardedMutation::Link {
                source,
                to_parent,
                to_name,
                destination,
            } => {
                let inode = Self::check_path_guard(&state, &source, "link")?;
                if state.nodes.get(&inode).is_some_and(Node::is_directory) {
                    return Err(FsError::new(ErrorCode::Eperm)
                        .with_syscall("link")
                        .with_path(source.path));
                }
                let to =
                    Self::guarded_child_entry(&state, &to_parent, &to_name, destination, "link")?;
                if to.node.is_some() {
                    return Err(FsError::new(ErrorCode::Eexist)
                        .with_syscall("link")
                        .with_path(source.path)
                        .with_dest(to.path));
                }
                let parent_touch = Self::planned_touch(&state, to.parent, "link", &to.path)?;
                let source_ctime =
                    Self::next_guarded_time(state.nodes[&inode].ctime_ms, "link", &source.path)?;
                if let Some(node) = state.nodes.get_mut(&inode) {
                    node.nlink += 1;
                    node.ctime_ms = source_ctime;
                }
                Self::link_entry(&mut state, &to, inode)?;
                Self::apply_touch(&mut state, to.parent, parent_touch);
                Ok(GuardedMutationResult::Applied)
            }
        }
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        let state = self.lock()?;
        let node = Self::resolve(&state, path, true, "stat")?;
        Self::stat_locked(&state, node, "stat")
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        let state = self.lock()?;
        let node = Self::resolve(&state, path, false, "lstat")?;
        Self::stat_locked(&state, node, "lstat")
    }

    async fn statfs(&self, path: &str) -> Result<StatsFs> {
        let state = self.lock()?;
        Self::resolve(&state, path, true, "statfs")?;
        const BLOCKS: u64 = 1024 * 1024;
        const FILES: u64 = 1024 * 1024;
        Ok(StatsFs {
            filesystem_type: 0x0102_1994,
            block_size: BLOCK_SIZE,
            blocks: BLOCKS,
            blocks_free: BLOCKS.saturating_sub(state.used_blocks),
            blocks_available: BLOCKS.saturating_sub(state.used_blocks),
            files: FILES,
            files_free: FILES.saturating_sub(state.node_count),
        })
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.readdir_with_limit(path, None)
    }

    async fn readdir_bounded(&self, path: &str, max_entries: usize) -> Result<Vec<DirEntry>> {
        self.readdir_with_limit(path, Some(max_entries))
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        let normalized = normalize_path(path);
        let parsed = OpenFlags::parse(flags, &normalized)?;
        self.open_flags(&normalized, parsed, mode).await
    }

    async fn open_flags(
        &self,
        path: &str,
        parsed: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        let mut state = self.lock()?;
        self.open_flags_locked(&mut state, path, parsed, mode)
            .map(|(handle, _identity)| handle)
    }

    async fn mkdir(&self, path: &str, options: MkdirOptions) -> Result<Option<String>> {
        if path.contains('\0') {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("mkdir")
                .with_path(path));
        }
        let normalized = normalize_path(path);
        let mut state = self.lock()?;
        let mode = S_IFDIR | ((options.mode.unwrap_or(0o777)) & !state.umask & 0o7777);
        if options.recursive {
            let mut current = "/".to_owned();
            let mut first_created = None;
            for segment in split_path(&normalized) {
                if current != "/" {
                    current.push('/');
                }
                current.push_str(&segment);
                let entry = Self::walk(&state, &current, true, "mkdir", 0)?;
                if let Some(node) = entry.node {
                    if state.nodes.get(&node).map(Node::is_directory) != Some(true) {
                        return Err(FsError::new(if current == normalized {
                            ErrorCode::Eexist
                        } else {
                            ErrorCode::Enotdir
                        })
                        .with_syscall("mkdir")
                        .with_path(current));
                    }
                } else {
                    let node = Self::create_node(&mut state, mode, 0, None)?;
                    Self::link_entry(&mut state, &entry, node)?;
                    first_created.get_or_insert_with(|| current.clone());
                }
            }
            return Ok(first_created);
        }
        let entry = Self::walk(&state, &normalized, false, "mkdir", 0)?;
        if entry.node.is_some() {
            return Err(FsError::new(ErrorCode::Eexist)
                .with_syscall("mkdir")
                .with_path(entry.path));
        }
        let node = Self::create_node(&mut state, mode, 0, None)?;
        Self::link_entry(&mut state, &entry, node)?;
        Ok(None)
    }

    async fn rmdir(&self, path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let entry = Self::walk(&state, path, false, "rmdir", 0)?;
        let node = entry.node.ok_or_else(|| {
            FsError::new(ErrorCode::Enoent)
                .with_syscall("rmdir")
                .with_path(entry.path.clone())
        })?;
        if node == state.root {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("rmdir")
                .with_path(entry.path));
        }
        let target = state
            .nodes
            .get(&node)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        if !target.is_directory() {
            return Err(FsError::new(ErrorCode::Enotdir)
                .with_syscall("rmdir")
                .with_path(entry.path));
        }
        if !target.children.is_empty() {
            return Err(FsError::new(ErrorCode::Enotempty)
                .with_syscall("rmdir")
                .with_path(entry.path));
        }
        Self::unlink_entry(&mut state, &entry, node)
    }

    async fn unlink(&self, path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let entry = Self::walk(&state, path, false, "unlink", 0)?;
        let node = entry.node.ok_or_else(|| {
            FsError::new(ErrorCode::Enoent)
                .with_syscall("unlink")
                .with_path(entry.path.clone())
        })?;
        if state.nodes.get(&node).map(Node::is_directory) == Some(true) {
            return Err(FsError::new(ErrorCode::Eisdir)
                .with_syscall("unlink")
                .with_path(entry.path));
        }
        Self::unlink_entry(&mut state, &entry, node)
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        let old_path = normalize_path(old_path);
        let new_path = normalize_path(new_path);
        let mut state = self.lock()?;
        let from = Self::walk(&state, &old_path, false, "rename", 0)?;
        if from.node.is_none() {
            return Err(FsError::new(ErrorCode::Enoent)
                .with_syscall("rename")
                .with_path(old_path)
                .with_dest(new_path));
        }
        let to = Self::walk(&state, &new_path, false, "rename", 0)?;
        Self::rename_entries(&mut state, from, to)
    }

    async fn link(&self, existing_path: &str, new_path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let from = Self::walk(&state, existing_path, false, "link", 0)?;
        let node = from.node.ok_or_else(|| {
            FsError::new(ErrorCode::Enoent)
                .with_syscall("link")
                .with_path(from.path.clone())
                .with_dest(normalize_path(new_path))
        })?;
        if state.nodes.get(&node).map(Node::is_directory) == Some(true) {
            return Err(FsError::new(ErrorCode::Eperm)
                .with_syscall("link")
                .with_path(from.path)
                .with_dest(normalize_path(new_path)));
        }
        let to = Self::walk(&state, new_path, false, "link", 0)?;
        if to.node.is_some() {
            return Err(FsError::new(ErrorCode::Eexist)
                .with_syscall("link")
                .with_path(from.path)
                .with_dest(to.path));
        }
        if let Some(source) = state.nodes.get_mut(&node) {
            source.nlink += 1;
            source.ctime_ms = now_ms();
        }
        Self::link_entry(&mut state, &to, node)
    }

    async fn symlink(&self, target: &str, path: &str) -> Result<()> {
        let mut state = self.lock()?;
        let entry = Self::walk(&state, path, false, "symlink", 0)?;
        if target.is_empty() {
            return Err(FsError::new(ErrorCode::Enoent)
                .with_syscall("symlink")
                .with_path(target)
                .with_dest(entry.path));
        }
        if entry.node.is_some() {
            return Err(FsError::new(ErrorCode::Eexist)
                .with_syscall("symlink")
                .with_path(target)
                .with_dest(entry.path));
        }
        let node = Self::create_node(
            &mut state,
            mount_rs_core::types::S_IFLNK | 0o777,
            0,
            Some(target.to_owned()),
        )?;
        Self::link_entry(&mut state, &entry, node)
    }

    async fn readlink(&self, path: &str) -> Result<String> {
        let state = self.lock()?;
        let node = Self::resolve(&state, path, false, "readlink")?;
        let target = state
            .nodes
            .get(&node)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        if !target.is_symlink() {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("readlink")
                .with_path(normalize_path(path)));
        }
        Ok(target.target.clone().unwrap_or_default())
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        let mut state = self.lock()?;
        let node = Self::resolve(&state, path, true, "chmod")?;
        let target = state
            .nodes
            .get_mut(&node)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        target.mode = (target.mode & S_IFMT) | (mode & 0o7777);
        target.ctime_ms = now_ms();
        Ok(())
    }

    async fn chown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.chown_impl(path, uid, gid, true, "chown")
    }

    async fn lchown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.chown_impl(path, uid, gid, false, "lchown")
    }

    async fn truncate(&self, path: &str, length: u64) -> Result<()> {
        let mut state = self.lock()?;
        let node = Self::resolve(&state, path, true, "truncate")?;
        let kind = state
            .nodes
            .get(&node)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?
            .file_type();
        if kind == FileType::Directory {
            return Err(FsError::new(ErrorCode::Eisdir)
                .with_syscall("open")
                .with_path(normalize_path(path)));
        }
        if kind.is_special() {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("open")
                .with_path(normalize_path(path)));
        }
        Self::resize(&mut state, node, length, "truncate")?;
        let target = state
            .nodes
            .get_mut(&node)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        target.mtime_ms = now_ms();
        target.ctime_ms = target.mtime_ms;
        Ok(())
    }

    async fn utimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.utimes_impl(path, atime_ms, mtime_ms, true, "utime")
    }
    async fn lutimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.utimes_impl(path, atime_ms, mtime_ms, false, "lutime")
    }

    async fn mknod(&self, path: &str, mode: u32, dev: u64) -> Result<()> {
        let mut state = self.lock()?;
        let umask = state.umask;
        let entry = Self::walk(&state, path, false, "mknod", 0)?;
        if entry.node.is_some() {
            return Err(FsError::new(ErrorCode::Eexist)
                .with_syscall("mknod")
                .with_path(entry.path));
        }
        let kind = FileType::from_mode(mode);
        if !kind.is_special() && kind != FileType::File {
            return Err(FsError::new(ErrorCode::Eperm)
                .with_syscall("mknod")
                .with_path(entry.path));
        }
        let node = Self::create_node(
            &mut state,
            kind.mode_bits() | (mode & !S_IFMT & !umask & 0o7777),
            if kind == FileType::CharacterDevice || kind == FileType::BlockDevice {
                dev
            } else {
                0
            },
            None,
        )?;
        Self::link_entry(&mut state, &entry, node)
    }
}

impl MemoryFs {
    fn chown_impl(
        &self,
        path: &str,
        uid: u32,
        gid: u32,
        follow: bool,
        syscall: &str,
    ) -> Result<()> {
        let mut state = self.lock()?;
        let node = Self::resolve(&state, path, follow, syscall)?;
        let target = state
            .nodes
            .get_mut(&node)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        if uid != u32::MAX {
            target.uid = uid;
        }
        if gid != u32::MAX {
            target.gid = gid;
        }
        target.ctime_ms = now_ms();
        Ok(())
    }

    fn utimes_impl(
        &self,
        path: &str,
        atime_ms: i64,
        mtime_ms: i64,
        follow: bool,
        syscall: &str,
    ) -> Result<()> {
        let mut state = self.lock()?;
        let node = Self::resolve(&state, path, follow, syscall)?;
        let target = state
            .nodes
            .get_mut(&node)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        target.atime_ms = atime_ms;
        target.mtime_ms = mtime_ms;
        target.ctime_ms = now_ms();
        Ok(())
    }

    fn readdir_with_limit(&self, path: &str, max_entries: Option<usize>) -> Result<Vec<DirEntry>> {
        if max_entries == Some(0) {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("scandir")
                .with_path(normalize_path(path))
                .with_message("directory entry limit must be positive"));
        }
        let mut state = self.lock()?;
        let node_id = Self::resolve(&state, path, true, "scandir")?;
        let children = {
            let node = state
                .nodes
                .get_mut(&node_id)
                .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
            if !node.is_directory() {
                return Err(FsError::new(ErrorCode::Enotdir)
                    .with_syscall("scandir")
                    .with_path(normalize_path(path)));
            }
            node.atime_ms = now_ms();
            let mut children = Vec::new();
            for (name, child) in node.children.iter() {
                if max_entries.is_some_and(|limit| children.len() == limit) {
                    return Err(FsError::new(ErrorCode::Eoverflow)
                        .with_syscall("scandir")
                        .with_path(normalize_path(path))
                        .with_message("directory exceeds the configured entry limit"));
                }
                children.push((name.clone(), *child));
            }
            children
        };
        let parent_path = normalize_path(path);
        let entries = children
            .into_iter()
            .filter_map(|(name, child)| {
                state.nodes.get(&child).map(|node| DirEntry {
                    name,
                    parent_path: parent_path.clone(),
                    file_type: node.file_type(),
                })
            })
            .collect();
        Ok(entries)
    }
}
