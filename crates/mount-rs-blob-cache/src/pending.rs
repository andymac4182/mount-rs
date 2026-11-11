//! Blocks written but not yet flushed, kept with no lock: an immutable
//! linked list behind one atomic pointer. A put prepends a node, a flush
//! swaps the whole list out, and a delete replaces the list with one that
//! leaves the deleted blocks out.
//!
//! A delete must never put a block back. Rebuilding the list and installing
//! it by compare-and-swap means a delete either removes its block from the
//! list every later flush sees, or retries against the newer list.
use arc_swap::ArcSwapOption;
use std::sync::Arc;

struct Node<T> {
    item: T,
    older: Chain<T>,
}

/// The rest of a list. Dropping it frees the nodes it alone owns in a loop,
/// so a long list does not recurse once per node.
struct Chain<T>(Option<Arc<Node<T>>>);

impl<T> Drop for Chain<T> {
    fn drop(&mut self) {
        let mut next = self.0.take();
        while let Some(node) = next {
            next = Arc::into_inner(node).and_then(|mut node| node.older.0.take());
        }
    }
}

pub(crate) struct PendingList<T> {
    head: ArcSwapOption<Node<T>>,
}

impl<T: Clone> PendingList<T> {
    pub(crate) fn new() -> Self {
        Self {
            head: ArcSwapOption::empty(),
        }
    }

    pub(crate) fn push(&self, item: T) {
        let mut node = Arc::new(Node {
            item,
            older: Chain(None),
        });
        loop {
            let current = self.head.load_full();
            // A failed exchange drops the copy it was given, so the node is
            // unshared again whenever this loop comes round.
            let Some(unpublished) = Arc::get_mut(&mut node) else {
                debug_assert!(false, "pending node shared before it was published");
                return;
            };
            unpublished.older = Chain(current.clone());
            let previous = self
                .head
                .compare_and_swap(&current, Some(Arc::clone(&node)));
            if same(&previous, &current) {
                return;
            }
        }
    }

    /// Takes every item, newest first. Each item is handed to one taker.
    pub(crate) fn take(&self) -> Batch<T> {
        Batch(Chain(self.head.swap(None)))
    }

    pub(crate) fn clear(&self) {
        drop(self.take());
    }

    /// Removes the items `keep` rejects. Allocates only when one is found,
    /// and then only for the nodes newer than the oldest rejected one.
    pub(crate) fn retain(&self, keep: impl Fn(&T) -> bool) {
        loop {
            let current = self.head.load_full();
            let mut rejected = None;
            let mut cursor = current.as_ref();
            let mut position = 0usize;
            while let Some(node) = cursor {
                if !keep(&node.item) {
                    rejected = Some(position);
                }
                position += 1;
                cursor = node.older.0.as_ref();
            }
            let Some(last) = rejected else { return };
            let mut kept = Vec::new();
            let mut cursor = current.as_ref();
            for _ in 0..=last {
                let Some(node) = cursor else { break };
                if keep(&node.item) {
                    kept.push(&node.item);
                }
                cursor = node.older.0.as_ref();
            }
            let mut rebuilt = cursor.cloned();
            for item in kept.into_iter().rev() {
                rebuilt = Some(Arc::new(Node {
                    item: item.clone(),
                    older: Chain(rebuilt),
                }));
            }
            let previous = self.head.compare_and_swap(&current, rebuilt);
            if same(&previous, &current) {
                return;
            }
        }
    }
}

fn same<T>(a: &Option<Arc<Node<T>>>, b: &Option<Arc<Node<T>>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

pub(crate) struct Batch<T>(Chain<T>);

impl<T: Clone> Iterator for Batch<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        let node = self.0.0.take()?;
        match Arc::try_unwrap(node) {
            Ok(Node { item, older }) => {
                self.0 = older;
                Some(item)
            }
            // A delete that loaded the list before it was taken still holds it.
            Err(shared) => {
                self.0 = Chain(shared.older.0.clone());
                Some(shared.item.clone())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    #[test]
    fn items_are_taken_once_and_retain_keeps_order() {
        let list = PendingList::new();
        for n in 0..10u64 {
            list.push(n);
        }
        list.retain(|n| n % 3 != 0);
        list.retain(|n| *n != 100);
        assert_eq!(list.take().collect::<Vec<_>>(), [8, 7, 5, 4, 2, 1]);
        assert_eq!(list.take().count(), 0);
        list.push(1);
        list.clear();
        assert_eq!(list.take().count(), 0);
    }

    #[test]
    fn a_long_list_is_freed_without_recursing() {
        let list = PendingList::new();
        for n in 0..500_000u64 {
            list.push(n);
        }
        // Reject the oldest so the whole list is rebuilt, then drop both.
        list.retain(|n| *n != 0);
        assert_eq!(list.take().next(), Some(499_999));
        for n in 0..500_000u64 {
            list.push(n);
        }
        drop(list);
    }

    /// Pushers, deleters and takers run together. Every item that was not
    /// deleted is taken exactly once, no item is taken twice, and an item
    /// whose delete had returned is never in a take that started later.
    #[test]
    fn concurrent_pushes_deletes_and_takes_lose_nothing_and_resurrect_nothing() {
        const THREADS: u64 = 6;
        const PER_THREAD: u64 = 4_000;
        for _ in 0..4 {
            let list = Arc::new(PendingList::<u64>::new());
            let clock = Arc::new(AtomicU64::new(1));
            let done = Arc::new(AtomicBool::new(false));
            // id -> clock reading taken after its delete returned.
            let deleted = Arc::new(Mutex::new(BTreeMap::<u64, u64>::new()));
            let takers: Vec<_> = (0..2)
                .map(|_| {
                    let list = list.clone();
                    let clock = clock.clone();
                    let done = done.clone();
                    std::thread::spawn(move || {
                        let mut taken = Vec::new();
                        loop {
                            let finished = done.load(Ordering::SeqCst);
                            let started = clock.fetch_add(1, Ordering::SeqCst);
                            taken.extend(list.take().map(|id| (id, started)));
                            if finished {
                                return taken;
                            }
                            std::thread::yield_now();
                        }
                    })
                })
                .collect();
            let writers: Vec<_> = (0..THREADS)
                .map(|thread| {
                    let list = list.clone();
                    let clock = clock.clone();
                    let deleted = deleted.clone();
                    std::thread::spawn(move || {
                        let mut mine = Vec::new();
                        for n in 0..PER_THREAD {
                            let id = thread * PER_THREAD + n;
                            list.push(id);
                            if n % 3 == 0 {
                                // Delete an item this thread pushed a little earlier.
                                let victim = id - (n % 7).min(n);
                                list.retain(|item| *item != victim);
                                mine.push((victim, clock.fetch_add(1, Ordering::SeqCst)));
                            }
                        }
                        let mut deleted = deleted.lock().unwrap();
                        for (victim, at) in mine {
                            deleted.entry(victim).or_insert(at);
                        }
                    })
                })
                .collect();
            for writer in writers {
                writer.join().unwrap();
            }
            done.store(true, Ordering::SeqCst);
            let mut seen = BTreeMap::new();
            for taker in takers {
                for (id, started) in taker.join().unwrap() {
                    assert!(
                        seen.insert(id, started).is_none(),
                        "{id} was handed to two takes"
                    );
                }
            }
            assert_eq!(list.take().count(), 0);
            let deleted = deleted.lock().unwrap();
            for id in 0..THREADS * PER_THREAD {
                match (seen.get(&id), deleted.get(&id)) {
                    (None, None) => panic!("{id} was lost"),
                    (Some(started), Some(at)) => assert!(
                        started < at,
                        "{id} was taken by a flush that began after its delete returned"
                    ),
                    _ => {}
                }
            }
        }
    }
}
