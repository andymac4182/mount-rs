//! A list that is only ever added to.
//!
//! `AppendLog` keeps values in the order they were pushed. A push is one
//! compare-and-swap and a read walks the list, so neither waits for the
//! other. Entries are freed only when the log is dropped, which is what
//! makes reading without a lock sound: a reader can never meet a freed entry.

use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

pub struct AppendLog<T> {
    /// The most recently pushed entry; each entry points at the one before.
    newest: AtomicPtr<Entry<T>>,
}

struct Entry<T> {
    value: T,
    older: *mut Entry<T>,
}

// Pushing moves a `T` in from any thread and reading hands out `&T` on any
// thread, so the log needs both of `T`.
unsafe impl<T: Send + Sync> Send for AppendLog<T> {}
unsafe impl<T: Send + Sync> Sync for AppendLog<T> {}

impl<T> Default for AppendLog<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> AppendLog<T> {
    pub const fn new() -> Self {
        Self {
            newest: AtomicPtr::new(ptr::null_mut()),
        }
    }

    pub fn push(&self, value: T) {
        let entry = Box::into_raw(Box::new(Entry {
            value,
            older: ptr::null_mut(),
        }));
        let mut newest = self.newest.load(Ordering::Acquire);
        loop {
            // SAFETY: `entry` is not yet published, so this is its only user.
            unsafe { (*entry).older = newest };
            match self.newest.compare_exchange_weak(
                newest,
                entry,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(current) => newest = current,
            }
        }
    }

    /// Every value pushed so far, oldest first.
    pub fn snapshot(&self) -> Vec<T>
    where
        T: Clone,
    {
        let mut values = Vec::new();
        let mut entry = self.newest.load(Ordering::Acquire);
        // SAFETY: a published entry is never changed or freed while the log
        // is alive, and `&self` keeps it alive.
        while let Some(current) = unsafe { entry.as_ref() } {
            values.push(current.value.clone());
            entry = current.older;
        }
        values.reverse();
        values
    }
}

impl<T> Drop for AppendLog<T> {
    fn drop(&mut self) {
        let mut entry = *self.newest.get_mut();
        while !entry.is_null() {
            // SAFETY: each entry came from `Box::into_raw` in `push`, and
            // `&mut self` means no reader or pusher is left.
            let current = unsafe { Box::from_raw(entry) };
            entry = current.older;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn values_come_back_in_push_order() {
        let log = AppendLog::new();
        assert!(log.snapshot().is_empty());
        for value in 0..5 {
            log.push(value);
        }
        assert_eq!(log.snapshot(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn concurrent_pushes_are_all_kept_and_each_thread_stays_in_order() {
        let log = Arc::new(AppendLog::new());
        let threads: Vec<_> = (0..8_u32)
            .map(|thread| {
                let log = Arc::clone(&log);
                std::thread::spawn(move || {
                    for value in 0..500_u32 {
                        log.push((thread, value));
                        if value % 50 == 0 {
                            // Reads run alongside pushes.
                            let _ = log.snapshot();
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let values = log.snapshot();
        assert_eq!(values.len(), 4_000);
        for thread in 0..8 {
            let own: Vec<u32> = values
                .iter()
                .filter(|(owner, _)| *owner == thread)
                .map(|(_, value)| *value)
                .collect();
            assert_eq!(own, (0..500).collect::<Vec<_>>());
        }
    }

    #[test]
    fn dropping_the_log_drops_every_value() {
        let value = Arc::new(());
        let log = AppendLog::new();
        for _ in 0..100 {
            log.push(Arc::clone(&value));
        }
        drop(log);
        assert_eq!(Arc::strong_count(&value), 1);
    }
}
