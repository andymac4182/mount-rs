//! `TurnCell` for sections that can be long: a waiter queues and parks.
//!
//! `TurnCell` spins and then yields until the holder leaves, which costs a
//! core for as long as the holder takes. That is right for a few
//! instructions and wrong for a disk sync, a scan of every entry or a copy
//! of a whole file. `ParkCell` has the same interface and the same
//! abandon-on-panic rule, but a thread that finds it taken tries a few more
//! times and then joins the `TurnGate` queue and parks, so waiting costs
//! nothing and waiters are served in arrival order.
//!
//! The guard is for synchronous sections: `lock` blocks the calling thread.
use crate::turn_gate::TurnGate;
use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Attempts made before queueing: enough to ride out a short section
/// without the cost of parking and being woken.
const SPINS: u32 = 64;

#[cfg(test)]
thread_local! {
    /// How many times this thread has parked waiting for a cell.
    static PARKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub struct ParkCell<T> {
    gate: TurnGate,
    /// Set when a holder panicked, as a poisoned mutex would be.
    abandoned: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: `value` is only reached through `ParkCellGuard`, of which `gate`
// admits one at a time.
unsafe impl<T: Send> Send for ParkCell<T> {}
unsafe impl<T: Send> Sync for ParkCell<T> {}

/// The second field is whether the thread was already unwinding when it
/// took the cell: only a panic that starts while holding abandons it.
pub struct ParkCellGuard<'a, T>(&'a ParkCell<T>, bool);

impl<T> std::fmt::Debug for ParkCell<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Without taking the cell: formatting must not wait or abandon it.
        formatter.debug_struct("ParkCell").finish_non_exhaustive()
    }
}

impl<T: Default> Default for ParkCell<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> ParkCell<T> {
    pub fn new(value: T) -> Self {
        Self {
            gate: TurnGate::new(),
            abandoned: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// The gate is held by the caller: wrap it, or give it back if a holder
    /// panicked.
    fn held(&self) -> Result<ParkCellGuard<'_, T>, ()> {
        if self.abandoned.load(Ordering::Acquire) {
            self.gate.release();
            return Err(());
        }
        Ok(ParkCellGuard(self, std::thread::panicking()))
    }

    /// The value, if no one has it. `Err` once a holder has panicked.
    #[allow(clippy::result_unit_err)]
    pub fn try_lock(&self) -> Option<Result<ParkCellGuard<'_, T>, ()>> {
        self.gate.try_acquire().then(|| self.held())
    }

    /// The value, parking the thread until its turn. `Err` once a holder
    /// has panicked.
    #[allow(clippy::result_unit_err)]
    pub fn lock(&self) -> Result<ParkCellGuard<'_, T>, ()> {
        for _ in 0..SPINS {
            if self.gate.try_acquire() {
                return self.held();
            }
            std::hint::spin_loop();
        }
        struct Unpark(std::thread::Thread);
        impl std::task::Wake for Unpark {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = std::task::Waker::from(Arc::new(Unpark(std::thread::current())));
        let mut context = std::task::Context::from_waker(&waker);
        let mut waiting = std::pin::pin!(self.gate.lock());
        loop {
            if let std::task::Poll::Ready(turn) =
                std::future::Future::poll(waiting.as_mut(), &mut context)
            {
                // The guard releases the gate itself.
                std::mem::forget(turn);
                return self.held();
            }
            // A wake that arrived before this makes `park` return at once.
            #[cfg(test)]
            PARKS.with(|parks| parks.set(parks.get() + 1));
            std::thread::park();
        }
    }

    /// The value even after a holder panicked, for state that is valid
    /// whatever a holder was doing (what `PoisonError::into_inner` is for).
    pub fn lock_recovered(&self) -> ParkCellGuard<'_, T> {
        loop {
            match self.lock() {
                Ok(guard) => return guard,
                Err(()) => self.abandoned.store(false, Ordering::Release),
            }
        }
    }

    /// The value, through exclusive access to the cell: no turn is taken,
    /// and it is there whether or not a holder panicked.
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }
}

impl<T> std::ops::Deref for ParkCellGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: this guard is the only one.
        unsafe { &*self.0.value.get() }
    }
}

impl<T> std::ops::DerefMut for ParkCellGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`, and `&mut self` excludes other borrows.
        unsafe { &mut *self.0.value.get() }
    }
}

impl<T> Drop for ParkCellGuard<'_, T> {
    fn drop(&mut self) {
        if !self.1 && std::thread::panicking() {
            self.0.abandoned.store(true, Ordering::Release);
        }
        self.0.gate.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn holders_exclude_each_other_and_a_panic_abandons_the_cell() {
        let cell = Arc::new(ParkCell::new(0_u64));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let cell = Arc::clone(&cell);
                std::thread::spawn(move || {
                    for _ in 0..10_000 {
                        *cell.lock().unwrap() += 1;
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(*cell.lock().unwrap(), 40_000);

        let panicking = Arc::clone(&cell);
        let _ = std::thread::spawn(move || {
            let _held = panicking.lock().unwrap();
            panic!("holder panics");
        })
        .join();
        assert!(cell.lock().is_err());
        assert!(matches!(cell.try_lock(), Some(Err(()))));
        assert_eq!(*cell.lock_recovered(), 40_000);
        assert!(cell.lock().is_ok());
    }

    /// The reason the type exists: a thread waiting out a long section is
    /// parked, not running. It goes round its wait loop a handful of times
    /// in 300 ms, where a spinning waiter would go round millions of times.
    #[test]
    fn a_waiter_parks_through_a_long_section() {
        let cell = Arc::new(ParkCell::new(()));
        let held = cell.lock().unwrap();
        let waiter = {
            let cell = Arc::clone(&cell);
            std::thread::spawn(move || {
                let started = Instant::now();
                drop(cell.lock().unwrap());
                (started.elapsed(), PARKS.with(std::cell::Cell::get))
            })
        };
        std::thread::sleep(Duration::from_millis(300));
        drop(held);
        let (waited, parks) = waiter.join().unwrap();
        assert!(waited >= Duration::from_millis(200), "waited {waited:?}");
        assert!((1..=8).contains(&parks), "parked {parks} times");
    }

    #[test]
    fn waiters_are_served_and_none_is_stranded() {
        let cell = Arc::new(ParkCell::new(Vec::new()));
        let threads: Vec<_> = (0..8)
            .map(|thread| {
                let cell = Arc::clone(&cell);
                std::thread::spawn(move || {
                    for _ in 0..200 {
                        let mut held = cell.lock().unwrap();
                        held.push(thread);
                        // Long enough that the others queue and park.
                        std::thread::sleep(Duration::from_micros(50));
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(cell.lock().unwrap().len(), 1600);
    }
}
