//! Fixture-only selection of immutable blocks; metadata stays independently configured.
use mount_rs_rustfs::{RustFsBlockStore, RustFsConfig};
use mount_rs_sdk::StoreConfig;
use serde_json::Value;
#[cfg(unix)]
use serde_json::json;

const PINNED_RUSTFS_IMAGE: &str =
    "rustfs/rustfs:1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff";

pub fn selector_from_environment(name: &str) -> Result<Option<String>, String> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(format!("{name} requires Unicode")),
    }
}

pub fn resolve_blocks(
    metadata_provider: &str,
    prefix: &str,
    selector: Option<&str>,
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> Result<Option<StoreConfig>, String> {
    match selector {
        None | Some("metadata") => return Ok(None),
        Some("filesystem") if metadata_provider == "tidb" => {
            return resolve_filesystem_blocks(prefix, &mut lookup).map(Some);
        }
        Some("filesystem") => return Err("filesystem harness blocks require TiDB metadata".into()),
        Some("rustfs") if metadata_provider == "tidb" => {}
        Some("rustfs") => return Err("RustFS harness blocks require TiDB metadata".into()),
        Some(_) => return Err("block provider must be metadata, rustfs or filesystem".into()),
    }
    fn required(value: Option<String>, name: &str) -> Result<String, String> {
        value
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("{name} required"))
    }
    let endpoint = required(
        lookup("MOUNT_RS_RUSTFS_ENDPOINT"),
        "MOUNT_RS_RUSTFS_ENDPOINT",
    )?;
    let bucket = required(lookup("MOUNT_RS_RUSTFS_BUCKET"), "MOUNT_RS_RUSTFS_BUCKET")?;
    let region = required(lookup("MOUNT_RS_RUSTFS_REGION"), "MOUNT_RS_RUSTFS_REGION")?;
    let access_key_id = required(
        lookup("MOUNT_RS_RUSTFS_ACCESS_KEY_ID"),
        "MOUNT_RS_RUSTFS_ACCESS_KEY_ID",
    )?;
    let secret_access_key = required(
        lookup("MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY"),
        "MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY",
    )?;
    if lookup("MOUNT_RS_RUSTFS_DURABLE").as_deref() != Some("1") {
        return Err("MOUNT_RS_RUSTFS_DURABLE=1 required for the durable harness".into());
    }
    let config = RustFsConfig {
        endpoint: endpoint.clone(),
        bucket: bucket.clone(),
        region: region.clone(),
        access_key_id: access_key_id.clone(),
        secret_access_key: secret_access_key.clone(),
    };
    config
        .validate()
        .map_err(|_| "invalid RustFS configuration (redacted)")?;
    Ok(Some(StoreConfig::RustFs {
        endpoint,
        bucket,
        region,
        prefix: prefix.into(),
        access_key_id,
        secret_access_key,
        durable: true,
    }))
}

pub fn child_blocks(blocks: &Option<StoreConfig>, index: usize) -> Option<StoreConfig> {
    blocks.as_ref().map(|blocks| {
        let mut child = blocks.clone();
        if let StoreConfig::RustFs { prefix, .. } = &mut child {
            *prefix = format!("{prefix}-sandbox-{index}");
        }
        if let StoreConfig::Filesystem { root, .. } = &mut child {
            *root = root.join(format!("sandbox-{index}"));
        }
        child
    })
}

pub fn require_online_preparation(blocks: &Option<StoreConfig>) -> Result<(), String> {
    if blocks.is_some() {
        Err(
            "external blocks require online preparation; offline TiDB block preseed is unsupported"
                .into(),
        )
    } else {
        Ok(())
    }
}

pub fn open_rustfs(blocks: &StoreConfig) -> Result<RustFsBlockStore, String> {
    let StoreConfig::RustFs {
        endpoint,
        bucket,
        region,
        prefix,
        access_key_id,
        secret_access_key,
        durable,
    } = blocks
    else {
        return Err("RustFS block configuration required".into());
    };
    RustFsBlockStore::from_config(
        &RustFsConfig {
            endpoint: endpoint.clone(),
            bucket: bucket.clone(),
            region: region.clone(),
            access_key_id: access_key_id.clone(),
            secret_access_key: secret_access_key.clone(),
        },
        prefix.clone(),
        *durable,
    )
    .map_err(|_| "RustFS block construction failed (redacted)".into())
}

/// The owner root is independent of the native evidence tree and retained after a run.
fn filesystem_root(value: &str) -> Result<std::path::PathBuf, String> {
    let path = std::path::Path::new(value);
    if !path.is_absolute()
        || value == "/"
        || value.ends_with('/')
        || value.chars().any(char::is_control)
        || value.contains('\\')
        || value[1..]
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("filesystem root must be a canonical absolute owner path".into());
    }
    Ok(path.to_path_buf())
}

fn filesystem_namespace(prefix: &str) -> Result<&std::path::Path, String> {
    if prefix.is_empty()
        || prefix.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        return Err("filesystem block namespace must be a canonical relative fixture key".into());
    }
    Ok(std::path::Path::new(prefix))
}

fn resolve_filesystem_blocks(
    prefix: &str,
    lookup: &mut impl FnMut(&str) -> Option<String>,
) -> Result<StoreConfig, String> {
    let root = lookup("MOUNT_RS_FILESYSTEM_ROOT").ok_or("MOUNT_RS_FILESYSTEM_ROOT required")?;
    let root = filesystem_root(&root)?;
    let namespace = filesystem_namespace(prefix)?;
    if lookup("MOUNT_RS_FILESYSTEM_DURABLE").as_deref() != Some("1") {
        return Err("MOUNT_RS_FILESYSTEM_DURABLE=1 required for the durable harness".into());
    }
    Ok(StoreConfig::Filesystem {
        root: root.join(namespace),
        durable: true,
    })
}

pub fn open_filesystem(
    blocks: &StoreConfig,
) -> Result<mount_rs_filesystem_blocks::FilesystemBlockStore, String> {
    let StoreConfig::Filesystem { root, durable } = blocks else {
        return Err("filesystem block configuration required".into());
    };
    mount_rs_filesystem_blocks::FilesystemBlockStore::open(root, *durable)
        .map_err(|_| "filesystem block construction failed (redacted)".into())
}

#[cfg(unix)]
#[path = "filesystem_preflight.rs"]
mod filesystem_preflight;

#[cfg(unix)]
pub fn prepare_filesystem_drive(blocks: &StoreConfig) -> Result<(), String> {
    filesystem_preflight::prepare(blocks)
}

fn fixture_port(blocks: &StoreConfig) -> Result<u16, String> {
    let StoreConfig::RustFs {
        endpoint,
        durable: true,
        ..
    } = blocks
    else {
        return Err("owned durable RustFS blocks required".into());
    };
    let endpoint =
        url::Url::parse(endpoint).map_err(|_| "RustFS fixture endpoint invalid (redacted)")?;
    if endpoint.scheme() != "http"
        || endpoint.host_str() != Some("127.0.0.1")
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.path() != "/"
    {
        return Err("RustFS fixture requires the owned loopback endpoint".into());
    }
    endpoint
        .port()
        .filter(|port| *port != 0)
        .ok_or("RustFS fixture requires an explicit port".into())
}

pub fn validate_owned_fixture(
    observed: &Value,
    cid: &str,
    owner: &str,
    blocks: &StoreConfig,
) -> Result<(), String> {
    if let Some(name) = validate_owned_fixture_mount(observed, cid, owner, blocks)? {
        let volume = &observed["data_volume"];
        let source_matches = observed["mounts"].as_array().is_some_and(|mounts| {
            mounts.iter().any(|mount| {
                mount["Destination"] == "/data" && volume["Mountpoint"] == mount["Source"]
            })
        });
        let options_empty = volume.get("Options").is_some_and(|options| {
            options.is_null()
                || options
                    .as_object()
                    .is_some_and(|options| options.is_empty())
        });
        if volume["Name"] != name
            || volume["Driver"] != "local"
            || volume["Scope"] != "local"
            || !source_matches
            || !options_empty
            || volume["Labels"]["com.mount-rs.rustfs-test"] != observed["owner_label"]
            || volume["Labels"]["com.mount-rs.rustfs-test-run"] != owner
            || volume["Labels"]["com.mount-rs.rustfs-stage-owner"] != owner
        {
            return Err("RustFS fixture named volume ownership or persistence mismatch".into());
        }
    }
    Ok(())
}

fn safe_volume_source(source: &str) -> bool {
    std::path::Path::new(source).is_absolute()
        && source.split('/').any(|part| !part.is_empty())
        && !source.split('/').any(|part| part == "." || part == "..")
        && !source.chars().any(char::is_control)
}

/// Validate the container and exact data mount before any named-volume query.
/// Bind fixtures keep their existing validation and require no volume lookup.
fn validate_owned_fixture_mount(
    observed: &Value,
    cid: &str,
    owner: &str,
    blocks: &StoreConfig,
) -> Result<Option<String>, String> {
    let port = fixture_port(blocks)?.to_string();
    let valid_cid = cid.len() == 64
        && cid
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    let valid_owner = owner.starts_with("mount-rs-rustfs-")
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    let image_id = observed["image_id"].as_str().unwrap_or_default();
    if !valid_cid
        || !valid_owner
        || observed["id"] != cid
        || observed["name"] != format!("/{owner}")
        || observed["owner_label"] != "mount-rs-rustfs-test"
        || observed["run"] != owner
        || !(observed["purpose"].is_null() || observed["purpose"].as_str() == Some(""))
        || observed["running"] != true
        || observed["image"] != PINNED_RUSTFS_IMAGE
        || image_id.len() != 71
        || !image_id.starts_with("sha256:")
        || !image_id[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("RustFS fixture ownership or image mismatch".into());
    }
    let ports = observed["ports"]["9000/tcp"]
        .as_array()
        .ok_or("RustFS fixture port unavailable")?;
    if ports.len() != 1 || ports[0]["HostIp"] != "127.0.0.1" || ports[0]["HostPort"] != port {
        return Err("RustFS fixture endpoint does not match owned container".into());
    }
    let mounts = observed["mounts"]
        .as_array()
        .ok_or("RustFS fixture persistence unavailable")?;
    let data: Vec<_> = mounts
        .iter()
        .filter(|mount| mount["Destination"] == "/data")
        .collect();
    if data.len() != 1 {
        return Err("RustFS fixture requires its persistent writable data bind".into());
    }
    let data = data[0];
    if data["Type"] == "bind" {
        if data["RW"] != true
            || data["Source"]
                .as_str()
                .is_none_or(|source| !std::path::Path::new(source).is_absolute() || source == "/")
        {
            return Err("RustFS fixture requires its persistent writable data bind".into());
        }
        return Ok(None);
    }
    let expected_name = format!("{owner}-data");
    if data["Type"] != "volume"
        || data["Name"] != expected_name
        || data["Driver"] != "local"
        || data["RW"] != true
        || data["Source"]
            .as_str()
            .is_none_or(|source| !safe_volume_source(source))
        || observed["stage_owner"] != owner
    {
        return Err("RustFS fixture requires its owned persistent writable data volume".into());
    }
    Ok(Some(expected_name))
}

#[cfg(unix)]
pub async fn preflight(
    commands: &mut super::command::Commands,
    blocks: &StoreConfig,
) -> Result<Value, String> {
    if matches!(blocks, StoreConfig::Filesystem { .. }) {
        return filesystem_preflight::preflight(blocks);
    }
    use std::time::Duration;
    fn required(name: &str) -> Result<String, String> {
        std::env::var(name)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{name} required"))
    }
    fn local_socket(endpoint: &str) -> Result<(), String> {
        if !endpoint.starts_with("unix:///") || endpoint.contains(['\n', '\r']) {
            return Err("RustFS preflight requires the existing local Docker Unix socket".into());
        }
        Ok(())
    }
    let cid = required("MOUNT_RS_REMOTE_RUSTFS_CID")?;
    let owner = required("MOUNT_RS_BACKING_RUSTFS_OWNER")?;
    // Reject malformed identities before passing them to Docker; never discover by name.
    if cid.len() != 64
        || !cid
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || !owner.starts_with("mount-rs-rustfs-")
        || !owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("RustFS preflight requires an owner-issued creation CID and owner".into());
    }
    let port = fixture_port(blocks)?;
    if let Ok(endpoint) = std::env::var("DOCKER_HOST") {
        local_socket(&endpoint)?;
    }
    let context = commands
        .capture(
            "docker",
            &["context", "show"],
            None,
            Duration::from_secs(10),
        )
        .await?;
    let endpoint = commands
        .capture(
            "docker",
            &[
                "context",
                "inspect",
                context.trim(),
                "--format",
                "{{.Endpoints.docker.Host}}",
            ],
            None,
            Duration::from_secs(10),
        )
        .await?;
    local_socket(endpoint.trim())?;
    let info = commands
        .capture(
            "docker",
            &[
                "info",
                "--format",
                r#"{"memory_bytes":{{.MemTotal}},"cpus":{{.NCPU}}}"#,
            ],
            None,
            Duration::from_secs(10),
        )
        .await?;
    let info: Value =
        serde_json::from_str(&info).map_err(|_| "RustFS Docker capacity observation invalid")?;
    if info["memory_bytes"].as_u64().unwrap_or(0) < 10 * 1024 * 1024 * 1024
        || info["cpus"].as_u64().unwrap_or(0) < 4
    {
        return Err("durable TiDB/RustFS requires measured10GiB Docker VM memory and4 CPUs".into());
    }
    let projection = r#"{"id":{{json .Id}},"name":{{json .Name}},"image":{{json .Config.Image}},"image_id":{{json .Image}},"running":{{json .State.Running}},"owner_label":{{json (index .Config.Labels "com.mount-rs.rustfs-test")}},"run":{{json (index .Config.Labels "com.mount-rs.rustfs-test-run")}},"stage_owner":{{json (index .Config.Labels "com.mount-rs.rustfs-stage-owner")}},"purpose":{{json (index .Config.Labels "com.mount-rs.rustfs-test-purpose")}},"ports":{{json .NetworkSettings.Ports}},"mounts":{{json .Mounts}}}"#;
    let observed = commands
        .capture(
            "docker",
            &["container", "inspect", "--format", projection, &cid],
            None,
            Duration::from_secs(10),
        )
        .await?;
    let mut observed: Value =
        serde_json::from_str(&observed).map_err(|_| "RustFS fixture observation invalid")?;
    let volume_name = validate_owned_fixture_mount(&observed, &cid, &owner, blocks)?;
    if let Some(name) = &volume_name {
        let projection = r#"{"Name":{{json .Name}},"Driver":{{json .Driver}},"Scope":{{json .Scope}},"Mountpoint":{{json .Mountpoint}},"Options":{{json .Options}},"Labels":{"com.mount-rs.rustfs-test":{{json (index .Labels "com.mount-rs.rustfs-test")}},"com.mount-rs.rustfs-test-run":{{json (index .Labels "com.mount-rs.rustfs-test-run")}},"com.mount-rs.rustfs-stage-owner":{{json (index .Labels "com.mount-rs.rustfs-stage-owner")}}}}"#;
        let volume = commands
            .capture(
                "docker",
                &["volume", "inspect", "--format", projection, name],
                None,
                Duration::from_secs(10),
            )
            .await?;
        observed["data_volume"] =
            serde_json::from_str(&volume).map_err(|_| "RustFS named volume observation invalid")?;
    }
    validate_owned_fixture(&observed, &cid, &owner, blocks)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| "RustFS health client failed")?;
    let response = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
        .map_err(|_| "owned RustFS health unavailable")?;
    if !response.status().is_success() {
        return Err("owned RustFS health failed".into());
    }
    Ok(
        json!({"provider":"rustfs","complete":true,"qualified":true,"creation_cid":cid,"owner":owner,
        "image":PINNED_RUSTFS_IMAGE,"image_id":observed["image_id"],"endpoint_port":port,
        "persistent_data_bind":volume_name.is_none(),"persistent_data_mount":if volume_name.is_some(){"volume"}else{"bind"},
        "persistent_data_volume":volume_name.as_ref().map(|_|json!({"owned_name_matches":true,"driver":"local","scope":"local",
            "writable_data_mount":true,"mountpoint_matches_source":true,"ownership_labels_match":true,"options_empty":true,
            "identity_redacted":true})),
        "scope":"owned existing loopback fixture and configured durable blocks; no process-crash or power-loss proof",
        "docker_vm_memory_bytes":info["memory_bytes"],"docker_vm_cpus":info["cpus"]}),
    )
}

#[cfg(all(test, unix))]
mod filesystem_selection_tests {
    use super::*;

    const ROOT: &str = "/private/tmp/mount-rs-filesystem-0123456789abcdef01234567";

    fn selected(
        root: Option<&str>,
        durable: Option<&str>,
        provider: &str,
        prefix: &str,
    ) -> Result<Option<StoreConfig>, String> {
        resolve_blocks(provider, prefix, Some("filesystem"), |name| match name {
            "MOUNT_RS_FILESYSTEM_ROOT" => root.map(str::to_owned),
            "MOUNT_RS_FILESYSTEM_DURABLE" => durable.map(str::to_owned),
            _ => panic!("filesystem selection must not read RustFS or SQL settings"),
        })
    }

    #[test]
    fn filesystem_selector_requires_durable_tidb_and_exact_per_drive_root() {
        let store = selected(Some(ROOT), Some("1"), "tidb", "target/drive-4/blocks")
            .unwrap()
            .unwrap();
        assert!(
            matches!(store, StoreConfig::Filesystem { root, durable: true }
            if root == std::path::Path::new(ROOT).join("target/drive-4/blocks"))
        );
        for durable in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some(" 1"),
            Some("1\n"),
        ] {
            assert!(selected(Some(ROOT), durable, "tidb", "target/drive-4/blocks").is_err());
        }
        assert!(selected(None, Some("1"), "tidb", "target/drive-4/blocks").is_err());
        for provider in ["sqlite", "pglite", "foundationdb", ""] {
            assert!(selected(Some(ROOT), Some("1"), provider, "target/drive-4/blocks").is_err());
        }
        for selector in [None, Some("metadata")] {
            assert!(
                resolve_blocks("tidb", "ignored", selector, |_| panic!(
                    "metadata needs no block environment"
                ))
                .unwrap()
                .is_none()
            );
        }
    }

    #[test]
    fn filesystem_selector_rejects_ambiguous_absolute_root_and_namespace() {
        for root in [
            "",
            "/",
            "/tmp/",
            "relative",
            "/private//tmp/owned",
            "/private/tmp/./owned",
            "/private/tmp/../owned",
            "/private/tmp/owned\n",
            "/private/tmp/owned\0",
            "/private/tmp/owned\\other",
        ] {
            assert!(selected(Some(root), Some("1"), "tidb", "target/drive-4/blocks").is_err());
        }
        for prefix in [
            "",
            "/absolute",
            "../foreign",
            "target/../foreign",
            "target//blocks",
            "target/./blocks",
            "target/blocks/",
            "target\\blocks",
            "target/blocks\n",
        ] {
            assert!(selected(Some(ROOT), Some("1"), "tidb", prefix).is_err());
        }
    }

    #[test]
    fn filesystem_selector_child_roots_remain_separate_and_require_online_population() {
        let selected = selected(Some(ROOT), Some("1"), "tidb", "target/blocks").unwrap();
        let child = child_blocks(&selected, 7).unwrap();
        assert!(
            matches!(child, StoreConfig::Filesystem { root, durable: true }
            if root == std::path::Path::new(ROOT).join("target/blocks/sandbox-7"))
        );
        assert!(require_online_preparation(&selected).is_err());
        require_online_preparation(&None).unwrap();
    }
}

#[cfg(all(test, unix))]
mod owned_named_volume_tests {
    use super::{PINNED_RUSTFS_IMAGE, validate_owned_fixture};
    use mount_rs_sdk::StoreConfig;
    use serde_json::{Value, json};

    const OWNER: &str = "mount-rs-rustfs-stage-unit";
    const DATA_SOURCE: &str = "/var/lib/docker/volumes/mount-rs-rustfs-stage-unit-data/_data";

    fn blocks() -> StoreConfig {
        StoreConfig::RustFs {
            endpoint: "http://127.0.0.1:9878".into(),
            bucket: "owned-fixture-test".into(),
            region: "us-east-1".into(),
            prefix: "owned/blocks".into(),
            access_key_id: "synthetic-key".into(),
            secret_access_key: "synthetic-secret".into(),
            durable: true,
        }
    }

    fn observed_named_volume() -> Value {
        json!({
            "id":"a".repeat(64), "name":format!("/{OWNER}"),
            "owner_label":"mount-rs-rustfs-test", "run":OWNER, "stage_owner":OWNER,
            "purpose":null, "running":true, "image":PINNED_RUSTFS_IMAGE,
            "image_id":format!("sha256:{}", "b".repeat(64)),
            "ports":{"9000/tcp":[{"HostIp":"127.0.0.1","HostPort":"9878"}]},
            "mounts":[{
                "Type":"volume", "Name":format!("{OWNER}-data"), "Driver":"local",
                "Source":DATA_SOURCE, "Destination":"/data", "RW":true
            }],
            "data_volume":{
                "Name":format!("{OWNER}-data"), "Driver":"local", "Scope":"local",
                "Mountpoint":DATA_SOURCE, "Options":null,
                "Labels":{
                    "com.mount-rs.rustfs-test":"mount-rs-rustfs-test",
                    "com.mount-rs.rustfs-test-run":OWNER,
                    "com.mount-rs.rustfs-stage-owner":OWNER
                }
            }
        })
    }

    #[test]
    fn owned_named_volume_accepts_exact_persistent_local_data_mount() {
        let blocks = blocks();
        let cid = "a".repeat(64);
        for source in [DATA_SOURCE, "/srv/docker-data/owned-volume/_data"] {
            for options in [Value::Null, json!({})] {
                let mut observed = observed_named_volume();
                observed["mounts"][0]["Source"] = json!(source);
                observed["data_volume"]["Mountpoint"] = json!(source);
                observed["data_volume"]["Options"] = options;
                validate_owned_fixture(&observed, &cid, OWNER, &blocks)
                    .expect("the exact owned persistent local named volume must be accepted");
            }
        }
    }

    #[test]
    fn owned_named_volume_rejects_unproven_mount_and_volume_identity() {
        let blocks = blocks();
        let cid = "a".repeat(64);
        for (pointer, changed) in [
            ("/mounts/0/Type", json!("tmpfs")),
            ("/mounts/0/Name", json!("")),
            ("/mounts/0/Name", json!("anonymous-volume")),
            ("/mounts/0/Name", json!("mount-rs-rustfs-foreign-data")),
            ("/mounts/0/Driver", json!("foreign-driver")),
            ("/mounts/0/Destination", json!("/foreign")),
            ("/mounts/0/RW", json!(false)),
            ("/mounts/0/Source", json!("/different-volume/_data")),
            ("/data_volume", Value::Null),
            ("/data_volume/Name", json!("mount-rs-rustfs-foreign-data")),
            ("/data_volume/Driver", json!("foreign-driver")),
            ("/data_volume/Scope", json!("global")),
            ("/data_volume/Mountpoint", json!("/different-volume/_data")),
            ("/data_volume/Labels", Value::Null),
            (
                "/data_volume/Labels/com.mount-rs.rustfs-test",
                json!("foreign-test"),
            ),
            (
                "/data_volume/Labels/com.mount-rs.rustfs-test-run",
                json!("mount-rs-rustfs-foreign"),
            ),
            (
                "/data_volume/Labels/com.mount-rs.rustfs-stage-owner",
                json!("mount-rs-rustfs-foreign"),
            ),
            (
                "/data_volume/Options",
                json!({"type":"tmpfs","device":"tmpfs"}),
            ),
            ("/data_volume/Options", json!([])),
            ("/data_volume/Options", json!("")),
            ("/stage_owner", json!("mount-rs-rustfs-foreign")),
        ] {
            let mut observed = observed_named_volume();
            *observed.pointer_mut(pointer).unwrap() = changed;
            assert!(
                validate_owned_fixture(&observed, &cid, OWNER, &blocks).is_err(),
                "unproven named volume field {pointer} must be rejected",
            );
        }
        let mut duplicate = observed_named_volume();
        let repeated = duplicate["mounts"][0].clone();
        duplicate["mounts"].as_array_mut().unwrap().push(repeated);
        assert!(validate_owned_fixture(&duplicate, &cid, OWNER, &blocks).is_err());
        let mut missing_options = observed_named_volume();
        missing_options["data_volume"]
            .as_object_mut()
            .unwrap()
            .remove("Options");
        assert!(validate_owned_fixture(&missing_options, &cid, OWNER, &blocks).is_err());
        for source in [
            "",
            "/",
            "relative-volume/_data",
            "/var/lib/docker/volumes/../foreign/_data",
            "/var/lib/docker/volumes/./foreign/_data",
            "/var/lib/docker/volumes/owned/\n_data",
            "/var/lib/docker/volumes/owned/\r_data",
            "/var/lib/docker/volumes/owned/\0_data",
        ] {
            let mut observed = observed_named_volume();
            observed["mounts"][0]["Source"] = json!(source);
            observed["data_volume"]["Mountpoint"] = json!(source);
            assert!(
                validate_owned_fixture(&observed, &cid, OWNER, &blocks).is_err(),
                "matching paths must also be safe absolute persistent volume paths",
            );
        }
    }

    #[test]
    fn owned_named_volume_preserves_container_and_loopback_guards() {
        let blocks = blocks();
        let cid = "a".repeat(64);
        for (pointer, changed) in [
            ("/id", json!("c".repeat(64))),
            ("/name", json!("/mount-rs-rustfs-foreign")),
            ("/owner_label", json!("foreign-test")),
            ("/run", json!("mount-rs-rustfs-foreign")),
            ("/purpose", json!("cleanup")),
            ("/running", json!(false)),
            ("/image", json!("rustfs/rustfs:latest")),
            ("/image_id", json!("")),
            ("/ports/9000~1tcp/0/HostIp", json!("0.0.0.0")),
            ("/ports/9000~1tcp/0/HostPort", json!("9879")),
        ] {
            let mut observed = observed_named_volume();
            *observed.pointer_mut(pointer).unwrap() = changed;
            assert!(
                validate_owned_fixture(&observed, &cid, OWNER, &blocks).is_err(),
                "named volume support must retain existing guard {pointer}",
            );
        }
    }
}
