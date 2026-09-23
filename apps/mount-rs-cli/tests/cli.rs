use mount_rs_cli::color::Color;
use mount_rs_cli::parse_args;
use mount_rs_cli::parser::{CliOptions, Command, DriverChoice, TransportChoice, help_text};
#[cfg(unix)]
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::fs;
use std::process::Command as ProcessCommand;

#[test]
fn non_sqlite_offline_migration_does_not_prepare_a_view() {
    let scope = tempfile::TempDir::new().unwrap();
    let view = scope.path().join("view");
    let config_path = scope.path().join("shared.json");
    let config = serde_json::json!({
        "version": 1,
        "mountpoint": view,
        "driver": {"kind": "splitstore", "storage": {
            "concurrent_writes": true,
            "metadata": {
                "kind": "pglite",
                "connection": {"env": "MOUNT_RS_OFFLINE_PATH_FIXTURE_PGLITE_URL"},
                "volume_key": "offline-path-fixture",
                "durable": true
            },
            "blocks": {
                "kind": "pglite",
                "connection": {"env": "MOUNT_RS_OFFLINE_PATH_FIXTURE_PGLITE_URL"},
                "volume_key": "offline-path-fixture",
                "durable": true
            }
        }}
    });
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args(["migrate-concurrent-backing", "--config"])
        .arg(&config_path)
        .args(["--expected-revision", "0"])
        .env(
            "MOUNT_RS_OFFLINE_PATH_FIXTURE_PGLITE_URL",
            "postgres://offline-fixture@127.0.0.1:1/postgres?sslmode=disable",
        )
        .output()
        .unwrap();
    assert!(!output.status.success(), "fixture has no PGlite service");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("error connecting to server"),
        "migration must reach provider open without mount inspection: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!view.exists(), "non-SQLite migration prepared a view");
}

#[cfg(unix)]
#[test]
fn migrate_concurrent_backing_cli() {
    let scope = tempfile::TempDir::new().expect("create owned migration test root");
    let metadata = scope.path().join("metadata.sqlite");
    let blocks = scope.path().join("blocks.sqlite");
    let config_path = scope.path().join("shared.json");
    let mountpoint = scope.path().join("view");

    // Let the provider exclusively create and stamp each owned file before
    // seeding the disposable historical MRC1 mode with raw SQLite.
    drop(SqliteMetadataStore::open(&metadata).expect("create stamped metadata database"));
    drop(SqliteBlockStore::open(&blocks).expect("create block database"));
    let connection = rusqlite::Connection::open(&metadata).expect("open metadata database");
    assert_eq!(
        connection
            .execute(
                "UPDATE mount_rs_metadata SET write_mode='MRC1', fence=9223372036854775807
             WHERE id=1 AND revision=0 AND write_mode IS NULL AND backing_id IS NULL",
                [],
            )
            .expect("create a real offline MRC1 metadata row"),
        1
    );
    drop(connection);

    let config = serde_json::json!({
        "version": 1,
        "mountpoint": mountpoint,
        "driver": {
            "kind": "splitstore",
            "storage": {
                "concurrent_writes": true,
                "metadata": {"kind": "sqlite", "path": metadata},
                "blocks": {"kind": "sqlite", "path": blocks}
            }
        }
    });
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("serialize migration config"),
    )
    .expect("write owned migration config");

    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_mount-rs"))
        .arg("migrate-concurrent-backing")
        .arg("--config")
        .arg(&config_path)
        .arg("--expected-revision")
        .arg("0")
        .output()
        .expect("run offline migration command");
    assert!(
        output.status.success(),
        "offline migration failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let connection = rusqlite::Connection::open(&metadata).expect("reopen migrated metadata");
    let (mode, backing_id, revision, namespace): (String, String, i64, Option<String>) = connection
        .query_row(
            "SELECT write_mode, backing_id, revision, namespace FROM mount_rs_metadata WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read migrated metadata row");
    assert_eq!(mode, "MRC2");
    assert_eq!(
        revision, 0,
        "offline migration preserves the exact revision"
    );
    assert!(
        namespace.is_none(),
        "offline migration preserves the namespace"
    );
    assert_eq!(backing_id.len(), 32, "authority ID is a 128-bit hex string");
    assert!(
        backing_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "authority ID uses canonical lowercase hex"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("revision 0"), "stdout={stdout}");
    assert!(stdout.contains(&backing_id), "stdout={stdout}");
    assert!(
        mountpoint.is_dir(),
        "offline migration prepares the view for physical path checks"
    );
    assert!(
        fs::read_dir(&mountpoint).unwrap().next().is_none(),
        "offline migration leaves an empty prepared view"
    );
}

#[cfg(unix)]
#[test]
fn offline_migration_rejects_stamped_sqlite_metadata_under_mountpoint_before_claim() {
    let scope = tempfile::TempDir::new().unwrap();
    let view = scope.path().join("view");
    fs::create_dir(&view).unwrap();
    let metadata = view.join("metadata.sqlite");
    let blocks = scope.path().join("blocks.sqlite");
    let config_path = scope.path().join("shared.json");
    drop(SqliteMetadataStore::open(&metadata).unwrap());
    assert_eq!(
        rusqlite::Connection::open(&metadata)
            .unwrap()
            .execute(
                "UPDATE mount_rs_metadata SET write_mode='MRC1', fence=9223372036854775807
                 WHERE id=1",
                [],
            )
            .unwrap(),
        1
    );
    let config = serde_json::json!({
        "version": 1,
        "mountpoint": view,
        "driver": {"kind": "splitstore", "storage": {
            "concurrent_writes": true,
            "metadata": {"kind": "sqlite", "path": metadata},
            "blocks": {"kind": "sqlite", "path": blocks}
        }}
    });
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_mount-rs"))
        .arg("migrate-concurrent-backing")
        .arg("--config")
        .arg(&config_path)
        .arg("--expected-revision")
        .arg("0")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "offline migration enrolled backing inside mountpoint: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("outside every mountpoint"));
    let (mode, backing): (String, Option<String>) = rusqlite::Connection::open(&metadata)
        .unwrap()
        .query_row(
            "SELECT write_mode, backing_id FROM mount_rs_metadata WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((mode.as_str(), backing), ("MRC1", None));
    assert!(
        !blocks.exists(),
        "rejected command opened the block backing"
    );
}

#[cfg(target_os = "macos")]
fn assert_offline_migration_rejects_uncreated_sqlite_block_alias(
    view_name: &str,
    alias_name: &str,
) {
    let scope = tempfile::TempDir::new().expect("own absent alias migration fixture");
    let view = scope.path().join(view_name);
    let alias = scope.path().join(alias_name);
    fs::create_dir(&view).expect("create alias sensitivity probe");
    assert!(
        alias.is_dir(),
        "this regression needs equivalent macOS directory spellings"
    );
    fs::remove_dir(&view).expect("remove alias sensitivity probe");
    let metadata = scope.path().join("metadata.sqlite");
    let blocks = alias.join("blocks.sqlite");
    let config_path = scope.path().join("shared.json");
    drop(SqliteMetadataStore::open(&metadata).expect("create stamped metadata database"));
    let connection = rusqlite::Connection::open(&metadata).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE mount_rs_metadata SET write_mode='MRC1', fence=9223372036854775807
                 WHERE id=1",
                [],
            )
            .unwrap(),
        1
    );
    let before: (String, Option<String>, i64, Option<String>) = connection
        .query_row(
            "SELECT write_mode, backing_id, revision, namespace FROM mount_rs_metadata WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    drop(connection);
    let config = serde_json::json!({
        "version": 1,
        "mountpoint": view,
        "driver": {"kind": "splitstore", "storage": {
            "concurrent_writes": true,
            "metadata": {"kind": "sqlite", "path": metadata},
            "blocks": {"kind": "sqlite", "path": blocks}
        }}
    });
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(!view.exists(), "view must start absent");
    assert!(!alias.exists(), "backing directory must start absent");
    assert!(!blocks.exists(), "block database must start absent");
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_mount-rs"))
        .arg("migrate-concurrent-backing")
        .arg("--config")
        .arg(&config_path)
        .arg("--expected-revision")
        .arg("0")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "offline migration enrolled absent alias backing: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("outside every mountpoint"));
    let after: (String, Option<String>, i64, Option<String>) = rusqlite::Connection::open(
        &metadata,
    )
    .unwrap()
    .query_row(
        "SELECT write_mode, backing_id, revision, namespace FROM mount_rs_metadata WHERE id=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
    .unwrap();
    assert_eq!(
        after, before,
        "rejected migration must preserve mode, authority, revision and namespace"
    );
    assert!(!blocks.exists(), "rejected migration opened block backing");
}

#[cfg(target_os = "macos")]
#[test]
fn offline_migration_rejects_uncreated_case_alias_before_claim() {
    assert_offline_migration_rejects_uncreated_sqlite_block_alias("mnt", "MNT");
}

#[cfg(target_os = "macos")]
#[test]
fn offline_migration_rejects_uncreated_normalization_alias_before_claim() {
    assert_offline_migration_rejects_uncreated_sqlite_block_alias("caf\u{e9}", "cafe\u{301}");
}

#[cfg(unix)]
#[test]
fn migrate_concurrent_backing_cli_rejects_historical_unstamped_sqlite_metadata() {
    let scope = tempfile::TempDir::new().expect("own historical SQLite migration fixture");
    let metadata = scope.path().join("historical-metadata.sqlite");
    let blocks = scope.path().join("blocks.sqlite");
    let config_path = scope.path().join("shared.json");
    let mountpoint = scope.path().join("view");
    let connection = rusqlite::Connection::open(&metadata).expect("create old metadata file");
    connection
        .execute_batch(
            "CREATE TABLE mount_rs_metadata (
                id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL,
                namespace TEXT, owner TEXT, fence INTEGER NOT NULL, expires INTEGER NOT NULL,
                write_mode TEXT, backing_id TEXT
             );
             INSERT INTO mount_rs_metadata VALUES(1,0,NULL,NULL,9223372036854775807,0,'MRC1',NULL);",
        )
        .expect("seed an unstamped historical MRC1 row");
    drop(connection);
    let config = serde_json::json!({
        "version": 1,
        "mountpoint": mountpoint,
        "driver": {
            "kind": "splitstore",
            "storage": {
                "concurrent_writes": true,
                "metadata": {"kind": "sqlite", "path": metadata},
                "blocks": {"kind": "sqlite", "path": blocks}
            }
        }
    });
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_mount-rs"))
        .arg("migrate-concurrent-backing")
        .arg("--config")
        .arg(&config_path)
        .arg("--expected-revision")
        .arg("0")
        .output()
        .expect("run historical migration probe");
    assert!(
        !output.status.success(),
        "historical unstamped metadata was bound: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "failed migration reported success"
    );
    let row: (
        String,
        Option<String>,
        i64,
        i64,
        Option<String>,
        Option<String>,
    ) = rusqlite::Connection::open(&metadata)
        .unwrap()
        .query_row(
            "SELECT write_mode, backing_id, revision, fence, physical_dev, physical_ino
                 FROM mount_rs_metadata WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row, ("MRC1".to_owned(), None, 0, i64::MAX, None, None));
    let block_markers: i64 = rusqlite::Connection::open(&blocks)
        .unwrap()
        .query_row("SELECT count(*) FROM mount_rs_block_authority", [], |row| {
            row.get(0)
        })
        .unwrap();
    println!("HISTORICAL_UNSTAMPED_SQLITE_MIGRATION_BLOCK_MARKERS={block_markers}");
    assert!(
        mountpoint.is_dir(),
        "path checks prepare the view before storage opens"
    );
    assert!(
        fs::read_dir(&mountpoint).unwrap().next().is_none(),
        "failed migration leaves an empty prepared view"
    );
}

#[test]
fn migrate_concurrent_backing_cli_requires_concurrent_splitstore_config() {
    let scope = tempfile::TempDir::new().expect("create owned config rejection root");
    let metadata = scope.path().join("metadata.sqlite");
    let blocks = scope.path().join("blocks.sqlite");
    let config_path = scope.path().join("single-writer.json");
    let config = serde_json::json!({
        "version": 1,
        "driver": {
            "kind": "splitstore",
            "storage": {
                "concurrent_writes": false,
                "metadata": {"kind": "sqlite", "path": metadata},
                "blocks": {"kind": "sqlite", "path": blocks}
            }
        }
    });
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_mount-rs"))
        .arg("migrate-concurrent-backing")
        .arg("--config")
        .arg(&config_path)
        .arg("--expected-revision")
        .arg("0")
        .output()
        .expect("run offline migration with a single-writer config");
    assert_eq!(
        output.status.code(),
        Some(2),
        "invalid migration config must exit as usage error: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("concurrent_writes=true"));
    assert!(!metadata.exists());
    assert!(!blocks.exists());
}

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
