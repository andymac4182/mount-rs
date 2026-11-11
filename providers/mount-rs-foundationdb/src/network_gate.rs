//! The lifecycle of a native client network that may be started once and
//! stopped once per process, kept without a lock.
//!
//! FoundationDB allows one network start per process and requires the
//! network thread to be stopped and joined before the process exits. The
//! rules that follow from that are independent of the native client, so they
//! live here, generic over the network value and over the start and stop
//! actions, and are tested with fake ones:
//!
//! - the start action runs at most once, however many callers arrive
//!   together, and is never retried after it fails or panics;
//! - every caller that gets the network holds a [`Lease`], and the network is
//!   not stopped while a lease is alive;
//! - once a stop has been accepted no new lease is given out;
//! - the stop action runs at most once, and every caller of
//!   [`NetworkGate::shutdown`] that returns `Ok` returns after it finished.
//!
//! The whole state is one counter and three write-once cells. The counter
//! holds twice the number of live leases, plus one once the gate is closed.
//! Taking a lease adds two unless the gate is closed; closing is a single
//! compare-and-swap from zero to one, so it succeeds only at an instant with
//! no lease, and a lease is taken before the start action runs, so a start in
//! flight also keeps the gate open.

use std::marker::PhantomData;
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

/// Set in the counter once a stop has been accepted. Never cleared.
const CLOSED: usize = 1;
/// One live lease in the counter.
const LEASE: usize = 2;

/// Why [`NetworkGate::acquire`] did not return a lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AcquireError {
    /// A stop was accepted. The network cannot be started again.
    Closed,
    /// The one start reported this failure. It is not retried.
    BootFailed(String),
    /// The one start panicked. It is not retried.
    BootDidNotComplete,
}

/// [`NetworkGate::shutdown`] found a live lease, or a start in flight, and
/// left the network running. The caller may drain and try again.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Busy;

pub(crate) struct NetworkGate<N> {
    /// `LEASE` times the live leases, plus `CLOSED` once closed.
    leases: AtomicUsize,
    /// Set by the first caller to reach the start action, before it runs.
    /// `boot` is left unset when its initializer panics and would run a later
    /// caller's initializer; this flag is what makes that one fail instead of
    /// starting the network a second time.
    boot_attempted: AtomicBool,
    /// Outcome of the one start. Callers that arrive during the start wait
    /// here for it.
    boot: OnceLock<Result<(), AcquireError>>,
    /// The started network, owned by the gate: set once by the start, taken
    /// once by the stop. Null before the start and after the stop.
    network: AtomicPtr<N>,
    /// Set when the one stop has finished. Callers that arrive during the
    /// stop wait here for it.
    stopped: OnceLock<()>,
    /// The gate owns an `N` and hands it to another thread to stop.
    _network: PhantomData<N>,
}

impl<N> NetworkGate<N> {
    pub(crate) const fn new() -> Self {
        Self {
            leases: AtomicUsize::new(0),
            boot_attempted: AtomicBool::new(false),
            boot: OnceLock::new(),
            network: AtomicPtr::new(ptr::null_mut()),
            stopped: OnceLock::new(),
            _network: PhantomData,
        }
    }

    /// Take a lease on the network, starting it with `boot` if no caller has
    /// tried to yet.
    ///
    /// `boot` runs at most once per gate, in the first caller to get here;
    /// callers that arrive while it runs wait for its outcome and share it.
    /// If it returns an error or panics, this and every later call fails
    /// without running `boot` again.
    pub(crate) fn acquire(
        &self,
        boot: impl FnOnce() -> Result<N, String>,
    ) -> Result<Lease<'_, N>, AcquireError> {
        let mut seen = self.leases.load(Ordering::SeqCst);
        loop {
            if seen & CLOSED != 0 {
                return Err(AcquireError::Closed);
            }
            // A start that already failed is reported without taking a lease,
            // so these callers never make a concurrent shutdown busy.
            if let Some(Err(failure)) = self.boot.get() {
                return Err(failure.clone());
            }
            match self.leases.compare_exchange_weak(
                seen,
                seen + LEASE,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(now) => seen = now,
            }
        }
        // Held from here on, also while `boot` runs and if it unwinds, so the
        // gate cannot close under a start in flight.
        let lease = Lease { gate: self };
        let outcome = self.boot.get_or_init(|| {
            if self.boot_attempted.swap(true, Ordering::SeqCst) {
                return Err(AcquireError::BootDidNotComplete);
            }
            let network = boot().map_err(AcquireError::BootFailed)?;
            self.network
                .store(Box::into_raw(Box::new(network)), Ordering::SeqCst);
            Ok(())
        });
        match outcome {
            Ok(()) => Ok(lease),
            Err(failure) => Err(failure.clone()),
        }
    }

    /// Close the gate and stop the network with `stop`, unless a lease is
    /// alive or a start is in flight, in which case nothing changes and the
    /// result is [`Busy`].
    ///
    /// `stop` runs at most once per gate, and only if the network was
    /// started. Every call that returns `Ok` returns after the stop finished,
    /// whichever caller ran it. `stop` must not unwind: the network it was
    /// given would be dropped as a plain value and later callers would be
    /// told the stop finished.
    pub(crate) fn shutdown(&self, stop: impl FnOnce(N)) -> Result<(), Busy> {
        match self
            .leases
            .compare_exchange(0, CLOSED, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => {}
            Err(seen) if seen & CLOSED != 0 => {}
            Err(_) => return Err(Busy),
        }
        // The gate is closed and was closed with no lease alive. A start
        // holds a lease while it runs, so none is running and none can begin:
        // the network pointer no longer changes except here.
        self.stopped.get_or_init(|| {
            let network = self.network.swap(ptr::null_mut(), Ordering::SeqCst);
            if !network.is_null() {
                // SAFETY: the pointer came from `Box::into_raw` in `acquire`
                // and this swap, which runs once, is the only thing that
                // takes it out.
                stop(*unsafe { Box::from_raw(network) });
            }
        });
        Ok(())
    }
}

impl<N> Drop for NetworkGate<N> {
    fn drop(&mut self) {
        let network = *self.network.get_mut();
        if !network.is_null() {
            // SAFETY: as in `shutdown`; `&mut self` rules out a concurrent take.
            drop(unsafe { Box::from_raw(network) });
        }
    }
}

/// Keeps the network of a [`NetworkGate`] running. Dropping it lets a later
/// [`NetworkGate::shutdown`] proceed.
pub(crate) struct Lease<'a, N> {
    gate: &'a NetworkGate<N>,
}

impl<N> Drop for Lease<'_, N> {
    fn drop(&mut self) {
        self.gate.leases.fetch_sub(LEASE, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::AtomicIsize;
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Stands in for the native network. Records what happened to it.
    #[derive(Default)]
    struct Probe {
        boots: AtomicUsize,
        stops: AtomicUsize,
        stop_finished: AtomicBool,
        /// Leases the test currently holds.
        held: AtomicIsize,
    }

    struct FakeNetwork(Arc<Probe>);

    fn boot_ok(probe: &Arc<Probe>) -> impl FnOnce() -> Result<FakeNetwork, String> + '_ {
        || {
            probe.boots.fetch_add(1, Ordering::SeqCst);
            Ok(FakeNetwork(probe.clone()))
        }
    }

    fn stop(network: FakeNetwork) {
        let probe = &network.0;
        assert_eq!(
            probe.held.load(Ordering::SeqCst),
            0,
            "stopped under a live lease"
        );
        probe.stops.fetch_add(1, Ordering::SeqCst);
        probe.stop_finished.store(true, Ordering::SeqCst);
    }

    fn never_boot() -> Result<FakeNetwork, String> {
        panic!("the start action must not run")
    }

    fn never_stop(_: FakeNetwork) {
        panic!("the stop action must not run")
    }

    #[test]
    fn concurrent_first_callers_start_the_network_once() {
        const CALLERS: usize = 16;
        let probe = Arc::new(Probe::default());
        let gate = NetworkGate::new();
        let barrier = Barrier::new(CALLERS);
        let leases: Vec<_> = thread::scope(|scope| {
            let callers: Vec<_> = (0..CALLERS)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        gate.acquire(|| {
                            // long enough for every other caller to arrive
                            thread::sleep(Duration::from_millis(20));
                            boot_ok(&probe)()
                        })
                        .expect("every caller shares the one start")
                    })
                })
                .collect();
            callers.into_iter().map(|c| c.join().unwrap()).collect()
        });
        assert_eq!(probe.boots.load(Ordering::SeqCst), 1);
        assert_eq!(gate.leases.load(Ordering::SeqCst), CALLERS * LEASE);

        // each lease on its own keeps the network running
        for lease in leases {
            assert_eq!(gate.shutdown(never_stop), Err(Busy));
            drop(lease);
        }
        assert_eq!(gate.shutdown(stop), Ok(()));
        assert_eq!(probe.stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn busy_shutdown_leaves_the_network_usable() {
        let probe = Arc::new(Probe::default());
        let gate = NetworkGate::new();
        let lease = gate.acquire(boot_ok(&probe)).ok().unwrap();
        assert_eq!(gate.shutdown(never_stop), Err(Busy));
        // still open, and the network is not started a second time
        let second = gate.acquire(never_boot).ok().unwrap();
        drop(lease);
        assert_eq!(gate.shutdown(never_stop), Err(Busy));
        drop(second);
        assert_eq!(gate.shutdown(stop), Ok(()));
        assert_eq!(probe.boots.load(Ordering::SeqCst), 1);
        assert_eq!(probe.stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn shutdown_is_idempotent_and_closes_the_gate() {
        let probe = Arc::new(Probe::default());
        let gate = NetworkGate::new();
        drop(gate.acquire(boot_ok(&probe)).ok().unwrap());
        assert_eq!(gate.shutdown(stop), Ok(()));
        assert_eq!(gate.shutdown(never_stop), Ok(()));
        assert_eq!(gate.shutdown(never_stop), Ok(()));
        assert_eq!(probe.stops.load(Ordering::SeqCst), 1);
        assert_eq!(gate.acquire(never_boot).err(), Some(AcquireError::Closed));
        assert_eq!(gate.leases.load(Ordering::SeqCst), CLOSED);
    }

    #[test]
    fn shutdown_before_any_start_closes_without_starting_or_stopping() {
        let gate = NetworkGate::<FakeNetwork>::new();
        assert_eq!(gate.shutdown(never_stop), Ok(()));
        assert_eq!(gate.acquire(never_boot).err(), Some(AcquireError::Closed));
        assert_eq!(gate.shutdown(never_stop), Ok(()));
    }

    #[test]
    fn failed_start_is_remembered_and_never_retried() {
        let gate = NetworkGate::<FakeNetwork>::new();
        let failed = AcquireError::BootFailed("no cluster".to_owned());
        assert_eq!(
            gate.acquire(|| Err("no cluster".to_owned())).err(),
            Some(failed.clone())
        );
        // the failing caller's lease is gone
        assert_eq!(gate.leases.load(Ordering::SeqCst), 0);
        assert_eq!(gate.acquire(never_boot).err(), Some(failed.clone()));
        assert_eq!(gate.leases.load(Ordering::SeqCst), 0);
        // nothing was started, so nothing is stopped
        assert_eq!(gate.shutdown(never_stop), Ok(()));
        assert_eq!(gate.acquire(never_boot).err(), Some(AcquireError::Closed));
    }

    #[test]
    fn concurrent_callers_share_one_failed_start() {
        const CALLERS: usize = 8;
        let gate = NetworkGate::<FakeNetwork>::new();
        let boots = AtomicUsize::new(0);
        let barrier = Barrier::new(CALLERS);
        thread::scope(|scope| {
            for _ in 0..CALLERS {
                scope.spawn(|| {
                    barrier.wait();
                    let error = gate
                        .acquire(|| {
                            boots.fetch_add(1, Ordering::SeqCst);
                            thread::sleep(Duration::from_millis(20));
                            Err("refused".to_owned())
                        })
                        .err();
                    assert_eq!(error, Some(AcquireError::BootFailed("refused".to_owned())));
                });
            }
        });
        assert_eq!(boots.load(Ordering::SeqCst), 1);
        assert_eq!(gate.leases.load(Ordering::SeqCst), 0);
        assert_eq!(gate.shutdown(never_stop), Ok(()));
    }

    #[test]
    fn panicking_start_fails_every_later_caller_without_a_second_start() {
        let gate = NetworkGate::<FakeNetwork>::new();
        let unwound = catch_unwind(AssertUnwindSafe(|| {
            let _ = gate.acquire(|| panic!("native client panicked"));
        }));
        assert!(unwound.is_err());
        // the panicking caller's lease was released as it unwound
        assert_eq!(gate.leases.load(Ordering::SeqCst), 0);
        assert_eq!(
            gate.acquire(never_boot).err(),
            Some(AcquireError::BootDidNotComplete)
        );
        assert_eq!(
            gate.acquire(never_boot).err(),
            Some(AcquireError::BootDidNotComplete)
        );
        assert_eq!(gate.leases.load(Ordering::SeqCst), 0);
        assert_eq!(gate.shutdown(never_stop), Ok(()));
        assert_eq!(gate.acquire(never_boot).err(), Some(AcquireError::Closed));
    }

    #[test]
    fn callers_waiting_on_a_panicking_start_fail_too() {
        const WAITERS: usize = 6;
        let gate = NetworkGate::<FakeNetwork>::new();
        let (in_boot, boot_entered) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let gate = &gate;
        thread::scope(|scope| {
            let first = scope.spawn(move || {
                catch_unwind(AssertUnwindSafe(|| {
                    let _ = gate.acquire(|| {
                        in_boot.send(()).unwrap();
                        released.recv().unwrap();
                        panic!("native client panicked")
                    });
                }))
            });
            boot_entered.recv().unwrap();
            let waiters: Vec<_> = (0..WAITERS)
                .map(|_| scope.spawn(|| gate.acquire(never_boot).err()))
                .collect();
            // let the waiters reach the start cell, then let the start panic
            thread::sleep(Duration::from_millis(30));
            release.send(()).unwrap();
            assert!(first.join().unwrap().is_err());
            for waiter in waiters {
                assert_eq!(
                    waiter.join().unwrap(),
                    Some(AcquireError::BootDidNotComplete)
                );
            }
        });
        assert_eq!(gate.leases.load(Ordering::SeqCst), 0);
        assert_eq!(gate.shutdown(never_stop), Ok(()));
    }

    #[test]
    fn shutdown_during_a_start_is_busy_and_does_not_wait() {
        let probe = Arc::new(Probe::default());
        let gate = NetworkGate::new();
        let (in_boot, boot_entered) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let (gate, probe) = (&gate, &probe);
        thread::scope(|scope| {
            let starter = scope.spawn(move || {
                gate.acquire(|| {
                    in_boot.send(()).unwrap();
                    released.recv().unwrap();
                    boot_ok(probe)()
                })
                .ok()
                .unwrap()
            });
            boot_entered.recv().unwrap();
            // The start is blocked until `release`; a shutdown that waited
            // for it would never return.
            assert_eq!(gate.shutdown(never_stop), Err(Busy));
            assert_eq!(gate.shutdown(never_stop), Err(Busy));
            release.send(()).unwrap();
            let lease = starter.join().unwrap();
            // the start completed normally and the gate is still open
            assert_eq!(gate.shutdown(never_stop), Err(Busy));
            drop(gate.acquire(never_boot).ok().unwrap());
            drop(lease);
        });
        assert_eq!(gate.shutdown(stop), Ok(()));
        assert_eq!(probe.boots.load(Ordering::SeqCst), 1);
        assert_eq!(probe.stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn shutdown_during_a_failing_start_is_busy_then_succeeds() {
        let gate = NetworkGate::<FakeNetwork>::new();
        let (in_boot, boot_entered) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let gate = &gate;
        thread::scope(|scope| {
            let starter = scope.spawn(move || {
                gate.acquire(|| {
                    in_boot.send(()).unwrap();
                    released.recv().unwrap();
                    Err("refused".to_owned())
                })
                .err()
            });
            boot_entered.recv().unwrap();
            assert_eq!(gate.shutdown(never_stop), Err(Busy));
            release.send(()).unwrap();
            assert_eq!(
                starter.join().unwrap(),
                Some(AcquireError::BootFailed("refused".to_owned()))
            );
        });
        // the retry the `Busy` result asks for
        assert_eq!(gate.shutdown(never_stop), Ok(()));
    }

    #[test]
    fn concurrent_shutdowns_stop_once_and_all_return_after_the_stop() {
        const CALLERS: usize = 12;
        let probe = Arc::new(Probe::default());
        let gate = NetworkGate::new();
        drop(gate.acquire(boot_ok(&probe)).ok().unwrap());
        let barrier = Barrier::new(CALLERS);
        thread::scope(|scope| {
            for _ in 0..CALLERS {
                scope.spawn(|| {
                    barrier.wait();
                    let stopped = gate.shutdown(|network| {
                        // long enough for every other caller to arrive
                        thread::sleep(Duration::from_millis(50));
                        stop(network);
                    });
                    assert_eq!(stopped, Ok(()));
                    assert!(
                        probe.stop_finished.load(Ordering::SeqCst),
                        "shutdown returned before the stop finished"
                    );
                });
            }
        });
        assert_eq!(probe.stops.load(Ordering::SeqCst), 1);
    }

    /// Callers opening and dropping leases race callers shutting down, from
    /// a fresh gate each round so the start is part of the race. Whatever the
    /// interleaving: one start at most, one stop at most, never a stop under
    /// a live lease (checked in `stop`), never a lease on a stopped network,
    /// and no lease once a shutdown has returned `Ok`.
    #[test]
    fn leases_and_shutdowns_racing_from_a_fresh_gate() {
        const ROUNDS: usize = 300;
        const OPENERS: usize = 5;
        const CLOSERS: usize = 3;
        for round in 0..ROUNDS {
            let probe = Arc::new(Probe::default());
            let gate = NetworkGate::new();
            let closed = AtomicBool::new(false);
            let barrier = Barrier::new(OPENERS + CLOSERS);
            thread::scope(|scope| {
                for opener in 0..OPENERS {
                    let (probe, gate, closed, barrier) = (&probe, &gate, &closed, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        for step in 0..40 {
                            let closed_before = closed.load(Ordering::SeqCst);
                            match gate.acquire(boot_ok(probe)) {
                                Ok(lease) => {
                                    assert!(!closed_before, "lease after an accepted shutdown");
                                    probe.held.fetch_add(1, Ordering::SeqCst);
                                    assert!(!probe.stop_finished.load(Ordering::SeqCst));
                                    assert_eq!(probe.stops.load(Ordering::SeqCst), 0);
                                    if (opener + step + round) % 3 == 0 {
                                        thread::yield_now();
                                    }
                                    assert_eq!(probe.stops.load(Ordering::SeqCst), 0);
                                    probe.held.fetch_sub(1, Ordering::SeqCst);
                                    drop(lease);
                                }
                                Err(error) => assert_eq!(error, AcquireError::Closed),
                            }
                        }
                    });
                }
                for _ in 0..CLOSERS {
                    let (probe, gate, closed, barrier) = (&probe, &gate, &closed, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        loop {
                            if gate.shutdown(stop) == Ok(()) {
                                closed.store(true, Ordering::SeqCst);
                                // started at most once, and stopped if started
                                let boots = probe.boots.load(Ordering::SeqCst);
                                assert!(boots <= 1);
                                assert_eq!(probe.stops.load(Ordering::SeqCst), boots);
                                return;
                            }
                            thread::yield_now();
                        }
                    });
                }
            });
            assert_eq!(gate.leases.load(Ordering::SeqCst), CLOSED);
            assert_eq!(probe.held.load(Ordering::SeqCst), 0);
            assert!(probe.boots.load(Ordering::SeqCst) <= 1);
            assert_eq!(
                probe.stops.load(Ordering::SeqCst),
                probe.boots.load(Ordering::SeqCst)
            );
        }
    }

    #[test]
    fn dropping_a_started_gate_drops_the_network() {
        struct Tracked(Arc<AtomicUsize>);
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let gate = NetworkGate::new();
        drop(gate.acquire(|| Ok(Tracked(drops.clone()))).ok().unwrap());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(gate);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    /// Cost of taking and dropping a lease on a started network: the only
    /// part of the lifecycle a provider pays more than once (per managed
    /// connection). One caller, then eight sharing the gate.
    ///
    /// `cargo test --release -p mount-rs-foundationdb --lib network_gate_bench -- --ignored --nocapture`
    #[test]
    #[ignore = "benchmark; run in release with --nocapture"]
    fn network_gate_bench() {
        const BATCH: usize = 256;
        const BATCHES: usize = 20_000;
        for round in 0..3 {
            for threads in [1usize, 8] {
                let probe = Arc::new(Probe::default());
                let gate = NetworkGate::new();
                drop(gate.acquire(boot_ok(&probe)).ok().unwrap());
                let barrier = Barrier::new(threads);
                let mut per_op = Vec::new();
                let mut ops_per_sec = 0.0;
                thread::scope(|scope| {
                    let workers: Vec<_> = (0..threads)
                        .map(|_| {
                            scope.spawn(|| {
                                let mut samples = Vec::with_capacity(BATCHES);
                                barrier.wait();
                                let begin = Instant::now();
                                for _ in 0..BATCHES {
                                    let start = Instant::now();
                                    for _ in 0..BATCH {
                                        let lease = gate.acquire(never_boot);
                                        assert!(std::hint::black_box(&lease).is_ok());
                                    }
                                    samples.push(start.elapsed().as_nanos() as u64 / BATCH as u64);
                                }
                                (begin.elapsed(), samples)
                            })
                        })
                        .collect();
                    for worker in workers {
                        let (spent, samples) = worker.join().unwrap();
                        ops_per_sec += (BATCH * BATCHES) as f64 / spent.as_secs_f64();
                        per_op.extend(samples);
                    }
                });
                per_op.sort_unstable();
                println!(
                    "network_gate_bench round={round} threads={threads} ops_per_sec={ops_per_sec:.0} p50_ns={} p99_ns={}",
                    per_op[per_op.len() / 2],
                    per_op[per_op.len() * 99 / 100]
                );
                assert_eq!(gate.shutdown(stop), Ok(()));
            }
        }
    }
}
