use mount_rs_cli::color::Color;
use mount_rs_cli::parse_args;
use mount_rs_cli::parser::{CliOptions, Command, DriverChoice, TransportChoice, help_text};
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
