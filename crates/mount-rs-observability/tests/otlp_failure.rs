#![cfg(feature = "otlp")]

use std::time::Duration;

use mount_rs_observability::{OtlpConfig, OtlpError, Telemetry, TelemetryConfig, install_otlp};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_setup_flush_and_shutdown_report_exporter_failures() {
    let setup_error = install_otlp(OtlpConfig {
        endpoint: "not a URI".to_owned(),
        ..OtlpConfig::default()
    })
    .expect_err("an invalid endpoint must fail during exporter setup");
    match setup_error {
        OtlpError::Exporter(message) => assert!(message.contains("traces:"), "{message}"),
        other => panic!("invalid endpoint returned the wrong error: {other}"),
    }

    let guard = install_otlp(OtlpConfig {
        endpoint: "http://127.0.0.1:0".to_owned(),
        export_timeout: Duration::from_millis(250),
        service_name: "w30-otlp-failure-test".to_owned(),
        ..OtlpConfig::default()
    })
    .expect("a valid endpoint URI should build the exporters");
    let telemetry = Telemetry::new(TelemetryConfig::enabled("w30-otlp-failure-test"));

    emit_signals(&telemetry).await;
    assert_shutdown_error(guard.force_flush(), "force_flush");

    // A failed flush drains the batch processors. Emit a second batch so
    // shutdown must report its own final-export failures as well.
    emit_signals(&telemetry).await;
    assert_shutdown_error(guard.shutdown(), "shutdown");
}

async fn emit_signals(telemetry: &Telemetry) {
    telemetry
        .observe_result(
            "test.boundary",
            "test.operation",
            Some("/secret/tenant/file.txt"),
            async { Ok::<_, ()>(()) },
            |_| None,
        )
        .await
        .expect("the test operation should succeed");
    telemetry.record_bytes("read", 7);
}

fn assert_shutdown_error(result: Result<(), OtlpError>, operation: &str) {
    match result {
        Err(OtlpError::Shutdown(message)) => {
            assert!(!message.is_empty(), "{operation} returned an empty error");
        }
        Ok(()) => panic!("{operation} unexpectedly succeeded"),
        Err(other) => panic!("{operation} returned the wrong error: {other}"),
    }
}
