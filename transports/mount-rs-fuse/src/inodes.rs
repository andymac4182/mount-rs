use mount_rs_core::{ErrorCode, FsError, Result, Stats};
use std::collections::HashMap;

#[derive(Debug)]
pub struct Inode {
    pub nodeid: u64,
    pub key: Option<(u64, u64)>,
    pub nlookup: u64,
    /// Insertion order determines the primary path.
    pub paths: Vec<String>,
}

pub struct InodeTable {
    nodes: HashMap<u64, Inode>,
    paths: HashMap<String, u64>,
    identities: HashMap<(u64, u64), u64>,
    next: u64,
    use_driver_ino: bool,
}

impl Default for InodeTable {
    fn default() -> Self {
        Self::new(true)
    }
}

impl InodeTable {
    pub fn new(use_driver_ino: bool) -> Self {
        let root = Inode {
            nodeid: 1,
            key: None,
            nlookup: 1,
            paths: vec!["/".into()],
        };
        Self {
            nodes: HashMap::from([(1, root)]),
            paths: HashMap::from([("/".into(), 1)]),
            identities: HashMap::new(),
            next: 2,
            use_driver_ino,
        }
    }
    pub fn get(&self, nodeid: u64) -> Option<&Inode> {
        self.nodes.get(&nodeid)
    }
    pub fn at(&self, path: &str) -> Option<&Inode> {
        self.paths.get(path).and_then(|id| self.get(*id))
    }
    pub fn require_path(&self, nodeid: u64) -> Result<&str> {
        let inode = self
            .get(nodeid)
            .ok_or_else(|| FsError::new(ErrorCode::Estale).with_syscall("lookup"))?;
        inode
            .paths
            .first()
            .map(String::as_str)
            .ok_or_else(|| FsError::new(ErrorCode::Enoent))
    }
    pub fn bind(&mut self, path: &str, stats: &Stats) -> u64 {
        let key = (self.use_driver_ino && stats.ino > 0).then_some((stats.dev, stats.ino));
        let previous = self.paths.get(path).copied();
        let same = previous.filter(|id| self.nodes[id].key.is_none() || self.nodes[id].key == key);
        let id = key
            .and_then(|key| self.identities.get(&key).copied())
            .or(same)
            .unwrap_or_else(|| {
                let id = self.next;
                self.next += 1;
                self.nodes.insert(
                    id,
                    Inode {
                        nodeid: id,
                        key,
                        nlookup: 0,
                        paths: Vec::new(),
                    },
                );
                id
            });
        if previous.is_some_and(|previous| previous != id) {
            self.unbind(path);
        }
        let inode = self.nodes.get_mut(&id).unwrap();
        if inode.key.is_none() {
            inode.key = key;
        }
        self.attach(id, path.to_owned());
        id
    }
    fn attach(&mut self, id: u64, path: String) {
        let inode = self.nodes.get_mut(&id).unwrap();
        if !inode.paths.contains(&path) {
            inode.paths.push(path.clone());
        }
        self.paths.insert(path, id);
        if let Some(key) = inode.key {
            self.identities.insert(key, id);
        }
    }
    pub fn acquire(&mut self, id: u64) -> Result<()> {
        let inode = self
            .nodes
            .get_mut(&id)
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
        inode.nlookup = inode
            .nlookup
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        Ok(())
    }
    pub fn unbind(&mut self, path: &str) -> Option<u64> {
        let id = self.paths.remove(path)?;
        let inode = self.nodes.get_mut(&id).unwrap();
        inode.paths.retain(|p| p != path);
        if inode.paths.is_empty()
            && let Some(key) = inode.key
            && self.identities.get(&key) == Some(&id)
        {
            self.identities.remove(&key);
        }
        Some(id)
    }
    pub fn forget(&mut self, id: u64, count: u64) -> bool {
        if id == 1 {
            return false;
        }
        let Some(inode) = self.nodes.get_mut(&id) else {
            return false;
        };
        inode.nlookup = inode.nlookup.saturating_sub(count);
        if inode.nlookup != 0 {
            return false;
        }
        let inode = self.nodes.remove(&id).unwrap();
        for path in inode.paths {
            self.paths.remove(&path);
        }
        if let Some(key) = inode.key
            && self.identities.get(&key) == Some(&id)
        {
            self.identities.remove(&key);
        }
        true
    }
    pub fn remap(&mut self, from: &str, to: &str) {
        if from == to {
            return;
        }
        let inside =
            |path: &str, parent: &str| path == parent || path.starts_with(&format!("{parent}/"));
        let moved: Vec<_> = self
            .paths
            .iter()
            .filter(|(path, _)| inside(path, from))
            .map(|(path, id)| (path.clone(), *id))
            .collect();
        let replaced: Vec<_> = self
            .paths
            .keys()
            .filter(|path| inside(path, to))
            .cloned()
            .collect();
        for path in replaced {
            self.unbind(&path);
        }
        for (path, _) in &moved {
            self.unbind(path);
        }
        for (path, id) in moved {
            self.attach(id, format!("{to}{}", &path[from.len()..]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stat(ino: u64) -> Stats {
        Stats {
            dev: 0,
            ino,
            mode: 0o100644,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            size: 0,
            blksize: 4096,
            blocks: 0,
            atime_ms: 0,
            mtime_ms: 0,
            ctime_ms: 0,
            birthtime_ms: 0,
        }
    }
    #[test]
    fn hardlinks_orphans_and_reused_driver_identity() {
        let mut table = InodeTable::default();
        let old = table.bind("/a", &stat(2));
        table.acquire(old).unwrap();
        assert_eq!(table.bind("/b", &stat(2)), old);
        table.unbind("/a");
        assert_eq!(table.require_path(old).unwrap(), "/b");
        table.unbind("/b");
        assert_eq!(table.require_path(old).unwrap_err().code, ErrorCode::Enoent);
        let new = table.bind("/new", &stat(2));
        assert_ne!(old, new);
        table.forget(old, 1);
        assert_eq!(table.bind("/new-hard", &stat(2)), new);
        assert!(!table.forget(1, u64::MAX));
    }
    #[test]
    fn directory_rename_preserves_ids_and_orphans_replaced_targets() {
        let mut table = InodeTable::default();
        let child = table.bind("/old/deep/file", &stat(2));
        let replaced = table.bind("/new/deep/file", &stat(3));
        table.remap("/old", "/new");
        assert_eq!(table.require_path(child).unwrap(), "/new/deep/file");
        assert_eq!(
            table.require_path(replaced).unwrap_err().code,
            ErrorCode::Enoent
        );
        assert!(table.at("/old/deep/file").is_none());
    }
}
