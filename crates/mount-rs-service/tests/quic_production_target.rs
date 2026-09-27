//! Invoked production target controller and private same-executable worker.
#![cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
#[path = "support/production_target/mod.rs"]
mod target;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit ten-process production target; requires retained artifact directory"]
async fn production_target_controller() {
    target::controller().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "private controller-owned worker role"]
async fn production_target_worker() {
    target::worker().await.unwrap();
}

#[test]
#[ignore = "owned ten-process two-file diagnostic, explicitly invoked"]
fn ten_process_online_smoke() {
    let directory = tempfile::Builder::new()
        .prefix("mount-rs-target-smoke-")
        .tempdir()
        .unwrap()
        .keep();
    eprintln!("retained smoke output: {}", directory.display());
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "production_target_controller",
            "--nocapture",
        ])
        .env("MOUNT_RS_TARGET_MODE", "control")
        .env("MOUNT_RS_TARGET_DRIVES", "10")
        .env("MOUNT_RS_TARGET_FILES", "2")
        .env("MOUNT_RS_TARGET_SECONDS", "1")
        .env("MOUNT_RS_TARGET_OUTPUT", directory.as_path())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "invoked online process path must complete"
    );
    let journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.as_path().join("terminal.json")).unwrap())
            .unwrap();
    assert_eq!(journal["full_target"], false);
    assert_eq!(journal["outcome"], "success");
    assert_eq!(journal["workers"].as_array().unwrap().len(), 10);
    assert_eq!(journal["namespace_files"], 20);
    assert_eq!(journal["population_bytes"], 81920);
    assert_eq!(journal["routes"], 100);
    assert_eq!(journal["verified_passes"], 2);
}

#[test]
#[ignore = "explicit owned child-loss, timeout and partial-start controls"]
fn process_failures_retain_partial_evidence_and_reap_children() {
    for injection in ["partial_start", "child_loss", "work_timeout"] {
        let directory = tempfile::Builder::new()
            .prefix("mount-rs-target-fault-")
            .tempdir()
            .unwrap()
            .keep();
        eprintln!("retained {injection}: {}", directory.display());
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "production_target_controller",
                "--nocapture",
            ])
            .env("MOUNT_RS_TARGET_MODE", "control")
            .env("MOUNT_RS_TARGET_DRIVES", "10")
            .env("MOUNT_RS_TARGET_FILES", "2")
            .env("MOUNT_RS_TARGET_SECONDS", "1")
            .env("MOUNT_RS_TARGET_INJECT", injection)
            .env("MOUNT_RS_TARGET_OUTPUT", &directory)
            .status()
            .unwrap();
        assert!(!status.success());
        let j: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("terminal.json")).unwrap())
                .unwrap();
        assert_eq!(j["outcome"], "incomplete");
        let expected_error = match injection {
            "partial_start" => "injected partial startup failure",
            "child_loss" => "owned worker 3 exited before shutdown",
            "work_timeout" => "\"injected_timeout\" phase deadline; partial work incomplete",
            _ => unreachable!(),
        };
        assert_eq!(
            j["error"], expected_error,
            "must reach intended fault, not an unrelated setup failure"
        );
        if injection == "work_timeout" {
            assert_eq!(j["last_work_phase"], "injected_timeout");
        }
        assert_eq!(j["initialization"]["initialized_drives"], 10);
        assert_eq!(j["initialization"]["complete"], true);
        assert_eq!(j["initialization"]["cleanup_confirmed"], true);
        assert_eq!(j["namespace_files"], 0);
        assert_eq!(j["population_bytes"], 0);
        let initialized =
            target::read_json(&directory.join("initialization-receipts.json")).unwrap();
        assert_eq!(initialized.as_array().unwrap().len(), 10);
        assert!(
            initialized
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["empty_root_verified"] == true
                    && r["namespace_entries"] == 0
                    && r["filesystem_closed"] == true)
        );

        assert_eq!(
            j["workers"].as_array().unwrap().len(),
            if injection == "partial_start" { 3 } else { 10 }
        );
        assert!(
            j["workers"]
                .as_array()
                .unwrap()
                .iter()
                .all(|w| w["reap_confirmed"] == true)
        );
        assert!(!directory.join("private/worker-config.json").exists());
        assert_eq!(j["controller_resources"]["terminal_sample"], true);
        assert_eq!(
            j["controller_resources"],
            target::read_json(&directory.join("controller-resources.json")).unwrap()
        );
        let expected_peak = j["workers"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|w| w["resources"]["peak_rss_bytes"].as_u64())
            .sum::<u64>()
            + j["controller_resources"]["peak_rss_bytes"]
                .as_u64()
                .unwrap();
        assert_eq!(
            j["aggregate_owned_resources"]["sum_individual_peak_rss_bytes"],
            expected_peak
        );
    }
}
