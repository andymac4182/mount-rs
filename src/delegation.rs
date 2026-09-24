//! Transaction-local directory authority for the persisted MRC3 protocol.
use crate::storage::{ConcurrentBackingId, InodeId, Namespace, NodeData};
use crate::{ErrorCode, FsError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GrantToken {
    pub root: InodeId,
    pub owner: String,
    pub fence: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryGrant {
    pub token: GrantToken,
    pub orphan_inodes: BTreeSet<InodeId>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationState {
    pub backing: ConcurrentBackingId,
    pub next_fence: u64,
    pub grants: BTreeMap<InodeId, DirectoryGrant>,
    pub retired: BTreeSet<GrantToken>,
}
#[derive(Debug, Clone)]
pub struct CheckoutRequest {
    pub backing: ConcurrentBackingId,
    pub root: InodeId,
    pub owner: String,
}
#[derive(Debug, Clone)]
pub struct DelegatedPublish {
    pub backing: ConcurrentBackingId,
    pub token: GrantToken,
    pub expected_revision: u64,
}
#[derive(Debug, Clone)]
pub struct DelegatedCheckin {
    pub backing: ConcurrentBackingId,
    pub token: GrantToken,
    pub expected_revision: u64,
}
#[derive(Debug, Clone)]
pub struct DelegatedRecovery {
    pub backing: ConcurrentBackingId,
    pub root: InodeId,
    pub expected_fence: u64,
}
/// Retained release/recovery receipts are bounded. Offline protocol migration is
/// required to compact them; session identities must never be reused.
pub const MAX_DELEGATION_IDENTITIES: usize = 4096;
fn denied(message: &str) -> FsError {
    FsError::new(ErrorCode::Estale).with_message(message)
}
fn subtree(ns: &Namespace, root: InodeId) -> Result<BTreeSet<InodeId>> {
    if !matches!(
        ns.nodes.get(&root).map(|n| &n.data),
        Some(NodeData::Directory { .. })
    ) {
        return Err(denied("grant root is not a directory"));
    }
    let mut found = BTreeSet::new();
    let mut pending = vec![root];
    while let Some(inode) = pending.pop() {
        if !found.insert(inode) {
            continue;
        }
        if let NodeData::Directory { entries } = &ns.nodes[&inode].data {
            pending.extend(entries.iter().map(|e| e.inode));
        }
    }
    for (parent, node) in &ns.nodes {
        if found.contains(parent) {
            continue;
        }
        if let NodeData::Directory { entries } = &node.data
            && entries
                .iter()
                .any(|e| e.inode != root && found.contains(&e.inode))
        {
            return Err(denied("subtree has an external hardlink"));
        }
    }
    Ok(found)
}
impl DelegationState {
    pub fn new(backing: ConcurrentBackingId) -> Self {
        Self {
            backing,
            next_fence: 1,
            grants: BTreeMap::new(),
            retired: BTreeSet::new(),
        }
    }
    /// Validate hostile persisted authority before executing a transaction.
    pub fn validate(&self, ns: &Namespace) -> Result<()> {
        ns.validate()?;
        ConcurrentBackingId::from_bytes(self.backing.as_bytes())?;
        if self.next_fence == 0
            || self.grants.len() + self.retired.len() > MAX_DELEGATION_IDENTITIES
        {
            return Err(denied("invalid delegation fence or receipt capacity"));
        }
        let mut fences = BTreeSet::new();
        let mut identities = BTreeSet::new();
        let mut owned = BTreeSet::new();
        for token in self
            .retired
            .iter()
            .chain(self.grants.values().map(|g| &g.token))
        {
            if token.owner.is_empty()
                || token.root == 0
                || token.fence == 0
                || token.fence >= self.next_fence
                || !fences.insert(token.fence)
                || !identities.insert((token.root, &token.owner))
            {
                return Err(denied("invalid or reused delegation identity"));
            }
        }
        for (&root, grant) in &self.grants {
            if root != grant.token.root {
                return Err(denied("grant key differs from root"));
            }
            let linked = subtree(ns, root)?;
            for &inode in &grant.orphan_inodes {
                if !ns.nodes.get(&inode).is_some_and(|n| {
                    n.stats.nlink == 0 && !matches!(n.data, NodeData::Directory { .. })
                }) {
                    return Err(denied("invalid orphan ownership provenance"));
                }
            }
            for inode in linked.iter().chain(&grant.orphan_inodes) {
                if !owned.insert(*inode) {
                    return Err(denied("overlapping directory grants"));
                }
            }
        }
        Ok(())
    }
    pub fn checkout(
        &mut self,
        ns: &Namespace,
        root: InodeId,
        owner: &str,
    ) -> Result<DirectoryGrant> {
        self.validate(ns)?;
        if owner.is_empty() {
            return Err(FsError::new(ErrorCode::Einval));
        }
        if let Some(grant) = self.grants.get(&root) {
            if grant.token.owner == owner {
                return Ok(grant.clone());
            }
            return Err(denied("directory already owned"));
        }
        if self
            .retired
            .iter()
            .any(|t| t.root == root && t.owner == owner)
        {
            return Err(denied("session identity already retired"));
        }
        if self.grants.len() + self.retired.len() >= MAX_DELEGATION_IDENTITIES {
            return Err(FsError::new(ErrorCode::Eoverflow));
        }
        let next = self
            .next_fence
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let claimed = subtree(ns, root)?;
        for grant in self.grants.values() {
            if !claimed.is_disjoint(&subtree(ns, grant.token.root)?) {
                return Err(denied("ancestor or descendant already owned"));
            }
        }
        let grant = DirectoryGrant {
            token: GrantToken {
                root,
                owner: owner.into(),
                fence: self.next_fence,
            },
            orphan_inodes: BTreeSet::new(),
        };
        self.grants.insert(root, grant.clone());
        self.next_fence = next;
        Ok(grant)
    }
    /// Complete comparison of authoritative old and hostile proposed namespace.
    /// Mutate provenance only after every check has passed; callers commit both
    /// namespace and state together. No client-provided touched list is trusted.
    pub fn authorize_publish(
        &mut self,
        old: &Namespace,
        new: &Namespace,
        token: &GrantToken,
    ) -> Result<()> {
        self.validate(old)?;
        new.validate()?;
        let grant = self
            .grants
            .get(&token.root)
            .filter(|g| g.token == *token)
            .ok_or_else(|| denied("stale directory token"))?;
        if old.format_version != new.format_version
            || old.root != new.root
            || old.default_uid != new.default_uid
            || old.default_gid != new.default_gid
            || old.umask != new.umask
            || old.default_chunker != new.default_chunker
            || new.next_inode < old.next_inode
        {
            return Err(denied("namespace header mutation"));
        }
        let before = subtree(old, token.root)?;
        let after = subtree(new, token.root)?;
        let mut authority = before.clone();
        authority.extend(&grant.orphan_inodes);
        for inode in &after {
            if old.nodes.contains_key(inode) && !before.contains(inode) {
                return Err(denied("cross-boundary link or orphan resurrection"));
            }
        }
        for inode in old.nodes.keys().chain(new.nodes.keys()) {
            if old.nodes.get(inode) == new.nodes.get(inode) {
                continue;
            }
            if old.nodes.contains_key(inode) {
                if let Some(node) = new.nodes.get(inode)
                    && (old.nodes[inode].stats.mode & crate::S_IFMT
                        != node.stats.mode & crate::S_IFMT
                        || old.nodes[inode].stats.dev != node.stats.dev
                        || old.nodes[inode].stats.birthtime_ms != node.stats.birthtime_ms)
                {
                    return Err(denied("existing inode identity mutation"));
                }
                if !authority.contains(inode) {
                    return Err(denied("outside inode mutation"));
                }
                if new.nodes.get(inode).is_some_and(|n| n.stats.nlink != 0)
                    && !after.contains(inode)
                {
                    return Err(denied("inode escaped owned subtree"));
                }
            } else if *inode < old.next_inode || !after.contains(inode) {
                return Err(denied("inode reuse or anonymous allocation"));
            }
        }
        // Each newly allocated ID must form a contiguous sequence from the
        // authoritative allocator. A sparse high ID could otherwise exhaust
        // allocation authority belonging to every other grant.
        let allocated: Vec<_> = new
            .nodes
            .keys()
            .filter(|i| !old.nodes.contains_key(i))
            .copied()
            .collect();
        let count =
            u64::try_from(allocated.len()).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let expected_next = old
            .next_inode
            .checked_add(count)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        for (offset, inode) in allocated.into_iter().enumerate() {
            let offset = u64::try_from(offset).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
            if inode
                != old
                    .next_inode
                    .checked_add(offset)
                    .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?
            {
                return Err(denied("sparse global inode allocation"));
            }
        }
        if new.next_inode != expected_next {
            return Err(denied("unauthorized global allocator mutation"));
        }
        let orphans = authority
            .into_iter()
            .filter(|i| new.nodes.get(i).is_some_and(|n| n.stats.nlink == 0))
            .collect();
        let mut candidate = self.clone();
        candidate.grants.get_mut(&token.root).unwrap().orphan_inodes = orphans;
        candidate.validate(new)?;
        *self = candidate;
        Ok(())
    }
    pub fn checkin(&mut self, token: &GrantToken, ns: &Namespace) -> Result<()> {
        self.validate(ns)?;
        if self.retired.contains(token) {
            return Ok(());
        }
        let grant = self
            .grants
            .get(&token.root)
            .filter(|g| g.token == *token)
            .ok_or_else(|| denied("stale checkin token"))?;
        if !grant.orphan_inodes.is_empty() {
            return Err(denied("open-unlinked provenance prevents checkin"));
        }
        self.grants.remove(&token.root);
        self.retired.insert(token.clone());
        Ok(())
    }
    /// Explicit crashed-owner recovery. The provider must persist both namespace
    /// and state atomically and advance namespace revision when orphans disappear.
    pub fn recover(
        &mut self,
        root: InodeId,
        expected_fence: u64,
        ns: &mut Namespace,
    ) -> Result<()> {
        self.validate(ns)?;
        if self
            .grants
            .get(&root)
            .is_some_and(|g| g.token.fence != expected_fence)
        {
            return Err(denied("recovery fence changed"));
        }
        if self
            .retired
            .iter()
            .any(|t| t.root == root && t.fence == expected_fence)
        {
            return Ok(());
        }
        let grant = self
            .grants
            .get(&root)
            .filter(|g| g.token.fence == expected_fence)
            .ok_or_else(|| denied("recovery fence changed"))?
            .clone();
        let mut candidate = ns.clone();
        for inode in &grant.orphan_inodes {
            candidate.nodes.remove(inode);
        }
        candidate.validate()?;
        self.grants.remove(&root);
        self.retired.insert(grant.token);
        *ns = candidate;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunking::ChunkerConfig;
    use crate::storage::{DirectoryEntry, FileLayout, NodeMetadata};
    use crate::{S_IFDIR, S_IFREG, Stats};
    fn namespace() -> Namespace {
        let chunker = ChunkerConfig {
            algorithm: "fixed-size".into(),
            version: 1,
            parameters: BTreeMap::from([("chunk_size".into(), 4096)]),
        };
        let nodes = (1..=5)
            .map(|ino| {
                let dir = ino <= 3;
                let entries = match ino {
                    1 => vec![("a", 2), ("b", 3)],
                    2 => vec![("file", 4)],
                    3 => vec![("file", 5)],
                    _ => vec![],
                };
                (
                    ino,
                    NodeMetadata {
                        stats: Stats {
                            dev: 1,
                            ino,
                            mode: if dir {
                                S_IFDIR | 0o755
                            } else {
                                S_IFREG | 0o600
                            },
                            nlink: if ino == 1 {
                                4
                            } else if dir {
                                2
                            } else {
                                1
                            },
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
                        },
                        data: if dir {
                            NodeData::Directory {
                                entries: entries
                                    .into_iter()
                                    .map(|(name, inode)| DirectoryEntry {
                                        name: name.into(),
                                        inode,
                                    })
                                    .collect(),
                            }
                        } else {
                            NodeData::File(FileLayout {
                                chunker: chunker.clone(),
                                extents: vec![],
                            })
                        },
                    },
                )
            })
            .collect();
        Namespace {
            format_version: 1,
            root: 1,
            next_inode: 8,
            default_uid: 0,
            default_gid: 0,
            umask: 0o022,
            default_chunker: chunker,
            nodes,
        }
    }
    fn state() -> DelegationState {
        DelegationState::new(ConcurrentBackingId::from_bytes([1; 16]).unwrap())
    }
    #[test]
    fn grants_are_disjoint_retry_safe_and_monotonically_fenced() {
        let ns = namespace();
        let mut s = state();
        let a = s.checkout(&ns, 2, "session-a").unwrap();
        assert_eq!(s.checkout(&ns, 2, "session-a").unwrap(), a);
        assert!(s.checkout(&ns, 1, "other").is_err());
        let b = s.checkout(&ns, 3, "session-b").unwrap();
        assert!(b.token.fence > a.token.fence);
        s.checkin(&a.token, &ns).unwrap();
        s.checkin(&a.token, &ns).unwrap();
        assert!(s.checkout(&ns, 2, "session-a").is_err());
        let newer = s.checkout(&ns, 2, "new").unwrap();
        assert!(s.recover(2, a.token.fence, &mut ns.clone()).is_err());
        s.recover(2, newer.token.fence, &mut ns.clone()).unwrap();
        assert!(s.authorize_publish(&ns, &ns, &newer.token).is_err());
    }
    #[test]
    fn hostile_namespace_deltas_are_denied() {
        let ns = namespace();
        let mut s = state();
        let grant = s.checkout(&ns, 2, "a").unwrap();
        let mut good = ns.clone();
        good.nodes.get_mut(&4).unwrap().stats.mtime_ms = 1;
        s.authorize_publish(&ns, &good, &grant.token).unwrap();
        for target in [1, 3, 5] {
            let mut bad = ns.clone();
            bad.nodes.get_mut(&target).unwrap().stats.mtime_ms = 1;
            assert!(s.authorize_publish(&ns, &bad, &grant.token).is_err());
        }
        let mut header = ns.clone();
        header.umask = 0;
        assert!(s.authorize_publish(&ns, &header, &grant.token).is_err());
        let mut reuse = ns.clone();
        let mut node = reuse.nodes[&4].clone();
        node.stats.ino = 6;
        reuse.nodes.insert(6, node);
        if let NodeData::Directory { entries } = &mut reuse.nodes.get_mut(&2).unwrap().data {
            entries.push(DirectoryEntry {
                name: "new".into(),
                inode: 6,
            });
        }
        assert!(s.authorize_publish(&ns, &reuse, &grant.token).is_err());
    }
    #[test]
    fn external_hardlinks_and_overflow_fail_closed() {
        let mut ns = namespace();
        if let NodeData::Directory { entries } = &mut ns.nodes.get_mut(&3).unwrap().data {
            entries.push(DirectoryEntry {
                name: "link".into(),
                inode: 4,
            });
        }
        ns.nodes.get_mut(&4).unwrap().stats.nlink = 2;
        assert!(state().checkout(&ns, 2, "a").is_err());
        let mut s = state();
        s.next_fence = u64::MAX;
        assert!(s.checkout(&namespace(), 2, "a").is_err());
        assert!(s.grants.is_empty());
    }
    #[test]
    fn orphan_provenance_survives_unlink_but_cannot_escape() {
        let ns = namespace();
        let mut s = state();
        let g = s.checkout(&ns, 2, "a").unwrap();
        let mut unlinked = ns.clone();
        if let NodeData::Directory { entries } = &mut unlinked.nodes.get_mut(&2).unwrap().data {
            entries.clear();
        }
        unlinked.nodes.get_mut(&4).unwrap().stats.nlink = 0;
        s.authorize_publish(&ns, &unlinked, &g.token).unwrap();
        assert!(s.grants[&2].orphan_inodes.contains(&4));
        assert!(s.checkin(&g.token, &unlinked).is_err());
        let mut write = unlinked.clone();
        write.nodes.get_mut(&4).unwrap().stats.mtime_ms = 99;
        s.authorize_publish(&unlinked, &write, &g.token).unwrap();
        let mut escape = write.clone();
        if let NodeData::Directory { entries } = &mut escape.nodes.get_mut(&3).unwrap().data {
            entries.push(DirectoryEntry {
                name: "stolen".into(),
                inode: 4,
            });
        }
        escape.nodes.get_mut(&4).unwrap().stats.nlink = 1;
        assert!(s.authorize_publish(&write, &escape, &g.token).is_err());
        let mut closed = write.clone();
        closed.nodes.remove(&4);
        s.authorize_publish(&write, &closed, &g.token).unwrap();
        s.checkin(&g.token, &closed).unwrap();
    }
    #[test]
    fn allocations_and_recovery_are_atomic_and_retry_safe() {
        let ns = namespace();
        let mut state = state();
        let grant = state.checkout(&ns, 2, "a").unwrap();
        let mut allocated = ns.clone();
        let mut node = ns.nodes[&4].clone();
        node.stats.ino = 8;
        allocated.nodes.insert(8, node);
        allocated.next_inode = 9;
        if let NodeData::Directory { entries } = &mut allocated.nodes.get_mut(&2).unwrap().data {
            entries.push(DirectoryEntry {
                name: "new".into(),
                inode: 8,
            });
        }
        state
            .authorize_publish(&ns, &allocated, &grant.token)
            .unwrap();
        let mut anonymous = allocated.clone();
        let mut node = ns.nodes[&4].clone();
        node.stats.ino = 9;
        node.stats.nlink = 0;
        anonymous.nodes.insert(9, node);
        anonymous.next_inode = 10;
        let prior = state.clone();
        assert!(
            state
                .authorize_publish(&allocated, &anonymous, &grant.token)
                .is_err()
        );
        assert_eq!(state, prior);
        let mut unlinked = allocated.clone();
        if let NodeData::Directory { entries } = &mut unlinked.nodes.get_mut(&2).unwrap().data {
            entries.clear();
        }
        for inode in [4, 8] {
            unlinked.nodes.get_mut(&inode).unwrap().stats.nlink = 0;
        }
        state
            .authorize_publish(&allocated, &unlinked, &grant.token)
            .unwrap();
        let prior = state.clone();
        let before = unlinked.clone();
        assert!(
            state
                .recover(2, grant.token.fence + 1, &mut unlinked)
                .is_err()
        );
        assert_eq!(state, prior);
        assert_eq!(unlinked.nodes, before.nodes);
        state.recover(2, grant.token.fence, &mut unlinked).unwrap();
        assert!(!unlinked.nodes.contains_key(&4));
        assert!(!unlinked.nodes.contains_key(&8));
        assert_eq!(unlinked.next_inode, 9);
        state.recover(2, grant.token.fence, &mut unlinked).unwrap();
        state.validate(&unlinked).unwrap();
    }
    #[test]
    fn malformed_persisted_grants_are_rejected() {
        let ns = namespace();
        let mut original = state();
        let grant = original.checkout(&ns, 2, "a").unwrap();
        let mut malformed = original.clone();
        malformed.next_fence = grant.token.fence;
        assert!(malformed.validate(&ns).is_err());
        let mut malformed = original.clone();
        malformed.grants.get_mut(&2).unwrap().token.root = 3;
        assert!(malformed.validate(&ns).is_err());
        let mut malformed = original.clone();
        malformed
            .grants
            .get_mut(&2)
            .unwrap()
            .orphan_inodes
            .insert(5);
        assert!(malformed.validate(&ns).is_err());
        let mut malformed = original.clone();
        malformed.retired.insert(grant.token.clone());
        assert!(malformed.validate(&ns).is_err());
        let malformed: DelegationState =
            serde_json::from_str(&serde_json::to_string(&original).unwrap().replace(
                "[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1]",
                "[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]",
            ))
            .unwrap();
        assert!(malformed.validate(&ns).is_err());
    }
    #[test]
    fn existing_inode_kind_cannot_be_reused() {
        let ns = namespace();
        let mut state = state();
        let grant = state.checkout(&ns, 2, "a").unwrap();
        let mut candidate = ns.clone();
        candidate.nodes.get_mut(&4).unwrap().stats.mode = crate::S_IFLNK | 0o777;
        candidate.nodes.get_mut(&4).unwrap().data = NodeData::Symlink {
            target: "elsewhere".into(),
        };
        candidate.validate().unwrap();
        assert!(
            state
                .authorize_publish(&ns, &candidate, &grant.token)
                .is_err()
        );
    }
    #[test]
    fn sparse_inode_allocations_cannot_exhaust_global_authority() {
        let ns = namespace();
        let mut original = state();
        let grant = original.checkout(&ns, 2, "a").unwrap();
        for inode in [ns.next_inode + 1, u64::MAX - 1] {
            let mut candidate = ns.clone();
            let mut node = ns.nodes[&4].clone();
            node.stats.ino = inode;
            candidate.nodes.insert(inode, node);
            candidate.next_inode = inode + 1;
            if let NodeData::Directory { entries } = &mut candidate.nodes.get_mut(&2).unwrap().data
            {
                entries.push(DirectoryEntry {
                    name: "allocation-gap".into(),
                    inode,
                });
            }
            candidate.validate().unwrap();
            let mut state = original.clone();
            assert!(
                state
                    .authorize_publish(&ns, &candidate, &grant.token)
                    .is_err(),
                "sparse inode {inode}"
            );
            assert_eq!(state, original);
        }
    }
}
