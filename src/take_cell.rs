//! A value that is handed over once.
//!
//! `TakeCell` holds a value until the first caller takes it; every later
//! caller gets `None`. It is an atomic pointer swap, so nothing waits. Unlike
//! an atomic reference-counted slot it never lends the value out, which is
//! why it can hold a value that is `Send` but not `Sync`.

use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

pub struct TakeCell<T> {
    value: AtomicPtr<T>,
}

// The cell only ever moves the value out to one thread; it gives out no
// reference to it. Sharing the cell therefore needs no more of `T` than
// sending it does.
unsafe impl<T: Send> Send for TakeCell<T> {}
unsafe impl<T: Send> Sync for TakeCell<T> {}

impl<T> TakeCell<T> {
    pub fn new(value: T) -> Self {
        Self {
            value: AtomicPtr::new(Box::into_raw(Box::new(value))),
        }
    }

    pub const fn empty() -> Self {
        Self {
            value: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Returns the value to the first caller and `None` to every other.
    pub fn take(&self) -> Option<T> {
        let value = self.value.swap(ptr::null_mut(), Ordering::AcqRel);
        if value.is_null() {
            return None;
        }
        // SAFETY: a non-null pointer came from `Box::into_raw` in `new`, and
        // the swap above removed it from the cell, so this is its only owner.
        Some(*unsafe { Box::from_raw(value) })
    }
}

impl<T> Drop for TakeCell<T> {
    fn drop(&mut self) {
        drop(self.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn only_the_first_take_gets_the_value() {
        let cell = TakeCell::new(7);
        assert_eq!(cell.take(), Some(7));
        assert_eq!(cell.take(), None);
        assert_eq!(TakeCell::<u8>::empty().take(), None);
    }

    #[test]
    fn concurrent_takers_get_the_value_exactly_once() {
        // `Cell` is `Send` but not `Sync`: the cell must still be shareable.
        let cell = Arc::new(TakeCell::new(Cell::new(1_usize)));
        let taken = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let cell = Arc::clone(&cell);
                let taken = Arc::clone(&taken);
                std::thread::spawn(move || {
                    if let Some(value) = cell.take() {
                        taken.fetch_add(value.get(), Ordering::Relaxed);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(taken.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn an_untaken_value_is_dropped_with_the_cell() {
        let value = Arc::new(());
        drop(TakeCell::new(Arc::clone(&value)));
        assert_eq!(Arc::strong_count(&value), 1);
    }
}
