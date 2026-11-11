//! The peer client's shared tables.  Each is one published immutable value
//! that readers load and writers replace with a compare-and-swap, so no
//! caller ever waits behind another one's turn.
//!
//! Every table here is small and bounded (by `max_destinations`,
//! `max_connections` or [`super::MAX_ROTATION_DRAINING_CONNECTIONS`]) and is
//! written only when a connection is dialled, replaced or closed, so copying
//! it on a write costs less than the handshake that caused the write.
use super::{ClientConnection, PeerDestination, PeerTransportError};
use arc_swap::ArcSwap;
use mount_rs_core::turn_gate::TurnLock;
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

/// The pooled connection of each destination.  Checkout is one load.
pub(super) struct PublishedPool<C = ClientConnection> {
    directory: ArcSwap<HashMap<PeerDestination, Arc<C>>>,
}

impl<C> Default for PublishedPool<C> {
    fn default() -> Self {
        Self {
            directory: ArcSwap::default(),
        }
    }
}

impl<C> PublishedPool<C> {
    pub(super) fn get(&self, destination: &PeerDestination) -> Option<Arc<C>> {
        self.directory.load().get(destination).cloned()
    }

    pub(super) fn values(&self) -> Vec<Arc<C>> {
        self.directory.load().values().cloned().collect()
    }

    /// Publish `connection` for `destination`, replacing any predecessor.
    ///
    /// Shutdown cancels and then sweeps the pool.  An insert that published
    /// after that sweep must not leave its connection behind, so the token is
    /// read again *after* publishing: either the sweep saw this entry, or
    /// this read sees the cancellation and takes the entry back out.
    pub(super) fn insert(
        &self,
        destination: PeerDestination,
        connection: Arc<C>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), PeerTransportError> {
        if cancel.is_cancelled() {
            return Err(PeerTransportError::Cancelled);
        }
        self.directory.rcu(|directory| {
            let mut next = HashMap::clone(directory);
            next.insert(destination.clone(), Arc::clone(&connection));
            next
        });
        if cancel.is_cancelled() {
            self.remove_if_same(&destination, &connection);
            return Err(PeerTransportError::Cancelled);
        }
        Ok(())
    }

    /// Unpublish `destination` only while it still holds `connection`.
    fn remove_if_same(&self, destination: &PeerDestination, connection: &Arc<C>) {
        loop {
            let current = self.directory.load_full();
            if !current
                .get(destination)
                .is_some_and(|found| Arc::ptr_eq(found, connection))
            {
                return;
            }
            let mut next = HashMap::clone(&current);
            next.remove(destination);
            if self.replace(&current, next) {
                return;
            }
        }
    }

    /// Unpublish `destination`.  Of several racing removers exactly one is
    /// handed the connection.
    pub(super) fn remove(&self, destination: &PeerDestination) -> Option<Arc<C>> {
        loop {
            let current = self.directory.load_full();
            current.get(destination)?;
            let mut next = HashMap::clone(&current);
            let removed = next.remove(destination);
            if self.replace(&current, next) {
                return removed;
            }
        }
    }

    /// Unpublish every connection `predicate` accepts.  Each one is handed
    /// to exactly one remover.
    pub(super) fn remove_matching(&self, predicate: impl Fn(&C) -> bool) -> Vec<Arc<C>> {
        loop {
            let current = self.directory.load_full();
            let mut removed = Vec::new();
            let mut next = HashMap::with_capacity(current.len());
            for (destination, connection) in current.iter() {
                if predicate(connection) {
                    removed.push(Arc::clone(connection));
                } else {
                    next.insert(destination.clone(), Arc::clone(connection));
                }
            }
            if removed.is_empty() || self.replace(&current, next) {
                return removed;
            }
        }
    }

    fn replace(
        &self,
        current: &Arc<HashMap<PeerDestination, Arc<C>>>,
        next: HashMap<PeerDestination, Arc<C>>,
    ) -> bool {
        let previous = self.directory.compare_and_swap(current, Arc::new(next));
        Arc::ptr_eq(&previous, current)
    }
}

/// One dial gate per destination.  The table holds only weak markers: a gate
/// lives exactly as long as the dialing caller or its waiters, so a
/// cancelled dial cannot retire a gate that still has a waiter.
pub(super) type DialLocks = ArcSwap<HashMap<PeerDestination, Weak<TurnLock<()>>>>;

/// The dial gate of `destination`, creating it if no caller holds one.
///
/// Two callers that both find no live gate each try to publish their own;
/// one publication wins and the other caller retries and receives the
/// winner's gate, so a destination never has two live gates.
pub(super) fn acquire_dial_entry<C>(
    locks: &DialLocks,
    pool: &PublishedPool<C>,
    destination: &PeerDestination,
    max_destinations: usize,
) -> Result<Arc<TurnLock<()>>, PeerTransportError> {
    loop {
        let current = locks.load_full();
        if let Some(gate) = current.get(destination).and_then(Weak::upgrade) {
            return Ok(gate);
        }
        let mut next = HashMap::clone(&current);
        if !next.contains_key(destination) && next.len() >= max_destinations {
            next.retain(|peer, gate| gate.strong_count() > 0 || pool.get(peer).is_some());
            if next.len() >= max_destinations {
                return Err(PeerTransportError::Capacity);
            }
        }
        let gate = Arc::new(TurnLock::new(()));
        next.insert(destination.clone(), Arc::downgrade(&gate));
        let previous = locks.compare_and_swap(&current, Arc::new(next));
        if Arc::ptr_eq(&previous, &current) {
            return Ok(gate);
        }
    }
}

/// A connection that still carries streams under a superseded local
/// identity.
pub(super) struct Draining<C> {
    pub(super) connection: Weak<C>,
    pub(super) local_generation: u64,
}

impl<C> Clone for Draining<C> {
    fn clone(&self) -> Self {
        Self {
            connection: Weak::clone(&self.connection),
            local_generation: self.local_generation,
        }
    }
}

/// What [`DrainSet::admit`] did with a connection.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum DrainAdmission {
    /// Newly added: this caller, and no other, starts its drain.
    Added,
    /// Already draining.
    Present,
    /// The set holds its maximum of live connections.
    Full,
}

/// The bounded set of draining connections.  No published value ever holds
/// more than `max` entries, whatever the interleaving of callers.
pub(super) struct DrainSet<C = ClientConnection> {
    entries: ArcSwap<Vec<Draining<C>>>,
}

impl<C> Default for DrainSet<C> {
    fn default() -> Self {
        Self {
            entries: ArcSwap::default(),
        }
    }
}

impl<C> DrainSet<C> {
    pub(super) fn admit(
        &self,
        connection: &Arc<C>,
        local_generation: u64,
        max: usize,
    ) -> DrainAdmission {
        loop {
            let current = self.entries.load_full();
            let mut next: Vec<Draining<C>> = current
                .iter()
                .filter(|entry| entry.connection.strong_count() > 0)
                .cloned()
                .collect();
            if next
                .iter()
                .any(|entry| std::ptr::eq(entry.connection.as_ptr(), Arc::as_ptr(connection)))
            {
                return DrainAdmission::Present;
            }
            if next.len() >= max {
                return DrainAdmission::Full;
            }
            next.push(Draining {
                connection: Arc::downgrade(connection),
                local_generation,
            });
            let previous = self.entries.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&previous, &current) {
                return DrainAdmission::Added;
            }
        }
    }

    /// Remove and return every live connection older than `generation`.
    /// Each is returned to exactly one caller.
    pub(super) fn retire_before(&self, generation: u64) -> Vec<Arc<C>> {
        loop {
            let current = self.entries.load_full();
            let mut retired = Vec::new();
            let mut kept = Vec::new();
            for entry in current.iter() {
                if entry.local_generation < generation {
                    retired.extend(entry.connection.upgrade());
                } else if entry.connection.strong_count() > 0 {
                    kept.push(entry.clone());
                }
            }
            if kept.len() == current.len() {
                return retired;
            }
            let previous = self.entries.compare_and_swap(&current, Arc::new(kept));
            if Arc::ptr_eq(&previous, &current) {
                return retired;
            }
        }
    }

    pub(super) fn live_count(&self) -> usize {
        self.entries
            .load()
            .iter()
            .filter(|entry| entry.connection.strong_count() > 0)
            .count()
    }
}

/// Add one entry to a bounded id-keyed table, first dropping entries that
/// `finished` accepts.  Returns `None` when the table is still full.
///
/// `create` runs at most once however often the publication is retried, so
/// an id it allocates is allocated once.
pub(super) fn register_bounded<V>(
    table: &ArcSwap<HashMap<usize, Arc<V>>>,
    max: usize,
    finished: impl Fn(&V) -> bool,
    create: impl FnOnce() -> (usize, Arc<V>),
) -> Option<Arc<V>> {
    let mut create = Some(create);
    let mut created: Option<(usize, Arc<V>)> = None;
    loop {
        let current = table.load_full();
        let mut next: HashMap<usize, Arc<V>> = current
            .iter()
            .filter(|(_, value)| !finished(value))
            .map(|(id, value)| (*id, Arc::clone(value)))
            .collect();
        if next.len() >= max {
            return None;
        }
        if created.is_none() {
            created = create.take().map(|create| create());
        }
        let (id, value) = created.as_ref()?;
        next.insert(*id, Arc::clone(value));
        let previous = table.compare_and_swap(&current, Arc::new(next));
        if Arc::ptr_eq(&previous, &current) {
            return Some(Arc::clone(value));
        }
    }
}

/// Remove one entry from an id-keyed table.
pub(super) fn unregister<V>(table: &ArcSwap<HashMap<usize, Arc<V>>>, id: usize) {
    table.rcu(|current| {
        if current.contains_key(&id) {
            let mut next = HashMap::clone(current);
            next.remove(&id);
            Arc::new(next)
        } else {
            Arc::clone(current)
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio_util::sync::CancellationToken;

    fn destination(port: u16) -> PeerDestination {
        PeerDestination::new(
            std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            "peer.test",
        )
    }

    /// Shutdown cancels and then sweeps.  Whatever the timing of a racing
    /// insert, nothing is pooled once both have returned, and a connection
    /// whose insert reported success was handed to the sweep.
    #[test]
    fn an_insert_racing_shutdown_never_outlives_the_sweep() {
        for round in 0..2_000_u64 {
            let pool = PublishedPool::<u64>::default();
            let cancel = CancellationToken::new();
            let inserters = 4_u64;
            let barrier = Barrier::new(inserters as usize + 1);
            let (inserted, swept) = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..inserters)
                    .map(|index| {
                        let (pool, cancel, barrier) = (&pool, &cancel, &barrier);
                        scope.spawn(move || {
                            barrier.wait();
                            pool.insert(
                                destination(index as u16 + 1),
                                Arc::new(round * 10 + index),
                                cancel,
                            )
                            .is_ok()
                            .then_some(round * 10 + index)
                        })
                    })
                    .collect();
                barrier.wait();
                cancel.cancel();
                let swept: Vec<u64> = pool
                    .remove_matching(|_| true)
                    .into_iter()
                    .map(|value| *value)
                    .collect();
                let inserted: Vec<u64> = handles
                    .into_iter()
                    .filter_map(|handle| handle.join().unwrap())
                    .collect();
                (inserted, swept)
            });
            assert!(
                pool.values().is_empty(),
                "a connection stayed pooled after shutdown's sweep"
            );
            for value in inserted {
                assert!(
                    swept.contains(&value),
                    "insert succeeded but shutdown never saw the connection"
                );
            }
        }
    }

    /// Racing removers -- by destination and by predicate -- between them
    /// receive every inserted connection exactly once.
    #[test]
    fn each_removed_connection_goes_to_exactly_one_remover() {
        const DESTINATIONS: u16 = 16;
        for _ in 0..500 {
            let pool = PublishedPool::<u16>::default();
            let cancel = CancellationToken::new();
            for port in 1..=DESTINATIONS {
                pool.insert(destination(port), Arc::new(port), &cancel)
                    .unwrap();
            }
            let barrier = Barrier::new(6);
            let mut removed: Vec<u16> = std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for _ in 0..3 {
                    let (pool, barrier) = (&pool, &barrier);
                    handles.push(scope.spawn(move || {
                        barrier.wait();
                        (1..=DESTINATIONS)
                            .filter_map(|port| pool.remove(&destination(port)))
                            .map(|value| *value)
                            .collect::<Vec<_>>()
                    }));
                }
                for parity in 0..3_u16 {
                    let (pool, barrier) = (&pool, &barrier);
                    handles.push(scope.spawn(move || {
                        barrier.wait();
                        pool.remove_matching(|value| value % 3 == parity)
                            .into_iter()
                            .map(|value| *value)
                            .collect::<Vec<_>>()
                    }));
                }
                handles
                    .into_iter()
                    .flat_map(|handle| handle.join().unwrap())
                    .collect()
            });
            removed.sort_unstable();
            assert_eq!(removed, (1..=DESTINATIONS).collect::<Vec<_>>());
            assert!(pool.values().is_empty());
        }
    }

    /// Many callers asking for one destination's gate at once all receive
    /// the same gate, and the destination cap holds for the others.
    #[test]
    fn racing_callers_share_one_dial_gate_and_the_cap_holds() {
        for _ in 0..500 {
            let locks = DialLocks::default();
            let pool = PublishedPool::<u8>::default();
            let barrier = Barrier::new(8);
            let gates: Vec<_> = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..8_u16)
                    .map(|index| {
                        let (locks, pool, barrier) = (&locks, &pool, &barrier);
                        scope.spawn(move || {
                            barrier.wait();
                            // Four callers share destination 1; the other
                            // four ask for four more with room for two.
                            let port = if index < 4 { 1 } else { index };
                            (port, acquire_dial_entry(locks, pool, &destination(port), 3))
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().unwrap())
                    .collect()
            });
            let shared: Vec<_> = gates
                .iter()
                .filter(|(port, _)| *port == 1)
                .filter_map(|(_, gate)| gate.as_ref().ok())
                .collect();
            for gate in &shared {
                assert!(Arc::ptr_eq(gate, shared[0]), "two live gates for one peer");
            }
            let live = gates.iter().filter(|(_, gate)| gate.is_ok()).count();
            assert!(locks.load().len() <= 3, "destination cap exceeded");
            // Destination 1 is granted unless the others filled the table
            // first; in every case at most three destinations hold a gate.
            let distinct: std::collections::HashSet<_> = gates
                .iter()
                .filter(|(_, gate)| gate.is_ok())
                .map(|(port, _)| *port)
                .collect();
            assert!(distinct.len() <= 3, "{distinct:?} of {live} live");
        }
    }

    /// The drain set never holds more than its maximum, a connection is
    /// added once, and a retired connection is returned to one caller.
    #[test]
    fn the_drain_bound_holds_under_concurrent_admission_and_retirement() {
        const MAX: usize = 4;
        for _ in 0..300 {
            let set = DrainSet::<usize>::default();
            let connections: Vec<Arc<usize>> = (0..12).map(Arc::new).collect();
            let added = AtomicUsize::new(0);
            let over = AtomicBool::new(false);
            let barrier = Barrier::new(9);
            let retired: Vec<usize> = std::thread::scope(|scope| {
                for thread in 0..6 {
                    let (set, connections, added, over, barrier) =
                        (&set, &connections, &added, &over, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        for step in 0..connections.len() {
                            // Every connection is offered by several threads.
                            let connection = &connections[(step + thread) % connections.len()];
                            if set.admit(connection, 1, MAX) == DrainAdmission::Added {
                                added.fetch_add(1, Ordering::SeqCst);
                            }
                            if set.entries.load().len() > MAX || set.live_count() > MAX {
                                over.store(true, Ordering::SeqCst);
                            }
                        }
                    });
                }
                let handles: Vec<_> = (0..3)
                    .map(|_| {
                        let (set, barrier) = (&set, &barrier);
                        scope.spawn(move || {
                            barrier.wait();
                            let mut retired = Vec::new();
                            for _ in 0..20 {
                                retired.extend(set.retire_before(2).into_iter().map(|c| *c));
                                std::thread::yield_now();
                            }
                            retired
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|handle| handle.join().unwrap())
                    .collect()
            });
            assert!(!over.load(Ordering::SeqCst), "more than {MAX} draining");
            let mut retired = retired;
            retired.extend(set.retire_before(2).into_iter().map(|c| *c));
            // Every successful admission is retired exactly once.
            assert_eq!(retired.len(), added.load(Ordering::SeqCst));
        }
    }

    /// The table never exceeds its maximum and `create` runs once per
    /// successful registration even when the publication is retried.
    #[test]
    fn a_bounded_table_keeps_its_maximum_and_creates_each_entry_once() {
        const MAX: usize = 5;
        for _ in 0..300 {
            let table: ArcSwap<HashMap<usize, Arc<AtomicBool>>> = ArcSwap::default();
            let next_id = AtomicUsize::new(1);
            let created = AtomicUsize::new(0);
            let over = AtomicBool::new(false);
            let barrier = Barrier::new(8);
            let registered: usize = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..8)
                    .map(|_| {
                        let (table, next_id, created, over, barrier) =
                            (&table, &next_id, &created, &over, &barrier);
                        scope.spawn(move || {
                            barrier.wait();
                            let mut registered = 0;
                            for step in 0..40 {
                                let mut id = 0;
                                let mut calls = 0;
                                let entry = register_bounded(
                                    table,
                                    MAX,
                                    |closed: &AtomicBool| closed.load(Ordering::SeqCst),
                                    || {
                                        calls += 1;
                                        created.fetch_add(1, Ordering::SeqCst);
                                        id = next_id.fetch_add(1, Ordering::SeqCst);
                                        (id, Arc::new(AtomicBool::new(false)))
                                    },
                                );
                                assert!(calls <= 1, "create ran {calls} times");
                                if table.load().len() > MAX {
                                    over.store(true, Ordering::SeqCst);
                                }
                                if let Some(entry) = entry {
                                    assert_eq!(calls, 1);
                                    registered += 1;
                                    assert!(table.load().contains_key(&id));
                                    if step % 2 == 0 {
                                        unregister(table, id);
                                    } else {
                                        entry.store(true, Ordering::SeqCst);
                                    }
                                }
                            }
                            registered
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().unwrap())
                    .sum()
            });
            assert!(!over.load(Ordering::SeqCst), "more than {MAX} entries");
            // A retry that finds the table full after creating leaves an
            // unused id; an id is never handed out twice.
            assert!(created.load(Ordering::SeqCst) >= registered);
            assert_eq!(
                next_id.load(Ordering::SeqCst) - 1,
                created.load(Ordering::SeqCst)
            );
        }
    }
}
