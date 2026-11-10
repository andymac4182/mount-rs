//! Reader-writer exclusion with no lock inside it.
//!
//! Ordinary operations enter shared; the rare transitions that must drain
//! them (shutdown, close, cursor moves, flushes) enter exclusive.
//! General-purpose async reader-writer locks keep their state behind an
//! internal mutex, which a lock shared by every operation turns into a
//! contention point on every core. Here shared entry and exit are an atomic
//! increment and decrement while no transition is queued or running, and a
//! transition is a turn at a lock-free queue followed by a wait for the
//! shared holders to leave. Nothing in the gate blocks a thread.
//!
//! A gate that every core enters on every operation can spread its counter
//! over `N` stripes (`DrainGate<N>`), each on cache lines of its own. A
//! thread always uses the same stripe, so cores stop passing one counter's
//! cache line between them; a transition waits for every stripe to empty.
//! The default is one stripe, which suits a gate with few concurrent users.
//!
//! Exclusive entry is first-come-first-served and takes priority: once a
//! transition is queued, new shared entries wait until every queued
//! transition has finished. Both futures are cancellation-safe.
use crate::turn_gate::{TurnGate, TurnWait};
use atomic_waker::AtomicWaker;
use crossbeam_queue::SegQueue;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

/// Every shared entry and exit writes this counter. Keeping it on cache lines
/// of its own stops those writes invalidating whatever the gate's owner
/// stores next to it, which readers on other cores are loading.
#[derive(Default)]
#[repr(align(128))]
struct Active(AtomicUsize);

static NEXT_THREAD: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    /// Which stripe this thread counts itself in, on every striped gate.
    static THREAD: usize = NEXT_THREAD.fetch_add(1, Ordering::Relaxed);
}

pub struct DrainGate<const N: usize = 1> {
    /// Shared holders admitted and not yet finished, by stripe. A holder
    /// leaves through the stripe it entered, so no stripe goes negative.
    active: [Active; N],
    /// Transitions queued or owning the gate. Shared entry waits while this
    /// is not zero.
    draining: AtomicUsize,
    /// Transitions take the gate one at a time, in arrival order.
    turn: TurnGate,
    /// The transition that has its turn, waiting for shared holders to leave.
    drain: AtomicWaker,
    /// Shared entries waiting for the transitions to finish.
    shared: SegQueue<Waker>,
}

/// Shared admission; released on drop.
pub struct DrainShared<'a, const N: usize = 1>(&'a DrainGate<N>, usize);

/// Exclusive admission; released on drop.
pub struct DrainExclusive<'a, const N: usize = 1>(&'a DrainGate<N>);

/// Exclusive admission that keeps its gate alive; released on drop.
pub struct OwnedDrainExclusive<const N: usize = 1>(Arc<DrainGate<N>>);

/// Shared admission that keeps its gate alive; released on drop.
pub struct OwnedDrainShared<const N: usize = 1>(Arc<DrainGate<N>>, usize);

impl<const N: usize> Default for DrainGate<N> {
    fn default() -> Self {
        Self {
            active: std::array::from_fn(|_| Active::default()),
            draining: AtomicUsize::new(0),
            turn: TurnGate::new(),
            drain: AtomicWaker::new(),
            shared: SegQueue::new(),
        }
    }
}

impl DrainGate {
    /// A gate with one counter.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<const N: usize> DrainGate<N> {
    /// A gate with `N` counters, for one that every core enters constantly.
    pub fn striped() -> Self {
        Self::default()
    }

    #[inline]
    fn stripe() -> usize {
        if N == 1 {
            0
        } else {
            THREAD.with(|thread| *thread) % N
        }
    }

    /// Whether any shared holder remains. Only meaningful while `draining`
    /// is set, which stops new holders from staying in a stripe already
    /// seen empty.
    fn occupied(&self) -> bool {
        self.active
            .iter()
            .any(|stripe| stripe.0.load(Ordering::SeqCst) != 0)
    }

    /// Enter shared. Resolves immediately unless a transition is in progress.
    #[inline]
    pub fn read(&self) -> DrainRead<'_, N> {
        DrainRead(self)
    }

    /// Enter shared only if that needs no waiting.
    #[inline]
    pub fn try_read(&self) -> Option<DrainShared<'_, N>> {
        self.try_shared().map(|stripe| DrainShared(self, stripe))
    }

    /// Enter exclusive once earlier transitions and all shared holders are done.
    pub fn write(&self) -> DrainWrite<'_, N> {
        DrainWrite {
            gate: self,
            stage: Stage::New,
        }
    }

    /// Enter exclusive only if nothing holds or awaits the gate.
    pub fn try_write(&self) -> Option<DrainExclusive<'_, N>> {
        // Lazily: an unused guard would release a gate it never held.
        self.try_acquire().then(|| DrainExclusive(self))
    }

    /// `read` for a guard that must outlive the borrow, such as one moved
    /// into a blocking worker.
    pub async fn read_owned(self: Arc<Self>) -> OwnedDrainShared<N> {
        let shared = self.read().await;
        let stripe = shared.1;
        // The owned guard takes over the admission.
        std::mem::forget(shared);
        OwnedDrainShared(self, stripe)
    }

    /// `write` for a guard that must outlive the borrow.
    pub async fn write_owned(self: Arc<Self>) -> OwnedDrainExclusive<N> {
        std::mem::forget(self.write().await);
        OwnedDrainExclusive(self)
    }

    /// `try_write` for a gate that must outlive the borrow.
    pub fn try_write_owned(self: &Arc<Self>) -> Option<OwnedDrainExclusive<N>> {
        self.try_acquire()
            .then(|| OwnedDrainExclusive(Arc::clone(self)))
    }

    fn try_acquire(&self) -> bool {
        // The turn is free only when no transition holds or awaits the gate.
        if !self.turn.try_acquire() {
            return false;
        }
        self.draining.fetch_add(1, Ordering::SeqCst);
        if self.occupied() {
            self.release();
            return false;
        }
        true
    }

    #[inline]
    fn try_shared(&self) -> Option<usize> {
        // Announce before checking: a transition raises `draining` and then
        // reads every stripe, so one side always observes the other.
        let stripe = Self::stripe();
        self.active[stripe].0.fetch_add(1, Ordering::SeqCst);
        if self.draining.load(Ordering::SeqCst) == 0 {
            return Some(stripe);
        }
        self.leave(stripe);
        None
    }

    #[inline]
    fn leave(&self, stripe: usize) {
        // The last holder of each stripe wakes the transition, which looks
        // at all of them again.
        if self.active[stripe].0.fetch_sub(1, Ordering::SeqCst) == 1
            && self.draining.load(Ordering::SeqCst) != 0
        {
            self.wake_drain();
        }
    }

    #[cold]
    fn wake_drain(&self) {
        self.drain.wake();
    }

    /// One transition fewer is queued or running. The last one re-opens
    /// shared entry.
    fn settle(&self) {
        if self.draining.fetch_sub(1, Ordering::SeqCst) == 1 {
            // A shared entry queues its waker and then looks at `draining`
            // again; this side lowers `draining` and then empties the queue.
            // One of the two sees the other.
            while let Some(waker) = self.shared.pop() {
                waker.wake();
            }
        }
    }

    /// Leave exclusive: the next queued transition gets its turn, or shared
    /// entry re-opens.
    fn release(&self) {
        drop(self.drain.take());
        self.turn.release();
        self.settle();
    }
}

pub struct DrainRead<'a, const N: usize = 1>(&'a DrainGate<N>);

impl<'a, const N: usize> Future for DrainRead<'a, N> {
    type Output = DrainShared<'a, N>;

    #[inline]
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let gate = self.0;
        if let Some(stripe) = gate.try_shared() {
            return Poll::Ready(DrainShared(gate, stripe));
        }
        gate.poll_shared_slow(context)
    }
}

impl<const N: usize> DrainGate<N> {
    #[cold]
    fn poll_shared_slow<'a>(&'a self, context: &mut Context<'_>) -> Poll<DrainShared<'a, N>> {
        loop {
            self.shared.push(context.waker().clone());
            // The transitions may have finished before the push, in which
            // case nobody is left to wake this task: look again.
            if self.draining.load(Ordering::SeqCst) != 0 {
                return Poll::Pending;
            }
            if let Some(stripe) = self.try_shared() {
                // The queued waker stays behind; the next re-opening wakes it
                // once for nothing.
                return Poll::Ready(DrainShared(self, stripe));
            }
        }
    }
}

enum Stage<'a> {
    New,
    /// Counted in `draining`, waiting for the turn.
    Queued(TurnWait<'a>),
    /// Has the turn, waiting for shared holders to leave.
    Draining,
    Done,
}

pub struct DrainWrite<'a, const N: usize = 1> {
    gate: &'a DrainGate<N>,
    stage: Stage<'a>,
}

impl<'a, const N: usize> Future for DrainWrite<'a, N> {
    type Output = DrainExclusive<'a, N>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let gate = self.gate;
        if matches!(self.stage, Stage::New) {
            // From here shared entry waits, so this transition cannot starve.
            gate.draining.fetch_add(1, Ordering::SeqCst);
            self.stage = Stage::Queued(gate.turn.lock());
        }
        if let Stage::Queued(wait) = &mut self.stage {
            match Pin::new(wait).poll(context) {
                // The turn is given back by `release`, not by a guard.
                Poll::Ready(turn) => std::mem::forget(turn),
                Poll::Pending => return Poll::Pending,
            }
            self.stage = Stage::Draining;
        }
        // Register before checking, so a holder leaving now wakes this task.
        gate.drain.register(context.waker());
        if gate.occupied() {
            return Poll::Pending;
        }
        drop(gate.drain.take());
        self.stage = Stage::Done;
        Poll::Ready(DrainExclusive(gate))
    }
}

impl<const N: usize> Drop for DrainWrite<'_, N> {
    fn drop(&mut self) {
        match std::mem::replace(&mut self.stage, Stage::Done) {
            Stage::New | Stage::Done => {}
            Stage::Draining => self.gate.release(),
            Stage::Queued(wait) => {
                // Dropping the wait leaves the queue, or passes the turn on
                // if it was already handed over.
                drop(wait);
                self.gate.settle();
            }
        }
    }
}

impl<const N: usize> Drop for DrainShared<'_, N> {
    #[inline]
    fn drop(&mut self) {
        self.0.leave(self.1);
    }
}

impl<const N: usize> Drop for DrainExclusive<'_, N> {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl<const N: usize> std::fmt::Debug for OwnedDrainShared<N> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OwnedDrainShared")
    }
}

impl<const N: usize> std::fmt::Debug for OwnedDrainExclusive<N> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OwnedDrainExclusive")
    }
}

impl<const N: usize> Drop for OwnedDrainShared<N> {
    fn drop(&mut self) {
        self.0.leave(self.1);
    }
}

impl<const N: usize> Drop for OwnedDrainExclusive<N> {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, atomic::AtomicBool};
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
        assert_eq!(gate.active[0].0.load(Ordering::SeqCst), 0);
        assert_eq!(gate.draining.load(Ordering::SeqCst), 0);
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
        assert_eq!(gate.draining.load(Ordering::SeqCst), 0);
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
        assert_eq!(gate.active[0].0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn striped_gate_drains_holders_from_every_thread() {
        let gate = Arc::new(DrainGate::<4>::striped());
        let (entered, wait) = std::sync::mpsc::channel();
        let (leave, left) = std::sync::mpsc::channel::<()>();
        let left = Arc::new(Mutex::new(left));
        let holders: Vec<_> = (0..6)
            .map(|_| {
                let gate = Arc::clone(&gate);
                let entered = entered.clone();
                let left = Arc::clone(&left);
                std::thread::spawn(move || {
                    let held = gate.try_read().unwrap();
                    entered.send(()).unwrap();
                    let _ = left.lock().unwrap().recv();
                    drop(held);
                })
            })
            .collect();
        for _ in 0..6 {
            wait.recv().unwrap();
        }
        // Holders sit in several stripes; the transition waits for all.
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let waker = Waker::from(Arc::clone(&flag));
        let mut context = Context::from_waker(&waker);
        let mut write = Box::pin(gate.write());
        assert!(write.as_mut().poll(&mut context).is_pending());
        assert!(gate.try_read().is_none());
        for remaining in (0..6).rev() {
            leave.send(()).unwrap();
            while gate
                .active
                .iter()
                .map(|s| s.0.load(Ordering::SeqCst))
                .sum::<usize>()
                != remaining
            {
                std::thread::yield_now();
            }
            if remaining != 0 {
                assert!(write.as_mut().poll(&mut context).is_pending());
            }
        }
        for holder in holders {
            holder.join().unwrap();
        }
        let exclusive = match write.as_mut().poll(&mut context) {
            Poll::Ready(exclusive) => exclusive,
            Poll::Pending => panic!("every stripe is empty"),
        };
        assert!(gate.try_read().is_none());
        drop(exclusive);
        assert!(gate.try_read().is_some());
    }

    #[test]
    fn owned_guards_hold_the_gate_until_dropped() {
        let gate = Arc::new(DrainGate::new());
        let shared = ready(Box::pin(Arc::clone(&gate).read_owned()));
        assert!(gate.try_read().is_some());
        assert!(gate.try_write().is_none());
        drop(shared);
        let exclusive = ready(Box::pin(Arc::clone(&gate).write_owned()));
        assert!(gate.try_read().is_none());
        drop(exclusive);
        assert!(gate.try_write().is_some());
    }
}
