//! Per-connection fid and qid state.

use std::collections::HashMap;
use std::sync::Arc;

use mount_rs_core::path::{is_path_inside, join_path, normalize_path};
use mount_rs_core::{ErrorCode, FileHandle, FsError, Result, Stats};

use crate::constants::{P9_NOFID, P9_QTDIR, P9_QTFILE, P9_QTSYMLINK};
use crate::wire::P9Qid;

#[derive(Clone)]
pub struct FidOpenState {
    pub flags: mount_rs_core::OpenFlags,
    pub handle: Option<Arc<dyn FileHandle>>,
    pub directory: bool,
    pub qid: Option<P9Qid>,
}

pub struct DirCursor {
    pub entries: Vec<String>,
    pub offsets: HashMap<u64, usize>,
}

pub struct Fid {
    pub fid: u32,
    pub path: String,
    pub open: Option<FidOpenState>,
    pub iounit: u32,
    pub cursor: Option<DirCursor>,
}

impl Fid {
    pub fn new(fid: u32, path: impl AsRef<str>) -> Self {
        Self {
            fid,
            path: normalize_path(path.as_ref()),
            open: None,
            iounit: 0,
            cursor: None,
        }
    }

    pub fn set_path(&mut self, path: impl AsRef<str>) {
        let path = normalize_path(path.as_ref());
        if path != self.path {
            self.path = path;
            self.cursor = None;
        }
    }
}

#[derive(Clone)]
struct QidIdentity {
    id: u64,
    key: Option<String>,
    paths: Vec<String>,
}

pub struct FidTable {
    use_driver_ino: bool,
    fids: HashMap<u32, Fid>,
    qid_by_path: HashMap<String, QidIdentity>,
    qid_by_key: HashMap<String, u64>,
    next_qid_path: u64,
}

impl FidTable {
    pub fn new(use_driver_ino: bool) -> Self {
        Self {
            use_driver_ino,
            fids: HashMap::new(),
            qid_by_path: HashMap::new(),
            qid_by_key: HashMap::new(),
            next_qid_path: 1,
        }
    }

    pub fn len(&self) -> usize {
        self.fids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fids.is_empty()
    }

    pub fn get(&self, fid: u32) -> Option<&Fid> {
        self.fids.get(&fid)
    }

    pub fn get_mut(&mut self, fid: u32) -> Option<&mut Fid> {
        self.fids.get_mut(&fid)
    }

    pub fn require(&self, fid: u32) -> Result<&Fid> {
        self.fids.get(&fid).ok_or_else(|| {
            FsError::new(ErrorCode::Ebadf).with_message(format!("EBADF: fid {fid} is not in use"))
        })
    }

    pub fn create(&mut self, fid: u32, path: impl AsRef<str>) -> Result<&mut Fid> {
        if fid == P9_NOFID {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("EINVAL: P9_NOFID cannot be used as a fid"));
        }
        if self.fids.contains_key(&fid) {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message(format!("EINVAL: fid {fid} is already in use")));
        }
        self.fids.insert(fid, Fid::new(fid, path));
        Ok(self.fids.get_mut(&fid).expect("inserted fid exists"))
    }

    pub fn clone_fid(&mut self, from: u32, to: u32) -> Result<&mut Fid> {
        let path = self.require(from)?.path.clone();
        if from == to {
            return self.get_mut(from).ok_or_else(|| {
                FsError::new(ErrorCode::Ebadf).with_message(format!("EBADF: fid {from}"))
            });
        }
        self.create(to, path)
    }

    pub fn clunk(&mut self, fid: u32) -> Result<Fid> {
        self.fids.remove(&fid).ok_or_else(|| {
            FsError::new(ErrorCode::Ebadf).with_message(format!("EBADF: fid {fid} is not in use"))
        })
    }

    pub fn open_handles(&self) -> Vec<(u32, Arc<dyn FileHandle>)> {
        self.fids
            .values()
            .filter_map(|fid| {
                fid.open.as_ref().and_then(|open| {
                    open.handle
                        .as_ref()
                        .map(|handle| (fid.fid, Arc::clone(handle)))
                })
            })
            .collect()
    }

    pub fn fids(&self) -> Vec<u32> {
        self.fids.keys().copied().collect()
    }

    pub fn clear(&mut self) {
        self.fids.clear();
        self.qid_by_path.clear();
        self.qid_by_key.clear();
    }

    pub fn resume(&mut self, fid: u32, offset: u64) -> Result<Option<(Vec<String>, usize)>> {
        let entry = self.require(fid)?;
        if offset == 0 {
            // Rewinding always invalidates previous cookies, even if the new
            // listing fails before it can replace the cursor.
            let entry = self.get_mut(fid).expect("required fid exists");
            entry.cursor = None;
            return Ok(None);
        }
        let cursor = entry.cursor.as_ref().ok_or_else(|| {
            FsError::new(ErrorCode::Einval).with_message(format!(
                "EINVAL: fid {fid} was never given readdir offset {offset}"
            ))
        })?;
        let index = cursor.offsets.get(&offset).copied().ok_or_else(|| {
            FsError::new(ErrorCode::Einval).with_message(format!(
                "EINVAL: fid {fid} was never given readdir offset {offset}"
            ))
        })?;
        Ok(Some((cursor.entries.clone(), index)))
    }

    pub fn snapshot(&mut self, fid: u32, entries: Vec<String>) -> Result<(Vec<String>, usize)> {
        let entry = self.get_mut(fid).ok_or_else(|| {
            FsError::new(ErrorCode::Ebadf).with_message(format!("EBADF: fid {fid}"))
        })?;
        entry.cursor = Some(DirCursor {
            entries: entries.clone(),
            offsets: HashMap::new(),
        });
        Ok((entries, 0))
    }

    pub fn note_offset(&mut self, fid: u32, offset: u64, index: usize) {
        if offset == 0 {
            return;
        }
        if let Some(cursor) = self.get_mut(fid).and_then(|entry| entry.cursor.as_mut()) {
            cursor.offsets.insert(offset, index);
        }
    }

    pub fn qid_for(&mut self, stats: &Stats, path: impl AsRef<str>) -> P9Qid {
        let path = normalize_path(path.as_ref());
        let key = if self.use_driver_ino && stats.ino > 0 {
            Some(format!("{}:{}", stats.dev, stats.ino))
        } else {
            None
        };
        let previous = self.qid_by_path.get(&path).cloned();
        let by_key = key
            .as_ref()
            .and_then(|key| self.qid_by_key.get(key))
            .and_then(|id| {
                self.qid_by_path
                    .values()
                    .find(|identity| identity.id == *id)
            })
            .cloned();
        let same_file = previous
            .as_ref()
            .is_some_and(|identity| identity.key.is_none() || identity.key == key);
        let mut identity = by_key.or_else(|| previous.clone().filter(|_| same_file));
        if identity.is_none() {
            identity = Some(QidIdentity {
                id: self.next_qid_path,
                key: key.clone(),
                paths: Vec::new(),
            });
            self.next_qid_path = self.next_qid_path.saturating_add(1);
        }
        let mut identity = identity.expect("identity was created");
        if let Some(previous) = previous
            && previous.id != identity.id
        {
            self.detach_qid(&previous, &path);
        }
        if identity.key.is_none() && key.is_some() {
            identity.key = key.clone();
        }
        self.attach_qid(identity.clone(), &path);
        P9Qid {
            type_: qid_type(stats.mode),
            version: qid_version(stats),
            path: identity.id,
        }
    }

    pub fn release(&mut self, path: impl AsRef<str>) {
        let path = normalize_path(path.as_ref());
        if let Some(identity) = self.qid_by_path.get(&path).cloned() {
            self.detach_qid(&identity, &path);
        }
    }

    fn attach_qid(&mut self, mut identity: QidIdentity, path: &str) {
        if !identity.paths.iter().any(|bound| bound == path) {
            identity.paths.push(path.to_owned());
        }
        if let Some(key) = &identity.key {
            self.qid_by_key.insert(key.clone(), identity.id);
        }
        self.qid_by_path.insert(path.to_owned(), identity);
    }

    fn detach_qid(&mut self, identity: &QidIdentity, path: &str) {
        self.qid_by_path.remove(path);
        let mut remaining = identity.clone();
        remaining.paths.retain(|bound| bound != path);
        if remaining.paths.is_empty() {
            if let Some(key) = &remaining.key
                && self.qid_by_key.get(key) == Some(&remaining.id)
            {
                self.qid_by_key.remove(key);
            }
        } else {
            for bound in &remaining.paths {
                self.qid_by_path.insert(bound.clone(), remaining.clone());
            }
        }
    }

    pub fn remap(&mut self, from: impl AsRef<str>, to: impl AsRef<str>) {
        let from = normalize_path(from.as_ref());
        let to = normalize_path(to.as_ref());
        if from == to {
            return;
        }
        let source_paths: Vec<String> = self
            .qid_by_path
            .keys()
            .filter(|path| is_path_inside(path, &from))
            .cloned()
            .collect();
        for old in source_paths {
            let Some(identity) = self.qid_by_path.get(&old).cloned() else {
                continue;
            };
            let new = format!("{}{}", to, &old[from.len()..]);
            self.detach_qid(&identity, &old);
            if let Some(destination) = self.qid_by_path.get(&new).cloned() {
                self.detach_qid(&destination, &new);
            }
            self.attach_qid(identity, &new);
        }
        for entry in self.fids.values_mut() {
            if is_path_inside(&entry.path, &from) {
                entry.set_path(format!("{}{}", to, &entry.path[from.len()..]));
            } else if is_path_inside(&entry.path, &to) {
                entry.cursor = None;
            }
        }
    }
}

pub fn qid_type(mode: u32) -> u8 {
    match mode & mount_rs_core::types::S_IFMT {
        mount_rs_core::types::S_IFDIR => P9_QTDIR,
        mount_rs_core::types::S_IFLNK => P9_QTSYMLINK,
        _ => P9_QTFILE,
    }
}

pub fn qid_version(stats: &Stats) -> u32 {
    if stats.mtime_ms <= 0 {
        0
    } else {
        stats.mtime_ms as u64 as u32
    }
}

pub fn walk_step(path: impl AsRef<str>, name: &str) -> Result<String> {
    if name.is_empty() || name.contains('/') || name.contains('\0') {
        return Err(FsError::new(ErrorCode::Einval)
            .with_message("EINVAL: walk element is not a single path name"));
    }
    Ok(join_path(&[path.as_ref(), name]))
}
