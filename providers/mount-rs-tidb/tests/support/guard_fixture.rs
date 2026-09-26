use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{DirectoryEntry, FileLayout, Namespace, NodeData, NodeMetadata};
use mount_rs_core::{S_IFDIR, S_IFREG, Stats};
use std::collections::BTreeMap;
const NODES: usize = 131;
pub fn namespace() -> Namespace {
    let chunker = FixedSizeChunker::new(4096).unwrap().config();
    let root_stats = Stats {
        dev: 0,
        ino: 1,
        mode: S_IFDIR | 0o755,
        nlink: 2,
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
    };
    let mut nodes = BTreeMap::new();
    let mut entries = Vec::new();
    for inode in 2..=NODES as u64 {
        let mut stats = root_stats.clone();
        stats.ino = inode;
        stats.mode = S_IFREG | 0o644;
        stats.nlink = 1;
        nodes.insert(
            inode,
            NodeMetadata {
                stats,
                data: NodeData::File(FileLayout {
                    chunker: chunker.clone(),
                    extents: vec![],
                }),
            },
        );
        entries.push(DirectoryEntry {
            name: format!("f{inode}"),
            inode,
        });
    }
    nodes.insert(
        1,
        NodeMetadata {
            stats: root_stats,
            data: NodeData::Directory { entries },
        },
    );
    Namespace {
        format_version: 1,
        root: 1,
        next_inode: NODES as u64 + 1,
        default_uid: 0,
        default_gid: 0,
        umask: 0o022,
        default_chunker: chunker,
        nodes,
    }
}
