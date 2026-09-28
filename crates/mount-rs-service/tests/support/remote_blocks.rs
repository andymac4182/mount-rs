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
        Some("rustfs") if metadata_provider == "tidb" => {}
        Some("rustfs") => return Err("RustFS harness blocks require TiDB metadata".into()),
        Some(_) => return Err("block provider must be metadata or rustfs".into()),
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
        child
    })
}

pub fn require_online_preparation(blocks: &Option<StoreConfig>) -> Result<(), String> {
    if blocks.is_some() {
        Err(
            "RustFS blocks require online preparation; offline TiDB block preseed is unsupported"
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
    if data.len() != 1
        || data[0]["Type"] != "bind"
        || data[0]["RW"] != true
        || data[0]["Source"]
            .as_str()
            .is_none_or(|source| !std::path::Path::new(source).is_absolute() || source == "/")
    {
        return Err("RustFS fixture requires its persistent writable data bind".into());
    }
    Ok(())
}

#[cfg(unix)]
pub async fn preflight(
    commands: &mut super::command::Commands,
    blocks: &StoreConfig,
) -> Result<Value, String> {
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
    let projection = r#"{"id":{{json .Id}},"name":{{json .Name}},"image":{{json .Config.Image}},"image_id":{{json .Image}},"running":{{json .State.Running}},"owner_label":{{json (index .Config.Labels "com.mount-rs.rustfs-test")}},"run":{{json (index .Config.Labels "com.mount-rs.rustfs-test-run")}},"purpose":{{json (index .Config.Labels "com.mount-rs.rustfs-test-purpose")}},"ports":{{json .NetworkSettings.Ports}},"mounts":{{json .Mounts}}}"#;
    let observed = commands
        .capture(
            "docker",
            &["container", "inspect", "--format", projection, &cid],
            None,
            Duration::from_secs(10),
        )
        .await?;
    let observed: Value =
        serde_json::from_str(&observed).map_err(|_| "RustFS fixture observation invalid")?;
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
        "image":PINNED_RUSTFS_IMAGE,"image_id":observed["image_id"],"endpoint_port":port,"persistent_data_bind":true,
        "scope":"owned existing loopback fixture and configured durable blocks; no process-crash or power-loss proof",
        "docker_vm_memory_bytes":info["memory_bytes"],"docker_vm_cpus":info["cpus"]}),
    )
}
