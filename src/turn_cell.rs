//! Exclusive access taken with one compare-and-swap and no lock object.
//!
//! For state that has one user at a time by construction, where a mutex
//! would be taken and never waited for: the callers are already serialized
//! (by a batch runner, by an asynchronous turn) and everything else reads
//! elsewhere. `TurnCell` keeps the exclusion, as a flag. A thread that does
//! find it taken spins briefly and then yields, so it is the wrong tool for
//! anything threads are expected to queue on.
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct TurnCell<T> {
    busy: AtomicBool,
    /// Set when a holder panicked, as a poisoned mutex would be.
    abandoned: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: `value` is only reached through `TurnCellGuard`, of which `busy`
// admits one at a time.
unsafe impl<T: Send> Send for TurnCell<T> {}
unsafe impl<T: Send> Sync for TurnCell<T> {}

pub struct TurnCellGuard<'a, T>(&'a TurnCell<T>, Unwinding);

/// Whether the thread was already unwinding when it took the cell. A holder
/// abandons the cell only if a panic started while it had it, as with a
/// mutex: a `Drop` that runs during an unwind may take and release the cell
/// without harm.
struct Unwinding(bool);

impl Unwinding {
    fn now() -> Self {
        Self(std::thread::panicking())
    }
}

impl<T> std::fmt::Debug for TurnCell<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Without taking the cell: formatting must not wait or abandon it.
        formatter.debug_struct("TurnCell").finish_non_exhaustive()
    }
}

impl<T: Default> Default for TurnCell<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> TurnCell<T> {
    pub fn new(value: T) -> Self {
        Self {
            busy: AtomicBool::new(false),
            abandoned: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// The value, if no one has it. `Err` once a holder has panicked.
    #[allow(clippy::result_unit_err)]
    pub fn try_lock(&self) -> Option<Result<TurnCellGuard<'_, T>, ()>> {
        self.busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        if self.abandoned.load(Ordering::Acquire) {
            self.busy.store(false, Ordering::Release);
            return Some(Err(()));
        }
        Some(Ok(TurnCellGuard(self, Unwinding::now())))
    }

    /// The value even after a holder panicked, for state that is valid
    /// whatever a holder was doing (what `PoisonError::into_inner` is for).
    pub fn lock_recovered(&self) -> TurnCellGuard<'_, T> {
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

    /// `Err` once a holder has panicked.
    #[allow(clippy::result_unit_err)]
    pub fn lock(&self) -> Result<TurnCellGuard<'_, T>, ()> {
        let mut attempts = 0_u32;
        loop {
            if self
                .busy
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                if self.abandoned.load(Ordering::Acquire) {
                    self.busy.store(false, Ordering::Release);
                    return Err(());
                }
                return Ok(TurnCellGuard(self, Unwinding::now()));
            }
            attempts += 1;
            if attempts < 64 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
    }
}

impl<T> std::ops::Deref for TurnCellGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: this guard is the only one.
        unsafe { &*self.0.value.get() }
    }
}

impl<T> std::ops::DerefMut for TurnCellGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`, and `&mut self` excludes other borrows.
        unsafe { &mut *self.0.value.get() }
    }
}

impl<T> Drop for TurnCellGuard<'_, T> {
    fn drop(&mut self) {
        if !self.1.0 && std::thread::panicking() {
            self.0.abandoned.store(true, Ordering::Release);
        }
        self.0.busy.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn holders_exclude_each_other_and_a_panic_abandons_the_cell() {
        let cell = Arc::new(TurnCell::new(0_u64));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let cell = Arc::clone(&cell);
                std::thread::spawn(move || {
                    for _ in 0..10_000 {
                        // Not atomic: lost updates would show a second holder.
                        let mut value = cell.lock().unwrap();
                        let seen = *value;
                        std::hint::black_box(&seen);
                        *value = seen + 1;
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(*cell.lock().unwrap(), 40_000);
        let held = cell.lock().unwrap();
        assert!(cell.try_lock().is_none());
        drop(held);

        let panicking = Arc::clone(&cell);
        let _ = std::thread::spawn(move || {
            let _held = panicking.lock().unwrap();
            panic!("abandon the cell");
        })
        .join();
        assert!(cell.lock().is_err());
        assert!(matches!(cell.try_lock(), Some(Err(()))));
    }

    #[test]
    fn a_drop_during_an_unwind_does_not_abandon_and_recovery_reaches_the_value() {
        struct Touches(Arc<TurnCell<u32>>);
        impl Drop for Touches {
            fn drop(&mut self) {
                *self.0.lock().unwrap() += 1;
            }
        }
        let cell = Arc::new(TurnCell::new(0));
        let touched = Arc::clone(&cell);
        let unwound = std::thread::spawn(move || {
            let _touches = Touches(touched);
            panic!("unwinding");
        })
        .join();
        assert!(unwound.is_err());
        assert_eq!(*cell.lock().unwrap(), 1);

        let held = Arc::clone(&cell);
        let abandoned = std::thread::spawn(move || {
            let _guard = held.lock().unwrap();
            panic!("abandon");
        })
        .join();
        assert!(abandoned.is_err() && cell.lock().is_err());
        *cell.lock_recovered() += 1;
        assert_eq!(*cell.lock().unwrap(), 2);
    }
}
