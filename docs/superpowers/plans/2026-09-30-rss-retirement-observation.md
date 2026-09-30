# Bounded RSS observation during owned server retirement

## Scope

The ten-process cache qualification monitor observed a Darwin `proc_pidinfo`
ESRCH result shortly before its retained server `Child` was actually reaped.
The original failed run and its diagnostic remain failed evidence. This change
allows one narrowly proved shutdown observation gap to settle inside the existing
limits. It does not change server storage, transport or benchmark throughput.

## Acceptance

An unavailable sample can enter pending retirement only for the exact canonical
server role, node, PID and generation retained by the monitor. It requires a
successful owned SIGINT, the first recorded stop budget, no forced exit, and typed
Darwin native facts: zero returned bytes, ESRCH, the actual taskinfo structure
size, and ordered known clocks inside the current acquisition after SIGINT.
Linux cannot qualify this Darwin exception.

Authority comes from the committed resource packet with its exact published
sequence, capture start and finish. Its complete supervisor and live child
identity set must match the retained roster; only exact actually reaped retained
identities may be removed. An unpublished candidate is insufficient.

Pending retirement retains the original shared 100 ms capture interval, committed
capture freshness, owner deadline and force point, outer deadline and any earlier
resource stop. Common-clock and `Instant` bounds can only tighten. Supervisors and
other live children are sampled on every pass, with the existing individual and
known aggregate caps. Missing RSS never becomes zero, a receipt sample or a
published incomplete frame. Success requires actual successful unforced reaping
of every pending child followed by a fresh complete numeric frame inside the
original bounds.

## Failure and cleanup

The proof and quarantine remain attached to the actual retained `OwnedProcess`,
including its canonical identity and original bounds. A fatal return seals its
disposition and optional settlement clock once. Frame, direct and legacy callers
cannot reread that child's RSS or create a replacement pending allowance.

Sealed cleanup polls the retained child and samples surviving supervisors and
other children in one pass under the original cleanup bounds, without waiting on
another missing sample. Later actual reaping can produce an ordinary complete
error-bearing cleanup frame. The first failure, diagnostic and sealed settlement
remain unchanged. Failed or forced sibling exits remain failures; genuine numeric
cap-crossing samples still update their owned receipts. The existing owner remains
responsible for signals, actual reaping, attestation, sockets and receipt closure.

## Verification

Tests first established actual assertion failures against an inert resolver.
Independent review then identified five additional regressions in an unqualified
implementation: cleanup reentry, capture renewal, incomplete roster authority,
stale native error clocks and failed sibling omission. All five failed actual
assertions before correction; the original 22 controls continued to pass.

The corrected monitor passed all **122 ordinary tests**, with the existing four
native entry points ignored. Fixtures use real retained children and real exit
statuses with controlled policy clocks. The inventory consumer passed **163
tests** and requires all 27 pending-retirement names: 66 required controls on Linux
and 67 on Darwin. Its original 180-second command and native selection remain.

These are ordinary state-machine and inventory results. Native TiDB/filesystem
or cache qualification, Linux performance, power-loss recovery, full production
scale and CI completion require their own evidence. The published filesystem
benchmark retains its original source and executable bindings.
