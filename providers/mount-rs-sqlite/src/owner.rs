//! One thread that owns a SQLite connection.
//!
//! The connection is not shared and has no lock around it: it lives on a
//! thread of its own, and everything done with it is a job sent to that
//! thread. Jobs run one at a time in the order they arrive, which is the
//! exclusion a lock would give, and a caller on an async runtime waits for
//! its reply without holding a runtime thread through a disk sync or
//! SQLite's busy wait.
//!
//! What must happen as one unit goes in one job: nothing else runs on the
//! connection between a job's first statement and its last.
//!
//! A job that panics poisons the owner. That job's caller and every later
//! caller get `SQLite storage lock poisoned`, the error a poisoned lock gave,
//! and no job runs on the connection again.
//!
//! A caller that stops waiting (its future is dropped) does not stop its
//! job: a queued job still runs, a running one finishes, and the result is
//! discarded.
//!
//! The thread ends when the last handle is dropped. That drop waits for the
//! jobs already sent and for the connection to close, so the database file
//! can be reopened or removed as soon as the drop returns.
use mount_rs_core::{FsError, Result, backend_error};
use rusqlite::Connection;
use std::{
    future::Future,
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicPtr, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, JoinHandle, Thread, ThreadId},
};

/// What the thread owns. Only jobs see it.
pub(crate) struct State {
    // Declared first so that it closes first: the trace callback installed
    // on it points at `counts`, which must outlive the connection.
    pub connection: Connection,
    /// The namespace last stored or loaded through this connection.
    pub published: Option<super::changes::Published>,
    /// Statement counters, when profiling installed a trace callback.
    pub counts: Option<Arc<super::io_diagnostics::Counts>>,
}

type Job = Box<dyn FnOnce(&mut State) + Send>;

/// A handle to the thread. Dropping the last one ends the thread.
pub(crate) struct Owner {
    jobs: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
    thread_id: ThreadId,
}

fn poisoned() -> FsError {
    backend_error("SQLite storage lock poisoned")
}

impl Owner {
    /// Moves `connection` to a new thread that runs jobs on it.
    pub fn start(connection: Connection) -> Result<Arc<Self>> {
        let (jobs, queue) = channel::<Job>();
        let state = State {
            connection,
            published: None,
            counts: None,
        };
        let thread = thread::Builder::new()
            .name("mount-rs-sqlite".into())
            .spawn(move || serve(state, queue))
            .map_err(backend_error)?;
        Ok(Arc::new(Self {
            jobs: Some(jobs),
            thread_id: thread.thread().id(),
            thread: Some(thread),
        }))
    }

    /// Sends `job`. `None` when it cannot be sent.
    fn send(&self, job: Job) -> Option<()> {
        // A job that waited for another job would wait for itself: the
        // thread runs one at a time.
        if thread::current().id() == self.thread_id {
            debug_assert!(false, "a SQLite job submitted another job");
            return None;
        }
        self.jobs.as_ref()?.send(job).ok()
    }

    /// Runs `work` on the connection's thread and waits for it without
    /// blocking: the calling task is woken when the job has finished.
    pub fn run<T, F>(&self, work: F) -> Reply<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut State) -> T + Send + 'static,
    {
        let (task, reply) = task(work);
        // A job that was not sent is dropped here, which settles the reply
        // without a value.
        let _ = self.send(Box::new(move |state| task.run(state)));
        reply
    }

    /// Runs `work` on the connection's thread and blocks the calling thread
    /// until it has finished. For callers that are not async. `work` may
    /// borrow from the caller, which stays blocked for as long as the job
    /// exists.
    pub fn run_blocking<'a, T, F>(&self, work: F) -> Result<T>
    where
        T: Send + 'a,
        F: FnOnce(&mut State) -> T + Send + 'a,
    {
        let (task, reply) = task(work);
        let job: Box<dyn FnOnce(&mut State) + Send + 'a> = Box::new(move |state| task.run(state));
        // SAFETY: the job borrows for `'a`, and this function does not
        // return before the job is gone. `wait` returns only when the reply
        // is settled, and a reply is settled by `Task::run` after `work`
        // has returned or unwound, or by dropping a `Task` that never ran,
        // which drops `work` first (`Task` declares it before the reply).
        // A job that could not be sent is dropped inside `send`. So nothing
        // borrowed for `'a` is used after this function returns.
        let job: Job = unsafe { std::mem::transmute(job) };
        let _ = self.send(job);
        wait(reply)
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        // With no sender left the thread finishes the jobs it was sent and
        // then closes the connection.
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            // The last handle can be dropped by a job, when the job owned
            // one and its caller had gone. The thread cannot wait for
            // itself; it ends when that job returns.
            if thread::current().id() != self.thread_id {
                let _ = thread.join();
            }
        }
    }
}

fn serve(mut state: State, queue: Receiver<Job>) {
    let mut poisoned = false;
    for job in queue {
        // A job that is dropped without running settles its reply without
        // a value, which its caller reports as poisoned.
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            if poisoned {
                drop(job);
            } else {
                job(&mut state);
            }
        }));
        poisoned |= outcome.is_err();
    }
    // Close the connection, then let go of what its callbacks point at.
    let State {
        connection,
        published,
        counts,
    } = state;
    drop(connection);
    drop(published);
    drop(counts);
}

/// A job's work and where its result goes.
struct Task<F, T> {
    // Dropped before `reply` (declaration order): when a task is dropped
    // without running, its caller is told only after the work is gone.
    work: Option<F>,
    reply: Settle<T>,
}

fn task<T, F: FnOnce(&mut State) -> T>(work: F) -> (Task<F, T>, Reply<T>) {
    let slot = Arc::new(Slot {
        value: AtomicPtr::new(ptr::null_mut()),
        waker: AtomicPtr::new(ptr::null_mut()),
        settled: AtomicBool::new(false),
        owns: PhantomData,
    });
    (
        Task {
            work: Some(work),
            reply: Settle(Arc::clone(&slot)),
        },
        Reply(slot),
    )
}

impl<F: FnOnce(&mut State) -> T, T> Task<F, T> {
    fn run(mut self, state: &mut State) {
        let work = self.work.take().expect("a task runs once");
        // If this unwinds, `work` is gone with the frame that was running
        // it, and dropping `self` then settles the reply without a value.
        let value = work(state);
        self.reply
            .0
            .value
            .store(Box::into_raw(Box::new(value)), Ordering::Release);
    }
}

/// One job's result on its way back: set once, taken once.
struct Slot<T> {
    value: AtomicPtr<T>,
    waker: AtomicPtr<Waker>,
    /// Set after `value`, by the job's side, when there is nothing more to
    /// wait for: the value is there, or it never will be.
    settled: AtomicBool,
    owns: PhantomData<T>,
}

// SAFETY: the slot hands a `T` from one thread to another and never lends
// it, and the waker is `Send + Sync`; the pointers are owned boxes moved by
// atomic swaps.
unsafe impl<T: Send> Send for Slot<T> {}
unsafe impl<T: Send> Sync for Slot<T> {}

impl<T> Slot<T> {
    fn take_waker(&self) -> Option<Waker> {
        let waker = self.waker.swap(ptr::null_mut(), Ordering::AcqRel);
        // SAFETY: a non-null pointer came from `Box::into_raw` in `poll`,
        // and the swap made this call its only owner.
        (!waker.is_null()).then(|| *unsafe { Box::from_raw(waker) })
    }

    fn take_value(&self) -> Option<T> {
        let value = self.value.swap(ptr::null_mut(), Ordering::AcqRel);
        // SAFETY: as in `take_waker`, for the box made in `Task::run`.
        (!value.is_null()).then(|| *unsafe { Box::from_raw(value) })
    }
}

impl<T> Drop for Slot<T> {
    fn drop(&mut self) {
        // A result nobody waited for is discarded here.
        self.take_value();
        self.take_waker();
    }
}

/// The job's side of a slot. Dropping it settles the slot, with the value
/// if the job stored one.
struct Settle<T>(Arc<Slot<T>>);

impl<T> Drop for Settle<T> {
    fn drop(&mut self) {
        self.0.settled.store(true, Ordering::SeqCst);
        if let Some(waker) = self.0.take_waker() {
            waker.wake();
        }
    }
}

/// The caller's side: resolves when the job has finished. Dropping it does
/// not stop the job.
pub(crate) struct Reply<T>(Arc<Slot<T>>);

impl<T> Future for Reply<T> {
    type Output = Result<T>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let slot = &self.0;
        let finished = || Poll::Ready(slot.take_value().ok_or_else(poisoned));
        if slot.settled.load(Ordering::SeqCst) {
            return finished();
        }
        let waker = Box::into_raw(Box::new(context.waker().clone()));
        let previous = slot.waker.swap(waker, Ordering::AcqRel);
        if !previous.is_null() {
            // SAFETY: as in `take_waker`.
            drop(unsafe { Box::from_raw(previous) });
        }
        // The job's side sets `settled` and then takes the waker. If it
        // took the one before this, it had already set `settled`.
        if slot.settled.load(Ordering::SeqCst) {
            return finished();
        }
        Poll::Pending
    }
}

/// Blocks the calling thread until `reply` is settled.
fn wait<T>(mut reply: Reply<T>) -> Result<T> {
    struct Unpark(Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(result) = Pin::new(&mut reply).poll(&mut context) {
            return result;
        }
        // A wake that came before this is remembered by `park`.
        thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    fn owner() -> Arc<Owner> {
        Owner::start(Connection::open_in_memory().unwrap()).unwrap()
    }

    #[test]
    fn jobs_from_many_threads_run_one_at_a_time_in_arrival_order() {
        let owner = owner();
        owner
            .run_blocking(|state| {
                state
                    .connection
                    .execute_batch(
                        "CREATE TABLE arrivals(position INTEGER PRIMARY KEY AUTOINCREMENT, sender INTEGER, sequence INTEGER)",
                    )
                    .unwrap()
            })
            .unwrap();
        let running = Arc::new(AtomicUsize::new(0));
        let overlapped = Arc::new(AtomicBool::new(false));
        let senders: Vec<_> = (0..8_i64)
            .map(|sender| {
                let (owner, running, overlapped) = (
                    Arc::clone(&owner),
                    Arc::clone(&running),
                    Arc::clone(&overlapped),
                );
                thread::spawn(move || {
                    // Sent without waiting in between, so each thread has
                    // several jobs queued at once.
                    let replies: Vec<_> = (0..50_i64)
                        .map(|sequence| {
                            let (running, overlapped) =
                                (Arc::clone(&running), Arc::clone(&overlapped));
                            owner.run(move |state| {
                                if running.fetch_add(1, Ordering::SeqCst) != 0 {
                                    overlapped.store(true, Ordering::SeqCst);
                                }
                                state
                                    .connection
                                    .execute(
                                        "INSERT INTO arrivals(sender, sequence) VALUES(?1, ?2)",
                                        [sender, sequence],
                                    )
                                    .unwrap();
                                thread::yield_now();
                                running.fetch_sub(1, Ordering::SeqCst);
                                sequence
                            })
                        })
                        .collect();
                    for (sequence, reply) in replies.into_iter().enumerate() {
                        assert_eq!(wait(reply).unwrap(), sequence as i64);
                    }
                })
            })
            .collect();
        for sender in senders {
            sender.join().unwrap();
        }
        assert!(!overlapped.load(Ordering::SeqCst));
        // Each thread's jobs ran in the order that thread sent them.
        let stored: Vec<(i64, i64)> = owner
            .run_blocking(|state| {
                let mut statement = state
                    .connection
                    .prepare("SELECT sender, sequence FROM arrivals ORDER BY position")
                    .unwrap();
                statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                    .unwrap()
                    .map(|row| row.unwrap())
                    .collect()
            })
            .unwrap();
        assert_eq!(stored.len(), 400);
        let mut next = [0_i64; 8];
        for (sender, sequence) in stored {
            assert_eq!(sequence, next[sender as usize]);
            next[sender as usize] += 1;
        }
    }

    #[test]
    fn a_blocking_job_may_borrow_from_its_caller() {
        let owner = owner();
        let mut seen = Vec::new();
        let text = String::from("borrowed");
        owner
            .run_blocking(|state| {
                let length: i64 = state
                    .connection
                    .query_row("SELECT length(?1)", [&text], |row| row.get(0))
                    .unwrap();
                seen.push(length);
            })
            .unwrap();
        assert_eq!(seen, [8]);
    }

    #[test]
    fn a_panicking_job_poisons_the_owner_and_later_calls_fail() {
        let owner = owner();
        let first = owner.run_blocking(|_| -> u8 { panic!("job failed") });
        assert_eq!(
            first.unwrap_err().to_string(),
            poisoned().to_string(),
            "the panicking job's caller is told"
        );
        // Queued behind nothing and behind each other: none hangs, none runs.
        let ran = Arc::new(AtomicBool::new(false));
        let later: Vec<_> = (0..4)
            .map(|_| {
                let ran = Arc::clone(&ran);
                owner.run(move |_| ran.store(true, Ordering::SeqCst))
            })
            .collect();
        for reply in later {
            assert_eq!(wait(reply).unwrap_err().to_string(), poisoned().to_string());
        }
        assert!(owner.run_blocking(|_| ()).is_err());
        assert!(!ran.load(Ordering::SeqCst));
        // The thread is still there to be shut down.
        drop(owner);
    }

    #[test]
    fn dropping_the_last_handle_closes_the_connection() {
        let path = std::env::temp_dir().join(format!(
            "mount-rs-sqlite-owner-{}-{:?}.sqlite",
            std::process::id(),
            thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE kept(value INTEGER);")
            .unwrap();
        let owner = Owner::start(connection).unwrap();
        let second = Arc::clone(&owner);
        // Jobs nobody waits for are still run before the connection closes.
        for value in 0..20_i64 {
            drop(owner.run(move |state| {
                thread::sleep(Duration::from_millis(1));
                state
                    .connection
                    .execute("INSERT INTO kept(value) VALUES(?1)", [value])
                    .unwrap();
            }));
        }
        drop(owner);
        drop(second);
        // Closing the last connection to a WAL database removes its WAL.
        assert!(!path.with_extension("sqlite-wal").exists());
        let reopened = Connection::open(&path).unwrap();
        let rows: i64 = reopened
            .query_row("SELECT count(*) FROM kept", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 20);
        drop(reopened);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn the_last_handle_can_be_dropped_by_a_job() {
        let owner = owner();
        let (finished, wait_finished) = channel();
        let held = Arc::clone(&owner);
        drop(owner.run(move |_| {
            // Let the caller's handle go first, so this one is the last.
            thread::sleep(Duration::from_millis(50));
            drop(held);
            finished.send(()).unwrap();
        }));
        drop(owner);
        wait_finished
            .recv_timeout(Duration::from_secs(10))
            .expect("the job finished without waiting for its own thread");
    }

    #[test]
    fn a_cancelled_async_caller_does_not_lose_or_tear_its_job() {
        let owner = owner();
        owner
            .run_blocking(|state| {
                state
                    .connection
                    .execute_batch("CREATE TABLE pairs(left INTEGER, right INTEGER)")
                    .unwrap()
            })
            .unwrap();
        let (started, wait_started) = channel();
        let mut running = owner.run(move |state| {
            let transaction = state.connection.transaction().unwrap();
            transaction
                .execute("INSERT INTO pairs VALUES(1, NULL)", [])
                .unwrap();
            started.send(()).unwrap();
            thread::sleep(Duration::from_millis(100));
            transaction
                .execute("UPDATE pairs SET right=2 WHERE left=1", [])
                .unwrap();
            transaction.commit().unwrap();
        });
        let mut queued = owner.run(|state| {
            state
                .connection
                .execute("INSERT INTO pairs VALUES(3, 4)", [])
                .unwrap();
        });
        // Poll both once, as a runtime would, then cancel them: one while
        // its job is running and one while its job is still queued.
        let mut context = Context::from_waker(Waker::noop());
        assert!(Pin::new(&mut running).poll(&mut context).is_pending());
        assert!(Pin::new(&mut queued).poll(&mut context).is_pending());
        wait_started.recv().unwrap();
        drop(running);
        drop(queued);
        let pairs: Vec<(i64, Option<i64>)> = owner
            .run_blocking(|state| {
                assert!(state.connection.is_autocommit());
                let mut statement = state
                    .connection
                    .prepare("SELECT left, right FROM pairs ORDER BY left")
                    .unwrap();
                statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                    .unwrap()
                    .map(|row| row.unwrap())
                    .collect()
            })
            .unwrap();
        assert_eq!(pairs, [(1, Some(2)), (3, Some(4))]);
    }

    #[test]
    fn an_async_caller_leaves_its_thread_free_while_a_slow_job_runs() {
        let owner = owner();
        let finished = Arc::new(AtomicBool::new(false));
        let job_finished = Arc::clone(&finished);
        let job = owner.run(move |_| {
            thread::sleep(Duration::from_millis(200));
            job_finished.store(true, Ordering::SeqCst);
            7
        });
        // One thread runs both: the caller waiting for the job, and another
        // task that counts how often it ran before the job had finished.
        let other = async {
            let mut turns = 0_u64;
            while !finished.load(Ordering::SeqCst) {
                turns += 1;
                async_io::Timer::after(Duration::from_millis(5)).await;
            }
            turns
        };
        let (value, turns) = futures_lite::future::block_on(futures_lite::future::zip(job, other));
        assert_eq!(value.unwrap(), 7);
        assert!(
            turns >= 5,
            "the other task ran {turns} times while the job slept"
        );
    }
}
