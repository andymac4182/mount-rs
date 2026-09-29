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

fn require_native_diagnostics(available: bool) {
    assert!(
        available,
        "owned native controls require --features sdk-runtime,resource-profiling"
    );
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

fn assert_crossnode_payload(journal: &serde_json::Value) {
    let payload = &journal["crossnode_payload"];
    assert_eq!(
        payload["verified_reads"], 100,
        "every Drive/server pair must verify acknowledged payload bytes over QUIC"
    );
    assert_eq!(payload["verified_bytes"], 409_600);
    assert_eq!(payload["completed_pairs"], 100);
    assert_eq!(payload["expected_pairs"], 100);
    assert_eq!(payload["complete"], true);

    let batches = journal["crossnode_route_batches"].as_array().unwrap();
    assert_eq!(batches.len(), 10);
    let mut offsets = std::collections::BTreeSet::new();
    for (rotation, batch) in batches.iter().enumerate() {
        let offset = (rotation + 1) % 10;
        assert_eq!(batch["generation"], rotation as u64 + 2);
        assert_eq!(batch["offset"], offset);
        assert!(offsets.insert(offset));
        assert_eq!(batch["acknowledged_stats"], 10);
        assert_eq!(batch["verified_reads"], 10);
        assert_eq!(batch["verified_bytes"], 40_960);
        assert_eq!(batch["expected_reads"], 10);
        assert_eq!(batch["expected_bytes"], 40_960);
        assert_eq!(batch["rpc_complete"], true);
        assert_eq!(batch["validation_complete"], true);
    }
}

fn assert_fresh_oracle_corpus(directory: &std::path::Path, journal: &serde_json::Value) {
    assert_eq!(journal["verified_passes"], 2);
    assert_eq!(journal["fresh_oracle_complete"], true);
    assert_eq!(journal["fresh_oracle_settled"], true);
    assert_eq!(journal["expected_state_observation"]["complete"], true);
    let drives = journal["configuration"]["drives"].as_u64().unwrap();
    let ledger_receipts = journal["expected_state_receipts"].as_array().unwrap();
    assert_eq!(ledger_receipts.len() as u64, drives);
    let mut ids = std::collections::BTreeSet::new();
    let mut final_files = 0u64;
    let mut final_bytes = 0u64;
    for receipt in ledger_receipts {
        let drive = receipt["drive"].as_u64().unwrap();
        assert!(drive < drives && ids.insert(drive));
        let name = format!("expected/drive-{drive}.json");
        assert_eq!(receipt["file"], name);
        let path = directory.join(name);
        assert_eq!(target::file_digest(&path).unwrap(), receipt["sha256"]);
        let ledger = target::read_json(&path).unwrap();
        assert_eq!(ledger["drive"], drive);
        let files = ledger["files"].as_object().unwrap();
        final_files = final_files.checked_add(files.len() as u64).unwrap();
        for file in files.values() {
            let length = file["length"].as_u64().unwrap();
            assert_eq!(length % 4096, 0);
            final_bytes = final_bytes.checked_add(length).unwrap();
        }
    }
    let ids: Vec<_> = ids.into_iter().collect();
    assert_eq!(ids, (0..drives).collect::<Vec<_>>());
    assert_eq!(journal["verified_files"], final_files);
    assert_eq!(journal["verified_bytes"], final_bytes);
    let passes = journal["fresh_oracle_passes"].as_array().unwrap();
    assert_eq!(passes.len(), 2);
    for (index, label) in ["initial", "final"].into_iter().enumerate() {
        let summary = &passes[index];
        assert_eq!(summary.as_object().unwrap().len(), 16);
        assert_eq!(summary["pass"], label);
        assert_eq!(summary["slot_limit"], 8);
        for field in ["expected_drives", "started_drives", "completed_drives"] {
            assert_eq!(summary[field], drives);
        }
        assert_eq!(summary["live_slots"], 0);
        assert_eq!(summary["complete"], true);
        assert_eq!(summary["settled"], true);
        assert_eq!(summary["after_boundary_complete"], true);
        let (files, bytes) = if index == 0 {
            (
                journal["namespace_files"].as_u64().unwrap(),
                journal["population_bytes"].as_u64().unwrap(),
            )
        } else {
            (final_files, final_bytes)
        };
        for field in ["expected_files", "completed_files", "checked_files"] {
            assert_eq!(summary[field], files);
        }
        for field in ["expected_bytes", "completed_bytes", "compared_bytes"] {
            assert_eq!(summary[field], bytes);
        }
        let name = format!("oracle-receipts/{label}.json");
        assert_eq!(summary["receipt"]["file"], name);
        let path = directory.join(name);
        assert_eq!(
            target::file_digest(&path).unwrap(),
            summary["receipt"]["sha256"]
        );
        let raw = target::read_json(&path).unwrap();
        assert_eq!(raw.as_object().unwrap().len(), 15);
        assert_eq!(raw["completed_drive_ids"], serde_json::json!(ids));
        for (field, value) in summary.as_object().unwrap() {
            if !matches!(field.as_str(), "after_boundary_complete" | "receipt") {
                assert_eq!(&raw[field], value);
            }
        }
        let phase = format!("{label}_fresh_oracle");
        let after: Vec<_> = journal["phase_metrics"]["boundaries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["phase"] == phase && row["boundary"] == "after")
            .collect();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0]["complete"], true);
        if journal["metrics_required_for_outcome"] == true {
            assert_eq!(after[0]["metrics_complete"], true);
        }
        let boundary = after[0];
        let generation = boundary["generation"].as_u64().unwrap();
        let sequence = boundary["sequence"].as_u64().unwrap();
        let name = format!("metrics/g{generation}-s{sequence}.json.gz");
        assert_eq!(boundary["controller"]["file"], name);
        let path = directory.join(name);
        assert_eq!(
            target::file_digest(&path).unwrap(),
            boundary["controller"]["sha256"]
        );
        let controller = target::read_json(&path).unwrap();
        let mut identity = serde_json::json!({
            "controller_pid":journal["controller_resources"]["pid"],
            "generation":generation,"sequence":sequence,"phase":phase,"boundary":"after",
            "source_digest":journal["source"]["digest"],"binary_digest":journal["source"]["binary_sha256"],
            "catalog_digest":controller["identity"]["catalog_digest"],
            "backend_prefix":controller["identity"]["backend_prefix"],
            "pid":journal["controller_resources"]["pid"],"role":"controller","server":null
        });
        let check_frame = |frame: &serde_json::Value, identity: &serde_json::Value| {
            assert_eq!(&frame["identity"], identity);
            assert_eq!(frame["capture_complete"], true);
            if journal["metrics_required_for_outcome"] == true {
                assert_eq!(frame["metrics_complete"], true);
            }
        };
        check_frame(&controller, &identity);
        let measured = boundary["workers"].as_array().unwrap();
        assert_eq!(measured.len(), 10);
        let mut seen = std::collections::BTreeSet::new();
        for worker in measured {
            let server = worker["server"].as_u64().unwrap();
            assert!(server < 10 && seen.insert(server));
            let owned: Vec<_> = journal["workers"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|owner| owner["server"] == server)
                .collect();
            assert_eq!(owned.len(), 1);
            assert_eq!(owned[0]["pid"], worker["pid"]);
            let name = format!("worker-{server}/metrics/g{generation}-s{sequence}.json.gz");
            assert_eq!(worker["file"], name);
            let path = directory.join(name);
            assert_eq!(target::file_digest(&path).unwrap(), worker["sha256"]);
            let frame = target::read_json(&path).unwrap();
            identity["pid"] = worker["pid"].clone();
            identity["server"] = serde_json::json!(server);
            identity["role"] = serde_json::json!("worker");
            check_frame(&frame, &identity);
        }
    }
}

#[test]
#[ignore = "owned ten-process two-file diagnostic, explicitly invoked"]
fn ten_process_online_smoke() {
    require_native_diagnostics(cfg!(feature = "resource-profiling"));
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
        .env("MOUNT_RS_PROFILE_IO", "1")
        .env("MOUNT_RS_TARGET_OUTPUT", directory.as_path())
        .status()
        .unwrap();
    let journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.as_path().join("terminal.json")).unwrap())
            .unwrap();
    assert!(
        status.success(),
        "invoked online process path must complete"
    );
    assert_crossnode_payload(&journal);
    assert_eq!(journal["full_target"], false);
    assert_eq!(journal["outcome"], "success");
    assert_eq!(journal["workers"].as_array().unwrap().len(), 10);
    assert_eq!(journal["namespace_files"], 20);
    assert_eq!(journal["population_bytes"], 81920);
    assert_eq!(journal["routes"], 100);
    assert_balanced_timed_runtime(directory.as_path(), &journal);
    assert_fresh_oracle_corpus(directory.as_path(), &journal);
}

#[test]
#[ignore = "explicit owned child-loss, timeout and partial-start controls"]
fn process_failures_retain_partial_evidence_and_reap_children() {
    require_native_diagnostics(cfg!(feature = "resource-profiling"));
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
            .env("MOUNT_RS_PROFILE_IO", "1")
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
    require_native_diagnostics(cfg!(feature = "resource-profiling"));
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
    assert_crossnode_payload(&journal);
    assert_eq!(journal["full_target"], false);
    assert_eq!(journal["outcome"], "success");
    assert_eq!(journal["metrics_complete"], true);
    assert_eq!(journal["namespace_files"], 20);
    assert_eq!(journal["population_bytes"], 81920);
    assert_eq!(journal["routes"], 100);
    assert_balanced_timed_runtime(directory.as_path(), &journal);
    assert_eq!(journal["scope_denials"]["sibling"], 10);
    assert_eq!(journal["scope_denials"]["partition"], 10);
    assert_fresh_oracle_corpus(directory.as_path(), &journal);
    let workers = journal["workers"].as_array().unwrap();
    assert_eq!(workers.len(), 10);
    assert!(
        workers
            .iter()
            .all(|worker| worker["reap_confirmed"] == true && worker["exit_code"] == 0)
    );
    assert!(journal["cleanup_errors"].as_array().unwrap().is_empty());
    assert!(!directory.join("private/worker-config.json").exists());
    let mut checkpoint_workers = std::collections::BTreeSet::new();
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
        let server = worker["server"].as_u64().unwrap();
        assert!(server < 10 && checkpoint_workers.insert(server));
        let root = directory.join(format!("worker-{server}"));
        let final_ready = target::read_json(&root.join("ready.json")).unwrap();
        assert_eq!(final_ready, worker["ready"]);
        let expected_identity = serde_json::json!({
            "pid":worker["pid"],
            "controller_pid":journal["controller_resources"]["pid"],
            "worker":server,
            "generation":final_ready["generation"],
            "source_digest":journal["source"]["digest"],
            "binary_digest":journal["source"]["binary_sha256"]
        });
        let checkpoint =
            target::retained_checkpoint(&root.join("checkpoint-latest.json"), &expected_identity)
                .expect("joined worker must retain a valid cumulative checkpoint");
        assert!(checkpoint["identity"]["sequence"].as_u64().unwrap() > 1);
        assert_eq!(checkpoint["resources"]["terminal_sample"], true);
        assert_eq!(checkpoint["resources"], worker["resources"]);
        assert_eq!(checkpoint["resources"], worker["terminal"]["resources"]);
        assert!(
            checkpoint["resources"]["samples"].as_u64().unwrap()
                > ready["resources"]["samples"].as_u64().unwrap()
        );
        for counter in ["cpu_user_us", "cpu_system_us"] {
            assert!(
                checkpoint["resources"]["process_delta"][counter]
                    .as_u64()
                    .unwrap()
                    >= ready["resources"]["process_delta"][counter]
                        .as_u64()
                        .unwrap()
            );
        }
        let startup = target::read_json(&root.join("metrics/startup-g0.json.gz")).unwrap();
        let completed_calls = |value: &serde_json::Value| -> u128 {
            value["storage"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| u128::from(row["calls"].as_u64().unwrap()))
                .sum()
        };
        assert!(
            completed_calls(&checkpoint) > completed_calls(&startup),
            "checkpoint must observe actual storage work after cold registration"
        );
    }
    assert_eq!(checkpoint_workers.len(), 10);
}
