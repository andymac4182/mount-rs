# Vendored `quinn-proto` 0.11.18 patch

This directory is a complete copy of the crates.io `quinn-proto` 0.11.18
package, which `quinn` 0.11.12 requires. The version is unchanged. The
upstream license remains `MIT OR Apache-2.0`.

The only source change is in
`src/connection/streams/state.rs::StreamsState::queue_max_stream_id`: the
announcement threshold changes from `diff > max_concurrent / 8` to `diff > 0`.
It carries forward the patch previously applied to 0.11.17 in the
agentuplink revision `626ba9365b5d3e1faf8265f0b6ff50cc9111b749`; upstream
0.11.18 still has the old threshold.

When a peer has consumed its previously announced stream IDs, a completed
stream can therefore advertise one newly available ID immediately. The old
hysteresis is unsafe for the tunnel's fixed stream ceiling: seven completions
at a configured limit of 128 leave `diff == 7`, below the old threshold of 16,
so the next H3 open can remain blocked indefinitely.

The tradeoff is more frequent `MAX_STREAMS` control frames when streams are
short-lived. The patch does not raise a QUIC or application cap, add a
connection, or change a timeout.
