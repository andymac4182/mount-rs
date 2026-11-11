//! Storing tokens sent from servers in NEW_TOKEN frames and using them in subsequent connections

use std::{
    collections::{HashMap, VecDeque, hash_map},
    sync::Arc,
};

use bytes::Bytes;
use lru_slab::LruSlab;
use tracing::trace;

use crate::{token::TokenStore, turn::Turn};

/// `TokenStore` implementation that stores up to `N` tokens per server name for up to a
/// limited number of server names, in-memory
#[derive(Debug)]
pub struct TokenMemoryCache(Turn<State>);

impl TokenMemoryCache {
    /// Construct empty
    pub fn new(max_server_names: u32, max_tokens_per_server: usize) -> Self {
        Self(Turn::new(State::new(
            max_server_names,
            max_tokens_per_server,
        )))
    }
}

impl TokenStore for TokenMemoryCache {
    fn insert(&self, server_name: &str, token: Bytes) {
        trace!(%server_name, "storing token");
        self.0.lock().store(server_name, token)
    }

    fn take(&self, server_name: &str) -> Option<Bytes> {
        let token = self.0.lock().take(server_name);
        trace!(%server_name, found=%token.is_some(), "taking token");
        token
    }
}

/// Defaults to a maximum of 256 servers and 2 tokens per server
impl Default for TokenMemoryCache {
    fn default() -> Self {
        Self::new(256, 2)
    }
}

/// Lockable inner state of `TokenMemoryCache`
#[derive(Debug)]
struct State {
    max_server_names: u32,
    max_tokens_per_server: usize,
    // map from server name to index in lru
    lookup: HashMap<Arc<str>, u32>,
    lru: LruSlab<CacheEntry>,
}

impl State {
    fn new(max_server_names: u32, max_tokens_per_server: usize) -> Self {
        Self {
            max_server_names,
            max_tokens_per_server,
            lookup: HashMap::new(),
            lru: LruSlab::default(),
        }
    }

    fn store(&mut self, server_name: &str, token: Bytes) {
        if self.max_server_names == 0 {
            // the rest of this method assumes that we can always insert a new entry so long as
            // we're willing to evict a pre-existing entry. thus, an entry limit of 0 is an edge
            // case we must short-circuit on now.
            return;
        }
        if self.max_tokens_per_server == 0 {
            // similarly to above, the rest of this method assumes that we can always push a new
            // token to a queue so long as we're willing to evict a pre-existing token, so we
            // short-circuit on the edge case of a token limit of 0.
            return;
        }

        let server_name = Arc::<str>::from(server_name);
        match self.lookup.entry(server_name.clone()) {
            hash_map::Entry::Occupied(hmap_entry) => {
                // key already exists, push the new token to its token queue
                let tokens = &mut self.lru.get_mut(*hmap_entry.get()).tokens;
                if tokens.len() >= self.max_tokens_per_server {
                    debug_assert!(tokens.len() == self.max_tokens_per_server);
                    tokens.pop_front().unwrap();
                }
                tokens.push_back(token);
            }
            hash_map::Entry::Vacant(hmap_entry) => {
                // key does not yet exist, create a new one, evicting the oldest if necessary
                let removed_key = if self.lru.len() >= self.max_server_names {
                    // unwrap safety: max_server_names is > 0, so there's at least one entry, so
                    //                lru() is some
                    Some(self.lru.remove(self.lru.lru().unwrap()).server_name)
                } else {
                    None
                };

                hmap_entry.insert(self.lru.insert(CacheEntry::new(server_name, token)));

                // for borrowing reasons, we must defer removing the evicted hmap entry to here
                if let Some(removed_slot) = removed_key {
                    let removed = self.lookup.remove(&removed_slot);
                    debug_assert!(removed.is_some());
                }
            }
        };
    }

    fn take(&mut self, server_name: &str) -> Option<Bytes> {
        let slab_key = *self.lookup.get(server_name)?;

        // pop from entry's token queue
        let entry = self.lru.get_mut(slab_key);
        // unwrap safety: we never leave tokens empty
        let token = entry.tokens.pop_front().unwrap();

        if entry.tokens.is_empty() {
            // token stack emptied, remove entry
            self.lru.remove(slab_key);
            self.lookup.remove(server_name);
        }

        Some(token)
    }
}

/// Cache entry within `TokenMemoryCache`'s LRU slab
#[derive(Debug)]
struct CacheEntry {
    server_name: Arc<str>,
    // invariant: tokens is never empty
    tokens: VecDeque<Bytes>,
}

impl CacheEntry {
    /// Construct with a single token
    fn new(server_name: Arc<str>, token: Bytes) -> Self {
        let mut tokens = VecDeque::new();
        tokens.push_back(token);
        Self {
            server_name,
            tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use rand::prelude::*;
    use rand_pcg::Pcg32;

    fn new_rng() -> impl Rng {
        Pcg32::new(0xdeadbeefdeadbeef, 0xdeadbeefdeadbeef)
    }

    #[test]
    fn cache_test() {
        let mut rng = new_rng();
        const N: usize = 2;

        for _ in 0..10 {
            let mut cache_1: Vec<(u32, VecDeque<Bytes>)> = Vec::new(); // keep it sorted oldest to newest
            let cache_2 = TokenMemoryCache::new(20, 2);

            for i in 0..200 {
                let server_name = rng.random::<u32>() % 10;
                if rng.random_bool(0.666) {
                    // store
                    let token = Bytes::from(vec![i]);
                    println!("STORE {server_name} {token:?}");
                    if let Some((j, _)) = cache_1
                        .iter()
                        .enumerate()
                        .find(|&(_, &(server_name_2, _))| server_name_2 == server_name)
                    {
                        let (_, mut queue) = cache_1.remove(j);
                        queue.push_back(token.clone());
                        if queue.len() > N {
                            queue.pop_front();
                        }
                        cache_1.push((server_name, queue));
                    } else {
                        let mut queue = VecDeque::new();
                        queue.push_back(token.clone());
                        cache_1.push((server_name, queue));
                        if cache_1.len() > 20 {
                            cache_1.remove(0);
                        }
                    }
                    cache_2.insert(&server_name.to_string(), token);
                } else {
                    // take
                    println!("TAKE {server_name}");
                    let expecting = cache_1
                        .iter()
                        .enumerate()
                        .find(|&(_, &(server_name_2, _))| server_name_2 == server_name)
                        .map(|(j, _)| j)
                        .map(|j| {
                            let (_, mut queue) = cache_1.remove(j);
                            let token = queue.pop_front().unwrap();
                            if !queue.is_empty() {
                                cache_1.push((server_name, queue));
                            }
                            token
                        });
                    println!("EXPECTING {expecting:?}");
                    assert_eq!(cache_2.take(&server_name.to_string()), expecting);
                }
            }
        }
    }

    #[test]
    fn zero_max_server_names() {
        // test that this edge case doesn't panic
        let cache = TokenMemoryCache::new(0, 2);
        for i in 0..10 {
            cache.insert(&i.to_string(), Bytes::from(vec![i]));
            for j in 0..10 {
                assert!(cache.take(&j.to_string()).is_none());
            }
        }
    }

    #[test]
    fn zero_queue_length() {
        // test that this edge case doesn't panic
        let cache = TokenMemoryCache::new(256, 0);
        for i in 0..10 {
            cache.insert(&i.to_string(), Bytes::from(vec![i]));
            for j in 0..10 {
                assert!(cache.take(&j.to_string()).is_none());
            }
        }
    }

    /// Timed operations per batch. One clock reading pair covers a whole
    /// batch because a single call is shorter than the clock's tick.
    const BATCH: usize = 64;

    #[derive(Clone, Copy, Debug)]
    enum BenchOp {
        /// Store a token under a name the cache already holds.
        Insert,
        /// Store a token under a new name while the cache is full.
        InsertEvict,
        /// Take a token that is present.
        Take,
    }

    fn bench_worker(
        store: &dyn TokenStore,
        op: BenchOp,
        thread: usize,
        batches: usize,
    ) -> (std::time::Duration, Vec<u64>) {
        use std::time::Instant;

        let token = Bytes::from_static(&[7u8; 48]);
        let shared: Vec<String> = (0..16).map(|i| format!("shared-{i}.example.com")).collect();
        let own: Vec<String> = (0..32)
            .map(|i| format!("own-{thread}-{i}.example.com"))
            .collect();
        let mut fresh: Vec<String> = Vec::with_capacity(BATCH);
        let mut spent = std::time::Duration::ZERO;
        let mut per_op = Vec::with_capacity(batches);
        for batch in 0..batches {
            let elapsed = match op {
                BenchOp::Insert => {
                    let start = Instant::now();
                    for i in 0..BATCH {
                        store.insert(&shared[(i + thread) % shared.len()], token.clone());
                    }
                    start.elapsed()
                }
                BenchOp::InsertEvict => {
                    fresh.clear();
                    fresh.extend(
                        (0..BATCH).map(|i| format!("fresh-{thread}-{batch}-{i}.example.com")),
                    );
                    let start = Instant::now();
                    for name in &fresh {
                        store.insert(name, token.clone());
                    }
                    start.elapsed()
                }
                BenchOp::Take => {
                    for name in &own {
                        store.insert(name, token.clone());
                        store.insert(name, token.clone());
                    }
                    let mut found = 0;
                    let start = Instant::now();
                    for i in 0..BATCH {
                        found += usize::from(store.take(&own[i % own.len()]).is_some());
                    }
                    let elapsed = start.elapsed();
                    assert_eq!(found, BATCH);
                    elapsed
                }
            };
            spent += elapsed;
            per_op.push(elapsed.as_nanos() as u64 / BATCH as u64);
        }
        (spent, per_op)
    }

    fn bench_case(label: &str, store: Arc<dyn TokenStore>, op: BenchOp, threads: usize) {
        const BATCHES: usize = 20_000;
        if matches!(op, BenchOp::InsertEvict) {
            for i in 0..256 {
                store.insert(&format!("prefill-{i}.example.com"), Bytes::from_static(b"p"));
            }
        }
        let barrier = Arc::new(std::sync::Barrier::new(threads));
        let workers: Vec<_> = (0..threads)
            .map(|thread| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    bench_worker(&*store, op, thread, BATCHES)
                })
            })
            .collect();
        let mut ops_per_sec = 0.0;
        let mut per_op = Vec::new();
        for worker in workers {
            let (spent, samples) = worker.join().unwrap();
            ops_per_sec += (BATCHES * BATCH) as f64 / spent.as_secs_f64();
            per_op.extend(samples);
        }
        per_op.sort_unstable();
        let p50 = per_op[per_op.len() / 2];
        let p99 = per_op[per_op.len() * 99 / 100];
        println!(
            "token_cache_bench impl={label} op={op:?} threads={threads} ops_per_sec={ops_per_sec:.0} p50_ns={p50} p99_ns={p99}"
        );
    }

    /// Throughput and per-call time of `insert` and `take`, for one caller
    /// and for eight sharing one cache. Times are per call, averaged over a
    /// batch of 64.
    ///
    /// This is the measurement any replacement for the `Turn` cell has to
    /// match; see "Why `TokenMemoryCache` keeps its cell" in
    /// `UPSTREAM_PATCH.md` for the two lock-free versions that did not.
    ///
    /// The crate is not a workspace member, so run it from a copy that has an
    /// empty `[workspace]` table appended to `Cargo.toml`:
    /// `cargo test --release --lib token_cache_bench -- --ignored --nocapture`.
    #[test]
    #[ignore = "benchmark; run in release with --nocapture"]
    fn token_cache_bench() {
        for round in 0..3 {
            println!("round {round}");
            for op in [BenchOp::Insert, BenchOp::InsertEvict, BenchOp::Take] {
                for threads in [1, 8] {
                    bench_case("turn", Arc::new(TokenMemoryCache::default()), op, threads);
                }
            }
        }
    }
}
