//! A compare-and-swap exclusive cell, used in place of `std::sync::Mutex`.
//!
//! Local to this vendored copy; see `UPSTREAM_PATCH.md`. The sections it
//! guards are a few map or filter operations, so a contender spins briefly
//! and then yields rather than parking. There is no poisoning: a holder that
//! panics releases the cell as it unwinds.

use std::{
    cell::UnsafeCell,
    fmt,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) struct Turn<T> {
    held: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: `held` admits one guard at a time, and only a guard reaches `value`.
unsafe impl<T: Send> Send for Turn<T> {}
// SAFETY: as above; sharing the cell only ever hands `T` to one thread at a time.
unsafe impl<T: Send> Sync for Turn<T> {}

impl<T> Turn<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            held: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub(crate) fn lock(&self) -> TurnGuard<'_, T> {
        let mut spins = 0u32;
        while self
            .held
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            if spins < 64 {
                spins += 1;
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        TurnGuard { turn: self }
    }

    #[cfg(test)]
    pub(crate) fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }
}

impl<T> fmt::Debug for Turn<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Turn").finish_non_exhaustive()
    }
}

pub(crate) struct TurnGuard<'a, T> {
    turn: &'a Turn<T>,
}

impl<T> Deref for TurnGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: this guard is the only holder of the cell.
        unsafe { &*self.turn.value.get() }
    }
}

impl<T> DerefMut for TurnGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: this guard is the only holder of the cell.
        unsafe { &mut *self.turn.value.get() }
    }
}

impl<T> Drop for TurnGuard<'_, T> {
    fn drop(&mut self) {
        self.turn.held.store(false, Ordering::Release);
    }
}
