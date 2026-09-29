//! Current-candidate checks for a prepared concurrent fresh create.
//!
//! This only rebinds an ephemeral batch mutation. The queued preparation and
//! the existing apply guard remain the source of truth for conflicts.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PreparedCreateRebase {
    Unchanged,
    Rebased,
    ChunkerChanged,
}

pub(super) fn rebase_prepared_create(
    namespace: &Namespace,
    current_revision: u64,
    mutation: &mut WholeFileMutation,
    require_parent: impl FnOnce(InodeId) -> Result<()>,
) -> Result<PreparedCreateRebase> {
    if !mutation.new_inode {
        return Ok(PreparedCreateRebase::Unchanged);
    }

    let entry = walk(namespace, &mutation.path, true, "open", 0)?;
    if entry.node.is_some() {
        return Ok(PreparedCreateRebase::Unchanged);
    }
    let parent = namespace
        .nodes
        .get(&entry.parent)
        .ok_or_else(|| error_with_path(ErrorCode::Estale, "open", &entry.path))?;
    if !matches!(parent.data, NodeData::Directory { .. }) {
        return Err(error_with_path(ErrorCode::Enotdir, "open", &entry.path));
    }
    require_parent(entry.parent)?;
    if mutation.layout.chunker != namespace.default_chunker {
        return Ok(PreparedCreateRebase::ChunkerChanged);
    }

    mutation.expected_revision = current_revision;
    mutation.inode = namespace.next_inode;
    Ok(PreparedCreateRebase::Rebased)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Namespace, WholeFileMutation) {
        let chunker = FixedSizeChunker::new(4096).unwrap().config();
        let mut nodes = BTreeMap::new();
        nodes.insert(
            1,
            NodeMetadata {
                stats: base_stats(1, S_IFDIR | 0o755, 11, 12, 2, BLOCK_SIZE, 8),
                data: NodeData::Directory {
                    entries: vec![mount_rs_core::storage::DirectoryEntry {
                        name: "parent".into(),
                        inode: 2,
                    }],
                },
            },
        );
        nodes.insert(
            2,
            NodeMetadata {
                stats: base_stats(2, S_IFDIR | 0o750, 21, 22, 2, BLOCK_SIZE, 8),
                data: NodeData::Directory { entries: vec![] },
            },
        );
        let namespace = Namespace {
            format_version: NAMESPACE_FORMAT_VERSION,
            root: 1,
            next_inode: 3,
            default_uid: 21,
            default_gid: 22,
            umask: 0o027,
            default_chunker: chunker.clone(),
            nodes,
        };
        let mutation = WholeFileMutation {
            path: "/parent/created".into(),
            inode: 99,
            expected_revision: 7,
            new_inode: true,
            original: None,
            layout: FileLayout {
                chunker,
                extents: Vec::new(),
            },
            data_length: 6,
        };
        (namespace, mutation)
    }

    fn assert_unchanged(actual: &WholeFileMutation, original: &WholeFileMutation) {
        assert_eq!(actual.path, original.path);
        assert_eq!(actual.inode, original.inode);
        assert_eq!(actual.expected_revision, original.expected_revision);
        assert_eq!(actual.new_inode, original.new_inode);
        assert_eq!(actual.original, original.original);
        assert_eq!(actual.layout, original.layout);
        assert_eq!(actual.data_length, original.data_length);
    }

    #[test]
    fn two_stale_creates_rebind_to_distinct_current_inodes_and_defaults() {
        let (mut namespace, prepared) = fixture();
        namespace.next_inode = u64::MAX - 2;
        namespace.default_uid = 31;
        namespace.default_gid = 32;
        namespace.umask = 0o077;
        let mut first = prepared.clone();
        let mut second = prepared;
        second.path = "/parent/second".into();
        second.layout.extents.push(BlockExtent {
            file_offset: 0,
            block: mount_rs_core::storage::BlockId("first-payload".into()),
            block_offset: 0,
            length: 6,
        });
        assert_eq!(
            rebase_prepared_create(&namespace, u64::MAX, &mut first, |parent| {
                assert_eq!(parent, 2);
                Ok(())
            })
            .unwrap(),
            PreparedCreateRebase::Rebased
        );
        assert_eq!(first.expected_revision, u64::MAX);
        assert_eq!(first.inode, u64::MAX - 2);
        assert!(matches!(
            apply_whole_file_mutation(&mut namespace, u64::MAX, &first, true).unwrap(),
            WholeFileMutationResult::Committed
        ));
        assert_eq!(namespace.nodes[&first.inode].stats.uid, 31);
        assert_eq!(namespace.nodes[&first.inode].stats.gid, 32);
        assert_eq!(namespace.nodes[&first.inode].stats.mode & 0o777, 0o600);
        assert_eq!(
            rebase_prepared_create(&namespace, u64::MAX, &mut second, |_| Ok(())).unwrap(),
            PreparedCreateRebase::Rebased
        );
        assert_eq!(second.inode, u64::MAX - 1);
        assert_eq!(
            second.layout.extents[0].block,
            mount_rs_core::storage::BlockId("first-payload".into())
        );
        assert!(matches!(
            apply_whole_file_mutation(&mut namespace, u64::MAX, &second, true).unwrap(),
            WholeFileMutationResult::Committed
        ));
        assert_ne!(first.inode, second.inode);
        assert_ne!(
            namespace.nodes[&first.inode].data,
            namespace.nodes[&second.inode].data
        );
    }

    #[test]
    fn occupied_or_nonfresh_mutation_is_unchanged() {
        let (mut namespace, mut mutation) = fixture();
        let original = mutation.clone();
        if let NodeData::Directory { entries } = &mut namespace.nodes.get_mut(&2).unwrap().data {
            entries.push(mount_rs_core::storage::DirectoryEntry {
                name: "created".into(),
                inode: 3,
            });
        }
        namespace.nodes.insert(
            3,
            NodeMetadata {
                stats: base_stats(3, S_IFREG | 0o644, 21, 22, 1, 0, 0),
                data: NodeData::File(FileLayout {
                    chunker: namespace.default_chunker.clone(),
                    extents: Vec::new(),
                }),
            },
        );
        namespace.next_inode = 4;
        assert_eq!(
            rebase_prepared_create(&namespace, 8, &mut mutation, |_| panic!("occupied path"))
                .unwrap(),
            PreparedCreateRebase::Unchanged
        );
        assert_unchanged(&mutation, &original);
        mutation.new_inode = false;
        mutation.path = "/missing/created".into();
        let original = mutation.clone();
        assert_eq!(
            rebase_prepared_create(&namespace, 8, &mut mutation, |_| panic!("replacement"))
                .unwrap(),
            PreparedCreateRebase::Unchanged
        );
        assert_unchanged(&mutation, &original);
    }

    #[test]
    fn chunker_change_and_current_authority_refusal_preserve_preparation() {
        let (mut namespace, mut mutation) = fixture();
        let original = mutation.clone();
        let denied = FsError::new(ErrorCode::Eacces);
        assert_eq!(
            rebase_prepared_create(&namespace, 8, &mut mutation, |parent| {
                assert_eq!(parent, 2);
                Err(denied.clone())
            })
            .unwrap_err()
            .code,
            ErrorCode::Eacces
        );
        assert_unchanged(&mutation, &original);
        namespace.default_chunker = FixedSizeChunker::new(8192).unwrap().config();
        assert_eq!(
            rebase_prepared_create(&namespace, 8, &mut mutation, |_| Ok(())).unwrap(),
            PreparedCreateRebase::ChunkerChanged
        );
        assert_unchanged(&mutation, &original);
    }

    #[test]
    fn current_parent_errors_and_symlink_resolution_are_preserved() {
        let (mut namespace, mut mutation) = fixture();
        let original = mutation.clone();
        namespace.nodes.get_mut(&1).unwrap().data = NodeData::Directory { entries: vec![] };
        assert_eq!(
            rebase_prepared_create(&namespace, 8, &mut mutation, |_| Ok(()))
                .unwrap_err()
                .code,
            ErrorCode::Enoent
        );
        assert_unchanged(&mutation, &original);

        let (mut namespace, mut mutation) = fixture();
        namespace.nodes.get_mut(&2).unwrap().data = NodeData::Special;
        assert_eq!(
            rebase_prepared_create(&namespace, 8, &mut mutation, |_| Ok(()))
                .unwrap_err()
                .code,
            ErrorCode::Enotdir
        );

        let (mut namespace, mut mutation) = fixture();
        namespace.nodes.get_mut(&2).unwrap().data = NodeData::Symlink {
            target: "/parent".into(),
        };
        assert_eq!(
            rebase_prepared_create(&namespace, 8, &mut mutation, |_| Ok(()))
                .unwrap_err()
                .code,
            ErrorCode::Eloop
        );

        let (mut namespace, mut mutation) = fixture();
        namespace.nodes.insert(
            3,
            NodeMetadata {
                stats: base_stats(3, S_IFDIR | 0o755, 31, 32, 2, BLOCK_SIZE, 8),
                data: NodeData::Directory { entries: vec![] },
            },
        );
        namespace.nodes.get_mut(&1).unwrap().data = NodeData::Directory {
            entries: vec![
                mount_rs_core::storage::DirectoryEntry {
                    name: "parent".into(),
                    inode: 2,
                },
                mount_rs_core::storage::DirectoryEntry {
                    name: "current".into(),
                    inode: 3,
                },
                mount_rs_core::storage::DirectoryEntry {
                    name: "alias".into(),
                    inode: 4,
                },
            ],
        };
        namespace.nodes.insert(
            4,
            NodeMetadata {
                stats: base_stats(4, mount_rs_core::types::S_IFLNK | 0o777, 0, 0, 1, 0, 0),
                data: NodeData::Symlink {
                    target: "/current".into(),
                },
            },
        );
        namespace.next_inode = 5;
        mutation.path = "/alias/created".into();
        assert_eq!(
            rebase_prepared_create(&namespace, 8, &mut mutation, |parent| {
                assert_eq!(parent, 3);
                Ok(())
            })
            .unwrap(),
            PreparedCreateRebase::Rebased
        );
        assert_eq!(mutation.inode, 5);
    }

    #[test]
    fn overflow_remains_at_unchanged_apply_boundary() {
        let (mut namespace, mut mutation) = fixture();
        namespace.next_inode = u64::MAX;
        assert_eq!(
            rebase_prepared_create(&namespace, 8, &mut mutation, |_| Ok(())).unwrap(),
            PreparedCreateRebase::Rebased
        );
        assert_eq!(mutation.inode, u64::MAX);
        assert_eq!(
            apply_whole_file_mutation(&mut namespace, 8, &mutation, true)
                .err()
                .expect("expected inode allocation overflow")
                .code,
            ErrorCode::Eoverflow
        );
    }

    #[test]
    fn failed_apply_discards_only_the_ephemeral_candidate() {
        let (namespace, mut mutation) = fixture();
        let mut candidate = namespace.clone();
        candidate.nodes.get_mut(&2).unwrap().stats.mtime_ms = i64::MAX;
        rebase_prepared_create(&candidate, 8, &mut mutation, |_| Ok(())).unwrap();
        assert_eq!(
            apply_whole_file_mutation(&mut candidate, 8, &mutation, true)
                .err()
                .expect("expected parent timestamp overflow")
                .code,
            ErrorCode::Eoverflow
        );
        // apply may already have changed its private copy before the checked
        // timestamp fails. The accumulated namespace remains authoritative.
        assert_eq!(namespace.next_inode, 3);
        assert_eq!(namespace.nodes.len(), 2);
        assert!(
            walk(&namespace, &mutation.path, true, "open", 0)
                .unwrap()
                .node
                .is_none()
        );
    }
}

#[cfg(kani)]
mod verification {
    use super::*;

    /// Bounded concrete root graph with symbolic scalar identities and branch
    /// choices. Symbolic next_inode includes values outside a valid namespace;
    /// those paths prove the helper's binding decision only, while production
    /// receives a validated namespace. This does not cover arbitrary path
    /// graphs, asynchronous publication, or provider durability.
    #[kani::proof]
    #[kani::unwind(64)]
    fn current_fresh_create_rebinds_only_eligible_preparation() {
        let revision: u64 = kani::any();
        let current_inode: InodeId = kani::any();
        let prepared_inode: InodeId = kani::any();
        let fresh: bool = kani::any();
        let occupied: bool = kani::any();
        let chunker_matches: bool = kani::any();
        let authority_denied: bool = kani::any();

        let chunker = FixedSizeChunker::new(4096).unwrap().config();
        let mut nodes = BTreeMap::new();
        let mut entries = Vec::new();
        if occupied {
            entries.push(mount_rs_core::storage::DirectoryEntry {
                name: "created".into(),
                inode: 2,
            });
        }
        nodes.insert(
            1,
            NodeMetadata {
                stats: base_stats(1, S_IFDIR | 0o755, 0, 0, 2, BLOCK_SIZE, 8),
                data: NodeData::Directory { entries },
            },
        );
        if occupied {
            nodes.insert(
                2,
                NodeMetadata {
                    stats: base_stats(2, S_IFREG | 0o644, 0, 0, 1, 0, 0),
                    data: NodeData::File(FileLayout {
                        chunker: chunker.clone(),
                        extents: Vec::new(),
                    }),
                },
            );
        }
        let namespace = Namespace {
            format_version: NAMESPACE_FORMAT_VERSION,
            root: 1,
            next_inode: current_inode,
            default_uid: 0,
            default_gid: 0,
            umask: 0,
            default_chunker: if chunker_matches {
                chunker.clone()
            } else {
                FixedSizeChunker::new(8192).unwrap().config()
            },
            nodes,
        };
        let mut mutation = WholeFileMutation {
            path: "/created".into(),
            inode: prepared_inode,
            expected_revision: 7,
            new_inode: fresh,
            original: None,
            layout: FileLayout {
                chunker,
                extents: Vec::new(),
            },
            data_length: 3,
        };
        let result = rebase_prepared_create(&namespace, revision, &mut mutation, |parent| {
            assert_eq!(parent, 1);
            if authority_denied {
                Err(FsError::new(ErrorCode::Eacces))
            } else {
                Ok(())
            }
        });
        let eligible = fresh && !occupied && !authority_denied && chunker_matches;
        assert_eq!(
            matches!(&result, Ok(PreparedCreateRebase::Rebased)),
            eligible
        );
        if eligible {
            assert_eq!(mutation.expected_revision, revision);
            assert_eq!(mutation.inode, current_inode);
        } else {
            assert_eq!(mutation.expected_revision, 7);
            assert_eq!(mutation.inode, prepared_inode);
        }
        assert_eq!(mutation.path, "/created");
        assert_eq!(mutation.new_inode, fresh);
        assert!(mutation.original.is_none());
        assert_eq!(
            mutation.layout.chunker,
            FixedSizeChunker::new(4096).unwrap().config()
        );
        assert!(mutation.layout.extents.is_empty());
        assert_eq!(mutation.data_length, 3);

        kani::cover!(eligible && revision == u64::MAX && current_inode == u64::MAX);
        kani::cover!(fresh && occupied && matches!(&result, Ok(PreparedCreateRebase::Unchanged)));
        kani::cover!(fresh && !occupied && authority_denied && result.is_err());
        kani::cover!(
            fresh
                && !occupied
                && !authority_denied
                && !chunker_matches
                && matches!(&result, Ok(PreparedCreateRebase::ChunkerChanged))
        );
        kani::cover!(!fresh && matches!(&result, Ok(PreparedCreateRebase::Unchanged)));
    }
}
