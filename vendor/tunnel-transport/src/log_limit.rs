//! A bounded rate limit for refusal log lines an unauthenticated peer can
//! trigger (task row M6-C52, review of `m6-client`).
//!
//! A refusal line is written before the peer has proved anything -- a TLS
//! handshake with no or a self-signed client certificate, a request with no or
//! a junk bearer token -- so without a limit anyone who can reach a public
//! listener decides how fast the relay's `info` log grows.  Admission bounds
//! concurrency, not rate.  This limiter admits at most `burst` lines per
//! `window` for each fixed key (the refusal's stage or label), counts the rest,
//! and hands that count to the next admitted line for the key, which logs it
//! as `suppressed`.  During a sustained flood that is one line per key per
//! window carrying the previous window's suppressed count; when the flood
//! stops, the count is reported by the next refusal of that key.
//!
//! The state is one small entry per key, and keys are `&'static str` labels
//! the caller chooses from a closed set, so the limiter itself is bounded.

use arc_swap::ArcSwap;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

/// Default: at most this many lines per key per window.
pub const DEFAULT_REFUSAL_LOG_BURST: u32 = 20;
/// Default window.
pub const DEFAULT_REFUSAL_LOG_WINDOW: Duration = Duration::from_secs(10);

/// A per-key fixed-window line budget.  See the module documentation.
///
/// The key table is replaced whole the first time a key is seen, which for a
/// closed set of labels is a handful of times in a process.  Each key then
/// has one immutable [`Window`] that a caller replaces with a
/// compare-and-swap, so the window start, the admitted count and the
/// suppressed count always change together: a window never admits more than
/// `burst` lines and no suppressed line is lost or reported twice.
#[derive(Debug)]
pub struct RefusalLogLimiter {
    burst: u32,
    window: Duration,
    keys: ArcSwap<HashMap<&'static str, Arc<ArcSwap<Window>>>>,
}

#[derive(Debug)]
struct Window {
    started: Instant,
    admitted: u32,
    suppressed: u64,
}

impl RefusalLogLimiter {
    /// A limiter admitting `burst` lines per `window` for each key.
    #[must_use]
    pub fn new(burst: u32, window: Duration) -> Self {
        Self {
            burst,
            window,
            keys: ArcSwap::default(),
        }
    }

    /// The limiter with the documented defaults.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_REFUSAL_LOG_BURST, DEFAULT_REFUSAL_LOG_WINDOW)
    }

    /// Whether a line for `key` may be written now.  `Some(n)` admits it and
    /// carries the number of lines for this key suppressed since the last
    /// admitted one, which the caller logs; `None` suppresses it and counts
    /// it.
    pub fn admit(&self, key: &'static str) -> Option<u64> {
        self.admit_at(key, Instant::now())
    }

    /// The window cell of `key`, added to the table on first use.
    fn cell(&self, key: &'static str, now: Instant) -> Arc<ArcSwap<Window>> {
        if let Some(cell) = self.keys.load().get(key) {
            return Arc::clone(cell);
        }
        // A racing caller may add the key first; whoever publishes, both
        // then read the same cell back.
        loop {
            let current = self.keys.load_full();
            if let Some(cell) = current.get(key) {
                return Arc::clone(cell);
            }
            let cell = Arc::new(ArcSwap::from_pointee(Window {
                started: now,
                admitted: 0,
                suppressed: 0,
            }));
            let mut next = HashMap::clone(&current);
            next.insert(key, Arc::clone(&cell));
            let previous = self.keys.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&previous, &current) {
                return cell;
            }
        }
    }

    /// [`RefusalLogLimiter::admit`] against an explicit clock.
    pub fn admit_at(&self, key: &'static str, now: Instant) -> Option<u64> {
        let cell = self.cell(key, now);
        loop {
            let current = cell.load_full();
            let (started, admitted) =
                if now.saturating_duration_since(current.started) >= self.window {
                    (now, 0)
                } else {
                    (current.started, current.admitted)
                };
            let (next, outcome) = if admitted < self.burst {
                (
                    Window {
                        started,
                        admitted: admitted + 1,
                        suppressed: 0,
                    },
                    Some(current.suppressed),
                )
            } else {
                (
                    Window {
                        started,
                        admitted,
                        suppressed: current.suppressed.saturating_add(1),
                    },
                    None,
                )
            };
            let previous = cell.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&previous, &current) {
                return outcome;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flood admits `burst` lines per window, suppresses the rest, and the
    /// first line of the next window reports how many were suppressed.  Keys
    /// are independent, so one flooded stage does not hide another.
    #[test]
    fn a_flood_is_bounded_per_window_and_reports_what_it_suppressed() {
        let limiter = RefusalLogLimiter::new(3, Duration::from_secs(10));
        let start = Instant::now();
        let admitted: Vec<_> = (0..1000)
            .filter_map(|_| limiter.admit_at("token", start))
            .collect();
        assert_eq!(admitted, vec![0, 0, 0], "three lines in the first window");
        assert_eq!(
            limiter.admit_at("bearer", start),
            Some(0),
            "keys are separate"
        );
        let later = start + Duration::from_secs(10);
        assert_eq!(limiter.admit_at("token", later), Some(997));
        assert_eq!(limiter.admit_at("token", later), Some(0));
    }

    /// Many threads refusing at once: a window never admits more than the
    /// burst, and every suppressed line is reported exactly once.
    #[test]
    fn concurrent_refusals_never_exceed_the_burst_and_lose_no_count() {
        const THREADS: u64 = 8;
        const PER_THREAD: u64 = 2_000;
        const BURST: u32 = 20;
        for _ in 0..50 {
            let limiter = RefusalLogLimiter::new(BURST, Duration::from_secs(10));
            let start = Instant::now();
            let barrier = std::sync::Barrier::new(THREADS as usize);
            let (lines, reported): (u64, u64) = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..THREADS)
                    .map(|_| {
                        let (limiter, barrier) = (&limiter, &barrier);
                        scope.spawn(move || {
                            barrier.wait();
                            let (mut lines, mut reported) = (0_u64, 0_u64);
                            for _ in 0..PER_THREAD {
                                if let Some(suppressed) = limiter.admit_at("token", start) {
                                    lines += 1;
                                    reported += suppressed;
                                }
                            }
                            (lines, reported)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().unwrap())
                    .fold((0, 0), |sum, each| (sum.0 + each.0, sum.1 + each.1))
            });
            assert_eq!(lines, u64::from(BURST), "lines in one window");
            assert_eq!(reported, 0, "nothing is reported inside the window");
            // The next window's lines carry everything that was suppressed.
            let later = start + Duration::from_secs(10);
            let barrier = std::sync::Barrier::new(THREADS as usize);
            let (lines, reported): (u64, u64) = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..THREADS)
                    .map(|_| {
                        let (limiter, barrier) = (&limiter, &barrier);
                        scope.spawn(move || {
                            barrier.wait();
                            let (mut lines, mut reported) = (0_u64, 0_u64);
                            for _ in 0..PER_THREAD {
                                if let Some(suppressed) = limiter.admit_at("token", later) {
                                    lines += 1;
                                    reported += suppressed;
                                }
                            }
                            (lines, reported)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().unwrap())
                    .fold((0, 0), |sum, each| (sum.0 + each.0, sum.1 + each.1))
            });
            assert_eq!(lines, u64::from(BURST), "lines in the second window");
            assert_eq!(reported, THREADS * PER_THREAD - u64::from(BURST));
        }
    }
}
