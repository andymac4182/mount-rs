use mount_rs_core::ErrorCode;
use mount_rs_core::storage::{MetadataStore, Namespace, records::*};
use mount_rs_memory::{ManualClock, MemoryMetadataStore};
use std::{sync::Arc, time::Duration};

fn namespace() -> Namespace {
    serde_json::from_value(serde_json::json!({
        "format_version": 1, "root": 1, "next_inode": 2,
        "default_uid": 0, "default_gid": 0, "umask": 18,
        "default_chunker": {"algorithm":"fixed-size","version":1,"parameters":{"chunk_size":4096}},
        "nodes": {"1": {
            "stats": {"dev":1,"ino":1,"mode":16877,"nlink":2,"uid":0,"gid":0,"rdev":0,
                "size":0,"blksize":4096,"blocks":0,"atime_ms":0,"mtime_ms":0,"ctime_ms":0,"birthtime_ms":0},
            "data": {"Directory":{"entries":[]}}
        }}
    })).unwrap()
}

#[tokio::test]
async fn compatibility_publication_preserves_revision_and_fencing() {
    let clock = ManualClock::new(1000);
    let store = MemoryMetadataStore::with_clock(Arc::new(clock.clone()));
    let lease = store
        .acquire_writer("writer", Duration::from_secs(10))
        .await
        .unwrap();
    let source = namespace();
    // Initialize through the same batch interface, then change one attribute row.
    let mut changes = Vec::new();
    visit_records(&source, |record| {
        changes.push(RecordChange::Put(record));
        Ok(())
    })
    .unwrap();
    let revision = store
        .publish_batch(0, &lease, MetadataBatch { changes })
        .await
        .unwrap();
    let mut value = InodeRecord::from_node(&source.nodes[&1]);
    value.stats.uid = 42;
    let batch = MetadataBatch {
        changes: vec![RecordChange::Put(MetadataRecord::Inode { inode: 1, value })],
    };
    let next = store
        .publish_batch(revision, &lease, batch.clone())
        .await
        .unwrap();
    assert_eq!(next, revision + 1);
    assert_eq!(
        store
            .publish_batch(revision, &lease, batch)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Eagain
    );
    let committed = store.load().await.unwrap();
    assert_eq!(
        committed.namespace.as_ref().unwrap().nodes[&1].stats.uid,
        42
    );
    clock.advance_ms(10001);
    assert_eq!(
        store
            .publish_batch(next, &lease, MetadataBatch::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Estale
    );
    let after = store.load().await.unwrap();
    assert_eq!(after.revision, next);
    assert_eq!(
        serde_json::to_value(after.namespace).unwrap(),
        serde_json::to_value(committed.namespace).unwrap()
    );
}

#[test]
fn file_batch_keeps_unchanged_extents_and_deletes_removed_offsets() {
    use mount_rs_core::storage::{BlockExtent, BlockId, FileLayout};
    let base = namespace();
    let extent = |offset, id: &str| BlockExtent {
        file_offset: offset,
        block_offset: 0,
        length: 4,
        block: BlockId(id.into()),
    };
    let layout = |extents: Vec<BlockExtent>| FileLayout {
        chunker: base.default_chunker.clone(),
        extents,
    };
    let old = layout(vec![
        extent(0, "first"),
        extent(8, "keep"),
        extent(12, "old"),
        extent(20, "tail"),
    ]);
    let new = layout(vec![
        extent(4, "insert"),
        extent(8, "keep"),
        extent(12, "changed"),
    ]);
    let mut stats = base.nodes[&1].stats.clone();
    stats.ino = 2;
    stats.size = 16;
    let batch = MetadataBatch::file_update(2, &stats, &old.extents, &new);
    batch.validate_keys().unwrap();
    assert_eq!(batch.changes.len(), 5);
    assert!(
        matches!(&batch.changes[0], RecordChange::Put(MetadataRecord::Inode { inode: 2, value }) if value.stats.size == 16)
    );
    assert!(matches!(
        &batch.changes[1],
        RecordChange::Delete(RecordKey::Extent {
            inode: 2,
            file_offset: 0
        })
    ));
    assert!(
        matches!(&batch.changes[2], RecordChange::Put(MetadataRecord::Extent { inode: 2, value }) if value == &new.extents[0])
    );
    assert!(
        matches!(&batch.changes[3], RecordChange::Put(MetadataRecord::Extent { inode: 2, value }) if value == &new.extents[2])
    );
    assert!(matches!(
        &batch.changes[4],
        RecordChange::Delete(RecordKey::Extent {
            inode: 2,
            file_offset: 20
        })
    ));
}

#[test]
fn open_create_batch_reconstructs_directory_order_and_header() {
    use mount_rs_core::storage::{DirectoryEntry, FileLayout, NodeData, NodeMetadata};
    let mut current = namespace();
    for name in ["kept", "new"] {
        let previous = current.clone();
        let inode = current.next_inode;
        current.next_inode += 1;
        let mut stats = current.nodes[&1].stats.clone();
        stats.ino = inode;
        stats.mode = 0o100640;
        stats.nlink = 1;
        current.nodes.insert(
            inode,
            NodeMetadata {
                stats,
                data: NodeData::File(FileLayout {
                    chunker: current.default_chunker.clone(),
                    extents: Default::default(),
                }),
            },
        );
        let parent = current.nodes.get_mut(&1).unwrap();
        parent.stats.mtime_ms += 1;
        let NodeData::Directory { entries } = &mut parent.data else {
            unreachable!()
        };
        entries.push(DirectoryEntry {
            name: name.into(),
            inode,
        });
        let batch = MetadataBatch::created_file(&current, 1, inode).unwrap();
        assert_eq!(batch.changes.len(), 4);
        let rebuilt = batch.apply_to(Some(&previous)).unwrap();
        assert_eq!(
            serde_json::to_vec(&rebuilt).unwrap(),
            serde_json::to_vec(&current).unwrap()
        );
    }
}
