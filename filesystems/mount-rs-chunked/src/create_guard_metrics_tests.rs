//! Opt-in serial control for the actual fresh-create mutation guard.

use super::*;

fn guard_delta(before: &profile::Snapshot, after: &profile::Snapshot, name: &str) -> (u64, u64) {
    let find = |snapshot: &profile::Snapshot| {
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap_or_else(|| panic!("missing fresh-create guard row {name}"));
        (entry.calls, entry.units)
    };
    let old = find(before);
    let new = find(after);
    (
        new.0.checked_sub(old.0).expect("monotonic calls"),
        new.1.checked_sub(old.1).expect("monotonic units"),
    )
}

#[test]
#[ignore = "run alone with MOUNT_RS_PROFILE_IO=1 and --test-threads=1"]
fn fresh_create_guard_records_overlapping_predicates() {
    assert!(profile::enabled(), "run with MOUNT_RS_PROFILE_IO=1");
    let options = ChunkedOptions::fixed("create-guard-matrix", 16).expect("fixed chunker");

    for mask in 0_u8..8 {
        let revision_mismatch = mask & 1 != 0;
        let allocation_mismatch = mask & 2 != 0;
        let path_present = mask & 4 != 0;
        let mut candidate = initial_namespace(&options).expect("fresh namespace");
        let mut mutation = WholeFileMutation {
            path: "/created".to_owned(),
            inode: candidate.next_inode,
            expected_revision: 7,
            new_inode: true,
            original: None,
            layout: FileLayout {
                chunker: candidate.default_chunker.clone(),
                extents: Vec::new(),
            },
            data_length: 0,
        };

        if path_present {
            assert!(matches!(
                apply_whole_file_mutation(&mut candidate, 7, &mutation, false)
                    .expect("seed existing path through the production create branch"),
                WholeFileMutationResult::Committed,
            ));
        }
        mutation.inode = candidate.next_inode + u64::from(allocation_mismatch);
        mutation.expected_revision = if revision_mismatch { 6 } else { 7 };
        let current_revision = 7;
        let unchanged = serde_json::to_vec(&candidate).expect("serialize candidate before guard");
        let before = profile::snapshot();
        let outcome = apply_whole_file_mutation(&mut candidate, current_revision, &mutation, false)
            .expect("fresh-create guard outcome");
        let after = profile::snapshot();

        if mask == 0 {
            assert!(matches!(outcome, WholeFileMutationResult::Committed));
            assert_eq!(
                walk(&candidate, "/created", true, "test", 0)
                    .expect("created path")
                    .node,
                Some(mutation.inode),
            );
            assert_eq!(candidate.next_inode, mutation.inode + 1);
            assert_eq!(candidate.nodes[&mutation.inode].stats.size, 0);
        } else {
            assert!(
                matches!(outcome, WholeFileMutationResult::Conflict),
                "mask {mask:03b}"
            );
            assert_eq!(
                serde_json::to_vec(&candidate).expect("serialize rejected candidate"),
                unchanged,
                "rejected fresh-create candidate changed for mask {mask:03b}",
            );
        }

        let expected_conflict = u64::from(mask != 0);
        assert_eq!(
            guard_delta(
                &before,
                &after,
                "filesystem.mutation.create_guard.evaluated"
            ),
            (1, 1),
            "mask {mask:03b}"
        );
        assert_eq!(
            guard_delta(&before, &after, "filesystem.mutation.create_guard.passed"),
            (1 - expected_conflict, 1 - expected_conflict),
            "mask {mask:03b}"
        );
        assert_eq!(
            guard_delta(&before, &after, "filesystem.mutation.create_guard.conflict"),
            (expected_conflict, expected_conflict),
            "mask {mask:03b}"
        );
        let revision_count = u64::from(revision_mismatch);
        assert_eq!(
            guard_delta(
                &before,
                &after,
                "filesystem.mutation.create_guard.revision_mismatch"
            ),
            (revision_count, revision_count),
            "mask {mask:03b}"
        );
        let allocation_count = u64::from(allocation_mismatch);
        assert_eq!(
            guard_delta(
                &before,
                &after,
                "filesystem.mutation.create_guard.allocation_mismatch"
            ),
            (allocation_count, allocation_count),
            "mask {mask:03b}"
        );
        let path_count = u64::from(path_present);
        assert_eq!(
            guard_delta(
                &before,
                &after,
                "filesystem.mutation.create_guard.path_present"
            ),
            (path_count, path_count),
            "mask {mask:03b}"
        );
    }

    let mut candidate = initial_namespace(&options).expect("fresh namespace");
    let unresolved = WholeFileMutation {
        path: "/missing/created".to_owned(),
        inode: candidate.next_inode,
        expected_revision: 7,
        new_inode: true,
        original: None,
        layout: FileLayout {
            chunker: candidate.default_chunker.clone(),
            extents: Vec::new(),
        },
        data_length: 0,
    };
    let unchanged = serde_json::to_vec(&candidate).expect("serialize unresolved candidate");
    let before = profile::snapshot();
    assert!(matches!(
        apply_whole_file_mutation(&mut candidate, 7, &unresolved, false),
        Err(error) if error.code == ErrorCode::Enoent,
    ));
    let after = profile::snapshot();
    assert_eq!(serde_json::to_vec(&candidate).unwrap(), unchanged);
    for name in [
        "filesystem.mutation.create_guard.evaluated",
        "filesystem.mutation.create_guard.passed",
        "filesystem.mutation.create_guard.conflict",
        "filesystem.mutation.create_guard.revision_mismatch",
        "filesystem.mutation.create_guard.allocation_mismatch",
        "filesystem.mutation.create_guard.path_present",
    ] {
        assert_eq!(
            guard_delta(&before, &after, name),
            (0, 0),
            "unresolved parent: {name}"
        );
    }

    let mut candidate = initial_namespace(&options).expect("fresh namespace");
    candidate.next_inode = u64::MAX;
    let overflow = WholeFileMutation {
        path: "/created".to_owned(),
        inode: candidate.next_inode,
        expected_revision: 7,
        new_inode: true,
        original: None,
        layout: FileLayout {
            chunker: candidate.default_chunker.clone(),
            extents: Vec::new(),
        },
        data_length: 0,
    };
    let unchanged = serde_json::to_vec(&candidate).expect("serialize overflow candidate");
    let before = profile::snapshot();
    assert!(matches!(
        apply_whole_file_mutation(&mut candidate, 7, &overflow, false),
        Err(error) if error.code == ErrorCode::Eoverflow,
    ));
    let after = profile::snapshot();
    assert_eq!(serde_json::to_vec(&candidate).unwrap(), unchanged);
    assert_eq!(
        guard_delta(
            &before,
            &after,
            "filesystem.mutation.create_guard.evaluated"
        ),
        (1, 1)
    );
    assert_eq!(
        guard_delta(&before, &after, "filesystem.mutation.create_guard.passed"),
        (1, 1)
    );
    assert_eq!(
        guard_delta(&before, &after, "filesystem.mutation.create_guard.conflict"),
        (0, 0)
    );
    assert_eq!(
        guard_delta(
            &before,
            &after,
            "filesystem.mutation.create_guard.revision_mismatch"
        ),
        (0, 0)
    );
    assert_eq!(
        guard_delta(
            &before,
            &after,
            "filesystem.mutation.create_guard.allocation_mismatch"
        ),
        (0, 0)
    );
    assert_eq!(
        guard_delta(
            &before,
            &after,
            "filesystem.mutation.create_guard.path_present"
        ),
        (0, 0)
    );
}
