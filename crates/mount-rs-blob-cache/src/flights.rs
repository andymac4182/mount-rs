//! Lets concurrent misses on one block share a single fetch, with no lock:
//! in-flight keys sit in a fixed table of atomic slots probed from the key's
//! hash, and callers for a key take turns on its gate. The first fetches;
//! the rest find the block cached when their turn comes.
//!
//! Sharing is an optimisation, not an invariant. A caller that finds its
//! probe window full fetches unshared, and so may a caller that arrives as
//! the last sharer of a key leaves. Both are safe: a fetch looks in the
//! cache again before it goes anywhere else.
use arc_swap::ArcSwapOption;
use mount_rs_core::turn_gate::TurnGate;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const PROBE: usize = 8;

struct Entry {
    key: String,
    turn: TurnGate,
    /// Callers sharing this entry. The one that takes it to zero frees the slot.
    sharers: AtomicUsize,
}

pub(crate) struct Flights {
    slots: Box<[ArcSwapOption<Entry>]>,
}

/// One caller's share of a key's fetch.
pub(crate) struct Share<'a> {
    slot: &'a ArcSwapOption<Entry>,
    entry: Arc<Entry>,
}

impl Flights {
    /// `inflight` is the most keys that can be in flight at once; the table
    /// is kept at most half full so a probe window is rarely exhausted.
    pub(crate) fn new(inflight: usize) -> Self {
        let slots = inflight.saturating_mul(2).max(PROBE).next_power_of_two();
        Self {
            slots: (0..slots).map(|_| ArcSwapOption::empty()).collect(),
        }
    }

    fn window<'a>(&'a self, key: &str) -> impl Iterator<Item = &'a ArcSwapOption<Entry>> + use<'a> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let start = hasher.finish() as usize;
        let mask = self.slots.len() - 1;
        (0..PROBE).map(move |step| &self.slots[start.wrapping_add(step) & mask])
    }

    /// `None` when every slot in the key's window holds another key.
    pub(crate) fn join(&self, key: String) -> Option<Share<'_>> {
        for slot in self.window(&key) {
            if let Some(entry) = slot.load_full()
                && entry.key == key
            {
                return Some(Share::new(slot, entry));
            }
        }
        let mine = Arc::new(Entry {
            key,
            turn: TurnGate::new(),
            sharers: AtomicUsize::new(0),
        });
        for slot in self.window(&mine.key) {
            loop {
                let current = slot.load();
                match &*current {
                    Some(entry) if entry.key == mine.key => {
                        return Some(Share::new(slot, Arc::clone(entry)));
                    }
                    Some(_) => break,
                    None => {
                        if slot
                            .compare_and_swap(&current, Some(Arc::clone(&mine)))
                            .is_none()
                        {
                            return Some(Share::new(slot, mine));
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
        let table = Flights::new(64);
        assert_eq!(table.slots.len(), 128);
        let first = table.join("block".into()).unwrap();
        let second = table.join("block".into()).unwrap();
        let other = table.join("other".into()).unwrap();
        assert!(Arc::ptr_eq(&first.entry, &second.entry));
        assert!(!Arc::ptr_eq(&first.entry, &other.entry));
        let turn = first.turn().try_lock().unwrap();
        assert!(second.turn().try_lock().is_none());
        assert!(other.turn().try_lock().is_some());
        drop(turn);
        drop(first);
        // Still shared by `second`: a newcomer joins the same entry.
        let third = table.join("block".into()).unwrap();
        assert!(Arc::ptr_eq(&second.entry, &third.entry));
        drop((second, third, other));
        assert!(table.slots.iter().all(|slot| slot.load().is_none()));
    }

    #[test]
    fn a_full_table_refuses_new_keys_but_still_shares_the_ones_it_holds() {
        let table = Flights::new(1);
        assert_eq!(table.slots.len(), PROBE);
        let mut held = Vec::new();
        let mut refused = 0;
        for n in 0..PROBE + 4 {
            match table.join(format!("key-{n}")) {
                Some(share) => held.push(share),
                None => refused += 1,
            }
        }
        assert_eq!(held.len(), PROBE);
        assert_eq!(refused, 4);
        assert!(table.join("key-0".into()).is_some());
        drop(held);
        assert!(table.slots.iter().all(|slot| slot.load().is_none()));
        assert!(table.join("key-11".into()).is_some());
    }

    /// Many threads join and leave a few keys. Two callers that hold a turn
    /// for one key at the same time must have been given different entries
    /// (the unshared case), never the same gate, and the table ends empty.
    #[test]
    fn concurrent_joins_never_hand_two_callers_one_turn_and_leave_no_slot_behind() {
        let table = Arc::new(Flights::new(8));
        let inside: Arc<Vec<AtomicUsize>> = Arc::new((0..4).map(|_| AtomicUsize::new(0)).collect());
        let unshared = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|thread| {
                let table = table.clone();
                let inside = inside.clone();
                let unshared = unshared.clone();
                std::thread::spawn(move || {
                    for round in 0..20_000usize {
                        let key = (thread + round) % 4;
                        let Some(share) = table.join(format!("key-{key}")) else {
                            panic!("four keys cannot fill an eight slot window");
                        };
                        if let Some(_turn) = share.turn().try_lock() {
                            if inside[key].fetch_add(1, Ordering::SeqCst) != 0 {
                                unshared.fetch_add(1, Ordering::Relaxed);
                            }
                            std::hint::spin_loop();
                            inside[key].fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(table.slots.iter().all(|slot| slot.load().is_none()));
        // Overlap only happens when a caller joins as the last sharer leaves.
        let unshared = unshared.load(Ordering::Relaxed);
        assert!(
            unshared < 8 * 20_000 / 10,
            "sharing mostly failed: {unshared}"
        );
    }
}
