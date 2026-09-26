#![cfg(any(not(feature = "local-oidc-fixture"), not(debug_assertions)))]

use std::process::Command;

#[test]
fn normal_or_release_binary_rejects_local_oidc_fixture_field_before_catalog_open() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("service.json");
    std::fs::write(
        &config,
        serde_json::json!({
            "version":1,
            "catalog":"absent/catalog.sqlite",
            "listen":"127.0.0.1:0",
            "websocket_listen":"127.0.0.1:0",
            "certificate":"absent.pem",
            "private_key":"absent-key.pem",
            "local_oidc_fixture":{
                "issuer":"https://issuer.example.com",
                "audiences":["mount-rs"],
                "jwks":"absent.jwks.json"
            }
        })
        .to_string(),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args(["serve-remote", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("invalid remote configuration"), "{stderr}");
    assert!(!directory.path().join("absent").exists());
}
