//! Reader-writer exclusion whose shared side is one atomic counter.
//!
//! Ordinary operations enter shared; the rare transitions that must drain
//! them (shutdown, close, cursor moves) enter exclusive. General-purpose
//! async reader-writer locks take an internal mutex when a shared guard is
//! released, so a lock shared by every operation becomes a contention point
//! on every core. Here shared entry and exit are an atomic increment and
//! decrement while no transition is queued or running; the mutex below is
//! only touched around transitions.
//!
//! Exclusive entry is first-come-first-served and takes priority: once a
//! transition is queued, new shared entries wait until every queued
//! transition has finished. Both futures are cancellation-safe.
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

#[derive(Default)]
struct Transitions {
    /// A transition owns the gate (it may still be draining shared holders).
    held: bool,
    next_ticket: u64,
    /// Transitions waiting their turn, oldest first.
    queued: VecDeque<(u64, Waker)>,
    /// Shared entries waiting for the transitions to finish.
    shared: Vec<Waker>,
    /// The owning transition, waiting for shared holders to leave.
    drain: Option<Waker>,
}

/// Every shared entry and exit writes this counter. Keeping it on cache lines
/// of its own stops those writes invalidating whatever the gate's owner
/// stores next to it, which readers on other cores are loading.
#[derive(Default)]
#[repr(align(128))]
struct Active(AtomicUsize);

#[derive(Default)]
pub struct DrainGate {
    /// Shared holders admitted and not yet finished.
    active: Active,
    /// True while a transition is queued or owns the gate. Written only
    /// under `transitions`.
    draining: AtomicBool,
    transitions: Mutex<Transitions>,
}

/// Shared admission; released on drop.
pub struct DrainShared<'a>(&'a DrainGate);

/// Exclusive admission; released on drop.
pub struct DrainExclusive<'a>(&'a DrainGate);

/// Exclusive admission that keeps its gate alive; released on drop.
pub struct OwnedDrainExclusive(Arc<DrainGate>);

impl DrainGate {
    pub fn new() -> Self {
        Self::default()
    }

    fn transitions(&self) -> MutexGuard<'_, Transitions> {
        // No code panics while holding this lock; recover the state anyway.
        self.transitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Enter shared. Resolves immediately unless a transition is in progress.
    #[inline]
    pub fn read(&self) -> DrainRead<'_> {
        DrainRead(self)
    }

    /// Enter exclusive once earlier transitions and all shared holders are done.
    pub fn write(&self) -> DrainWrite<'_> {
        DrainWrite {
            gate: self,
            stage: Stage::New,
        }
    }

    /// Enter exclusive only if nothing holds or awaits the gate.
    pub fn try_write(&self) -> Option<DrainExclusive<'_>> {
        // Lazily: an unused guard would release a gate it never held.
        self.try_acquire().then(|| DrainExclusive(self))
    }

    /// `try_write` for a gate that must outlive the borrow.
    pub fn try_write_owned(self: &Arc<Self>) -> Option<OwnedDrainExclusive> {
        self.try_acquire()
            .then(|| OwnedDrainExclusive(Arc::clone(self)))
    }

    fn try_acquire(&self) -> bool {
        let mut transitions = self.transitions();
        if transitions.held || !transitions.queued.is_empty() {
            return false;
        }
        self.draining.store(true, Ordering::SeqCst);
        if self.active.0.load(Ordering::SeqCst) != 0 {
            self.draining.store(false, Ordering::SeqCst);
            let waiting = std::mem::take(&mut transitions.shared);
            drop(transitions);
            waiting.into_iter().for_each(Waker::wake);
            return false;
        }
        transitions.held = true;
        true
    }

    #[inline]
    fn try_shared(&self) -> bool {
        // Announce before checking: a transition sets `draining` and then
        // reads `active`, so one side always observes the other.
        self.active.0.fetch_add(1, Ordering::SeqCst);
        if !self.draining.load(Ordering::SeqCst) {
            return true;
        }
        self.leave();
        false
    }

    #[inline]
    fn leave(&self) {
        if self.active.0.fetch_sub(1, Ordering::SeqCst) == 1 && self.draining.load(Ordering::SeqCst)
        {
            self.wake_drain();
        }
    }

    #[cold]
    fn wake_drain(&self) {
        let waker = self.transitions().drain.take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Hand the gate to the next queued transition, or re-open shared entry.
    fn pass_on(&self, mut transitions: MutexGuard<'_, Transitions>) {
        if let Some((_, next)) = transitions.queued.front() {
            let next = next.clone();
            drop(transitions);
            next.wake();
            return;
        }
        self.draining.store(false, Ordering::SeqCst);
        let waiting = std::mem::take(&mut transitions.shared);
        drop(transitions);
        waiting.into_iter().for_each(Waker::wake);
    }

    fn release(&self) {
        let mut transitions = self.transitions();
        transitions.held = false;
        transitions.drain = None;
        self.pass_on(transitions);
    }
}

pub struct DrainRead<'a>(&'a DrainGate);

impl<'a> Future for DrainRead<'a> {
    type Output = DrainShared<'a>;

    #[inline]
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let gate = self.0;
        if gate.try_shared() {
            return Poll::Ready(DrainShared(gate));
        }
        gate.poll_shared_slow(context)
    }
}

impl DrainGate {
    #[cold]
    fn poll_shared_slow<'a>(&'a self, context: &mut Context<'_>) -> Poll<DrainShared<'a>> {
        let gate = self;
        let mut transitions = gate.transitions();
        // `draining` only changes under this lock, so the answer is stable.
        if !gate.draining.load(Ordering::SeqCst) {
            gate.active.0.fetch_add(1, Ordering::SeqCst);
            return Poll::Ready(DrainShared(gate));
        }
        transitions.shared.push(context.waker().clone());
        Poll::Pending
    }
}

enum Stage {
    New,
    Queued(u64),
    Draining,
    Done,
}

pub struct DrainWrite<'a> {
    gate: &'a DrainGate,
    stage: Stage,
}

impl<'a> Future for DrainWrite<'a> {
    type Output = DrainExclusive<'a>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let gate = self.gate;
        let mut transitions = gate.transitions();
        if matches!(self.stage, Stage::New) {
            let ticket = transitions.next_ticket;
            transitions.next_ticket += 1;
            transitions
                .queued
                .push_back((ticket, context.waker().clone()));
            // From here shared entry waits, so this transition cannot starve.
            gate.draining.store(true, Ordering::SeqCst);
            self.stage = Stage::Queued(ticket);
        }
        if let Stage::Queued(ticket) = self.stage {
            let first = transitions.queued.front().map(|(first, _)| *first);
            if transitions.held || first != Some(ticket) {
                if let Some(entry) = transitions
                    .queued
                    .iter_mut()
                    .find(|(queued, _)| *queued == ticket)
                {
                    entry.1.clone_from(context.waker());
                }
                return Poll::Pending;
            }
            transitions.queued.pop_front();
            transitions.held = true;
            self.stage = Stage::Draining;
        }
        // Register before checking, so a holder leaving now wakes this task.
        transitions.drain = Some(context.waker().clone());
        if gate.active.0.load(Ordering::SeqCst) != 0 {
            return Poll::Pending;
        }
        transitions.drain = None;
        self.stage = Stage::Done;
        Poll::Ready(DrainExclusive(gate))
    }
}

impl Drop for DrainWrite<'_> {
    fn drop(&mut self) {
        match self.stage {
            Stage::New | Stage::Done => {}
            Stage::Draining => self.gate.release(),
            Stage::Queued(ticket) => {
                let mut transitions = self.gate.transitions();
                transitions.queued.retain(|(queued, _)| *queued != ticket);
                if !transitions.held {
                    self.gate.pass_on(transitions);
                }
            }
        }
    }
}

impl Drop for DrainShared<'_> {
    #[inline]
    fn drop(&mut self) {
        self.0.leave();
    }
}

impl Drop for DrainExclusive<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl Drop for OwnedDrainExclusive {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Wake;

    struct Flag(AtomicBool);
    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn poll<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
        let waker = Waker::from(Arc::new(Flag(AtomicBool::new(false))));
        Pin::new(future).poll(&mut Context::from_waker(&waker))
    }

    fn ready<F: Future + Unpin>(mut future: F) -> F::Output {
        match poll(&mut future) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("future must be ready"),
        }
    }

    #[test]
    fn transitions_drain_shared_holders_and_queue_new_ones() {
        let gate = DrainGate::new();
        let admitted = ready(gate.read());
        let overlapping = ready(gate.read());
        let mut transition = gate.write();
        assert!(poll(&mut transition).is_pending());
        // Shared entry arriving during the drain waits for the transition.
        let mut late = gate.read();
        assert!(poll(&mut late).is_pending());
        assert!(gate.try_write().is_none());
        drop(admitted);
        assert!(poll(&mut transition).is_pending());
        drop(overlapping);
        let exclusive = ready(&mut transition);
        assert!(poll(&mut late).is_pending());
        drop(exclusive);
        drop(ready(&mut late));
        drop(transition);
        assert_eq!(gate.active.0.load(Ordering::SeqCst), 0);
        assert!(!gate.draining.load(Ordering::SeqCst));
    }

    #[test]
    fn transitions_run_in_arrival_order_and_wake_their_successors() {
        let gate = DrainGate::new();
        let first = ready(gate.write());
        let woken = Arc::new(Flag(AtomicBool::new(false)));
        let waker = Waker::from(woken.clone());
        let mut second = gate.write();
        assert!(
            Pin::new(&mut second)
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        let mut third = gate.write();
        assert!(poll(&mut third).is_pending());
        let mut reader = gate.read();
        assert!(poll(&mut reader).is_pending());
        drop(first);
        assert!(woken.0.load(Ordering::SeqCst));
        // Readers stay behind every queued transition.
        assert!(poll(&mut reader).is_pending());
        assert!(poll(&mut third).is_pending());
        drop(ready(&mut second));
        drop(ready(&mut third));
        drop(ready(&mut reader));
    }

    #[test]
    fn cancelled_transitions_release_their_place() {
        let gate = Arc::new(DrainGate::new());
        let admitted = ready(gate.read());
        // Cancelled while draining.
        let mut draining = gate.write();
        assert!(poll(&mut draining).is_pending());
        drop(draining);
        drop(ready(gate.read()));
        // Cancelled while queued behind another transition.
        drop(admitted);
        let owner = gate.try_write_owned().unwrap();
        let mut queued = gate.write();
        assert!(poll(&mut queued).is_pending());
        let mut reader = gate.read();
        assert!(poll(&mut reader).is_pending());
        drop(queued);
        assert!(poll(&mut reader).is_pending());
        drop(owner);
        drop(ready(&mut reader));
        // Cancelled at the head of the queue after the owner left.
        let owner = gate.try_write().unwrap();
        let mut head = gate.write();
        assert!(poll(&mut head).is_pending());
        let mut next = gate.write();
        assert!(poll(&mut next).is_pending());
        drop(owner);
        drop(head);
        drop(ready(&mut next));
        drop(ready(gate.read()));
        assert!(!gate.draining.load(Ordering::SeqCst));
    }

    #[test]
    fn concurrent_shared_and_exclusive_holders_never_overlap() {
        let gate = Arc::new(DrainGate::new());
        let inside = Arc::new(AtomicUsize::new(0));
        let exclusive = Arc::new(AtomicBool::new(false));
        let threads: Vec<_> = (0..8)
            .map(|index| {
                let gate = gate.clone();
                let inside = inside.clone();
                let exclusive = exclusive.clone();
                std::thread::spawn(move || {
                    for round in 0..20_000 {
                        if index == 0 && round % 16 == 0 {
                            let mut write = gate.write();
                            let guard = loop {
                                if let Poll::Ready(guard) = poll(&mut write) {
                                    break guard;
                                }
                                std::thread::yield_now();
                            };
                            exclusive.store(true, Ordering::SeqCst);
                            assert_eq!(inside.load(Ordering::SeqCst), 0);
                            exclusive.store(false, Ordering::SeqCst);
                            drop(guard);
                        } else {
                            let mut read = gate.read();
                            let guard = loop {
                                if let Poll::Ready(guard) = poll(&mut read) {
                                    break guard;
                                }
                                std::thread::yield_now();
                            };
                            inside.fetch_add(1, Ordering::SeqCst);
                            assert!(!exclusive.load(Ordering::SeqCst));
                            inside.fetch_sub(1, Ordering::SeqCst);
                            drop(guard);
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(gate.active.0.load(Ordering::SeqCst), 0);
    }
}
