# Automatic fallback with a lost committed write reply

This fixture composes automatic transport selection with the existing signed
OIDC, authorization and durable SQLite reply-loss check. The [report](report.json)
joins the final owned gates, their raw-log hashes and unchanged source pins.

## What the fixture observes

1. A bound loopback UDP socket receives real QUIC client probes and sends no
   responses. The credential file is absent when the first probe is observed.
2. The TLS WebSocket relay records TCP acceptance at least three seconds after
   selection starts. It validates the upgrade request and creates the private
   credential file before returning the upgrade response.
3. The selected session authenticates with the actual catalog authenticator,
   negotiates protocol 2, denies another partition and denies writes to a
   read-only drive. Separate direct connections also reject an unauthorized
   partition and an untrusted TLS root.
4. Initial QUIC datagrams are counted and settled through a bounded 100 ms quiet
   interval. A contact-first UDP observer stays active across all subsequent
   filesystem work and the post-loss window.
5. The actual service receives exactly one binary write of 65,673 bytes. The
   relay receives its complete successful reply and terminator, holds both
   sockets open, and waits for an explicit release.
6. Before release or any service/write-handle close, a fresh SQLite reader checks
   metadata, permissions, every byte and exact EOF. It drops its read-only
   observer without close or sync, which could otherwise persist a snapshot.
7. The relay suppresses the whole reply and disconnects. The caller remains
   uncertain. Later control, binary write and binary read calls fail closed;
   the failed read leaves its buffer unchanged.
8. Contact takes precedence over simultaneous completion. A terminal nonblocking
   UDP check and a 100 ms TCP/UDP observation window reject reconnect or transport
   switching. After observed service-close completion and original-owner drop,
   another fresh filesystem repeats metadata, full-byte and EOF verification.

The existing explicit WebSocket arm runs the same strengthened checks. Its
initial QUIC count and deferred credential issuance count remain zero. The Auto
arm requires positive initial probes and exactly one deferred issuance. Issuance
does not independently count credential reads; the closed client guards provide
the source evidence that failed followups stop before credential/network work.

## Qualification and retained negatives

The initial Auto case failed against the existing explicit WebSocket fixture
because it observed no initial QUIC attempt. A subsequent draft passed, but
review found that unbiased terminal selects could hide queued network contact
behind simultaneous completion. Final source gives contact priority and adds
a real ready-datagram control exercising the same observer. The preliminary
pass is retained separately and is not a final qualification gate.

The parent requires the exact named case and exactly one complete LF-framed
schema-2 fault receipt. Closed field/type/value checks reject missing or
duplicate records, private/unknown keys, contradictions, malformed JSON,
truncation and a zero-case test run. Pure controls verify this result gating;
they are modeled evidence, distinct from the actual TLS/UDP/SQLite executions.

Run serially from the checkout with an isolated target:

```sh
CARGO_TARGET_DIR=/absolute/isolated-target \
  python3 -B scripts/test-remote-failures.py wsautoloss
CARGO_TARGET_DIR=/absolute/isolated-target \
  python3 -B scripts/test-remote-failures.py wsloss
```

CI uses these same owned-parent selectors. Each fixture retains its private
directory throughout success, failure and cancellation. Removal follows the
parent's observed process reap, process-group absence and output EOF.

## Scope

Both arms use the SDK snapshot SQLite filesystem. Compact MRC5 selection has
separate signed CLI coverage; this artifact does not qualify Auto plus MRC5
reply loss. Initial-attempt counts are separate from the zero-contact scope
after settlement through the bounded post-loss window. TCP acceptance timing is
elapsed wall time from selection entry, not exclusive QUIC or TLS time.

This is local loopback and SQLite commit/reopen evidence. It does not establish
cross-host behavior, future absence of traffic beyond the observed window,
process-crash or power-loss durability, other backing providers, throughput or
the full 10-server / 10,000-client / 10,000-Drive / 5,000-Partition / 10M-file
production capacity. Hosted CI and the PR stack still require qualification
before merge; the full goal remains active.
