# Fresh RustFS bind and named-volume diagnostic controls

One ordered trial per mount, prefix 1 and 4KiB objects, same archived native executable and 16 source files. All 18 phase cells were positive, uncapped and settled; exact payload cleanup completed. Both new fixtures stopped; collector groups were absent. All original-eight identity, lifecycle, limits, mounts and port checks remained unchanged. The original RustFS network check retained its declared incomplete scope.

Actual bucket policy and strict crash persistence were not verified. These are diagnostic controls, not a 10-server/10,000-drive capacity result.

## Logical operations per second

| Pattern | C | Bind | Volume | Volume / bind |
| --- | ---: | ---: | ---: | ---: |
| raw_get | 1 | 1970.545 | 1807.483 | 0.917 |
| raw_unique_put | 1 | 87.476 | 313.477 | 3.584 |
| configured_authority_verify | 1 | 984.249 | 1011.834 | 1.028 |
| raw_get | 10 | 11303.485 | 10048.849 | 0.889 |
| raw_unique_put | 10 | 270.343 | 579.870 | 2.145 |
| configured_authority_verify | 10 | 6309.009 | 5557.230 | 0.881 |
| raw_get | 100 | 17444.473 | 17250.352 | 0.989 |
| raw_unique_put | 100 | 337.088 | 542.155 | 1.608 |
| configured_authority_verify | 100 | 8897.942 | 11606.255 | 1.304 |

Raw GET/PUT each issued one observed HTTP dispatch; configured authority verification issued two GETs per logical operation. Every timed dispatch returned 2xx. No failed/uncertain operations, harness retries, SDK retries or capacity caps were recorded.

## PUT wall-time means

| Mount | C | Logical ms | SDK put ms | Headers ms | Summed headers / SDK wall |
| --- | ---: | ---: | ---: | ---: | ---: |
| bind | 1 | 11.430875 | 11.398641 | 11.353096 | 99.600441% |
| bind | 10 | 36.921227 | 36.885511 | 36.847379 | 99.896621% |
| bind | 100 | 291.504911 | 291.470167 | 291.435218 | 99.988009% |
| volume | 1 | 3.189369 | 3.151596 | 3.113324 | 98.785612% |
| volume | 10 | 17.236741 | 17.200266 | 17.165898 | 99.800188% |
| volume | 100 | 182.738461 | 182.708504 | 182.678455 | 99.983553% |

Dispatch-to-response-headers accounts for almost all measured SDK PUT wall duration. It includes async waiting, scheduling, network and service work; it does not isolate CPU or disk. Successful empty PUT responses are dropped without polling, so `body_dropped = operations` is expected coverage and does not indicate failure. Observed response lifetime is not a drained-body timing.

## Cumulative server-stage differences

For each series independently, subtract its latest export at/before the guest-cgroup before stamp from its final export after benchmark completion. The target resource and scope stayed fixed, starts were stable, counts/sums/buckets did not reset, and histogram count equals the sum of buckets. Repeated cumulative exports were not summed. Individual windows are retained in JSON.

| Mount | Stage | Count delta | Sum ms | Mean ms |
| --- | --- | ---: | ---: | ---: |
| bind | store_commit | 3823 | 551718.943 | 144.315706 |
| bind | app_store_put | 3823 | 551588.816 | 144.281668 |
| bind | set_disk_rename | 4983 | 274631.182 | 55.113623 |
| bind | set_disk_rename_quorum_wait | 4983 | 207584.713 | 41.658582 |
| bind | set_disk_rename_disk_wait | 4983 | 206958.110 | 41.532834 |
| bind | set_disk_rename_rename_syscall | 26417 | 79903.653 | 3.024706 |
| bind | put_object_commit_namespace_lock_wait | 4984 | 3378.021 | 0.677773 |
| bind | set_disk_rename_dst_dir_fsync | 1161 | 1645.320 | 1.417158 |
| bind | set_disk_rename_backup_dir_fsync | 1150 | 1625.057 | 1.413093 |
| volume | store_commit | 7535 | 531953.946 | 70.597737 |
| volume | app_store_put | 7535 | 531801.808 | 70.577546 |
| volume | set_disk_rename | 9993 | 272441.664 | 27.263251 |
| volume | set_disk_rename_quorum_wait | 9993 | 207433.496 | 20.757880 |
| volume | set_disk_rename_disk_wait | 9993 | 206730.766 | 20.687558 |
| volume | set_disk_rename_rename_syscall | 52578 | 8218.333 | 0.156307 |
| volume | set_disk_rename_dst_dir_fsync | 2459 | 4745.137 | 1.929702 |
| volume | set_disk_rename_backup_dir_fsync | 2453 | 3936.648 | 1.604830 |
| volume | put_object_commit_namespace_lock_wait | 9994 | 2750.512 | 0.275216 |

Store commit includes application/store work; rename, quorum wait and disk-future lifetimes overlap, including scheduler and async waits. Summing them cannot produce an exclusive CPU/disk breakdown. Fixed rename labels repeat within commits and include internal writes. Namespace-lock waits here do not establish TiDB contention.

| Mount | Rename syscall / rename observations | Rename / app-store observations |
| --- | ---: | ---: |
| bind | 5.301425 | 1.303427 |
| volume | 5.261483 | 1.326211 |

These are inclusive same-window observation-count ratios, not per-object physical I/O amplification. The fixed stages cover repeated calls and internal/system work.

The committed PUT counter difference matches timed PUTs + 256 seeds + three successful authority preparation writes: bind 3,822; volume 7,534. The initial authority HTTP bank contains four PUT attempts, three 2xx and one expected losing conditional Create (4xx); its winner role varies. App/store histograms include that losing attempt (3,823 / 7,535). Exact payload cleanup deleted 3,819 / 7,531 acknowledged blocks; authority markers remained.

Three stage baselines were absent (`set_disk_old_data_cleanup`, `set_disk_rename_file_fdatasync`, `set_disk_rename_src_dir_fsync`). Their differences are unavailable, never zero. JSON retains all 32 final stages, 29 valid differences, all bounds/buckets, and cumulative min/max endpoints without subtracting maxima.

## Whole sampled resources

| Mount | Stats window s | CPU s | Average cores | Aggregate throttle s | Throttled periods | Peak cgroup bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| bind | 73.823746 | 62.473486 | 0.846252 | 0.231083 | 16 | 230150144 |
| volume | 70.006294 | 83.271113 | 1.189480 | 50.218014 | 207 | 395747328 |

Both containers were capped at two CPUs and 2GiB memory. Volume recorded 50.218 aggregate throttle seconds / 207 throttled periods versus bind 0.231 seconds / 16. Aggregate throttled time can overlap threads and is not exclusive wall time. Cgroup memory includes anonymous memory and page cache and is not process RSS. The JSON retains network deltas, anon/file peaks, guest headroom and sample endpoints. Whole resource windows include startup, preparation, workload, cleanup and server background.

### Sample windows inside each phase enclosure

| Mount | Pattern | C | Sample window s | Coverage of enclosure | Average cores | Aggregate throttle s | Peak sampled cgroup bytes |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| bind | raw_get | 1 | 4.131 | 82.6% | 0.305 | 0.000 | 82485248 |
| bind | raw_unique_put | 1 | 2.077 | 41.5% | 0.500 | 0.000 | 86949888 |
| bind | configured_authority_verify | 1 | 2.058 | 41.2% | 0.284 | 0.000 | 95866880 |
| bind | raw_get | 10 | 2.076 | 41.5% | 1.339 | 0.000 | 105578496 |
| bind | raw_unique_put | 10 | 2.070 | 41.2% | 0.807 | 0.000 | 118538240 |
| bind | configured_authority_verify | 10 | 2.097 | 41.9% | 1.364 | 0.000 | 121352192 |
| bind | raw_get | 100 | 2.081 | 41.6% | 1.850 | 0.000 | 145166336 |
| bind | raw_unique_put | 100 | 2.075 | 39.6% | 0.910 | 0.000 | 195104768 |
| bind | configured_authority_verify | 100 | 2.085 | 41.6% | 1.942 | 0.020 | 218517504 |
| volume | raw_get | 1 | 4.130 | 82.6% | 0.271 | 0.000 | 43626496 |
| volume | raw_unique_put | 1 | 2.075 | 41.5% | 1.135 | 0.000 | 68546560 |
| volume | configured_authority_verify | 1 | 2.071 | 41.4% | 0.244 | 0.000 | 76521472 |
| volume | raw_get | 10 | 2.079 | 41.6% | 1.358 | 0.000 | 87547904 |
| volume | raw_unique_put | 10 | 2.115 | 42.2% | 2.012 | 4.534 | 145620992 |
| volume | configured_authority_verify | 10 | 2.091 | 41.8% | 1.380 | 0.000 | 156815360 |
| volume | raw_get | 100 | 2.102 | 42.0% | 1.840 | 0.001 | 180682752 |
| volume | raw_unique_put | 100 | 2.087 | 40.4% | 1.997 | 8.775 | 256466944 |
| volume | configured_authority_verify | 100 | 2.087 | 41.7% | 1.960 | 0.001 | 279838720 |

Volume PUT interior samples at C10/C100 show substantial throttling and roughly two cores of CPU use, supporting a CPU-limit hypothesis. Their coverage is partial. Sampling cadence is ~2s; host post-read lag is 49–175ms. Guest/host UTC alignment was not calibrated. Phase startup precedes release; phase-end follows settlement/snapshot work. Wall enclosure and monotonic elapsed differ by -0.295ms to +0.126ms.

Outer sample brackets retained in JSON cover more of each phase but include adjacent work and can overlap. Memory maxima are only observed sample maxima. No interpolation or exact per-request resource attribution is claimed. Sample indices and exact counter/host wall/host monotonic endpoints are retained.

## Guest cgroup block I/O

| Mount | Window s | Read ops | Write ops | Read bytes | Write bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| bind | 64.594974 | 0 | 1 | 0 | 8192 |
| volume | 58.792979 | 786 | 18792 | 5746688 | 80711680 |

These device 254:0 counters are guest-cgroup observations spanning preparation, timed reads/writes, payload cleanup and background. Bind data travels through VirtioFS and is incompletely covered by guest block accounting. Named-volume counts remain guest observations, not host physical NVMe IOPS. Docker operation counts were unavailable. No per-PUT I/O-amplification factor is derived.

## Source, qualification and limits

The JSON binds both actual receipt hashes, complete uncapped raw files, archived executable, all 16 source files, image, owner source, original-eight health receipts and primary tagged stage sources. It omits credentials, endpoints, raw labels, buckets/prefixes, container IDs and authority identifiers. The successful owner process path requires child reap and group retirement, but no separate observed benchmark PID/group receipt field was emitted. Data was retained and new fixtures stopped.

Limits stayed at two CPUs, 2GiB memory+swap, PID512, loopback listeners; 3GiB initial and 1GiB running guest headroom; five-second admission, ten-second request, 120-second phase, 600-second suite, 30-second payload cleanup, 630-second native owner, and 800-second overall owner with an 80-second cleanup reserve. Raw/resource files were below 64MiB; OTLP below 128MiB.

Primary tagged RustFS source defines a relaxed new-bucket default and no per-object fsync for relaxed inline writes; internal/system paths can retain sync. This is source context, not live policy proof. Both trials are diagnostic until separate actual strict-policy controls corroborate durability settings. Bind ran first then volume, with separate fresh stores and only one trial each; host load, ordering, caching and CPU throttle confound causal attribution. Repeated matched controls are needed before a filesystem-cause or durable-capacity conclusion.

Exact values and individual windows are in `derived.json`; rates use integer counts and monotonic nanoseconds, while exported floating sums are parsed as Decimal before differences. This report describes these two completed controls only.
