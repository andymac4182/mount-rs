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

fn assert_balanced_timed_runtime(directory: &std::path::Path, journal: &serde_json::Value) {
    // Inspect actual retained runtime evidence, not a synthetic profile declaration.
    let boundaries = journal["phase_metrics"]["boundaries"].as_array().unwrap();
    let first = boundaries
        .iter()
        .find(|row| {
            row["phase"] == "mostly_idle/sequential_read" && row["boundary"] == "before_active"
        })
        .expect("first timed runtime boundary must be retained");
    let workers = first["workers"].as_array().unwrap();
    assert_eq!(workers.len(), 10);
    let mut seen = std::collections::BTreeSet::new();
    let mut opened_cluster = 0;
    for worker in workers {
        let server = worker["server"].as_u64().unwrap() as usize;
        assert!(seen.insert(server), "duplicate measured worker");
        let path = directory.join(worker["file"].as_str().unwrap());
        let receipt = target::read_json(&path).unwrap();
        assert_eq!(
            target::file_digest(&path).unwrap(),
            worker["sha256"].as_str().unwrap()
        );
        let runtime = &receipt["runtime_activation"];
        let observations = runtime["observations"].as_array().unwrap();
        assert_eq!(observations.len(), 10);
        for (drive, observation) in observations.iter().enumerate() {
            assert_eq!(
                observation["constructed"],
                u64::from(drive % 10 == server),
                "timed phase contains a runtime on an unassigned worker"
            );
        }
        assert_eq!(runtime["pool"]["open_success"], 1);
        assert_eq!(runtime["pool"]["resident"], 1);
        opened_cluster += runtime["pool"]["open_success"].as_u64().unwrap();
    }
    assert_eq!(
        opened_cluster, 10,
        "one opened runtime per sandbox, not every server"
    );
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
    assert_balanced_timed_runtime(directory.as_path(), &journal);
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

#[test]
#[ignore = "owned ten-process cold registration regression; explicitly invoked"]
fn ten_process_lazy_startup_preserves_exact_backing_and_workload() {
    let directory = tempfile::Builder::new()
        .prefix("mount-rs-target-lazy-")
        .tempdir()
        .unwrap()
        .keep();
    eprintln!("retained lazy control: {}", directory.display());
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "production_target_controller",
            "--nocapture",
        ])
        .env("MOUNT_RS_TARGET_MODE", "control")
        .env("MOUNT_RS_TARGET_PROVIDER", "sqlite")
        .env("MOUNT_RS_TARGET_BLOCK_PROVIDER", "metadata")
        .env("MOUNT_RS_TARGET_DRIVES", "10")
        .env("MOUNT_RS_TARGET_FILES", "2")
        .env("MOUNT_RS_TARGET_SECONDS", "1")
        .env("MOUNT_RS_PROFILE_IO", "1")
        .env_remove("MOUNT_RS_TARGET_INJECT")
        .env("MOUNT_RS_TARGET_OUTPUT", directory.as_path())
        .status()
        .unwrap();
    let journal = target::read_json(&directory.join("terminal.json")).unwrap();
    if !status.success() {
        // Retain fixed public flags before parent cleanup, without raw storage
        // errors, credentials, checkout text or private paths.
        eprintln!(
            "MOUNT_RS_TARGET_CONTROL_FAILURE {}",
            serde_json::json!({
                "schema_version":1,
                "checkout_clean":journal["source"]["checkout_status"].as_str() == Some(""),
                "revision_shape_valid":journal["source"]["revision"].as_str()
                    .is_some_and(|revision| revision.len() == 40 && revision.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))),
                "workload_complete":journal["workload_complete"].as_bool(),
                "metrics_complete":journal["metrics_complete"].as_bool(),
                "error_present":!journal["error"].is_null(),
                "cleanup_error_count":journal["cleanup_errors"].as_array().map(Vec::len),
                "observer_complete":journal["observer_accounting"]["_status"]["complete"].as_bool(),
                "oracle_complete":journal["oracle_accounting"]["_status"]["complete"].as_bool(),
                "initialization_complete":journal["initialization_accounting"]["_status"]["complete"].as_bool()
            })
        );
    }
    assert!(
        status.success(),
        "must finish the real signed workload and owned cleanup before the cold assertion"
    );
    assert_eq!(journal["full_target"], false);
    assert_eq!(journal["outcome"], "success");
    assert_eq!(journal["metrics_complete"], true);
    assert_eq!(journal["namespace_files"], 20);
    assert_eq!(journal["population_bytes"], 81920);
    assert_eq!(journal["routes"], 100);
    assert_balanced_timed_runtime(directory.as_path(), &journal);
    assert_eq!(journal["scope_denials"]["sibling"], 10);
    assert_eq!(journal["scope_denials"]["partition"], 10);
    assert_eq!(journal["verified_passes"], 2);
    let workers = journal["workers"].as_array().unwrap();
    assert_eq!(workers.len(), 10);
    assert!(
        workers
            .iter()
            .all(|worker| worker["reap_confirmed"] == true && worker["exit_code"] == 0)
    );
    assert!(journal["cleanup_errors"].as_array().unwrap().is_empty());
    assert!(!directory.join("private/worker-config.json").exists());
    for (index, worker) in workers.iter().enumerate() {
        let ready =
            target::read_json(&directory.join(format!("worker-{index}/ready-generation-0.json")))
                .unwrap();
        assert_eq!(ready["pid"], worker["pid"]);
        assert_eq!(ready["generation"], 0);
        assert_eq!(ready["source_digest"], journal["source"]["digest"]);
        assert_eq!(ready["binary_digest"], journal["source"]["binary_sha256"]);
        let actual_opens = ready["startup_diagnostics"]["open_success"]
            .as_u64()
            .expect("actual enabled startup evidence must precede cold assertion");
        assert_eq!(
            actual_opens, 0,
            "registered Drive plans must not eagerly open providers on worker {index}"
        );
    }
}
