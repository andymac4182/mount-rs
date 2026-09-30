# Retaining resource timestamp failures

The first native attempt reported `resource coverage stale or invalid`, but its
rejected timestamp and validation clock were lost. The later valid resource
snapshot cannot explain that initiating failure. Its cause remains unresolved.

The diagnostic now retains these public scalars in the initiating error:

- `reason`: `zero_timestamp`, `future_timestamp` or `stale_timestamp`.
- Process PID and sample count.
- `observed_unix_ms`, `checked_unix_ms` and `max_age_ms=10000`.

The original rejection predicate is unchanged: zero timestamps, timestamps after
the validation clock, and observations older than 10,000 ms are rejected. No
clock, sampling interval, disk/RSS limit, deadline, retry or workload changed.
The detailed string is built only on rejection. Existing sampler error retention
and worker terminal reporting preserve it; public progress and checkpoint error
categories stay unchanged.

Two deterministic controls first failed against the original generic error. They
then passed with the new diagnostic, checking all three reasons, acceptance at
exactly 10,000 ms age, and preservation of the first error through a later valid
terminal sample. The current native test binary passed 172 controls with six
explicit integration opt-ins ignored. Formatting and strict Clippy passed.

These controls verify diagnostic retention and the unchanged admission boundary.
They do not reproduce or resolve the earlier native failure. No new ten-process
backend benchmark was run for this message-only change; the historical benchmark
still measures `fa70c174`, as its report states. Exact source and gate evidence are
in [resource-diagnostics.json](resource-diagnostics.json).
