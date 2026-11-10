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
