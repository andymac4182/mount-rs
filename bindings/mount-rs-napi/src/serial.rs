//! State that JavaScript calls reach one at a time, in the order they were
//! issued. There is no lock to wait for: a call either has the state from the
//! moment it is made, or is queued and waits for its answer.
//!
//! JavaScript may call a method again before the promise of the previous
//! call settles. Each such call is a `Request`. `Serial::submit` is called
//! synchronously on the JavaScript thread, so the position of a request is
//! fixed by the order of the calls themselves and not by which runtime
//! worker happens to poll its future first.
//!
//! A request that finds the state idle runs in its caller's own future:
//! nothing is allocated and no task is involved. A request issued while
//! another is running is put on a lock-free queue. Whoever finishes with the
//! state and finds the queue non-empty hands the state to one drain task,
//! which runs the queued requests in order and answers each through its own
//! reply channel. Nobody waits for the state: a caller either has it from
//! the moment it submits, or waits for its answer.
//!
//! No request is stranded. A submitter pushes its request and then tries to
//! take the state; the party giving the state up marks it idle and then
//! looks at the queue. With sequentially consistent ordering one of the two
//! sees the other: either the submitter takes the idle state and starts a
//! drain, or the party that gave it up sees the request and takes the state
//! back. No request runs twice: a queued request is popped once and consumed
//! by running it, and a request that runs in its caller's future is never
//! queued.
use crossbeam_queue::SegQueue;
use napi::bindgen_prelude::{Env, PromiseRaw, ToNapiValue};
use napi::tokio::runtime::Handle;
use std::{
    cell::UnsafeCell,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    pin::{Pin, pin},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::oneshot;

/// One call against the state.
pub trait Request<T>: Send + 'static {
    type Reply: Send + 'static;

    fn run(self, state: &mut T) -> impl Future<Output = Self::Reply> + Send;
}

/// A queued request: its future, made when it was submitted and polled only
/// by the party that has the state. It answers through its reply channel.
type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

struct StatePointer<T>(*mut T);

// SAFETY: the pointer is only dereferenced by the party that has the state,
// which may be on any thread.
unsafe impl<T: Send> Send for StatePointer<T> {}

impl<T> StatePointer<T> {
    /// SAFETY: the caller has the state (`busy` is its), and the `Serial`
    /// that owns the state is alive.
    unsafe fn state<'a>(self) -> &'a mut T {
        unsafe { &mut *self.0 }
    }
}

/// Run a future to its end, turning a panic inside it into an error instead
/// of unwinding through the caller, so that one request that panics does not
/// take the state away from the requests behind it.
pub(crate) async fn caught<F: Future>(future: F) -> std::thread::Result<F::Output> {
    let mut running = pin!(future);
    std::future::poll_fn(|context| {
        match catch_unwind(AssertUnwindSafe(|| running.as_mut().poll(context))) {
            Ok(poll) => poll.map(Ok),
            Err(panic) => std::task::Poll::Ready(Err(panic)),
        }
    })
    .await
}

pub struct Serial<T> {
    /// Set while some party (a caller running its own request, or the drain
    /// task) has the state.
    busy: AtomicBool,
    queue: SegQueue<Job>,
    state: UnsafeCell<T>,
}

// SAFETY: `state` is only reached by the party that set `busy`, one at a
// time, and may be reached from any thread.
unsafe impl<T: Send> Send for Serial<T> {}
unsafe impl<T: Send> Sync for Serial<T> {}

/// Possession of the state. Given up with `release`; dropping it instead
/// (the future that had the state was dropped, which only happens when the
/// runtime is torn down) cancels the queued requests, because no task is
/// left that could run them.
struct Held<T>(Arc<Serial<T>>);

impl<T> Serial<T> {
    fn try_take(&self) -> bool {
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Mark the state idle. Returns true if a request was queued meanwhile
    /// and the state was taken back for it.
    fn idle_or_retake(&self) -> bool {
        self.busy.store(false, Ordering::SeqCst);
        !self.queue.is_empty() && self.try_take()
    }
}

impl<T> Drop for Held<T> {
    fn drop(&mut self) {
        loop {
            while self.0.queue.pop().is_some() {}
            if !self.0.idle_or_retake() {
                return;
            }
        }
    }
}

impl<T: Send + 'static> Held<T> {
    fn into_serial(self) -> Arc<Serial<T>> {
        let this = std::mem::ManuallyDrop::new(self);
        // SAFETY: `this` is never dropped, so the `Arc` is moved out once.
        unsafe { std::ptr::read(&this.0) }
    }

    /// Give the state up: to a drain task if requests are queued.
    fn release(self) {
        if self.0.queue.is_empty() {
            let serial = self.into_serial();
            if !serial.idle_or_retake() {
                return;
            }
            Held(serial).release();
            return;
        }
        match Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(drain(self));
            }
            // Not on the runtime, so nothing can run the queued requests.
            Err(_) => drop(self),
        }
    }
}

async fn drain<T: Send + 'static>(held: Held<T>) {
    loop {
        while let Some(job) = held.0.queue.pop() {
            job.await;
        }
        if !held.0.idle_or_retake() {
            drop(held.into_serial());
            return;
        }
    }
}

impl<T: Send + 'static> Serial<T> {
    pub fn new(state: T) -> Arc<Self> {
        Arc::new(Self {
            busy: AtomicBool::new(false),
            queue: SegQueue::new(),
            state: UnsafeCell::new(state),
        })
    }

    /// Fix the position of `request` after every request submitted before
    /// it. Call this where the order of calls is defined (synchronously in
    /// the N-API entry point) and await `Submitted::reply` on the runtime.
    pub fn submit<Q: Request<T>>(self: &Arc<Self>, request: Q) -> Submitted<T, Q> {
        let taken = self.try_take();
        if taken && self.queue.is_empty() {
            return Submitted(Stage::Run {
                held: Held(Arc::clone(self)),
                request,
            });
        }
        // Either a request is running, or the state was idle with requests
        // still queued (the party that gave it up has not looked at the
        // queue yet): this request goes behind them.
        let (reply, receiver) = oneshot::channel();
        let state = StatePointer(self.state.get());
        self.queue.push(Box::pin(async move {
            // SAFETY: a queued request is polled only by the drain task,
            // which has the state and holds the `Serial`.
            let state = unsafe { state.state() };
            // Withdrawn: the caller dropped the request before its turn.
            if reply.is_closed() {
                return;
            }
            // A request that panics answers nobody: its sender is dropped
            // and its caller sees the request as cancelled. The requests
            // behind it still run.
            if let Ok(answer) = caught(request.run(state)).await {
                let _ = reply.send(answer);
            }
        }));
        let duty = (taken || self.try_take()).then(|| Held(Arc::clone(self)));
        Submitted(Stage::Wait {
            duty,
            reply: receiver,
        })
    }
}

/// A request whose position is fixed. Dropping it without awaiting `reply`
/// withdraws it if it has not started.
pub struct Submitted<T, Q: Request<T>>(Stage<T, Q>);

enum Stage<T, Q: Request<T>> {
    /// The state was idle: this caller has it and runs its own request.
    Run { held: Held<T>, request: Q },
    /// Queued. `duty` is possession of the state when this caller took it
    /// with requests queued; it starts the drain.
    Wait {
        duty: Option<Held<T>>,
        reply: oneshot::Receiver<Q::Reply>,
    },
}

impl<T: Send + 'static, Q: Request<T>> Submitted<T, Q> {
    /// The reply, or `None` if the request was cancelled: the runtime is
    /// being torn down, or the request panicked in the drain task.
    pub async fn reply(self) -> Option<Q::Reply> {
        match self.0 {
            Stage::Run { held, request } => {
                // SAFETY: `held` is possession of the state.
                let state = unsafe { &mut *held.0.state.get() };
                let outcome = caught(request.run(state)).await;
                held.release();
                match outcome {
                    Ok(reply) => Some(reply),
                    Err(panic) => resume_unwind(panic),
                }
            }
            Stage::Wait { duty, reply } => {
                if let Some(held) = duty {
                    held.release();
                }
                reply.await.ok()
            }
        }
    }
}

/// The promise for a request whose position is already fixed. Called in the
/// synchronous N-API entry point; `reply` yields `None` if the request was
/// cancelled.
pub fn promise<R, V>(
    env: Env,
    reply: impl Future<Output = Option<R>> + Send + 'static,
    settle: impl FnOnce(R) -> napi::Result<V> + Send + 'static,
) -> napi::Result<PromiseRaw<'static, V>>
where
    V: ToNapiValue + Send + 'static,
{
    let promise: PromiseRaw<'_, V> = env.spawn_future(async move {
        match reply.await {
            Some(reply) => settle(reply),
            None => Err(napi::Error::new(
                napi::Status::Cancelled,
                "mount-rs request was cancelled before it completed",
            )),
        }
    })?;
    // SAFETY: `PromiseRaw` only carries the Env lifetime at the type level.
    // Node owns the promise, and everything the future captured is owned.
    Ok(unsafe { std::mem::transmute::<PromiseRaw<'_, V>, PromiseRaw<'static, V>>(promise) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::turn_gate::TurnLock;
    use napi::tokio::{self as rt_tokio, runtime::Runtime};
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    fn runtime() -> Runtime {
        rt_tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap()
    }

    #[derive(Default)]
    struct Log {
        inside: bool,
        order: Vec<(usize, u64)>,
    }

    struct Append {
        issuer: usize,
        sequence: u64,
        yields: usize,
    }

    impl Request<Log> for Append {
        type Reply = (usize, u64);

        async fn run(self, state: &mut Log) -> Self::Reply {
            assert!(!state.inside, "two requests ran at once");
            state.inside = true;
            for _ in 0..self.yields {
                rt_tokio::task::yield_now().await;
            }
            state.order.push((self.issuer, self.sequence));
            state.inside = false;
            (self.issuer, self.sequence)
        }
    }

    struct Snapshot;

    impl Request<Log> for Snapshot {
        type Reply = Vec<(usize, u64)>;

        async fn run(self, state: &mut Log) -> Self::Reply {
            state.order.clone()
        }
    }

    /// Several threads issue overlapping requests. Every request runs
    /// exactly once, never beside another, and each thread's requests run in
    /// the order that thread issued them; every reply arrives.
    #[test]
    fn overlapping_requests_run_once_in_issue_order() {
        const ISSUERS: usize = 4;
        const PER_ISSUER: u64 = 20_000;
        let runtime = runtime();
        let serial = Serial::new(Log::default());
        let replies = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..ISSUERS)
            .map(|issuer| {
                let serial = Arc::clone(&serial);
                let handle = runtime.handle().clone();
                let replies = Arc::clone(&replies);
                std::thread::spawn(move || {
                    let mut tasks = Vec::new();
                    for sequence in 0..PER_ISSUER {
                        let submitted = serial.submit(Append {
                            issuer,
                            sequence,
                            yields: (sequence % 3) as usize,
                        });
                        let replies = Arc::clone(&replies);
                        tasks.push(handle.spawn(async move {
                            let reply = submitted.reply().await;
                            assert_eq!(reply, Some((issuer, sequence)));
                            replies.fetch_add(1, Ordering::SeqCst);
                        }));
                        if sequence % 64 == 0 {
                            // Let the queue run dry so the idle path and
                            // the hand-over are both exercised.
                            std::thread::sleep(Duration::from_micros(50));
                        }
                    }
                    tasks
                })
            })
            .collect();
        for thread in threads {
            for task in thread.join().unwrap() {
                runtime.block_on(task).unwrap();
            }
        }
        assert_eq!(
            replies.load(Ordering::SeqCst),
            ISSUERS * PER_ISSUER as usize
        );
        let order = runtime
            .block_on(serial.submit(Snapshot).reply())
            .expect("snapshot");
        assert_eq!(order.len(), ISSUERS * PER_ISSUER as usize);
        let mut next = [0_u64; ISSUERS];
        for (issuer, sequence) in order {
            assert_eq!(sequence, next[issuer], "issuer {issuer} out of order");
            next[issuer] += 1;
        }
        assert!(!serial.busy.load(Ordering::SeqCst));
        assert!(serial.queue.is_empty());
    }

    /// One thread issues bursts without waiting, as JavaScript does: the
    /// requests run in exactly the order of the calls.
    #[test]
    fn a_burst_from_one_thread_runs_in_call_order() {
        let runtime = runtime();
        let serial = Serial::new(Log::default());
        let mut expected = Vec::new();
        for round in 0..2_000_u64 {
            let tasks: Vec<_> = (0..16)
                .map(|slot| {
                    let sequence = round * 16 + slot;
                    expected.push((0, sequence));
                    let submitted = serial.submit(Append {
                        issuer: 0,
                        sequence,
                        yields: (slot % 2) as usize,
                    });
                    runtime.spawn(submitted.reply())
                })
                .collect();
            for task in tasks {
                assert!(runtime.block_on(task).unwrap().is_some());
            }
        }
        let order = runtime.block_on(serial.submit(Snapshot).reply()).unwrap();
        assert_eq!(order, expected);
    }

    struct Stall(Arc<rt_tokio::sync::Notify>);

    impl Request<Log> for Stall {
        type Reply = ();

        async fn run(self, _state: &mut Log) {
            self.0.notified().await;
        }
    }

    /// A request withdrawn before it starts, and a caller dropped while it
    /// has the state, leave the state usable: later requests still run and
    /// nothing waits forever.
    #[test]
    fn dropped_callers_do_not_strand_later_requests() {
        let runtime = runtime();
        let serial = Serial::new(Log::default());

        // Withdrawn while idle: it had the state and never ran.
        drop(serial.submit(Append {
            issuer: 0,
            sequence: 0,
            yields: 0,
        }));
        assert!(!serial.busy.load(Ordering::SeqCst));

        // Dropped mid-request with two requests queued behind it. The
        // queued requests are cancelled, not left waiting.
        let gate = Arc::new(rt_tokio::sync::Notify::new());
        let stalled = runtime.spawn(serial.submit(Stall(Arc::clone(&gate))).reply());
        let queued: Vec<_> = (1..3)
            .map(|sequence| {
                runtime.spawn(
                    serial
                        .submit(Append {
                            issuer: 0,
                            sequence,
                            yields: 0,
                        })
                        .reply(),
                )
            })
            .collect();
        stalled.abort();
        assert!(runtime.block_on(stalled).unwrap_err().is_cancelled());
        for task in queued {
            assert_eq!(runtime.block_on(task).unwrap(), None);
        }
        assert!(!serial.busy.load(Ordering::SeqCst));

        let reply = runtime.block_on(
            serial
                .submit(Append {
                    issuer: 0,
                    sequence: 9,
                    yields: 1,
                })
                .reply(),
        );
        assert_eq!(reply, Some((0, 9)));
        let order = runtime.block_on(serial.submit(Snapshot).reply()).unwrap();
        assert_eq!(order, vec![(0, 9)]);

        // A queued request withdrawn by its caller is skipped; the request
        // behind it runs.
        let stalled = runtime.spawn(serial.submit(Stall(Arc::clone(&gate))).reply());
        let append = |sequence| Append {
            issuer: 0,
            sequence,
            yields: 0,
        };
        drop(serial.submit(append(10)));
        let behind = runtime.spawn(serial.submit(append(11)).reply());
        gate.notify_one();
        assert_eq!(runtime.block_on(stalled).unwrap(), Some(()));
        assert_eq!(runtime.block_on(behind).unwrap(), Some((0, 11)));
        let order = runtime.block_on(serial.submit(Snapshot).reply()).unwrap();
        assert_eq!(order, vec![(0, 9), (0, 11)]);
        assert!(!serial.busy.load(Ordering::SeqCst));
    }

    struct Explode;

    impl Request<Log> for Explode {
        type Reply = ();

        async fn run(self, _state: &mut Log) {
            panic!("request panicked");
        }
    }

    /// A panicking request fails its own caller only, whether it ran in the
    /// caller's future or in the drain task.
    #[test]
    fn a_panicking_request_does_not_cancel_the_others() {
        let runtime = runtime();
        let serial = Serial::new(Log::default());
        let gate = Arc::new(rt_tokio::sync::Notify::new());
        let first = runtime.spawn(serial.submit(Stall(Arc::clone(&gate))).reply());
        let inline_after = |sequence| Append {
            issuer: 0,
            sequence,
            yields: 0,
        };
        let queued_panic = runtime.spawn(serial.submit(Explode).reply());
        let behind = runtime.spawn(serial.submit(inline_after(1)).reply());
        gate.notify_one();
        assert_eq!(runtime.block_on(first).unwrap(), Some(()));
        assert_eq!(runtime.block_on(queued_panic).unwrap(), None);
        assert_eq!(runtime.block_on(behind).unwrap(), Some((0, 1)));

        let inline_panic = runtime.spawn(serial.submit(Explode).reply());
        let behind = runtime.spawn(serial.submit(inline_after(2)).reply());
        assert!(runtime.block_on(inline_panic).unwrap_err().is_panic());
        assert_eq!(runtime.block_on(behind).unwrap(), Some((0, 2)));
        assert!(!serial.busy.load(Ordering::SeqCst));
    }

    // ---------------------------------------------------------------------
    // Benchmark: the lock this module replaced, an owner task, and `Serial`,
    // around the same work, driven the way N-API drives it: a single issuing
    // thread (JavaScript), one spawned runtime task per call, and a channel
    // back to the issuing thread standing in for the promise settling.
    //
    //   CARGO_INCREMENTAL=0 ./scripts/cargo-shared test --release --locked \
    //     -p mount-rs-napi serial::tests::bench -- --ignored --nocapture
    // ---------------------------------------------------------------------

    use mount_rs_core::{FileHandle, FsDriver};
    use mount_rs_fuse::session::{FuseSession, FuseSessionOptions};
    use mount_rs_memfs::MemoryFs;
    use rt_tokio::sync::mpsc;

    fn fuse_frame(opcode: u32, nodeid: u64, body: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(40 + body.len());
        frame.extend_from_slice(&((40 + body.len()) as u32).to_le_bytes());
        frame.extend_from_slice(&opcode.to_le_bytes());
        frame.extend_from_slice(&42_u64.to_le_bytes());
        frame.extend_from_slice(&nodeid.to_le_bytes());
        frame.extend_from_slice(&[0; 16]);
        frame.extend_from_slice(body);
        frame
    }

    fn fuse_session(runtime: &Runtime) -> FuseSession {
        let mut session =
            FuseSession::with_options(Arc::new(MemoryFs::empty()), FuseSessionOptions::default());
        let mut init = Vec::new();
        for word in [7_u32, 41, 65_536, 0, 0] {
            init.extend_from_slice(&word.to_le_bytes());
        }
        let reply = runtime
            .block_on(session.handle(&fuse_frame(26, 0, &init)))
            .unwrap()
            .unwrap();
        assert_eq!(&reply[4..8], &[0; 4], "INIT failed");
        session
    }

    /// What `FuseSession.__handleObserved` does with the session.
    async fn fuse_work(session: &mut FuseSession, frame: &[u8]) -> usize {
        let reply = session.handle(frame).await.unwrap();
        let error = session.take_last_error();
        assert!(error.is_none());
        reply.map_or(0, |reply| reply.len())
    }

    struct FuseHandle(Arc<Vec<u8>>);

    impl Request<FuseSession> for FuseHandle {
        type Reply = usize;

        async fn run(self, state: &mut FuseSession) -> usize {
            fuse_work(state, &self.0).await
        }
    }

    /// The position and handle of a file-backed WebDAV response body. The
    /// benchmark body wraps around so one file serves any number of chunks.
    struct Body {
        handle: Arc<dyn FileHandle>,
        position: u64,
        end: u64,
        chunk: usize,
    }

    async fn body_work(body: &mut Body) -> usize {
        if body.position >= body.end {
            body.position = 0;
        }
        let wanted = (body.end - body.position).min(body.chunk as u64) as usize;
        let mut bytes = vec![0_u8; wanted];
        let count = body
            .handle
            .read(&mut bytes, Some(body.position))
            .await
            .unwrap();
        assert!(count > 0 && count <= wanted);
        body.position += count as u64;
        bytes.truncate(count);
        std::hint::black_box(&bytes);
        count
    }

    fn body(runtime: &Runtime, chunk: usize) -> Body {
        const SIZE: usize = 16 * 1024 * 1024;
        let driver = MemoryFs::empty();
        runtime.block_on(async {
            driver.write_file("/body", &vec![7_u8; SIZE]).await.unwrap();
            Body {
                handle: driver.open("/body", "r", 0o644).await.unwrap(),
                position: 0,
                end: SIZE as u64,
                chunk,
            }
        })
    }

    struct BodyRead;

    impl Request<Body> for BodyRead {
        type Reply = usize;

        async fn run(self, state: &mut Body) -> usize {
            body_work(state).await
        }
    }

    #[derive(Clone, Copy)]
    struct Numbers {
        ops_per_second: f64,
        p50: Duration,
        p99: Duration,
    }

    fn numbers(mut latencies: Vec<Duration>, elapsed: Duration, calls: usize) -> Numbers {
        latencies.sort_unstable();
        Numbers {
            ops_per_second: calls as f64 / elapsed.as_secs_f64(),
            p50: latencies[latencies.len() / 2],
            p99: latencies[latencies.len() * 99 / 100],
        }
    }

    /// The way N-API drives a call: this thread (JavaScript) issues `width`
    /// calls at once, each becomes a task on the worker threads, and each
    /// completion comes back to this thread, which sleeps meanwhile.
    /// Latency is from the issue of a call to its completion being seen
    /// here, so it includes two thread wake-ups.
    fn drive_threads<F>(
        runtime: &Runtime,
        width: usize,
        rounds: usize,
        issue: &mut impl FnMut() -> F,
    ) -> Numbers
    where
        F: Future<Output = usize> + Send + 'static,
    {
        let (done, completions) = std::sync::mpsc::channel::<(usize, usize)>();
        let mut latencies = Vec::with_capacity(width * rounds);
        let mut issued = vec![Instant::now(); width];
        let started = Instant::now();
        for _ in 0..rounds {
            for (slot, at) in issued.iter_mut().enumerate() {
                *at = Instant::now();
                let future = issue();
                let done = done.clone();
                runtime.spawn(async move {
                    let _ = done.send((slot, future.await));
                });
            }
            for _ in 0..width {
                let (slot, bytes) = completions.recv().unwrap();
                assert!(bytes > 0);
                latencies.push(issued[slot].elapsed());
            }
        }
        numbers(latencies, started.elapsed(), width * rounds)
    }

    /// The same calls with the issuer and every task on one thread (a
    /// runtime with no I/O driver, so waking the issuer is not a system
    /// call): what is left is the processor cost of the design, its
    /// allocations and task switches. Latency is per batch of `width`
    /// calls, divided by `width`.
    fn drive_one_thread<F>(
        runtime: &Runtime,
        width: usize,
        rounds: usize,
        issue: &mut impl FnMut() -> F,
    ) -> Numbers
    where
        F: Future<Output = usize> + Send + 'static,
    {
        runtime.block_on(async {
            let mut latencies = Vec::with_capacity(rounds);
            let mut tasks = Vec::with_capacity(width);
            let started = Instant::now();
            for _ in 0..rounds {
                let at = Instant::now();
                for _ in 0..width {
                    tasks.push(rt_tokio::spawn(issue()));
                }
                for task in tasks.drain(..) {
                    assert!(task.await.unwrap() > 0);
                }
                latencies.push(at.elapsed() / width as u32);
            }
            numbers(latencies, started.elapsed(), width * rounds)
        })
    }

    /// An owner task: requests arrive over a channel in issue order and are
    /// answered through a reply channel each.
    fn owner<T: Send + 'static, W>(
        runtime: &Runtime,
        mut state: T,
        work: W,
    ) -> mpsc::UnboundedSender<oneshot::Sender<usize>>
    where
        W: for<'a> Fn(&'a mut T) -> Pin<Box<dyn Future<Output = usize> + Send + 'a>>
            + Send
            + 'static,
    {
        let (requests, mut inbox) = mpsc::unbounded_channel::<oneshot::Sender<usize>>();
        runtime.spawn(async move {
            while let Some(reply) = inbox.recv().await {
                let _ = reply.send(work(&mut state).await);
            }
        });
        requests
    }

    fn setting(name: &str, default: usize) -> usize {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    }

    /// Run the three designs in alternating rounds, so that a disturbance on
    /// a shared machine falls on all of them, and print the best and the
    /// median round of each.
    fn compare<A, B, C>(
        path: &str,
        one_thread: bool,
        width: usize,
        mut lock: impl FnMut() -> A,
        mut owner: impl FnMut() -> B,
        mut serial: impl FnMut() -> C,
        runtime: &Runtime,
    ) where
        A: Future<Output = usize> + Send + 'static,
        B: Future<Output = usize> + Send + 'static,
        C: Future<Output = usize> + Send + 'static,
    {
        let rounds = setting("SERIAL_BENCH_ROUNDS", 9);
        let calls = setting(
            "SERIAL_BENCH_CALLS",
            if one_thread { 160_000 } else { 16_000 },
        );
        let batches = (calls / width).max(1);
        let mut results: [Vec<Numbers>; 3] = Default::default();
        for round in 0..=rounds {
            let round_numbers = if one_thread {
                [
                    drive_one_thread(runtime, width, batches, &mut lock),
                    drive_one_thread(runtime, width, batches, &mut owner),
                    drive_one_thread(runtime, width, batches, &mut serial),
                ]
            } else {
                [
                    drive_threads(runtime, width, batches, &mut lock),
                    drive_threads(runtime, width, batches, &mut owner),
                    drive_threads(runtime, width, batches, &mut serial),
                ]
            };
            // The first round warms up.
            if round > 0 {
                for (all, one) in results.iter_mut().zip(round_numbers) {
                    all.push(one);
                }
            }
        }
        let mode = if one_thread { "one thread" } else { "threads" };
        for (design, mut all) in ["TurnLock", "owner task", "Serial"]
            .into_iter()
            .zip(results)
        {
            all.sort_by(|a, b| a.ops_per_second.total_cmp(&b.ops_per_second));
            let best = all[all.len() - 1];
            let median = all[all.len() / 2];
            let worst = all[0];
            println!(
                "{path:<16} {mode:<10} width {width:>2}  {design:<10}  ops/s best {:>9.0} median {:>9.0} worst {:>9.0}  | best round p50 {:>9.3?} p99 {:>9.3?}  | median round p50 {:>9.3?} p99 {:>9.3?}",
                best.ops_per_second,
                median.ops_per_second,
                worst.ops_per_second,
                best.p50,
                best.p99,
                median.p50,
                median.p99,
            );
        }
    }

    fn bench_runtime(one_thread: bool) -> Runtime {
        if one_thread {
            rt_tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
        } else {
            runtime()
        }
    }

    fn bench_fuse(one_thread: bool, width: usize) {
        let runtime = bench_runtime(one_thread);
        let frame = Arc::new(fuse_frame(3, 1, &[0; 16]));
        let lock = Arc::new(TurnLock::new(fuse_session(&runtime)));
        let owner_frame = Arc::clone(&frame);
        let requests = owner(&runtime, fuse_session(&runtime), move |session| {
            let frame = Arc::clone(&owner_frame);
            Box::pin(async move { fuse_work(session, &frame).await })
        });
        let serial = Serial::new(fuse_session(&runtime));
        compare(
            "fuse handle",
            one_thread,
            width,
            || {
                let lock = Arc::clone(&lock);
                let frame = Arc::clone(&frame);
                async move {
                    let mut session = lock.lock().await;
                    fuse_work(&mut session, &frame).await
                }
            },
            || {
                let (reply, receiver) = oneshot::channel();
                requests.send(reply).unwrap();
                async move { receiver.await.unwrap() }
            },
            || {
                let submitted = serial.submit(FuseHandle(Arc::clone(&frame)));
                async move { submitted.reply().await.unwrap() }
            },
            &runtime,
        );
    }

    fn bench_body(one_thread: bool, width: usize, chunk: usize) {
        let runtime = bench_runtime(one_thread);
        let lock = Arc::new(TurnLock::new(body(&runtime, chunk)));
        let requests = owner(&runtime, body(&runtime, chunk), |body| {
            Box::pin(body_work(body))
        });
        let serial = Serial::new(body(&runtime, chunk));
        compare(
            &format!("body {}KiB chunk", chunk / 1024),
            one_thread,
            width,
            || {
                let lock = Arc::clone(&lock);
                async move {
                    let mut body = lock.lock().await;
                    body_work(&mut body).await
                }
            },
            || {
                let (reply, receiver) = oneshot::channel();
                requests.send(reply).unwrap();
                async move { receiver.await.unwrap() }
            },
            || {
                let submitted = serial.submit(BodyRead);
                async move { submitted.reply().await.unwrap() }
            },
            &runtime,
        );
    }

    struct Count;

    impl Request<usize> for Count {
        type Reply = usize;

        async fn run(self, state: &mut usize) -> usize {
            *state += 1;
            *state
        }
    }

    /// No work at all inside the section: the cost of the design itself.
    fn bench_empty(one_thread: bool, width: usize) {
        let runtime = bench_runtime(one_thread);
        let lock = Arc::new(TurnLock::new(0_usize));
        let requests = owner(&runtime, 0_usize, |count| {
            Box::pin(async move {
                *count += 1;
                *count
            })
        });
        let serial = Serial::new(0_usize);
        compare(
            "empty section",
            one_thread,
            width,
            || {
                let lock = Arc::clone(&lock);
                async move {
                    let mut count = lock.lock().await;
                    *count += 1;
                    *count
                }
            },
            || {
                let (reply, receiver) = oneshot::channel();
                requests.send(reply).unwrap();
                async move { receiver.await.unwrap() }
            },
            || {
                let submitted = serial.submit(Count);
                async move { submitted.reply().await.unwrap() }
            },
            &runtime,
        );
    }

    #[test]
    #[ignore = "benchmark; run with --release --ignored --nocapture"]
    fn bench_serialised_calls() {
        for one_thread in [true, false] {
            for width in [1, 2, 16] {
                bench_empty(one_thread, width);
            }
            for width in [1, 2, 16] {
                bench_fuse(one_thread, width);
            }
            for width in [1, 2] {
                bench_body(one_thread, width, 64 * 1024);
            }
        }
    }
}
