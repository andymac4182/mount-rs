//! The namespace's inode table: an ordered map that shares structure between
//! copies.
//!
//! A mutation works on a copy of the namespace and publishes it, and readers
//! keep using the copy they started with. With an ordinary map every one of
//! those copies costs time and memory proportional to the whole volume.
//! `NodeMap` is a persistent 64-way radix tree over the inode number: cloning
//! it is one reference-count increment, and changing an inode in a clone
//! copies only the few branches on the way to that inode. Everything else
//! stays shared with the original.
//!
//! The interface follows the `BTreeMap` it replaces, including ascending
//! iteration order and the serialized form.
use crate::storage::{InodeId, NodeMetadata};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::Arc;

const BITS: u32 = 6;
const WIDTH: u64 = 1 << BITS;
/// Levels needed to cover every 64-bit inode number.
const MAX_LEVELS: u32 = 11;

type Entry = (InodeId, NodeMetadata);

#[derive(Clone)]
enum Slot {
    Branch(Arc<Branch>),
    Leaf(Arc<Entry>),
}

/// Up to 64 children, stored densely in index order; `occupied` says which
/// indexes are present.
#[derive(Clone, Default)]
struct Branch {
    occupied: u64,
    slots: Vec<Slot>,
}

impl Branch {
    fn position(&self, index: u64) -> Option<usize> {
        let bit = 1_u64 << index;
        (self.occupied & bit != 0).then(|| (self.occupied & (bit - 1)).count_ones() as usize)
    }

    fn get(&self, index: u64) -> Option<&Slot> {
        self.position(index).map(|position| &self.slots[position])
    }

    fn set(&mut self, index: u64, slot: Slot) {
        let bit = 1_u64 << index;
        let position = (self.occupied & (bit - 1)).count_ones() as usize;
        if self.occupied & bit != 0 {
            self.slots[position] = slot;
        } else {
            self.occupied |= bit;
            self.slots.insert(position, slot);
        }
    }

    fn take(&mut self, index: u64) -> Option<Slot> {
        let position = self.position(index)?;
        self.occupied &= !(1_u64 << index);
        Some(self.slots.remove(position))
    }
}

#[derive(Clone, Default)]
pub struct NodeMap {
    root: Arc<Branch>,
    /// Levels below the root, so the tree covers inode numbers below
    /// `64^(levels + 1)`. Grows as larger numbers are inserted.
    levels: u32,
    len: usize,
}

fn index_at(key: InodeId, level: u32) -> u64 {
    (key >> (level * BITS)) & (WIDTH - 1)
}

impl NodeMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn covers(&self, key: InodeId) -> bool {
        self.levels + 1 >= MAX_LEVELS || key >> ((self.levels + 1) * BITS) == 0
    }

    fn entry(&self, key: InodeId) -> Option<&Arc<Entry>> {
        if !self.covers(key) {
            return None;
        }
        let mut branch = &*self.root;
        for level in (1..=self.levels).rev() {
            match branch.get(index_at(key, level))? {
                Slot::Branch(child) => branch = child,
                Slot::Leaf(_) => return None,
            }
        }
        match branch.get(index_at(key, 0))? {
            Slot::Leaf(entry) => Some(entry),
            Slot::Branch(_) => None,
        }
    }

    pub fn get(&self, key: &InodeId) -> Option<&NodeMetadata> {
        self.entry(*key).map(|entry| &entry.1)
    }

    pub fn contains_key(&self, key: &InodeId) -> bool {
        self.entry(*key).is_some()
    }

    /// The branch that holds `key`'s leaf, with every branch on the way made
    /// this map's own. `create` adds missing branches; without it a missing
    /// one ends the walk.
    fn leaf_branch(&mut self, key: InodeId, create: bool) -> Option<&mut Branch> {
        let mut branch = Arc::make_mut(&mut self.root);
        for level in (1..=self.levels).rev() {
            let index = index_at(key, level);
            if branch.position(index).is_none() {
                if !create {
                    return None;
                }
                branch.set(index, Slot::Branch(Arc::default()));
            }
            let position = branch.position(index)?;
            match &mut branch.slots[position] {
                Slot::Branch(child) => branch = Arc::make_mut(child),
                Slot::Leaf(_) => return None,
            }
        }
        Some(branch)
    }

    /// A mutable reference to the inode. Only it and the branches leading to
    /// it are copied if they are shared with another map.
    pub fn get_mut(&mut self, key: &InodeId) -> Option<&mut NodeMetadata> {
        // Look first: a miss must not copy anything.
        self.entry(*key)?;
        let branch = self.leaf_branch(*key, false)?;
        let position = branch.position(index_at(*key, 0))?;
        match &mut branch.slots[position] {
            Slot::Leaf(entry) => Some(&mut Arc::make_mut(entry).1),
            Slot::Branch(_) => None,
        }
    }

    pub fn insert(&mut self, key: InodeId, value: NodeMetadata) -> Option<NodeMetadata> {
        while !self.covers(key) {
            // One more level on top; the old root becomes its first child.
            let below = std::mem::take(&mut self.root);
            if below.occupied != 0 {
                Arc::make_mut(&mut self.root).set(0, Slot::Branch(below));
            }
            self.levels += 1;
        }
        let branch = self.leaf_branch(key, true)?;
        let index = index_at(key, 0);
        let previous = branch.take(index);
        branch.set(index, Slot::Leaf(Arc::new((key, value))));
        match previous {
            Some(Slot::Leaf(entry)) => Some(unshare(entry)),
            _ => {
                self.len += 1;
                None
            }
        }
    }

    pub fn remove(&mut self, key: &InodeId) -> Option<NodeMetadata> {
        self.entry(*key)?;
        let removed = remove_below(Arc::make_mut(&mut self.root), *key, self.levels)?;
        self.len -= 1;
        Some(unshare(removed))
    }

    /// Calls `visit(inode, before, after)` for every inode that may differ
    /// between `self` (before) and `newer` (after), in ascending order, and
    /// stops early when `visit` returns `false`. The return value says
    /// whether the walk finished.
    ///
    /// Subtrees the two maps share are skipped without being looked at, so
    /// when one map was made from the other the cost follows the number of
    /// changed inodes and not the size of the volume. An inode that was
    /// borrowed mutably and left as it was is still reported.
    pub fn differences(
        &self,
        newer: &Self,
        mut visit: impl FnMut(InodeId, Option<&NodeMetadata>, Option<&NodeMetadata>) -> bool,
    ) -> bool {
        // Bring both roots to the same height: a taller map keeps the
        // shorter one's range under its first child.
        let empty = Arc::<Branch>::default();
        let (mut before, mut after) = (&self.root, &newer.root);
        let (mut before_levels, mut after_levels) = (self.levels, newer.levels);
        let mut skipped = Vec::new();
        while before_levels != after_levels {
            let (tall, levels, is_before) = if before_levels > after_levels {
                (&mut before, &mut before_levels, true)
            } else {
                (&mut after, &mut after_levels, false)
            };
            // Everything beside the first child exists on one side only.
            // Those inodes are larger than any shared one, so they are
            // visited last; the shallowest level holds the smallest of them.
            skipped.push((&***tall, *levels, is_before));
            *tall = match tall.get(0) {
                Some(Slot::Branch(child)) => child,
                _ => &empty,
            };
            *levels -= 1;
        }
        if !Arc::ptr_eq(before, after) && !differ(Some(before), Some(after), 0, &mut visit) {
            return false;
        }
        for (branch, _, is_before) in skipped.into_iter().rev() {
            let (before, after) = if is_before {
                (Some(branch), None)
            } else {
                (None, Some(branch))
            };
            // Index 0 was compared above.
            if !differ(before, after, 1, &mut visit) {
                return false;
            }
        }
        true
    }

    /// The largest inode number present.
    pub fn last_key(&self) -> Option<InodeId> {
        let mut branch = &*self.root;
        loop {
            match branch.slots.last()? {
                Slot::Branch(child) => branch = child,
                Slot::Leaf(entry) => return Some(entry.0),
            }
        }
    }

    pub fn iter(&self) -> Iter<'_> {
        Iter {
            stack: vec![self.root.slots.iter()],
            remaining: self.len,
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = &InodeId> {
        self.iter().map(|(key, _)| key)
    }

    pub fn values(&self) -> impl Iterator<Item = &NodeMetadata> {
        self.iter().map(|(_, value)| value)
    }
}

type Visit<'a> = dyn FnMut(InodeId, Option<&NodeMetadata>, Option<&NodeMetadata>) -> bool + 'a;

/// Compares two branches at the same position, from index `from` up.
fn differ(
    before: Option<&Branch>,
    after: Option<&Branch>,
    from: u64,
    visit: &mut Visit<'_>,
) -> bool {
    let occupied = |branch: Option<&Branch>| branch.map_or(0, |branch| branch.occupied);
    let mut indexes = (occupied(before) | occupied(after)) & (u64::MAX << from);
    while indexes != 0 {
        let index = u64::from(indexes.trailing_zeros());
        indexes &= indexes - 1;
        let finished = match (slot_at(before, index), slot_at(after, index)) {
            (Some(Slot::Leaf(old)), Some(Slot::Leaf(new))) => {
                Arc::ptr_eq(old, new) || visit(old.0, Some(&old.1), Some(&new.1))
            }
            (Some(Slot::Branch(old)), Some(Slot::Branch(new))) => {
                Arc::ptr_eq(old, new) || differ(Some(old), Some(new), 0, visit)
            }
            // Present on one side only. (A leaf never shares a position with
            // a branch: the level decides which one a slot holds.)
            (old, new) => one_sided(old, true, visit) && one_sided(new, false, visit),
        };
        if !finished {
            return false;
        }
    }
    true
}

fn slot_at(branch: Option<&Branch>, index: u64) -> Option<&Slot> {
    branch.and_then(|branch| branch.get(index))
}

fn one_sided(slot: Option<&Slot>, is_before: bool, visit: &mut Visit<'_>) -> bool {
    match slot {
        None => true,
        Some(Slot::Leaf(entry)) if is_before => visit(entry.0, Some(&entry.1), None),
        Some(Slot::Leaf(entry)) => visit(entry.0, None, Some(&entry.1)),
        Some(Slot::Branch(branch)) if is_before => differ(Some(branch), None, 0, visit),
        Some(Slot::Branch(branch)) => differ(None, Some(branch), 0, visit),
    }
}

fn unshare(entry: Arc<Entry>) -> NodeMetadata {
    Arc::try_unwrap(entry).map_or_else(|shared| shared.1.clone(), |owned| owned.1)
}

/// Removes `key` from under `branch`, dropping branches left empty.
fn remove_below(branch: &mut Branch, key: InodeId, level: u32) -> Option<Arc<Entry>> {
    let index = index_at(key, level);
    if level == 0 {
        return match branch.take(index)? {
            Slot::Leaf(entry) => Some(entry),
            Slot::Branch(_) => None,
        };
    }
    let position = branch.position(index)?;
    let Slot::Branch(child) = &mut branch.slots[position] else {
        return None;
    };
    let child = Arc::make_mut(child);
    let removed = remove_below(child, key, level - 1)?;
    if child.occupied == 0 {
        branch.take(index);
    }
    Some(removed)
}

/// Inodes in ascending order.
pub struct Iter<'a> {
    stack: Vec<std::slice::Iter<'a, Slot>>,
    remaining: usize,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a InodeId, &'a NodeMetadata);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.stack.last_mut()?.next() {
                Some(Slot::Leaf(entry)) => {
                    self.remaining -= 1;
                    return Some((&entry.0, &entry.1));
                }
                Some(Slot::Branch(branch)) => self.stack.push(branch.slots.iter()),
                None => {
                    self.stack.pop();
                }
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for Iter<'_> {}

impl<'a> IntoIterator for &'a NodeMap {
    type Item = (&'a InodeId, &'a NodeMetadata);
    type IntoIter = Iter<'a>;

    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

impl Extend<(InodeId, NodeMetadata)> for NodeMap {
    fn extend<I: IntoIterator<Item = (InodeId, NodeMetadata)>>(&mut self, entries: I) {
        for (key, value) in entries {
            self.insert(key, value);
        }
    }
}

impl FromIterator<(InodeId, NodeMetadata)> for NodeMap {
    fn from_iter<I: IntoIterator<Item = (InodeId, NodeMetadata)>>(entries: I) -> Self {
        let mut map = Self::new();
        map.extend(entries);
        map
    }
}

impl From<std::collections::BTreeMap<InodeId, NodeMetadata>> for NodeMap {
    fn from(entries: std::collections::BTreeMap<InodeId, NodeMetadata>) -> Self {
        entries.into_iter().collect()
    }
}

impl<const N: usize> From<[(InodeId, NodeMetadata); N]> for NodeMap {
    fn from(entries: [(InodeId, NodeMetadata); N]) -> Self {
        entries.into_iter().collect()
    }
}

impl PartialEq for NodeMap {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl Eq for NodeMap {}

impl std::fmt::Debug for NodeMap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

impl std::ops::Index<&InodeId> for NodeMap {
    type Output = NodeMetadata;

    fn index(&self, key: &InodeId) -> &NodeMetadata {
        self.get(key).expect("no entry found for inode")
    }
}

impl Serialize for NodeMap {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.len))?;
        for (key, value) in self {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for NodeMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = NodeMap;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a map of inode numbers to node metadata")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut access: A,
            ) -> Result<NodeMap, A::Error> {
                let mut map = NodeMap::new();
                while let Some((key, value)) = access.next_entry::<InodeId, NodeMetadata>()? {
                    map.insert(key, value);
                }
                Ok(map)
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::NodeData;
    use crate::types::Stats;
    use std::collections::BTreeMap;

    fn node(size: u64) -> NodeMetadata {
        NodeMetadata {
            stats: Stats {
                dev: 0,
                ino: 0,
                mode: 0,
                nlink: 1,
                uid: 0,
                gid: 0,
                rdev: 0,
                size,
                blksize: 4096,
                blocks: 0,
                atime_ms: 0,
                mtime_ms: 0,
                ctime_ms: 0,
                birthtime_ms: 0,
            },
            data: NodeData::Special,
        }
    }

    /// Keys spread over every tree height, including the largest.
    fn keys() -> Vec<InodeId> {
        let mut keys: Vec<InodeId> = (0..300).collect();
        keys.extend([
            4095,
            4096,
            262_143,
            262_144,
            1 << 40,
            u64::MAX - 1,
            u64::MAX,
        ]);
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..500 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            keys.push(state >> (state % 60));
        }
        keys
    }

    #[test]
    fn behaves_like_an_ordered_map_at_every_height() {
        let mut map = NodeMap::new();
        let mut model = BTreeMap::new();
        for (round, key) in keys().into_iter().enumerate() {
            let value = node(round as u64);
            assert_eq!(map.insert(key, value.clone()), model.insert(key, value));
            assert_eq!(map.len(), model.len());
        }
        assert!(map.iter().eq(model.iter()));
        assert!(map.keys().eq(model.keys()));
        assert_eq!(map.last_key(), model.keys().next_back().copied());
        assert!(map.values().eq(model.values()));
        for key in keys() {
            assert_eq!(map.get(&key), model.get(&key));
            assert_eq!(map.contains_key(&(key ^ 1)), model.contains_key(&(key ^ 1)));
            if let Some(node) = map.get_mut(&key) {
                node.stats.size += 1;
            }
            model.get_mut(&key).unwrap().stats.size += 1;
        }
        assert!(map.get_mut(&777_777_777_777).is_none());
        for key in keys().into_iter().step_by(2) {
            assert_eq!(map.remove(&key), model.remove(&key));
            assert_eq!(map.remove(&key), None);
        }
        assert!(map.iter().eq(model.iter()));
        assert_eq!(map.len(), model.len());
        assert_eq!(map.iter().len(), model.len());
        for key in model.keys().copied().collect::<Vec<_>>() {
            assert!(map.remove(&key).is_some());
        }
        assert!(map.is_empty());
        assert_eq!(map.root.occupied, 0);
    }

    #[test]
    fn a_clone_shares_everything_until_one_side_changes_an_inode() {
        let original: NodeMap = (0..10_000).map(|key| (key, node(key))).collect();
        let mut changed = original.clone();
        assert!(Arc::ptr_eq(&original.root, &changed.root));

        changed.get_mut(&4242).unwrap().stats.size = 1;
        changed.insert(20_000, node(7));
        changed.remove(&17);
        // The original is untouched.
        assert_eq!(original.get(&4242).unwrap().stats.size, 4242);
        assert!(original.get(&20_000).is_none());
        assert!(original.contains_key(&17));
        assert_eq!(original.len(), 10_000);
        assert_eq!(changed.len(), 10_000);
        // An inode neither side touched is still the same allocation.
        assert!(Arc::ptr_eq(
            original.entry(9000).unwrap(),
            changed.entry(9000).unwrap()
        ));
        // A lookup that misses copies nothing.
        let mut reader = original.clone();
        assert!(reader.get_mut(&999_999).is_none());
        assert!(reader.remove(&999_999).is_none());
        assert!(Arc::ptr_eq(&original.root, &reader.root));
    }

    #[test]
    fn differences_report_every_change_and_skip_what_is_shared() {
        let changes = |before: &NodeMap, after: &NodeMap| {
            let mut seen = Vec::new();
            assert!(before.differences(after, |inode, old, new| {
                seen.push((inode, old.map(|n| n.stats.size), new.map(|n| n.stats.size)));
                true
            }));
            seen
        };
        let expected = |before: &NodeMap, after: &NodeMap| {
            let mut all: Vec<InodeId> = before.keys().chain(after.keys()).copied().collect();
            all.sort_unstable();
            all.dedup();
            all.into_iter()
                .map(|inode| {
                    let size = |map: &NodeMap| map.get(&inode).map(|n| n.stats.size);
                    (inode, size(before), size(after))
                })
                .filter(|(_, old, new)| old != new)
                .collect::<Vec<_>>()
        };
        // Unrelated maps of different heights, both ways, and an empty one.
        let small: NodeMap = (0..70).map(|key| (key, node(key))).collect();
        let large: NodeMap = keys().into_iter().map(|key| (key, node(key ^ 1))).collect();
        for (before, after) in [
            (&small, &large),
            (&large, &small),
            (&NodeMap::new(), &large),
            (&large, &NodeMap::new()),
        ] {
            assert_eq!(changes(before, after), expected(before, after));
        }
        // A map made from another reports exactly what was done to it.
        let mut edited = large.clone();
        assert!(changes(&large, &edited).is_empty());
        edited.get_mut(&4096).unwrap().stats.size = 1;
        edited.remove(&7);
        edited.remove(&u64::MAX);
        edited.insert(1 << 50, node(2));
        edited.insert(12, node(3));
        assert_eq!(changes(&large, &edited), expected(&large, &edited));
        assert_eq!(changes(&edited, &large), expected(&edited, &large));
        assert_eq!(changes(&large, &edited).len(), 5);
        // It visits the changed inodes only, however large the map is.
        let mut visited = 0;
        assert!(large.differences(&edited, |_, _, _| {
            visited += 1;
            true
        }));
        assert_eq!(visited, 5);
        // The walk stops when asked to.
        let mut visited = 0;
        assert!(!large.differences(&edited, |_, _, _| {
            visited += 1;
            visited < 2
        }));
        assert_eq!(visited, 2);
        // Growing a level leaves the old range shared.
        let mut grown = small.clone();
        grown.insert(1 << 30, node(9));
        assert_eq!(changes(&small, &grown), vec![(1 << 30, None, Some(9))]);
    }

    #[test]
    fn serializes_exactly_like_the_map_it_replaces() {
        let model: BTreeMap<InodeId, NodeMetadata> =
            [1, 2, 70, 5000, 1 << 33].map(|key| (key, node(key))).into();
        let map: NodeMap = model.clone().into_iter().collect();
        let encoded = serde_json::to_string(&map).unwrap();
        assert_eq!(encoded, serde_json::to_string(&model).unwrap());
        let decoded: NodeMap = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, map);
        assert_eq!(format!("{decoded:?}"), format!("{model:?}"));
    }
}
