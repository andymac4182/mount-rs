use std::future::Future;
use std::task::{Context, Poll, Waker};

use mount_rs_core::{
    ErrorCode, FsDriver, GuardedMutation, GuardedMutationResult, GuardedRead, GuardedReadResult,
    GuardedSetattr, MkdirOptions, ObservedEntry, OpenFlags, PathGuard, PathIdentity,
};
use mount_rs_memfs::MemoryFs;

fn identity(fs: &mount_rs_core::Stats) -> PathIdentity {
    PathIdentity::from_stats(fs).expect("memory nodes have stable inode identities")
}

fn run<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("MemoryFs operation unexpectedly required an async runtime"),
    }
}

fn restored_with_future_time(fs: &MemoryFs, ino: u64, field: &str) -> MemoryFs {
    let mut snapshot: serde_json::Value =
        serde_json::from_slice(&fs.snapshot_bytes().unwrap()).unwrap();
    let node = snapshot["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|node| node["ino"].as_u64() == Some(ino))
        .unwrap();
    node[field] = serde_json::Value::from(mount_rs_core::types::now_ms() + 5_000);
    MemoryFs::from_snapshot(&serde_json::to_vec(&snapshot).unwrap()).unwrap()
}

async fn create(fs: &MemoryFs, path: &str, mode: u32) {
    fs.open(path, "wx", mode)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
}

#[test]
fn guarded_child_create_rejects_a_replaced_parent() {
    run(async {
        let fs = MemoryFs::empty();
        fs.mkdir("/parent", MkdirOptions::default()).await.unwrap();
        let original = identity(&fs.stat("/parent").await.unwrap());
        fs.rename("/parent", "/moved").await.unwrap();
        fs.mkdir("/parent", MkdirOptions::default()).await.unwrap();

        let result = fs
            .guarded_mutation(GuardedMutation::Open {
                parent: PathGuard {
                    path: "/parent".into(),
                    identity: original,
                },
                name: "child".into(),
                observed: ObservedEntry::Any,
                flags: OpenFlags::parse("wx", "/parent/child").unwrap(),
                mode: 0o600,
            })
            .await;
        assert_eq!(
            result.err().expect("stale parent must fail").code,
            ErrorCode::Estale
        );
        assert_eq!(
            fs.lstat("/parent/child").await.unwrap_err().code,
            ErrorCode::Enoent
        );
        assert_eq!(
            fs.lstat("/moved/child").await.unwrap_err().code,
            ErrorCode::Enoent
        );
    });
}

#[test]
fn guarded_setattr_rejects_a_replaced_file() {
    run(async {
        let fs = MemoryFs::empty();
        create(&fs, "/file", 0o600).await;
        let original = identity(&fs.lstat("/file").await.unwrap());
        fs.unlink("/file").await.unwrap();
        create(&fs, "/file", 0o644).await;

        let result = fs
            .guarded_mutation(GuardedMutation::Setattr {
                target: PathGuard {
                    path: "/file".into(),
                    identity: original,
                },
                change: GuardedSetattr {
                    mode: Some(0o777),
                    size: Some(99),
                    ..GuardedSetattr::default()
                },
            })
            .await;
        assert_eq!(
            result.err().expect("stale file must fail").code,
            ErrorCode::Estale
        );
        let replacement = fs.lstat("/file").await.unwrap();
        assert_eq!(replacement.mode & 0o777, 0o644);
        assert_eq!(replacement.size, 0);
    });
}

#[test]
fn guarded_unlink_rejects_a_replaced_child() {
    run(async {
        let fs = MemoryFs::empty();
        let root = identity(&fs.stat("/").await.unwrap());
        create(&fs, "/file", 0o600).await;
        let original = identity(&fs.lstat("/file").await.unwrap());
        fs.unlink("/file").await.unwrap();
        create(&fs, "/file", 0o644).await;

        let result = fs
            .guarded_mutation(GuardedMutation::Unlink {
                parent: PathGuard {
                    path: "/".into(),
                    identity: root,
                },
                name: "file".into(),
                entry: ObservedEntry::Identity(original),
            })
            .await;
        assert_eq!(
            result.err().expect("stale child must fail").code,
            ErrorCode::Estale
        );
        assert_eq!(fs.lstat("/file").await.unwrap().mode & 0o777, 0o644);
    });
}

#[test]
fn guarded_rename_rejects_a_replaced_destination() {
    run(async {
        let fs = MemoryFs::empty();
        let root = identity(&fs.stat("/").await.unwrap());
        create(&fs, "/source", 0o600).await;
        create(&fs, "/destination", 0o600).await;
        let source = identity(&fs.lstat("/source").await.unwrap());
        let original_destination = identity(&fs.lstat("/destination").await.unwrap());
        fs.unlink("/destination").await.unwrap();
        create(&fs, "/destination", 0o644).await;

        let result = fs
            .guarded_mutation(GuardedMutation::Rename {
                from_parent: PathGuard {
                    path: "/".into(),
                    identity: root,
                },
                from_name: "source".into(),
                source: ObservedEntry::Identity(source),
                to_parent: PathGuard {
                    path: "/".into(),
                    identity: root,
                },
                to_name: "destination".into(),
                destination: ObservedEntry::Identity(original_destination),
            })
            .await;
        assert_eq!(
            result.err().expect("stale destination must fail").code,
            ErrorCode::Estale
        );
        assert_eq!(identity(&fs.lstat("/source").await.unwrap()), source);
        assert_eq!(fs.lstat("/destination").await.unwrap().mode & 0o777, 0o644);
    });
}

#[test]
fn guarded_setattr_applies_compound_change_to_original_inode() {
    run(async {
        let fs = MemoryFs::empty();
        create(&fs, "/file", 0o600).await;
        let before = fs.lstat("/file").await.unwrap();
        let result = fs
            .guarded_mutation(GuardedMutation::Setattr {
                target: PathGuard {
                    path: "/file".into(),
                    identity: identity(&before),
                },
                change: GuardedSetattr {
                    mode: Some(0o640),
                    uid: Some(123),
                    gid: Some(456),
                    size: Some(7),
                    atime_ms: Some(111),
                    mtime_ms: Some(222),
                    expected_ctime_ms: Some(before.ctime_ms),
                },
            })
            .await
            .unwrap();
        assert!(matches!(result, GuardedMutationResult::Applied));
        let after = fs.lstat("/file").await.unwrap();
        assert_eq!(identity(&after), identity(&before));
        assert_eq!(after.mode & 0o777, 0o640);
        assert_eq!(after.uid, 123);
        assert_eq!(after.gid, 456);
        assert_eq!(after.size, 7);
        assert_eq!(after.atime_ms, 111);
        assert_eq!(after.mtime_ms, 222);
    });
}

#[test]
fn guarded_setattr_advances_change_time_after_a_clock_step_back() {
    run(async {
        let fs = MemoryFs::empty();
        create(&fs, "/file", 0o600).await;
        let ino = fs.lstat("/file").await.unwrap().ino;
        let fs = restored_with_future_time(&fs, ino, "ctime_ms");
        let before = fs.lstat("/file").await.unwrap();
        fs.guarded_mutation(GuardedMutation::Setattr {
            target: PathGuard {
                path: "/file".into(),
                identity: identity(&before),
            },
            change: GuardedSetattr {
                mode: Some(0o644),
                expected_ctime_ms: Some(before.ctime_ms),
                ..GuardedSetattr::default()
            },
        })
        .await
        .unwrap();
        let after = fs.lstat("/file").await.unwrap();
        assert!(after.ctime_ms > before.ctime_ms);
    });
}

#[test]
fn guarded_mkdir_advances_parent_mtime_after_a_clock_step_back() {
    run(async {
        let fs = MemoryFs::empty();
        let fs = restored_with_future_time(&fs, 1, "mtime_ms");
        let root = fs.stat("/").await.unwrap();
        let created = fs
            .guarded_mutation(GuardedMutation::Mkdir {
                parent: PathGuard {
                    path: "/".into(),
                    identity: identity(&root),
                },
                name: "child".into(),
                mode: 0o755,
            })
            .await
            .unwrap();
        assert!(matches!(created, GuardedMutationResult::Created(_)));
        let after = fs.stat("/").await.unwrap();
        assert!(after.mtime_ms > root.mtime_ms);
    });
}

#[test]
fn guarded_unlink_advances_parent_mtime_after_a_clock_step_back() {
    run(async {
        let fs = MemoryFs::empty();
        create(&fs, "/file", 0o600).await;
        let fs = restored_with_future_time(&fs, 1, "mtime_ms");
        let root = fs.stat("/").await.unwrap();
        let file = fs.lstat("/file").await.unwrap();
        fs.guarded_mutation(GuardedMutation::Unlink {
            parent: PathGuard {
                path: "/".into(),
                identity: identity(&root),
            },
            name: "file".into(),
            entry: ObservedEntry::Identity(identity(&file)),
        })
        .await
        .unwrap();
        assert!(fs.stat("/").await.unwrap().mtime_ms > root.mtime_ms);
    });
}

#[test]
fn guarded_mkdir_preserves_existing_target_error() {
    run(async {
        let fs = MemoryFs::empty();
        fs.mkdir("/child", MkdirOptions::default()).await.unwrap();
        let root = identity(&fs.stat("/").await.unwrap());
        let result = fs
            .guarded_mutation(GuardedMutation::Mkdir {
                parent: PathGuard {
                    path: "/".into(),
                    identity: root,
                },
                name: "child".into(),
                mode: 0o755,
            })
            .await;
        assert_eq!(
            result.err().expect("existing child must fail").code,
            ErrorCode::Eexist
        );
    });
}

#[test]
fn ordinary_rename_reports_a_missing_source_before_a_missing_destination_parent() {
    run(async {
        let fs = MemoryFs::empty();
        let error = fs
            .rename("/source", "/missing-parent/destination")
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Enoent);
        assert_eq!(error.path.as_deref(), Some("/source"));
        assert_eq!(error.dest.as_deref(), Some("/missing-parent/destination"));
    });
}

#[test]
fn memory_fs_reports_stable_inode_ids() {
    assert!(MemoryFs::empty().stable_inode_ids());
}

#[test]
fn snapshot_restore_does_not_reuse_an_unlinked_inode() {
    run(async {
        let fs = MemoryFs::empty();
        create(&fs, "/old", 0o600).await;
        let old_ino = fs.lstat("/old").await.unwrap().ino;
        fs.unlink("/old").await.unwrap();
        let restored = MemoryFs::from_snapshot(&fs.snapshot_bytes().unwrap()).unwrap();
        create(&restored, "/new", 0o600).await;
        assert!(restored.lstat("/new").await.unwrap().ino > old_ino);
    });
}

#[test]
fn guarded_open_rejects_a_final_symlink_before_truncating_its_target() {
    run(async {
        let fs = MemoryFs::empty();
        let target = fs.open("/target", "wx", 0o600).await.unwrap();
        target.write(b"keep", Some(0)).await.unwrap();
        target.close().await.unwrap();
        fs.symlink("/target", "/link").await.unwrap();
        let root = identity(&fs.stat("/").await.unwrap());
        let link = identity(&fs.lstat("/link").await.unwrap());

        let result = fs
            .guarded_mutation(GuardedMutation::Open {
                parent: PathGuard {
                    path: "/".into(),
                    identity: root,
                },
                name: "link".into(),
                observed: ObservedEntry::Identity(link),
                flags: OpenFlags::parse("w", "/link").unwrap(),
                mode: 0o600,
            })
            .await;
        assert_eq!(
            result.err().expect("final symlink must fail").code,
            ErrorCode::Eexist
        );
        assert_eq!(fs.stat("/target").await.unwrap().size, 4);
    });
}

#[test]
fn guarded_unchecked_create_rejects_a_final_symlink() {
    run(async {
        let fs = MemoryFs::empty();
        create(&fs, "/target", 0o600).await;
        fs.symlink("/target", "/link").await.unwrap();
        let result = fs
            .guarded_mutation(GuardedMutation::Open {
                parent: PathGuard {
                    path: "/".into(),
                    identity: identity(&fs.stat("/").await.unwrap()),
                },
                name: "link".into(),
                observed: ObservedEntry::Identity(identity(&fs.lstat("/link").await.unwrap())),
                flags: OpenFlags {
                    read: false,
                    write: true,
                    create: true,
                    truncate: false,
                    append: false,
                    exclusive: false,
                },
                mode: 0o600,
            })
            .await;
        assert_eq!(
            result.err().expect("symlink create must fail").code,
            ErrorCode::Eexist
        );
    });
}

#[test]
fn guarded_open_rejects_a_replaced_regular_file_before_truncating() {
    run(async {
        let fs = MemoryFs::empty();
        create(&fs, "/file", 0o600).await;
        let old = identity(&fs.lstat("/file").await.unwrap());
        fs.unlink("/file").await.unwrap();
        let replacement = fs.open("/file", "wx", 0o600).await.unwrap();
        replacement.write(b"keep", Some(0)).await.unwrap();
        replacement.close().await.unwrap();
        let root = identity(&fs.stat("/").await.unwrap());

        let result = fs
            .guarded_mutation(GuardedMutation::Open {
                parent: PathGuard {
                    path: "/".into(),
                    identity: root,
                },
                name: "file".into(),
                observed: ObservedEntry::Identity(old),
                flags: OpenFlags::parse("w", "/file").unwrap(),
                mode: 0o600,
            })
            .await;
        assert_eq!(
            result.err().expect("replaced child must fail").code,
            ErrorCode::Estale
        );
        assert_eq!(fs.stat("/file").await.unwrap().size, 4);
    });
}

#[test]
fn accepted_snapshot_at_inode_limit_fails_a_new_create_without_reuse() {
    run(async {
        let fs = MemoryFs::empty();
        let mut snapshot: serde_json::Value =
            serde_json::from_slice(&fs.snapshot_bytes().unwrap()).unwrap();
        snapshot["next_ino"] = serde_json::Value::from(u64::MAX);
        let restored = MemoryFs::from_snapshot(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        let error = restored
            .mkdir("/new", MkdirOptions::default())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Eoverflow);
        assert_eq!(
            restored.lstat("/new").await.unwrap_err().code,
            ErrorCode::Enoent
        );
    });
}

#[test]
fn guarded_child_name_with_nul_is_rejected_before_snapshot_roundtrip() {
    run(async {
        let fs = MemoryFs::empty();
        let root = identity(&fs.stat("/").await.unwrap());
        let result = fs
            .guarded_mutation(GuardedMutation::Mkdir {
                parent: PathGuard {
                    path: "/".into(),
                    identity: root,
                },
                name: "bad\0name".into(),
                mode: 0o755,
            })
            .await;
        assert_eq!(
            result.err().expect("NUL child name must fail").code,
            ErrorCode::Einval
        );
        let restored = MemoryFs::from_snapshot(&fs.snapshot_bytes().unwrap()).unwrap();
        assert!(restored.readdir("/").await.unwrap().is_empty());
    });
}

#[test]
fn ordinary_child_name_with_nul_is_rejected_before_snapshot_roundtrip() {
    run(async {
        let fs = MemoryFs::empty();
        let result = fs.mkdir("/bad\0name", MkdirOptions::default()).await;
        assert_eq!(
            result.expect_err("NUL child name must fail").code,
            ErrorCode::Einval
        );
        let restored = MemoryFs::from_snapshot(&fs.snapshot_bytes().unwrap()).unwrap();
        assert!(restored.readdir("/").await.unwrap().is_empty());
    });
}

#[test]
fn recursive_mkdir_rejects_nul_before_creating_a_prefix() {
    run(async {
        let fs = MemoryFs::empty();
        let result = fs
            .mkdir(
                "/valid/bad\0name",
                MkdirOptions {
                    recursive: true,
                    mode: None,
                },
            )
            .await;
        assert_eq!(
            result.expect_err("NUL path must fail").code,
            ErrorCode::Einval
        );
        assert_eq!(
            fs.lstat("/valid").await.unwrap_err().code,
            ErrorCode::Enoent
        );
        MemoryFs::from_snapshot(&fs.snapshot_bytes().unwrap()).unwrap();
    });
}

#[test]
fn guarded_reads_reject_replaced_file_and_directory_paths() {
    run(async {
        let fs = MemoryFs::empty();
        create(&fs, "/file", 0o600).await;
        fs.mkdir("/parent", MkdirOptions::default()).await.unwrap();
        create(&fs, "/parent/child", 0o600).await;
        let file = identity(&fs.lstat("/file").await.unwrap());
        let parent = identity(&fs.lstat("/parent").await.unwrap());
        fs.unlink("/file").await.unwrap();
        create(&fs, "/file", 0o644).await;
        fs.rename("/parent", "/moved").await.unwrap();
        fs.mkdir("/parent", MkdirOptions::default()).await.unwrap();
        create(&fs, "/parent/replacement", 0o644).await;

        for request in [
            GuardedRead::Stat {
                target: PathGuard {
                    path: "/file".into(),
                    identity: file,
                },
            },
            GuardedRead::Lookup {
                parent: PathGuard {
                    path: "/parent".into(),
                    identity: parent,
                },
                name: "replacement".into(),
            },
            GuardedRead::Readdir {
                directory: PathGuard {
                    path: "/parent".into(),
                    identity: parent,
                },
                max_entries: 10,
            },
        ] {
            assert_eq!(
                fs.guarded_read(request).await.unwrap_err().code,
                ErrorCode::Estale
            );
        }
        assert_eq!(fs.lstat("/file").await.unwrap().mode & 0o777, 0o644);
        assert_eq!(fs.readdir("/parent").await.unwrap().len(), 1);
        assert_eq!(fs.readdir("/moved").await.unwrap().len(), 1);
    });
}

#[test]
fn guarded_lookup_returns_original_dot_entries_and_nofollow_child_stats() {
    run(async {
        let fs = MemoryFs::empty();
        fs.mkdir("/parent", MkdirOptions::default()).await.unwrap();
        fs.symlink("/outside", "/parent/link").await.unwrap();
        let parent = identity(&fs.lstat("/parent").await.unwrap());
        let root = identity(&fs.lstat("/").await.unwrap());
        let link = identity(&fs.lstat("/parent/link").await.unwrap());
        let guard = PathGuard {
            path: "/parent".into(),
            identity: parent,
        };
        for (name, expected) in [(".", parent), ("..", root), ("link", link)] {
            let GuardedReadResult::Lookup {
                parent: parent_stats,
                child: stats,
            } = fs
                .guarded_read(GuardedRead::Lookup {
                    parent: guard.clone(),
                    name: name.into(),
                })
                .await
                .unwrap()
            else {
                panic!("lookup must return child attributes");
            };
            assert_eq!(identity(&parent_stats), parent);
            assert_eq!(identity(&stats), expected);
        }
        let GuardedReadResult::Lookup {
            parent: parent_stats,
            child: link_stats,
        } = fs
            .guarded_read(GuardedRead::Lookup {
                parent: guard,
                name: "link".into(),
            })
            .await
            .unwrap()
        else {
            panic!("lookup must return child attributes");
        };
        assert_eq!(identity(&parent_stats), parent);
        assert_eq!(
            link_stats.mode & mount_rs_core::types::S_IFMT,
            mount_rs_core::types::S_IFLNK
        );
    });
}

#[test]
fn guarded_readdir_returns_complete_nofollow_snapshot_or_overflow() {
    run(async {
        let fs = MemoryFs::empty();
        fs.mkdir("/parent", MkdirOptions::default()).await.unwrap();
        create(&fs, "/parent/a", 0o600).await;
        fs.symlink("/outside", "/parent/link").await.unwrap();
        create(&fs, "/parent/c", 0o644).await;
        let parent = identity(&fs.lstat("/parent").await.unwrap());
        let guard = PathGuard {
            path: "/parent".into(),
            identity: parent,
        };
        assert_eq!(
            fs.guarded_read(GuardedRead::Readdir {
                directory: guard.clone(),
                max_entries: 2,
            })
            .await
            .unwrap_err()
            .code,
            ErrorCode::Eoverflow
        );
        let GuardedReadResult::Directory { stats, entries } = fs
            .guarded_read(GuardedRead::Readdir {
                directory: guard,
                max_entries: 3,
            })
            .await
            .unwrap()
        else {
            panic!("readdir must return directory and child attributes");
        };
        assert_eq!(identity(&stats), parent);
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "link", "c"]
        );
        for entry in entries {
            let child = fs.lstat(&format!("/parent/{}", entry.name)).await.unwrap();
            assert_eq!(identity(&entry.stats), identity(&child));
        }
    });
}

#[test]
fn guarded_readlink_returns_original_stats_and_rejects_replacement() {
    run(async {
        let fs = MemoryFs::empty();
        fs.symlink("/first", "/link").await.unwrap();
        let link = identity(&fs.lstat("/link").await.unwrap());
        let guard = PathGuard {
            path: "/link".into(),
            identity: link,
        };
        let GuardedReadResult::Readlink { stats, target } = fs
            .guarded_read(GuardedRead::Readlink {
                target: guard.clone(),
            })
            .await
            .unwrap()
        else {
            panic!("readlink must return link attributes and target");
        };
        assert_eq!(identity(&stats), link);
        assert_eq!(target, "/first");
        fs.unlink("/link").await.unwrap();
        fs.symlink("/second", "/link").await.unwrap();
        assert_eq!(
            fs.guarded_read(GuardedRead::Readlink { target: guard })
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        assert_eq!(fs.readlink("/link").await.unwrap(), "/second");
    });
}
