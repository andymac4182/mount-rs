# Strict-policy RustFS bind and volume pair

Two ordered single trials: fresh bind first, fresh named volume second. Prefix 1, 4KiB objects, five-second admission, zero application cache. Server OS caches could be warm: 256 seed objects were prepared before timed reads and all phases reused the process. The archived release executable, all 16 source files, helper and RustFS image matched.

Both authenticated fresh-bucket policy reads returned the exact strict override before object writes. Destination/ancestor-directory fsync stage counts advanced after the baseline: 19→27 and 26→30 in each trial. These are process-global corroboration, not per-request attribution or crash/power-loss durability qualification.

## Logical operations per second

| Pattern | C | Bind | Volume | Volume / bind |
| --- | ---: | ---: | ---: | ---: |
| raw_get | 1 | 1873.939 | 1898.330 | 1.013 |
| raw_unique_put | 1 | 91.781 | 176.553 | 1.924 |
| configured_authority_verify | 1 | 1138.960 | 971.391 | 0.853 |
| raw_get | 10 | 10803.599 | 11555.585 | 1.070 |
| raw_unique_put | 10 | 244.236 | 569.805 | 2.333 |
| configured_authority_verify | 10 | 6712.454 | 6016.257 | 0.896 |
| raw_get | 100 | 16025.301 | 15083.957 | 0.941 |
| raw_unique_put | 100 | 284.909 | 502.270 | 1.763 |
| configured_authority_verify | 100 | 9852.526 | 10381.174 | 1.054 |

Rates use exact successful-operation counts divided by recorded monotonic admission-to-settlement nanoseconds, including the completed tail after the five-second admission cutoff. JSON retains all 18 counts/clocks/latency sums. Each raw GET/PUT had one HTTP dispatch per logical operation. Configured-authority verification had two separately observed GETs per logical operation.

All 18 cells were positive, uncapped and settled. All timed HTTP responses were 2xx; status counts and request/body terminal outcomes conserved. No operation failures, uncertainty, harness retries, SDK retries, unacknowledged payloads or cache hits were recorded. Successful empty PUT response bodies were dropped without polling; those `body_dropped` counts are expected completion coverage, not write failure.

## PUT wall-time means

| Mount | C | Logical ms | SDK PUT ms | Headers ms | Sum headers / SDK wall |
| --- | ---: | ---: | ---: | ---: | ---: |
| bind | 1 | 10.894625 | 10.847419 | 10.792887 | 99.497278% |
| bind | 10 | 40.841049 | 40.816458 | 40.784816 | 99.922479% |
| bind | 100 | 344.898482 | 344.864611 | 344.825464 | 99.988648% |
| volume | 1 | 5.663493 | 5.624234 | 5.581294 | 99.236516% |
| volume | 10 | 17.531364 | 17.501582 | 17.468215 | 99.809347% |
| volume | 100 | 195.489854 | 195.457711 | 195.425688 | 99.983617% |

These summed SDK/API and dispatch-to-response-header wall spans include waiting, scheduling, network and server work. They are not an exclusive CPU, disk or network breakdown. HTTP response-body observed lifetime does not mean the response was drained. No maxima or gauges are subtracted.

## Resource samples

| Mount | Whole stats window s | CPU s | Mean cores | Aggregate throttle s | Throttled periods | Peak cgroup bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| bind | 85.260568 | 64.500118 | 0.756506 | 5.011215 | 26 | 194363392 |
| volume | 79.339995 | 76.904903 | 0.969308 | 16.592040 | 178 | 352473088 |

| Mount | PUT C | Interior sample window s | Enclosure coverage | Mean cores | Aggregate throttle s |
| --- | ---: | ---: | ---: | ---: | ---: |
| bind | 1 | 2.069691 | 41.38% | 0.432617 | 0.000000 |
| bind | 10 | 2.067891 | 41.16% | 0.921050 | 0.000000 |
| bind | 100 | 2.077567 | 39.99% | 1.282126 | 4.401053 |
| volume | 1 | 2.082385 | 41.64% | 0.715889 | 0.000000 |
| volume | 10 | 2.073708 | 41.39% | 2.008160 | 1.005125 |
| volume | 100 | 2.072286 | 39.46% | 1.896968 | 0.101904 |

Both containers were capped at two CPUs and 2GiB memory+swap. Whole samples include setup, workload, cleanup and background. Aggregate throttled time can overlap threads and is not exclusive request wall time. Cgroup memory includes anonymous memory/page cache and is not process RSS. JSON retains network deltas and sampled memory maxima.

Interior PUT samples cover only about 39–42% of each phase enclosure at ~2s cadence. Volume C10/C100 samples used roughly two cores; CPU limits remain a plausible constraint. Host post-read lag was 50–321ms. Guest/host UTC alignment was not calibrated; exact wall-versus-monotonic differences and outer brackets are retained. Outer windows contain adjacent work and overlap. No interpolation or exact per-request resource attribution is claimed.

## Settlement, provenance and limits

Exact payload cleanup deleted all acknowledged blocks: bind 3,422; volume 6,632, including 256 seeds per trial. No failed, uncertain or unattempted deletes, quarantine, range reconciliation or retained unknown payloads. Authority markers and fixture data were retained. Both new fixtures stopped; collector groups were absent; original-eight identity/lifecycle/limits/ports/mount checks stayed unchanged. Original RustFS network-check scope remains explicitly incomplete.

The successful owner source path requires native child reap and group retirement; no standalone observed benchmark PID/group receipt field was emitted. Both native exact cases passed, stdout was complete and uncapped, stderr empty, and source-before/after equality held. JSON binds receipt/log/binary/helper/source/image identities with full hashes and source path names, without credentials, endpoints, raw IDs, buckets or prefixes.

Limits were unchanged: PID512; loopback listeners; 3GiB initial and 1GiB running shared-VM guest headroom; ten-second request, 120-second phase, 600-second suite, 30-second payload cleanup, 630-second native owner and 800-second total owner with 80-second cleanup reserve. Raw/resource captures stayed below 64MiB and OTLP below 128MiB.

Guest cgroup I/O remains distinct from physical host NVMe IOPS. Bind VirtioFS data is incompletely accounted; a missing boundary/device stays unavailable rather than zero. Named-volume observations are guest block counts across preparation/workload/cleanup, not exact per-PUT amplification. Docker operation counts were unavailable.

This pair is not TiDB, Drive, distributed-cache or full-cluster capacity qualification. One ordered trial per mount cannot establish filesystem causality; host load, caching and throttling remain confounds. The existing public v1 bind/volume report remains valid as an unverified-policy diagnostic and is not overwritten by this separate strict-policy pair.
