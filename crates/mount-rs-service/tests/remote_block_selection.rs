// Role/config/fixture controls use no environment mutation, provider open, or network I/O.
// The reused command module also runs its existing owned subprocess cancellation control.
#[cfg(unix)]
#[allow(dead_code)]
#[path = "support/production_target/command.rs"]
mod command;
#[allow(dead_code)]
#[path = "support/remote_blocks.rs"]
mod remote_blocks;

use mount_rs_sdk::StoreConfig;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn configured() -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("MOUNT_RS_RUSTFS_ENDPOINT", "http://127.0.0.1:9878".into()),
        ("MOUNT_RS_RUSTFS_BUCKET", "mount-rs-test".into()),
        ("MOUNT_RS_RUSTFS_REGION", "us-east-1".into()),
        ("MOUNT_RS_RUSTFS_ACCESS_KEY_ID", "test-key".into()),
        ("MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY", "test-secret".into()),
        ("MOUNT_RS_RUSTFS_DURABLE", "1".into()),
    ])
}

fn rustfs(prefix: &str) -> StoreConfig {
    StoreConfig::RustFs {
        endpoint: "http://127.0.0.1:9878".into(),
        bucket: "mount-rs-test".into(),
        region: "us-east-1".into(),
        prefix: prefix.into(),
        access_key_id: "test-key".into(),
        secret_access_key: "test-secret".into(),
        durable: true,
    }
}

#[test]
fn absent_or_metadata_selection_preserves_existing_roles_without_rustfs_configuration() {
    for provider in ["tidb", "sqlite", "pglite", "foundationdb"] {
        for selector in [None, Some("metadata")] {
            assert!(
                remote_blocks::resolve_blocks(provider, "owned/blocks", selector, |_| panic!(
                    "unselected RustFS configuration must not be read"
                ))
                .unwrap()
                .is_none()
            );
        }
    }
}

#[test]
fn explicit_rustfs_uses_rustfs_blocks_with_a_distinct_owned_prefix() {
    let values = configured();
    let blocks =
        remote_blocks::resolve_blocks("tidb", "owned/drive-7/blocks", Some("rustfs"), |name| {
            values.get(name).cloned()
        })
        .unwrap();
    assert_eq!(blocks, Some(rustfs("owned/drive-7/blocks")));
}

#[test]
fn selected_rustfs_requires_every_explicit_setting_without_r2_fallback() {
    for &missing in configured().keys() {
        let mut values = configured();
        values.remove(missing);
        values.insert("MOUNT_RS_R2_ENDPOINT", "http://127.0.0.1:9878".into());
        let error = remote_blocks::resolve_blocks("tidb", "owned/blocks", Some("rustfs"), |name| {
            values.get(name).cloned()
        })
        .expect_err("missing RustFS setting must fail");
        assert!(
            error.contains(missing),
            "missing setting name must be reported"
        );
        assert!(!error.contains("test-secret"));
        assert!(!error.contains("http://"));
    }
}

#[test]
fn rustfs_selection_rejects_other_metadata_roles_before_configuration_lookup() {
    for provider in ["sqlite", "pglite", "foundationdb", "rustfs"] {
        assert!(
            remote_blocks::resolve_blocks(provider, "owned/blocks", Some("rustfs"), |_| panic!(
                "role validation must precede configuration lookup"
            ))
            .is_err()
        );
    }
    for selector in ["", "r2", "RustFS", "rustfs ", "tidb"] {
        assert!(
            remote_blocks::resolve_blocks("tidb", "owned/blocks", Some(selector), |_| panic!(
                "selector validation must precede configuration lookup"
            ))
            .is_err()
        );
    }
}

#[test]
fn rustfs_selection_requires_explicit_durability_and_rejects_malformed_configuration() {
    for (name, value) in [
        ("MOUNT_RS_RUSTFS_DURABLE", "0"),
        ("MOUNT_RS_RUSTFS_DURABLE", "true"),
        ("MOUNT_RS_RUSTFS_REGION", ""),
        (
            "MOUNT_RS_RUSTFS_ENDPOINT",
            "http://user:secret@127.0.0.1:9878",
        ),
    ] {
        let mut values = configured();
        values.insert(name, value.into());
        let error = remote_blocks::resolve_blocks("tidb", "owned/blocks", Some("rustfs"), |key| {
            values.get(key).cloned()
        })
        .expect_err("unsafe configuration must fail");
        assert!(!error.contains("test-secret"));
        assert!(!error.contains(value) || value.is_empty());
    }
}

#[test]
fn separate_drives_keep_rustfs_configuration_and_get_distinct_block_prefixes() {
    let parent = Some(rustfs("owned/blocks"));
    assert_eq!(
        remote_blocks::child_blocks(&parent, 3),
        Some(rustfs("owned/blocks-sandbox-3"))
    );
    assert_eq!(
        remote_blocks::child_blocks(&parent, 4),
        Some(rustfs("owned/blocks-sandbox-4"))
    );
    assert!(remote_blocks::child_blocks(&None, 3).is_none());
}

#[test]
fn rustfs_rejects_offline_tidb_block_preseed_before_preparation() {
    assert!(remote_blocks::require_online_preparation(&Some(rustfs("owned/blocks"))).is_err());
    remote_blocks::require_online_preparation(&None).unwrap();
}

fn owned_fixture() -> Value {
    #[cfg(windows)]
    let data_source = r"C:\private\tmp\owned-rustfs\data";
    #[cfg(not(windows))]
    let data_source = "/private/tmp/owned-rustfs/data";

    json!({
        "id":"a".repeat(64), "name":"/mount-rs-rustfs-test-run",
        "owner_label":"mount-rs-rustfs-test", "run":"mount-rs-rustfs-test-run",
        "purpose":null, "running":true,
        "image":"rustfs/rustfs:1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff",
        "image_id":format!("sha256:{}", "b".repeat(64)),
        "ports":{"9000/tcp":[{"HostIp":"127.0.0.1","HostPort":"9878"}]},
        "mounts":[{"Type":"bind","Source":data_source,"Destination":"/data","RW":true}]
    })
}

#[test]
fn rustfs_owned_fixture_requires_exact_creation_cid_labels_pinned_image_and_endpoint() {
    let cid = "a".repeat(64);
    let owner = "mount-rs-rustfs-test-run";
    let blocks = rustfs("owned/blocks");
    remote_blocks::validate_owned_fixture(&owned_fixture(), &cid, owner, &blocks).unwrap();
    let mut absent_purpose = owned_fixture();
    absent_purpose["purpose"] = json!("");
    remote_blocks::validate_owned_fixture(&absent_purpose, &cid, owner, &blocks).unwrap();
    for (field, value) in [
        ("id", json!("c".repeat(64))),
        ("name", json!("/foreign")),
        ("owner_label", json!("foreign")),
        ("run", json!("foreign")),
        ("purpose", json!("cleanup")),
        ("running", json!(false)),
        ("image", json!("rustfs/rustfs:latest")),
        ("image_id", json!("")),
        (
            "ports",
            json!({"9000/tcp":[{"HostIp":"0.0.0.0","HostPort":"9878"}]}),
        ),
        ("mounts", json!([])),
    ] {
        let mut observed = owned_fixture();
        observed[field] = value;
        assert!(
            remote_blocks::validate_owned_fixture(&observed, &cid, owner, &blocks).is_err(),
            "foreign or incomplete fixture must be rejected"
        );
    }
    let mut wrong_port = owned_fixture();
    wrong_port["ports"]["9000/tcp"][0]["HostPort"] = json!("9879");
    assert!(remote_blocks::validate_owned_fixture(&wrong_port, &cid, owner, &blocks).is_err());
    let mut read_only = owned_fixture();
    read_only["mounts"][0]["RW"] = json!(false);
    assert!(remote_blocks::validate_owned_fixture(&read_only, &cid, owner, &blocks).is_err());
    for endpoint in [
        "http://example.com:9878",
        "http://127.0.0.1:9878/foreign",
        "http://127.0.0.1:9878?token=secret",
    ] {
        let StoreConfig::RustFs {
            endpoint: _,
            bucket,
            region,
            prefix,
            access_key_id,
            secret_access_key,
            durable,
        } = blocks.clone()
        else {
            unreachable!()
        };
        let selected = StoreConfig::RustFs {
            endpoint: endpoint.into(),
            bucket,
            region,
            prefix,
            access_key_id,
            secret_access_key,
            durable,
        };
        assert!(
            remote_blocks::validate_owned_fixture(&owned_fixture(), &cid, owner, &selected)
                .is_err()
        );
    }
}
