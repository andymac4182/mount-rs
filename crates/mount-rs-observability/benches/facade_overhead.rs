//! Stable-Rust microbenchmark for the mount-rs observability facade.
//!
//! This target uses a small stable-Rust binary benchmark, so it does not need
//! nightly-only `#[bench]`. Run it with `cargo bench --bench facade_overhead` to
//! see the measurements printed by the binary.
//!
//! The workload is deterministic: every case runs the same fixed number of
//! successful async operations, uses the same bounded path and static labels,
//! and consumes a checksum with `black_box` so the loop cannot be discarded.
//! No OTLP provider is installed, so the enabled case measures facade/span/local
//! metric work only; it does not measure network exporters or collector I/O.
//! Timings are machine-dependent comparative observations, not a pass/fail SLO.

use mount_rs_observability::{Telemetry, TelemetryConfig};
use std::hint::black_box;
use std::time::{Duration, Instant};
use tokio::runtime::{Builder, Runtime};

const ITERATIONS: usize = 20_000;
const BENCHMARK_PATH: &str = "/benchmark/fixture.txt";

#[derive(Debug, Clone, Copy)]
struct Measurement {
    elapsed: Duration,
    checksum: u64,
}

impl Measurement {
    fn nanos_per_operation(self) -> u128 {
        self.elapsed.as_nanos() / ITERATIONS as u128
    }
}

fn runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("benchmark runtime should build")
}

fn measure_baseline(runtime: &Runtime) -> Measurement {
    let started = Instant::now();
    let checksum = runtime.block_on(async {
        let mut checksum = 0_u64;
        for _ in 0..ITERATIONS {
            let value = async { 1_u64 }.await;
            checksum = checksum.wrapping_add(black_box(value));
        }
        black_box(checksum)
    });

    Measurement {
        elapsed: started.elapsed(),
        checksum,
    }
}

fn measure_facade(runtime: &Runtime, telemetry: &Telemetry) -> Measurement {
    let started = Instant::now();
    let checksum = runtime.block_on(async {
        let mut checksum = 0_u64;
        for _ in 0..ITERATIONS {
            let result = telemetry
                .observe_result(
                    "benchmark",
                    "read",
                    Some(BENCHMARK_PATH),
                    async { Ok::<u64, ()>(1) },
                    |_: &()| None,
                )
                .await;
            checksum = checksum.wrapping_add(black_box(
                result.expect("benchmark operation should succeed"),
            ));
        }
        black_box(checksum)
    });

    Measurement {
        elapsed: started.elapsed(),
        checksum,
    }
}

fn main() {
    let runtime = runtime();
    let disabled = Telemetry::disabled();
    let enabled = Telemetry::new(TelemetryConfig::enabled("mount-rs-benchmark"));

    let baseline = measure_baseline(&runtime);
    let disabled_measurement = measure_facade(&runtime, &disabled);
    let enabled_measurement = measure_facade(&runtime, &enabled);

    assert_eq!(baseline.checksum, ITERATIONS as u64);
    assert_eq!(disabled_measurement.checksum, baseline.checksum);
    assert_eq!(enabled_measurement.checksum, baseline.checksum);

    println!(
        "facade_overhead iterations={ITERATIONS} baseline_total_ns={} disabled_total_ns={} enabled_total_ns={} baseline_ns_per_op={} disabled_ns_per_op={} enabled_ns_per_op={} disabled_overhead_ns={} enabled_overhead_ns={} checksum={}",
        baseline.elapsed.as_nanos(),
        disabled_measurement.elapsed.as_nanos(),
        enabled_measurement.elapsed.as_nanos(),
        baseline.nanos_per_operation(),
        disabled_measurement.nanos_per_operation(),
        enabled_measurement.nanos_per_operation(),
        disabled_measurement
            .elapsed
            .as_nanos()
            .saturating_sub(baseline.elapsed.as_nanos()),
        enabled_measurement
            .elapsed
            .as_nanos()
            .saturating_sub(baseline.elapsed.as_nanos()),
        black_box(enabled_measurement.checksum),
    );
}
