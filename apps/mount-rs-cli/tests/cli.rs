use mount_rs_cli::color::Color;
use mount_rs_cli::parse_args;
use mount_rs_cli::parser::{CliOptions, Command, DriverChoice, TransportChoice, help_text};
use std::fs;
use std::process::Command as ProcessCommand;

#[test]
fn help_and_version_paths_are_pure() {
    assert!(matches!(
        parse_args(["mount-rs", "--help"]),
        Ok(Command::Help)
    ));
    assert!(matches!(
        parse_args(["mount-rs", "--version"]),
        Ok(Command::Version)
    ));
    assert!(help_text(Color::disabled()).contains("--transport"));
    assert!(help_text(Color::disabled()).contains("--sqlite-single-host"));
    assert!(help_text(Color::disabled()).contains("serve-http --config"));
    assert!(help_text(Color::disabled()).contains("sdk-self-test"));
}

#[test]
fn probe_is_a_subcommand_and_does_not_start_a_mount() {
    assert!(matches!(
        parse_args(["mount-rs", "probe"]),
        Ok(Command::Probe)
    ));
    assert!(matches!(
        parse_args(["mount-rs", "--probe"]),
        Ok(Command::Probe)
    ));
    assert!(matches!(
        parse_args(["mount-rs", "mount", "--help"]),
        Ok(Command::Help)
    ));
}

#[test]
fn driver_and_transport_selection_are_typed() {
    let command = parse_args([
        "mount-rs",
        "/tmp/mount-rs-cli-test",
        "--transport",
        "fuse",
        "--driver",
        "host",
        "--root",
        ".",
        "--quiet",
    ])
    .unwrap();
    assert_eq!(
        command,
        Command::Mount(CliOptions {
            mountpoint: Some("/tmp/mount-rs-cli-test".into()),
            transport: TransportChoice::Fuse,
            quiet: true,
            driver: DriverChoice::Host,
            root: Some(".".into()),
            ..CliOptions::default()
        })
    );
}

#[test]
fn invalid_arguments_are_reported_without_touching_the_filesystem() {
    let error = parse_args(["mount-rs", "--transport", "not-a-transport"]).unwrap_err();
    assert!(error.to_string().contains("unknown transport"));
    let error = parse_args(["mount-rs", "one", "two"]).unwrap_err();
    assert!(error.to_string().contains("one mountpoint"));
}

#[test]
fn sqlite_single_host_is_available_for_nfs_and_auto() {
    for args in [
        &["mount-rs", "--transport", "nfs", "--sqlite-single-host"][..],
        &["mount-rs", "--transport", "auto", "--sqlite-single-host"][..],
        &["mount-rs", "--sqlite-single-host"][..],
    ] {
        let Ok(Command::Mount(options)) = parse_args(args) else {
            panic!("expected a mount command for {args:?}");
        };
        assert!(
            options.sqlite_single_host,
            "flag was not retained for {args:?}"
        );
    }
}

#[test]
fn sqlite_single_host_rejects_non_nfs_transports() {
    for transport in ["fuse", "9p"] {
        let error =
            parse_args(["mount-rs", "--transport", transport, "--sqlite-single-host"]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("only valid with --transport nfs or auto"),
            "unexpected error for {transport}: {error}"
        );
    }
}

#[test]
fn actual_binary_validates_provider_config_without_credentials_or_network() {
    let config = format!(
        "{}/examples/config-pglite-r2.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args(["validate-config", "--config", &config])
        .env_remove("MOUNT_RS_PGLITE_URL")
        .env_remove("R2_ACCESS_KEY_ID")
        .env_remove("R2_SECRET_ACCESS_KEY")
        .output()
        .expect("run actual mount-rs config validator");
    assert!(
        output.status.success(),
        "validator failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("valid config:"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("missing environment variable"));
}

#[test]
fn actual_binary_validates_the_loopback_ozone_provider_config_without_credentials_or_network() {
    let config = format!(
        "{}/examples/config-pglite-ozone.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args(["validate-config", "--config", &config])
        .env_remove("MOUNT_RS_PGLITE_URL")
        .env_remove("R2_ACCESS_KEY_ID")
        .env_remove("R2_SECRET_ACCESS_KEY")
        .output()
        .expect("run actual mount-rs Ozone config validator");
    assert!(
        output.status.success(),
        "Ozone config validator failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("valid config:"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("missing environment variable"));
}

#[test]
fn actual_binary_uses_the_public_rust_sdk_for_mount_free_self_tests() {
    let memory = ProcessCommand::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args(["sdk-self-test"])
        .output()
        .expect("run Rust SDK memory self-test");
    assert!(
        memory.status.success(),
        "Rust SDK memory self-test failed: stdout={} stderr={}",
        String::from_utf8_lossy(&memory.stdout),
        String::from_utf8_lossy(&memory.stderr)
    );
    assert!(String::from_utf8_lossy(&memory.stdout).contains("Rust SDK wrote and read"));

    let root = std::env::temp_dir().join(format!(
        "mount-rs-cli-sdk-self-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("create Rust SDK self-test directory");
    let config_path = root.join("config.json");
    let metadata_path = root.join("metadata.sqlite");
    let blocks_path = root.join("blocks.sqlite");
    let config = serde_json::json!({
        "version": 1,
        "driver": {
            "kind": "splitstore",
            "storage": {
                "metadata": {"kind": "sqlite", "path": metadata_path},
                "blocks": {"kind": "sqlite", "path": blocks_path},
                "chunk_size_bytes": 7,
                "owner": "rust-cli-sdk-self-test"
            }
        }
    });
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("serialize Rust SDK config"),
    )
    .expect("write Rust SDK config");

    let durable = ProcessCommand::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args([
            "sdk-self-test",
            "--config",
            config_path.to_str().expect("UTF-8 config path"),
            "--reopen",
        ])
        .output()
        .expect("run Rust SDK SQLite self-test");
    let stdout = String::from_utf8_lossy(&durable.stdout);
    let stderr = String::from_utf8_lossy(&durable.stderr);
    let status = durable.status;
    let _ = fs::remove_dir_all(&root);
    assert!(
        status.success(),
        "Rust SDK SQLite self-test failed: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("Rust SDK wrote, shut down, reopened, and read"));
}

#[test]
#[ignore = "requires the explicit live Apache Ozone and PGlite composition harness"]
fn actual_binary_runs_live_ozone_split_provider_self_test() {
    let endpoint = std::env::var("R2_ENDPOINT").expect("R2_ENDPOINT must be set");
    let bucket = std::env::var("R2_BUCKET").expect("R2_BUCKET must be set");
    let pglite_url = std::env::var("PGLITE_DATABASE_URL").expect("PGLITE_DATABASE_URL must be set");
    let access_key_id = std::env::var("R2_ACCESS_KEY_ID").expect("R2_ACCESS_KEY_ID must be set");
    let secret_access_key =
        std::env::var("R2_SECRET_ACCESS_KEY").expect("R2_SECRET_ACCESS_KEY must be set");
    let run_id = std::env::var("MOUNT_RS_PROVIDER_MATRIX_RUN_ID")
        .unwrap_or_else(|_| std::process::id().to_string());
    assert!(
        run_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-_".contains(character)),
        "provider-matrix run id must be safe for an object prefix"
    );

    let prefix = format!("mount-rs-provider-matrix/{run_id}/node-cli-ozone/rust-cli");
    let root = std::env::temp_dir().join(format!(
        "mount-rs-cli-ozone-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("create live Ozone CLI config directory");
    let config_path = root.join("config.json");
    let config = serde_json::json!({
        "version": 1,
        "driver": {
            "kind": "splitstore",
            "storage": {
                "metadata": {
                    "kind": "pglite",
                    "connection": {"env": "PGLITE_DATABASE_URL"},
                    "volume_key": format!("{prefix}/metadata"),
                    "durable": true
                },
                "blocks": {
                    "kind": "r2",
                    "endpoint": endpoint,
                    "bucket": bucket,
                    "prefix": prefix,
                    "access_key_id": {"env": "R2_ACCESS_KEY_ID"},
                    "secret_access_key": {"env": "R2_SECRET_ACCESS_KEY"},
                    "durable": true
                },
                "chunk_size_bytes": 7,
                "owner": format!("rust-cli-ozone-{}", std::process::id())
            }
        }
    });
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("serialize live Ozone CLI config"),
    )
    .expect("write live Ozone CLI config");

    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args([
            "sdk-self-test",
            "--config",
            config_path
                .to_str()
                .expect("UTF-8 live Ozone CLI config path"),
            "--reopen",
        ])
        .env("PGLITE_DATABASE_URL", pglite_url)
        .env("R2_ENDPOINT", std::env::var("R2_ENDPOINT").unwrap())
        .env("R2_BUCKET", std::env::var("R2_BUCKET").unwrap())
        .env("R2_ACCESS_KEY_ID", access_key_id)
        .env("R2_SECRET_ACCESS_KEY", secret_access_key)
        .output()
        .expect("run live Ozone Rust CLI self-test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let status = output.status;
    let _ = fs::remove_dir_all(&root);
    assert!(
        status.success(),
        "live Ozone Rust CLI self-test failed: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("Rust SDK wrote, shut down, reopened, and read"));
}
