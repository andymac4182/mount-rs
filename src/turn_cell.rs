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

pub struct TurnCellGuard<'a, T>(&'a TurnCell<T>);

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
        Some(Ok(TurnCellGuard(self)))
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
                return Ok(TurnCellGuard(self));
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
        if std::thread::panicking() {
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
}
