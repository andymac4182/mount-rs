//! Asynchronous mutual exclusion with no lock inside it.
//!
//! General-purpose async mutexes keep their waiters in a list behind an
//! ordinary mutex, which every release takes. Here the gate is one atomic
//! flag and a lock-free queue of waiters: taking a free gate is a single
//! compare-and-swap, and releasing one that nobody waits for is a store and
//! a look at the queue.
//!
//! A release hands the gate straight to the oldest waiter that is still
//! waiting, so waiters are served in arrival order. A caller that finds the
//! gate free takes it without queueing. The future is cancellation-safe: a
//! waiter dropped after it was handed the gate passes it on.
use atomic_waker::AtomicWaker;
use crossbeam_queue::SegQueue;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    task::{Context, Poll},
};

const WAITING: u8 = 0;
const GRANTED: u8 = 1;
const CANCELLED: u8 = 2;

struct Waiter {
    state: AtomicU8,
    waker: AtomicWaker,
}

#[derive(Default)]
pub struct TurnGate {
    held: AtomicBool,
    waiters: SegQueue<Arc<Waiter>>,
}

/// Exclusive admission; released on drop.
pub struct Turn<'a>(&'a TurnGate);

impl TurnGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the gate, waiting for whoever holds it.
    pub fn lock(&self) -> TurnWait<'_> {
        TurnWait {
            gate: self,
            waiter: None,
            done: false,
        }
    }

    /// Take the gate only if it is free.
    pub fn try_lock(&self) -> Option<Turn<'_>> {
        self.try_acquire().then(|| Turn(self))
    }

    pub(crate) fn try_acquire(&self) -> bool {
        self.held
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Give up the gate. For a holder that took it with `acquire_raw` or
    /// forgot its `Turn`; everyone else releases by dropping the guard.
    pub(crate) fn release(&self) {
        loop {
            // Hand the gate, still held, to the oldest waiter left waiting.
            while let Some(waiter) = self.waiters.pop() {
                if waiter
                    .state
                    .compare_exchange(WAITING, GRANTED, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    waiter.waker.wake();
                    return;
                }
            }
            self.held.store(false, Ordering::SeqCst);
            // A waiter queues and then tries the gate; this side frees the
            // gate and then looks at the queue. One of the two sees the
            // other, so a waiter that queued in between is not stranded.
            if self.waiters.is_empty() || !self.try_acquire() {
                return;
            }
        }
    }
}

pub struct TurnWait<'a> {
    gate: &'a TurnGate,
    waiter: Option<Arc<Waiter>>,
    done: bool,
}

impl<'a> Future for TurnWait<'a> {
    type Output = Turn<'a>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let gate = self.gate;
        let waiter = match &self.waiter {
            Some(waiter) => {
                waiter.waker.register(context.waker());
                Arc::clone(waiter)
            }
            None => {
                if gate.try_acquire() {
                    self.done = true;
                    return Poll::Ready(Turn(gate));
                }
                let waiter = Arc::new(Waiter {
                    state: AtomicU8::new(WAITING),
                    waker: AtomicWaker::new(),
                });
                waiter.waker.register(context.waker());
                gate.waiters.push(Arc::clone(&waiter));
                self.waiter = Some(Arc::clone(&waiter));
                // The holder may have released between the attempt above and
                // the push. Holding the gate now, nobody can grant this
                // waiter, so withdrawing it cannot fail.
                if gate.try_acquire() {
                    let withdrawn = waiter.state.compare_exchange(
                        WAITING,
                        CANCELLED,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                    debug_assert!(withdrawn.is_ok());
                    self.done = true;
                    return Poll::Ready(Turn(gate));
                }
                waiter
            }
        };
        // The waker is in place: a grant from here on wakes this task, and
        // one made before it is seen now.
        if waiter.state.load(Ordering::SeqCst) == GRANTED {
            self.done = true;
            return Poll::Ready(Turn(gate));
        }
        Poll::Pending
    }
}

impl Drop for TurnWait<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        if let Some(waiter) = &self.waiter
            && waiter
                .state
                .compare_exchange(WAITING, CANCELLED, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            // Handed the gate and never took it: pass it on.
            self.gate.release();
        }
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// A value behind a `TurnGate`: an asynchronous mutex with no lock inside
/// it. The guard may be held across an await; waiters hold no thread and are
/// served in arrival order.
pub struct TurnLock<T> {
    gate: TurnGate,
    value: std::cell::UnsafeCell<T>,
}

// SAFETY: `value` is only reached through `TurnLockGuard`, and the gate
// admits one guard at a time.
unsafe impl<T: Send> Send for TurnLock<T> {}
unsafe impl<T: Send> Sync for TurnLock<T> {}

pub struct TurnLockGuard<'a, T> {
    lock: &'a TurnLock<T>,
    _turn: Turn<'a>,
}

// SAFETY: the guard is exclusive access to a `T`, like `&mut T`.
unsafe impl<T: Send> Send for TurnLockGuard<'_, T> {}
unsafe impl<T: Send + Sync> Sync for TurnLockGuard<'_, T> {}

impl<T: Default> Default for TurnLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> TurnLock<T> {
    pub fn new(value: T) -> Self {
        Self {
            gate: TurnGate::new(),
            value: std::cell::UnsafeCell::new(value),
        }
    }

    pub async fn lock(&self) -> TurnLockGuard<'_, T> {
        let turn = self.gate.lock().await;
        TurnLockGuard {
            lock: self,
            _turn: turn,
        }
    }

    pub fn try_lock(&self) -> Option<TurnLockGuard<'_, T>> {
        self.gate.try_lock().map(|turn| TurnLockGuard {
            lock: self,
            _turn: turn,
        })
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }

    /// `lock`, with a guard that owns its share of the lock and can move to
    /// another thread, such as a blocking worker.
    pub async fn lock_owned(self: Arc<Self>) -> OwnedTurnLockGuard<T> {
        std::mem::forget(self.gate.lock().await);
        OwnedTurnLockGuard(self)
    }
}

/// Holds the gate, which it gives up on drop.
pub struct OwnedTurnLockGuard<T>(Arc<TurnLock<T>>);

// SAFETY: the guard is exclusive access to a `T`, like `&mut T`.
unsafe impl<T: Send> Send for OwnedTurnLockGuard<T> {}
unsafe impl<T: Send + Sync> Sync for OwnedTurnLockGuard<T> {}

impl<T> std::ops::Deref for OwnedTurnLockGuard<T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: this guard holds the gate, so it is the only one.
        unsafe { &*self.0.value.get() }
    }
}

impl<T> std::ops::DerefMut for OwnedTurnLockGuard<T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`, and `&mut self` excludes other borrows.
        unsafe { &mut *self.0.value.get() }
    }
}

impl<T> Drop for OwnedTurnLockGuard<T> {
    fn drop(&mut self) {
        self.0.gate.release();
    }
}

impl<T> std::ops::Deref for TurnLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: this guard holds the gate, so it is the only one.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> std::ops::DerefMut for TurnLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`, and `&mut self` excludes other borrows.
        unsafe { &mut *self.lock.value.get() }
    }
}

/// Set once by whoever did a waiter's work for it, so the waiter can stop
/// waiting for its own turn. See `TurnGate::lock_unless`.
#[derive(Default)]
pub struct Served {
    done: AtomicBool,
    waker: AtomicWaker,
}

impl Served {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark(&self) {
        self.done.store(true, Ordering::SeqCst);
        self.waker.wake();
    }

    pub fn is_marked(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }
}

impl TurnGate {
    /// Take the gate, or stop waiting as soon as `served` is marked: `None`
    /// means it was marked and the gate was not taken.
    pub fn lock_unless<'a>(&'a self, served: &'a Served) -> TurnUnless<'a> {
        TurnUnless {
            wait: self.lock(),
            served,
        }
    }
}

pub struct TurnUnless<'a> {
    wait: TurnWait<'a>,
    served: &'a Served,
}

impl<'a> Future for TurnUnless<'a> {
    type Output = Option<Turn<'a>>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        // Register before checking, so a mark made now wakes this task.
        self.served.waker.register(context.waker());
        if self.served.is_marked() {
            // Dropping `wait` gives up its place, or passes the gate on if
            // it had just been handed over.
            return Poll::Ready(None);
        }
        Pin::new(&mut self.wait).poll(context).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Wake, Waker};

    struct Count(AtomicUsize);
    impl Wake for Count {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn poll<F: Future + Unpin>(future: &mut F, wakes: &Arc<Count>) -> Poll<F::Output> {
        let waker = Waker::from(Arc::clone(wakes));
        Pin::new(future).poll(&mut Context::from_waker(&waker))
    }

    fn wakes() -> Arc<Count> {
        Arc::new(Count(AtomicUsize::new(0)))
    }

    #[test]
    fn a_turn_lock_gives_one_holder_at_a_time_the_value() {
        let lock = TurnLock::new(1_u32);
        let mut held = lock.try_lock().unwrap();
        *held += 1;
        assert!(lock.try_lock().is_none());
        let waiting_wakes = wakes();
        let mut waiting = Box::pin(lock.lock());
        assert!(poll(&mut waiting, &waiting_wakes).is_pending());
        drop(held);
        assert_eq!(waiting_wakes.0.load(Ordering::SeqCst), 1);
        let Poll::Ready(mut next) = poll(&mut waiting, &waiting_wakes) else {
            panic!("the waiter gets the value");
        };
        assert_eq!(*next, 2);
        *next = 5;
        drop(next);
        assert_eq!(*lock.try_lock().unwrap(), 5);

        // An owned guard holds the gate wherever it goes.
        let lock = Arc::new(TurnLock::new(1_u32));
        let mut owned = Box::pin(Arc::clone(&lock).lock_owned());
        let Poll::Ready(mut owned) = poll(&mut owned, &wakes()) else {
            panic!("the lock is free");
        };
        assert!(lock.try_lock().is_none());
        std::thread::spawn(move || *owned = 9).join().unwrap();
        assert_eq!(*lock.try_lock().unwrap(), 9);
    }

    #[test]
    fn a_served_waiter_stops_waiting_and_passes_the_gate_on() {
        let gate = TurnGate::new();
        let held = gate.try_lock().unwrap();
        let (served, unserved) = (Served::new(), Served::new());
        let (first_wakes, second_wakes) = (wakes(), wakes());
        let mut first = gate.lock_unless(&served);
        let mut second = gate.lock_unless(&unserved);
        assert!(poll(&mut first, &first_wakes).is_pending());
        assert!(poll(&mut second, &second_wakes).is_pending());
        served.mark();
        assert_eq!(first_wakes.0.load(Ordering::SeqCst), 1);
        assert!(matches!(poll(&mut first, &first_wakes), Poll::Ready(None)));
        drop(first);
        drop(held);
        // The served waiter's place went to the next one.
        assert_eq!(second_wakes.0.load(Ordering::SeqCst), 1);
        let Poll::Ready(Some(turn)) = poll(&mut second, &second_wakes) else {
            panic!("the second waiter gets the gate");
        };
        drop(turn);
        assert!(gate.try_lock().is_some());
        // Marked after being handed the gate: the gate is passed on.
        let held = gate.try_lock().unwrap();
        let late = Served::new();
        let mut waiting = gate.lock_unless(&late);
        assert!(poll(&mut waiting, &first_wakes).is_pending());
        drop(held);
        late.mark();
        assert!(matches!(
            poll(&mut waiting, &first_wakes),
            Poll::Ready(None)
        ));
        drop(waiting);
        assert!(gate.try_lock().is_some());
    }

    #[test]
    fn waiters_take_turns_in_arrival_order() {
        let gate = TurnGate::new();
        let held = gate.try_lock().unwrap();
        assert!(gate.try_lock().is_none());
        let (first_wakes, second_wakes) = (wakes(), wakes());
        let mut first = gate.lock();
        let mut second = gate.lock();
        assert!(poll(&mut first, &first_wakes).is_pending());
        assert!(poll(&mut second, &second_wakes).is_pending());
        drop(held);
        assert_eq!(first_wakes.0.load(Ordering::SeqCst), 1);
        assert_eq!(second_wakes.0.load(Ordering::SeqCst), 0);
        // The gate went straight to the first waiter: nobody can barge in.
        assert!(gate.try_lock().is_none());
        assert!(poll(&mut second, &second_wakes).is_pending());
        let Poll::Ready(turn) = poll(&mut first, &first_wakes) else {
            panic!("the first waiter was handed the gate");
        };
        drop(turn);
        assert_eq!(second_wakes.0.load(Ordering::SeqCst), 1);
        let Poll::Ready(turn) = poll(&mut second, &second_wakes) else {
            panic!("the second waiter was handed the gate");
        };
        drop(turn);
        assert!(gate.try_lock().is_some());
    }

    #[test]
    fn cancelled_waiters_are_skipped_or_pass_the_gate_on() {
        let gate = TurnGate::new();
        let held = gate.try_lock().unwrap();
        let (first_wakes, second_wakes, third_wakes) = (wakes(), wakes(), wakes());
        let mut first = gate.lock();
        let mut second = gate.lock();
        let mut third = gate.lock();
        assert!(poll(&mut first, &first_wakes).is_pending());
        assert!(poll(&mut second, &second_wakes).is_pending());
        assert!(poll(&mut third, &third_wakes).is_pending());
        // Cancelled while waiting: skipped.
        drop(first);
        drop(held);
        assert_eq!(second_wakes.0.load(Ordering::SeqCst), 1);
        // Cancelled after being handed the gate: it goes to the next one.
        drop(second);
        assert_eq!(third_wakes.0.load(Ordering::SeqCst), 1);
        let Poll::Ready(turn) = poll(&mut third, &third_wakes) else {
            panic!("the third waiter was handed the gate");
        };
        drop(turn);
        assert!(gate.try_lock().is_some());
    }

    #[test]
    fn concurrent_holders_never_overlap_and_none_is_stranded() {
        struct Park(std::thread::Thread);
        impl Wake for Park {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let gate = Arc::new(TurnGate::new());
        let inside = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (gate, inside, total) = (gate.clone(), inside.clone(), total.clone());
                std::thread::spawn(move || {
                    let waker = Waker::from(Arc::new(Park(std::thread::current())));
                    let mut context = Context::from_waker(&waker);
                    for _ in 0..2000 {
                        let mut wait = gate.lock();
                        let turn = loop {
                            match Pin::new(&mut wait).poll(&mut context) {
                                Poll::Ready(turn) => break turn,
                                Poll::Pending => std::thread::park(),
                            }
                        };
                        assert_eq!(inside.fetch_add(1, Ordering::SeqCst), 0);
                        total.fetch_add(1, Ordering::SeqCst);
                        inside.fetch_sub(1, Ordering::SeqCst);
                        drop(turn);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(total.load(Ordering::SeqCst), 16000);
        assert!(gate.try_lock().is_some());
    }
}
