# Vendored `quinn-proto` 0.11.18 patch

This directory is a complete copy of the crates.io `quinn-proto` 0.11.18
package, which `quinn` 0.11.12 requires. The version is unchanged. The
upstream license remains `MIT OR Apache-2.0`.

There are two source changes. The first is in
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

## Second change: no `std::sync::Mutex`

The four mutexes outside test code are replaced by `Turn`, a small
compare-and-swap exclusive cell added in `src/turn.rs`: `TokenMemoryCache`,
`BloomTokenLog`, and the qlog stream handle (`QlogStream`, built in
`config/transport.rs`). Each `.lock().unwrap()` becomes `.lock()`, and one
test reads through `get_mut()` without the `unwrap()`. `Cargo.toml` is
unchanged, so no lockfile moves.

These sections are a few map or filter operations taken once per handshake
(or per qlog event when that feature is on), so a contender spins and then
yields instead of parking. `Turn` has no poisoning: where upstream would
panic on a poisoned mutex, a later caller proceeds.

To carry this forward to a new upstream release, copy `src/turn.rs`, add
`mod turn;` to `src/lib.rs`, and repeat the replacements above. The crate's
own tests pass with `--features qlog,bloom` (303 unit, 3 doc).

## Why `TokenMemoryCache` keeps its cell

`TokenMemoryCache` is called once per client handshake (`take`) and once per
NEW_TOKEN frame (`insert`), and by default holds at most 256 names with two
tokens each. Two versions without the cell were built and measured against it
with `token_memory_cache::tests::token_cache_bench` (release build, Apple M1
Pro, three rounds, per-call time at the median). Both passed the existing
tests and a comparison against the cell version on a full cache; both were
slower, so the cell stays.

Each replaced the single map with a table of buckets, each bucket an immutable
value swapped by compare-and-swap, so a call copied one bucket and not the
whole cache.

| Call, one caller | `Turn` cell | Buckets in `arc-swap` | Buckets under `crossbeam-epoch` |
|---|---|---|---|
| `insert`, name present | 92 to 102 ns | 203 to 285 ns | 117 to 130 ns |
| `insert`, new name, cache full | 166 to 174 ns | 677 to 686 ns | 414 to 434 ns |
| `take`, token present | 54 to 63 ns | 247 to 255 ns | 59 to 61 ns |

With eight callers on one cache the cell version completed 9.5 to 12.2 million
`insert` calls a second against 5.4 to 7.4 million for the epoch version, and
3.2 to 4.1 million evicting inserts against 1.0 to 1.4 million; `take` was
level (5.7 to 9.6 million against 8.7 to 10.0 million).

The cost is structural. The cell version already pays one allocation and one
hash per insert; a copy-on-write bucket adds a pin or a guard, a deferred
free, and the bookkeeping that finds the least recently used name without a
recency list.
