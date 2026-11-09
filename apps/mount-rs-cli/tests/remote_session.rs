use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, VolumeSessions};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn cli_remote_self_test_uses_public_endpoint_and_drains_session() {
    let owner = Arc::new(
        mount_rs_sdk::Filesystem::split(mount_rs_sdk::SplitOptions::memory(
            "cli-remote-test",
            4096,
        ))
        .await
        .unwrap(),
    );
    let registry =
        Arc::new(VolumeSessions::new(owner.clone(), 1, 8, Duration::from_secs(60)).unwrap());
    let secret = "r".repeat(32);
    let handler = SessionHttpHandler::new(
        "volume",
        SessionCredentials::new(vec![("cli".into(), secret.clone())]).unwrap(),
        registry.clone(),
    )
    .unwrap();
    let server =
        mount_rs_http::HttpServer::start_with_handler(Arc::new(handler), Default::default())
            .await
            .unwrap();
    let config_path =
        std::env::temp_dir().join(format!("mount-rs-remote-cli-{}.json", std::process::id()));
    std::fs::write(&config_path, serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "driver": {"kind":"remote", "server":server.url(), "volume":"volume", "token":{"env":"MOUNT_TEST_REMOTE_TOKEN"}}
    })).unwrap()).unwrap();
    // A regular file cannot be a mountpoint. Fail before invoking a native
    // helper, but after opening the remote session, then verify its slot is free.
    let blocked_mount = config_path.with_extension("mount-file");
    std::fs::write(&blocked_mount, b"not a directory").unwrap();
    let failed = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_mount-rs"))
            .arg("mount")
            .arg(&blocked_mount)
            .args(["--transport", "fuse", "--config"])
            .arg(&config_path)
            .env("MOUNT_TEST_REMOTE_TOKEN", &secret)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!failed.status.success());
    assert_eq!(std::fs::read(&blocked_mount).unwrap(), b"not a directory");
    std::fs::remove_file(blocked_mount).unwrap();
    // Two sequential sessions with a one-session budget also prove explicit
    // CLI cleanup, without relying on expiry or native mount availability.
    for _ in 0..2 {
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_mount-rs"))
                .args(["sdk-self-test", "--config"])
                .arg(&config_path)
                .env("MOUNT_TEST_REMOTE_TOKEN", &secret)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("driver=remote"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&secret));
        assert!(owner.driver().readdir("/").await.unwrap().is_empty());
    }
    std::fs::remove_file(config_path).unwrap();
    server.close().await.unwrap();
    registry.close().await.unwrap();
    owner.shutdown().await.unwrap();
}
