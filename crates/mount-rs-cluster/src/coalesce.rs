//! Lets concurrent callers working on one key share a single piece of work,
//! with no lock: in-flight keys sit in a fixed table of atomic slots probed
//! from the key's hash, and callers for a key take turns on its gate. The
//! first does the work; the rest find its result when their turn comes.
//!
//! Sharing is an optimisation, not an invariant. A caller that finds its
//! probe window full proceeds unshared, and two callers racing a finishing
//! one may each do the work.
use arc_swap::ArcSwapOption;
use mount_rs_core::turn_gate::TurnGate;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const SLOTS: usize = 256;
const PROBE: usize = 8;

struct Entry {
    key: String,
    turn: TurnGate,
    /// Callers sharing this entry. The one that takes it to zero frees the slot.
    sharers: AtomicUsize,
}

pub(crate) struct Coalesce {
    slots: Box<[ArcSwapOption<Entry>]>,
}

impl Default for Coalesce {
    fn default() -> Self {
        Self {
            slots: (0..SLOTS).map(|_| ArcSwapOption::empty()).collect(),
        }
    }
}

/// One caller's share of a key's work.
pub(crate) struct Share<'a> {
    slot: &'a ArcSwapOption<Entry>,
    entry: Arc<Entry>,
}

impl Coalesce {
    fn window(&self, key: &str) -> impl Iterator<Item = &ArcSwapOption<Entry>> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let start = hasher.finish() as usize;
        (0..PROBE).map(move |step| &self.slots[start.wrapping_add(step) % SLOTS])
    }

    pub(crate) fn join(&self, key: &str) -> Option<Share<'_>> {
        for slot in self.window(key) {
            if let Some(entry) = slot.load_full()
                && entry.key == key
            {
                return Some(Share::new(slot, entry));
            }
        }
        let mut mine: Option<Arc<Entry>> = None;
        for slot in self.window(key) {
            loop {
                let current = slot.load();
                match &*current {
                    Some(entry) if entry.key == key => {
                        return Some(Share::new(slot, Arc::clone(entry)));
                    }
                    Some(_) => break,
                    None => {
                        let entry = Arc::clone(mine.get_or_insert_with(|| {
                            Arc::new(Entry {
                                key: key.to_owned(),
                                turn: TurnGate::new(),
                                sharers: AtomicUsize::new(0),
                            })
                        }));
                        if slot
                            .compare_and_swap(&current, Some(Arc::clone(&entry)))
                            .is_none()
                        {
                            return Some(Share::new(slot, entry));
                        }
                    }
                }
            }
        }
        None
    }
}

impl<'a> Share<'a> {
    fn new(slot: &'a ArcSwapOption<Entry>, entry: Arc<Entry>) -> Self {
        entry.sharers.fetch_add(1, Ordering::AcqRel);
        Self { slot, entry }
    }

    pub(crate) fn turn(&self) -> &TurnGate {
        &self.entry.turn
    }
}

impl Drop for Share<'_> {
    fn drop(&mut self) {
        // A caller that joins after this count reaches zero keeps its own
        // reference and simply stops being shared with.
        if self.entry.sharers.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        let current = self.slot.load();
        if current
            .as_ref()
            .is_some_and(|entry| Arc::ptr_eq(entry, &self.entry))
        {
            self.slot.compare_and_swap(&current, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callers_for_one_key_share_a_gate_and_the_last_frees_the_slot() {
        let table = Coalesce::default();
        let first = table.join("volume").unwrap();
        let second = table.join("volume").unwrap();
        let other = table.join("other").unwrap();
        assert!(Arc::ptr_eq(&first.entry, &second.entry));
        assert!(!Arc::ptr_eq(&first.entry, &other.entry));
        let turn = first.turn().try_lock().unwrap();
        assert!(second.turn().try_lock().is_none());
        assert!(other.turn().try_lock().is_some());
        drop(turn);
        drop(first);
        // Still shared by `second`: a newcomer joins the same entry.
        let third = table.join("volume").unwrap();
        assert!(Arc::ptr_eq(&second.entry, &third.entry));
        drop((second, third, other));
        assert!(table.slots.iter().all(|slot| slot.load().is_none()));
    }
}
