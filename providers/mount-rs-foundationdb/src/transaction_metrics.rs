//! Shared FoundationDB retry and range-page control flow.
//!
//! Retry and pager control flow is adapted from foundationdb-rs 0.11.0,
//! Copyright 2018 foundationdb-rs developers,
//! https://github.com/Clikengo/foundationdb-rs/graphs/contributors.
//! Licensed under Apache-2.0 OR MIT, at the recipient's option, preserving the
//! upstream permission to copy/modify/distribute only according to those terms.
//! Locked package SHA256:
//! c9a0b9e89be4942ad3c6cf76789e7eaf9489fcca2b06b1449b2d9eb015424793.
//! Sources: database.rs:465-506 and transaction.rs:679-699. Dependency upgrades
//! require re-review of this adapter against those algorithms and ownership,
//! maybe-committed, retry-budget and RangeOption::next_range semantics.

use std::future::Future;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Create,
    ClosureAttempt,
    Get,
    GetKey,
    RangePage,
    Commit,
    OnError,
}

pub(crate) trait AttemptGuard: Send {
    fn success(&mut self, bytes: u64);
    fn error(&mut self);
}

pub(crate) trait Observer: Sync {
    type Guard: AttemptGuard;
    fn begin(&self, kind: Kind) -> Self::Guard;
}

pub(crate) fn call_sync<O, F, T, E, B>(observer: &O, kind: Kind, make: F, bytes: B) -> Result<T, E>
where
    O: Observer,
    F: FnOnce() -> Result<T, E>,
    B: FnOnce(&T) -> u64,
{
    let mut guard = observer.begin(kind);
    let result = make();
    match &result {
        Ok(value) => guard.success(bytes(value)),
        Err(_) => guard.error(),
    }
    result
}

pub(crate) async fn call_async<O, F, Fut, T, E, B>(
    observer: &O,
    kind: Kind,
    make: F,
    bytes: B,
) -> Result<T, E>
where
    O: Observer,
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = Result<T, E>> + Send,
    T: Send,
    E: Send,
    B: FnOnce(&T) -> u64 + Send,
{
    let mut guard = observer.begin(kind);
    let result = make().await;
    match &result {
        Ok(value) => guard.success(bytes(value)),
        Err(_) => guard.error(),
    }
    result
}

#[cfg(feature = "foundationdb")]
pub(crate) struct CoreObserver;

#[cfg(feature = "foundationdb")]
impl Observer for CoreObserver {
    type Guard = mount_rs_core::diagnostics::storage::Span<'static>;
    fn begin(&self, kind: Kind) -> Self::Guard {
        use mount_rs_core::diagnostics::storage::{Operation, Span};
        let operation = match kind {
            Kind::Create => Operation::FoundationDbTransactionCreate,
            Kind::ClosureAttempt => Operation::FoundationDbTransactionClosureAttempt,
            Kind::Get => Operation::FoundationDbReadGet,
            Kind::GetKey => Operation::FoundationDbReadGetKey,
            Kind::RangePage => Operation::FoundationDbReadGetRangePage,
            Kind::Commit => Operation::FoundationDbTransactionCommit,
            Kind::OnError => Operation::FoundationDbTransactionOnError,
        };
        Span::new(operation)
    }
}

#[cfg(feature = "foundationdb")]
impl AttemptGuard for mount_rs_core::diagnostics::storage::Span<'_> {
    fn success(&mut self, bytes: u64) {
        self.finish_success(bytes);
    }
    fn error(&mut self) {
        self.finish_error();
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RetryOptions {
    pub(crate) retry_limit: Option<u32>,
    pub(crate) time_out: Option<Duration>,
    pub(crate) is_idempotent: bool,
}

pub(crate) trait TransactionDriver: Send {
    type Transaction: Send + Sync;
    type Item: Send;
    type Error: Send;
    type RetryError: Send;
    type CommitError: Send;

    fn now(&self) -> Instant;
    fn create(&mut self) -> Result<Self::Transaction, Self::Error>;
    fn invoke<'a>(
        &'a mut self,
        transaction: &'a Self::Transaction,
    ) -> impl Future<Output = Result<Self::Item, Self::Error>> + Send + 'a;
    fn commit(
        &mut self,
        transaction: Self::Transaction,
    ) -> impl Future<Output = Result<(), Self::CommitError>> + Send;
    fn commit_on_error(
        &mut self,
        error: Self::CommitError,
    ) -> impl Future<Output = Result<Self::Transaction, Self::Error>> + Send;
    fn on_error(
        &mut self,
        transaction: Self::Transaction,
        error: Self::RetryError,
    ) -> impl Future<Output = Result<Self::Transaction, Self::Error>> + Send;
    fn commit_maybe_committed(error: &Self::CommitError) -> bool;
    fn retry_maybe_committed(error: &Self::RetryError) -> bool;
    fn into_retry_error(error: Self::Error) -> Result<Self::RetryError, Self::Error>;
    fn from_commit_error(error: Self::CommitError) -> Self::Error;
    fn from_retry_error(error: Self::RetryError) -> Self::Error;
}

fn can_retry<D: TransactionDriver>(
    driver: &D,
    tries: &mut u32,
    retry_limit: Option<u32>,
    deadline: Option<Instant>,
) -> bool {
    *tries += 1;
    retry_limit.map(|limit| *tries < limit).unwrap_or(true)
        && deadline.map(|time| driver.now() < time).unwrap_or(true)
}

/// Control flow adapted from locked foundationdb 0.11.0 database.rs:465-506.
/// Native-driver differences are restricted to owned handle/error conversions.
pub(crate) async fn run_transaction<D: TransactionDriver, O: Observer>(
    driver: &mut D,
    options: RetryOptions,
    observer: &O,
) -> Result<D::Item, D::Error> {
    let deadline = options.time_out.map(|duration| driver.now() + duration);
    let mut tries = 0;
    let mut transaction = call_sync(observer, Kind::Create, || driver.create(), |_| 0)?;
    loop {
        let result = call_async(
            observer,
            Kind::ClosureAttempt,
            || driver.invoke(&transaction),
            |_| 0,
        )
        .await;
        transaction = match result {
            Ok(item) => {
                match call_async(observer, Kind::Commit, || driver.commit(transaction), |_| 0).await
                {
                    Ok(()) => return Ok(item),
                    Err(error) => {
                        if (options.is_idempotent || !D::commit_maybe_committed(&error))
                            && can_retry(driver, &mut tries, options.retry_limit, deadline)
                        {
                            call_async(
                                observer,
                                Kind::OnError,
                                || driver.commit_on_error(error),
                                |_| 0,
                            )
                            .await?
                        } else {
                            return Err(D::from_commit_error(error));
                        }
                    }
                }
            }
            Err(error) => match D::into_retry_error(error) {
                Ok(error) => {
                    if (options.is_idempotent || !D::retry_maybe_committed(&error))
                        && can_retry(driver, &mut tries, options.retry_limit, deadline)
                    {
                        call_async(
                            observer,
                            Kind::OnError,
                            || driver.on_error(transaction, error),
                            |_| 0,
                        )
                        .await?
                    } else {
                        return Err(D::from_retry_error(error));
                    }
                }
                Err(error) => return Err(error),
            },
        };
    }
}

pub(crate) trait RangeDriver: Send + Sync {
    type Options: Send + Sync;
    type Page: Send;
    type Error: Send;

    fn get_range(
        &self,
        options: &Self::Options,
        iteration: usize,
    ) -> impl Future<Output = Result<Self::Page, Self::Error>> + Send;
    fn next_range(&self, options: Self::Options, page: &Self::Page) -> Option<Self::Options>;
    fn successful_bytes(page: &Self::Page) -> u64;
}

pub(crate) type RangeState<O> = (i32, Option<O>);
pub(crate) type RangeStep<P, E, O> = Option<(Result<P, E>, RangeState<O>)>;

/// One unfold step matching transaction.rs:679-699; no row flattening occurs here.
pub(crate) async fn range_step<D: RangeDriver, O: Observer>(
    driver: D,
    observer: &O,
    state: RangeState<D::Options>,
) -> RangeStep<D::Page, D::Error, D::Options> {
    let (iteration, options) = state;
    let options = options?;
    let result = call_async(
        observer,
        Kind::RangePage,
        || driver.get_range(&options, iteration as usize),
        D::successful_bytes,
    )
    .await;
    let next = match &result {
        Ok(page) => driver.next_range(options, page),
        Err(_) => None,
    };
    Some((result, (iteration + 1, next)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};

    thread_local! {
        static COUNT_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
        static ALLOCATION_CALLS: Cell<u64> = const { Cell::new(0) };
    }
    struct CountingAllocator;
    fn allocated() {
        let _ = COUNT_ALLOCATIONS.try_with(|enabled| {
            if enabled.get() {
                let _ = ALLOCATION_CALLS.try_with(|calls| calls.set(calls.get() + 1));
            }
        });
    }
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            allocated();
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            allocated();
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            allocated();
            unsafe { System.realloc(pointer, layout, size) }
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) }
        }
    }
    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct Row {
        calls: u64,
        success: u64,
        error: u64,
        cancelled: u64,
        bytes: u64,
        in_flight: u64,
    }
    #[derive(Clone, Default)]
    struct Recording(Arc<Mutex<[Row; 7]>>);
    impl Recording {
        fn row(&self, kind: Kind) -> Row {
            self.0.lock().unwrap()[kind as usize]
        }
    }
    struct Guard {
        recording: Recording,
        kind: Kind,
        finished: bool,
    }
    impl Guard {
        fn finish(&mut self, outcome: u8, bytes: u64) {
            if self.finished {
                return;
            }
            self.finished = true;
            let mut rows = self.recording.0.lock().unwrap();
            let row = &mut rows[self.kind as usize];
            row.calls += 1;
            match outcome {
                0 => row.success += 1,
                1 => row.error += 1,
                _ => row.cancelled += 1,
            }
            row.bytes += bytes;
            row.in_flight -= 1;
        }
    }
    impl AttemptGuard for Guard {
        fn success(&mut self, bytes: u64) {
            self.finish(0, bytes);
        }
        fn error(&mut self) {
            self.finish(1, 0);
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            self.finish(2, 0);
        }
    }
    impl Observer for Recording {
        type Guard = Guard;
        fn begin(&self, kind: Kind) -> Guard {
            self.0.lock().unwrap()[kind as usize].in_flight += 1;
            Guard {
                recording: self.clone(),
                kind,
                finished: false,
            }
        }
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Error {
        Fdb { code: u16, maybe: bool },
        Fs(&'static str),
    }
    const CONFLICT: Error = Error::Fdb {
        code: 1020,
        maybe: false,
    };
    const UNKNOWN: Error = Error::Fdb {
        code: 1021,
        maybe: true,
    };
    struct CommitError {
        transaction: u64,
        error: Error,
    }
    struct ModeledFuture<T> {
        result: Option<T>,
    }
    impl<T: Unpin> Future for ModeledFuture<T> {
        type Output = T;
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<T> {
            match self.get_mut().result.take() {
                Some(result) => Poll::Ready(result),
                None => Poll::Pending,
            }
        }
    }
    #[derive(Debug, PartialEq, Eq)]
    struct Dispatch {
        kind: Kind,
        transaction: u64,
        active_at_native_call: u64,
    }
    struct Driver {
        recording: Recording,
        dispatches: Vec<Dispatch>,
        closures: VecDeque<Result<u64, Error>>,
        commits: VecDeque<Result<(), Error>>,
        resets: VecDeque<Result<(), Error>>,
        create_error: Option<Error>,
        pending: Option<Kind>,
        base: Instant,
        elapsed: Duration,
        create_elapsed: Duration,
        commit_elapsed: Duration,
        clock_reads: std::sync::atomic::AtomicU64,
    }
    impl Driver {
        fn new(recording: &Recording) -> Self {
            Self {
                recording: recording.clone(),
                dispatches: Vec::new(),
                closures: VecDeque::new(),
                commits: VecDeque::new(),
                resets: VecDeque::new(),
                create_error: None,
                pending: None,
                base: Instant::now(),
                elapsed: Duration::ZERO,
                create_elapsed: Duration::ZERO,
                commit_elapsed: Duration::ZERO,
                clock_reads: std::sync::atomic::AtomicU64::new(0),
            }
        }
        fn dispatch(&mut self, kind: Kind, transaction: u64) {
            self.dispatches.push(Dispatch {
                kind,
                transaction,
                active_at_native_call: self.recording.row(kind).in_flight,
            });
        }
        fn future<T>(&self, kind: Kind, value: T) -> ModeledFuture<T> {
            ModeledFuture {
                result: (self.pending != Some(kind)).then_some(value),
            }
        }
        fn reset(&mut self, transaction: u64) -> ModeledFuture<Result<u64, Error>> {
            self.dispatch(Kind::OnError, transaction);
            let result = self
                .resets
                .pop_front()
                .unwrap_or(Ok(()))
                .map(|()| transaction);
            self.future(Kind::OnError, result)
        }
    }
    impl TransactionDriver for Driver {
        type Transaction = u64;
        type Item = u64;
        type Error = Error;
        type RetryError = Error;
        type CommitError = CommitError;
        fn now(&self) -> Instant {
            self.clock_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.base + self.elapsed
        }
        fn create(&mut self) -> Result<u64, Error> {
            self.dispatch(Kind::Create, 7);
            self.elapsed += self.create_elapsed;
            self.create_error.map_or(Ok(7), Err)
        }
        fn invoke<'a>(
            &'a mut self,
            transaction: &'a u64,
        ) -> impl Future<Output = Result<u64, Error>> + Send + 'a {
            self.dispatch(Kind::ClosureAttempt, *transaction);
            let result = self.closures.pop_front().unwrap_or(Ok(91));
            self.future(Kind::ClosureAttempt, result)
        }
        fn commit(
            &mut self,
            transaction: u64,
        ) -> impl Future<Output = Result<(), CommitError>> + Send {
            self.dispatch(Kind::Commit, transaction);
            self.elapsed += self.commit_elapsed;
            let result = self
                .commits
                .pop_front()
                .unwrap_or(Ok(()))
                .map_err(|error| CommitError { transaction, error });
            self.future(Kind::Commit, result)
        }
        fn commit_on_error(
            &mut self,
            error: CommitError,
        ) -> impl Future<Output = Result<u64, Error>> + Send {
            self.reset(error.transaction)
        }
        fn on_error(
            &mut self,
            transaction: u64,
            _: Error,
        ) -> impl Future<Output = Result<u64, Error>> + Send {
            self.reset(transaction)
        }
        fn commit_maybe_committed(error: &CommitError) -> bool {
            Self::retry_maybe_committed(&error.error)
        }
        fn retry_maybe_committed(error: &Error) -> bool {
            matches!(error, Error::Fdb { maybe: true, .. })
        }
        fn into_retry_error(error: Error) -> Result<Error, Error> {
            match error {
                Error::Fdb { .. } => Ok(error),
                _ => Err(error),
            }
        }
        fn from_commit_error(error: CommitError) -> Error {
            error.error
        }
        fn from_retry_error(error: Error) -> Error {
            error
        }
    }
    fn options(idempotent: bool) -> RetryOptions {
        RetryOptions {
            retry_limit: Some(4),
            time_out: None,
            is_idempotent: idempotent,
        }
    }
    fn ready<F: Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut context = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("owned modeled future unexpectedly pending"),
        }
    }
    fn assert_row(recording: &Recording, kind: Kind, success: u64, error: u64, cancelled: u64) {
        let row = recording.row(kind);
        assert_eq!(
            (
                row.calls,
                row.success,
                row.error,
                row.cancelled,
                row.in_flight
            ),
            (success + error + cancelled, success, error, cancelled, 0),
            "{kind:?}"
        );
    }

    #[test]
    fn runner_records_creation_closure_and_commit_before_dispatch() {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        assert_eq!(
            ready(run_transaction(&mut driver, options(false), &recording)),
            Ok(91)
        );
        for kind in [Kind::Create, Kind::ClosureAttempt, Kind::Commit] {
            assert_row(&recording, kind, 1, 0, 0);
        }
        assert!(
            driver
                .dispatches
                .iter()
                .all(|event| event.active_at_native_call == 1)
        );
    }
    #[test]
    fn creation_error_is_terminal_without_closure_or_commit() {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        driver.create_error = Some(CONFLICT);
        assert_eq!(
            ready(run_transaction(&mut driver, options(false), &recording)),
            Err(CONFLICT)
        );
        assert_eq!(driver.dispatches.len(), 1);
        assert_row(&recording, Kind::Create, 0, 1, 0);
    }
    #[test]
    fn commit_retry_records_actual_attempts_and_preserves_same_handle() {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        driver.commits.extend([Err(CONFLICT), Ok(())]);
        assert_eq!(
            ready(run_transaction(&mut driver, options(false), &recording)),
            Ok(91)
        );
        assert!(driver.dispatches.iter().all(|event| event.transaction == 7));
        assert_row(&recording, Kind::Create, 1, 0, 0);
        assert_row(&recording, Kind::ClosureAttempt, 2, 0, 0);
        assert_row(&recording, Kind::Commit, 1, 1, 0);
        assert_row(&recording, Kind::OnError, 1, 0, 0);
    }
    #[test]
    fn closure_retry_does_not_invent_first_commit() {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        driver.closures.extend([Err(CONFLICT), Ok(55)]);
        assert_eq!(
            ready(run_transaction(&mut driver, options(false), &recording)),
            Ok(55)
        );
        assert_row(&recording, Kind::ClosureAttempt, 1, 1, 0);
        assert_row(&recording, Kind::Commit, 1, 0, 0);
        assert_row(&recording, Kind::OnError, 1, 0, 0);
    }
    #[test]
    fn on_error_failure_keeps_original_error_without_second_closure() {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        driver.commits.push_back(Err(CONFLICT));
        driver.resets.push_back(Err(Error::Fs("on_error failed")));
        assert_eq!(
            ready(run_transaction(&mut driver, options(false), &recording)),
            Err(Error::Fs("on_error failed"))
        );
        assert_eq!(
            driver
                .dispatches
                .iter()
                .filter(|event| event.kind == Kind::ClosureAttempt)
                .count(),
            1
        );
        assert_row(&recording, Kind::OnError, 0, 1, 0);
    }
    #[test]
    fn non_fdb_closure_error_skips_commit_and_reset() {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        driver.closures.push_back(Err(Error::Fs("decode failure")));
        assert_eq!(
            ready(run_transaction(&mut driver, options(false), &recording)),
            Err(Error::Fs("decode failure"))
        );
        assert_eq!(
            driver
                .dispatches
                .iter()
                .map(|event| event.kind)
                .collect::<Vec<_>>(),
            [Kind::Create, Kind::ClosureAttempt]
        );
        assert_row(&recording, Kind::ClosureAttempt, 0, 1, 0);
    }
    #[test]
    fn metadata_unknown_commit_does_not_reset_or_replay() {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        driver.commits.push_back(Err(UNKNOWN));
        assert_eq!(
            ready(run_transaction(&mut driver, options(false), &recording)),
            Err(UNKNOWN)
        );
        assert_eq!(driver.dispatches.len(), 3);
        assert_row(&recording, Kind::Commit, 0, 1, 0);
        assert_row(&recording, Kind::OnError, 0, 0, 0);
    }
    #[test]
    fn metadata_unknown_closure_error_skips_commit_and_reset() {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        driver.closures.push_back(Err(UNKNOWN));
        assert_eq!(
            ready(run_transaction(&mut driver, options(false), &recording)),
            Err(UNKNOWN)
        );
        assert_eq!(driver.dispatches.len(), 2);
        assert_row(&recording, Kind::ClosureAttempt, 0, 1, 0);
    }
    #[test]
    fn idempotent_unknown_commit_can_reset_and_retry() {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        driver.commits.extend([Err(UNKNOWN), Ok(())]);
        assert_eq!(
            ready(run_transaction(&mut driver, options(true), &recording)),
            Ok(91)
        );
        assert_row(&recording, Kind::Commit, 1, 1, 0);
        assert_row(&recording, Kind::OnError, 1, 0, 0);
    }
    fn cancelled_runner_stage(kind: Kind) {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        driver.pending = Some(kind);
        if kind == Kind::OnError {
            driver.commits.push_back(Err(CONFLICT));
        }
        {
            let mut future =
                std::pin::pin!(run_transaction(&mut driver, options(false), &recording));
            let mut context = Context::from_waker(Waker::noop());
            assert!(future.as_mut().poll(&mut context).is_pending());
            assert_eq!(recording.row(kind).in_flight, 1, "{kind:?}");
        }
        assert_row(&recording, kind, 0, 0, 1);
    }
    #[test]
    fn pending_closure_drop_records_abandoned_attempt() {
        cancelled_runner_stage(Kind::ClosureAttempt);
    }
    #[test]
    fn pending_commit_drop_records_abandoned_attempt() {
        cancelled_runner_stage(Kind::Commit);
    }
    #[test]
    fn pending_on_error_drop_records_abandoned_attempt() {
        cancelled_runner_stage(Kind::OnError);
    }
    #[test]
    fn pending_commit_drop_is_cancelled_not_successful_or_rolled_back() {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        driver.pending = Some(Kind::Commit);
        {
            let mut future =
                std::pin::pin!(run_transaction(&mut driver, options(false), &recording));
            assert!(
                future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
        }
        assert_row(&recording, Kind::Commit, 0, 0, 1);
        assert_eq!(
            driver
                .dispatches
                .iter()
                .filter(|event| event.kind == Kind::OnError)
                .count(),
            0
        );
    }
    fn eager_read_factory(kind: Kind) {
        let recording = Recording::default();
        let result = ready(call_async(
            &recording,
            kind,
            || {
                assert_eq!(
                    recording.row(kind).in_flight,
                    1,
                    "span must precede eager native call"
                );
                std::future::ready(Ok::<_, Error>([1u8, 2, 3]))
            },
            |value| value.len() as u64,
        ));
        assert_eq!(result, Ok([1, 2, 3]));
        assert_row(&recording, kind, 1, 0, 0);
        assert_eq!(recording.row(kind).bytes, 3);
    }
    #[test]
    fn eager_get_starts_before_dispatch_and_preserves_payload() {
        eager_read_factory(Kind::Get);
    }
    #[test]
    fn eager_get_key_starts_before_dispatch_and_preserves_payload() {
        eager_read_factory(Kind::GetKey);
    }
    #[test]
    fn eager_range_page_starts_before_dispatch_and_preserves_payload() {
        eager_read_factory(Kind::RangePage);
    }
    #[test]
    fn absent_get_is_successful_with_zero_selected_payload() {
        let recording = Recording::default();
        assert_eq!(
            ready(call_async(
                &recording,
                Kind::Get,
                || std::future::ready(Ok::<Option<[u8; 3]>, Error>(None)),
                |value| value.as_ref().map_or(0, |bytes| bytes.len() as u64)
            )),
            Ok(None)
        );
        assert_row(&recording, Kind::Get, 1, 0, 0);
        assert_eq!(recording.row(Kind::Get).bytes, 0);
    }
    #[test]
    fn read_error_preserves_error_without_known_payload() {
        let recording = Recording::default();
        assert_eq!(
            ready(call_async(
                &recording,
                Kind::Get,
                || std::future::ready(Err::<[u8; 3], Error>(CONFLICT)),
                |_| 3
            )),
            Err(CONFLICT)
        );
        assert_row(&recording, Kind::Get, 0, 1, 0);
        assert_eq!(recording.row(Kind::Get).bytes, 0);
    }
    #[test]
    fn unpolled_factory_does_not_dispatch_or_create_attempt() {
        let recording = Recording::default();
        let dispatched = std::sync::atomic::AtomicU64::new(0);
        let future = call_async(
            &recording,
            Kind::Get,
            || {
                dispatched.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                std::future::ready(Ok::<(), Error>(()))
            },
            |_| 0,
        );
        drop(future);
        assert_eq!(dispatched.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert_row(&recording, Kind::Get, 0, 0, 0);
    }
    fn cancelled_read(kind: Kind) {
        let recording = Recording::default();
        {
            let mut future = std::pin::pin!(call_async(
                &recording,
                kind,
                std::future::pending::<Result<(), Error>>,
                |_| 0
            ));
            assert!(
                future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
        }
        assert_row(&recording, kind, 0, 0, 1);
    }
    #[test]
    fn dispatched_pending_get_drop_records_one_cancelled_attempt() {
        cancelled_read(Kind::Get);
    }
    #[test]
    fn dispatched_pending_get_key_drop_records_one_cancelled_attempt() {
        cancelled_read(Kind::GetKey);
    }
    #[test]
    fn dispatched_pending_range_page_drop_records_one_cancelled_attempt() {
        cancelled_read(Kind::RangePage);
    }

    #[test]
    fn warmed_generic_helpers_record_without_added_allocations() {
        let recording = Recording::default();
        let _ = recording.row(Kind::Get);
        ALLOCATION_CALLS.with(|calls| calls.set(0));
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(true));
        let create = call_sync(&recording, Kind::Create, || Ok::<u64, Error>(7), |_| 0);
        let get = ready(call_async(
            &recording,
            Kind::Get,
            || std::future::ready(Ok::<[u8; 2], Error>([1, 2])),
            |_| 2,
        ));
        let failed = ready(call_async(
            &recording,
            Kind::Get,
            || std::future::ready(Err::<(), Error>(CONFLICT)),
            |_| 0,
        ));
        {
            let mut future = std::pin::pin!(call_async(
                &recording,
                Kind::OnError,
                std::future::pending::<Result<(), Error>>,
                |_| 0
            ));
            assert!(
                future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
        }
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
        assert_eq!(
            ALLOCATION_CALLS.with(Cell::get),
            0,
            "shared diagnostics helper allocated"
        );
        assert_eq!((create, get, failed), (Ok(7), Ok([1, 2]), Err(CONFLICT)));
        assert_row(&recording, Kind::Create, 1, 0, 0);
        assert_row(&recording, Kind::Get, 1, 1, 0);
        assert_row(&recording, Kind::OnError, 0, 0, 1);
    }

    // These pass on the uninstrumented baseline and protect actual shared retry
    // control flow; they are not a second independently maintained runner model.
    #[test]
    fn strict_retry_limit_does_not_consume_clock_after_limit_rejection() {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        driver.commits.push_back(Err(CONFLICT));
        let opts = RetryOptions {
            retry_limit: Some(1),
            time_out: Some(Duration::from_secs(1)),
            is_idempotent: false,
        };
        assert_eq!(
            ready(run_transaction(&mut driver, opts, &recording)),
            Err(CONFLICT)
        );
        assert_eq!(
            driver
                .clock_reads
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(driver.dispatches.len(), 3);
    }
    #[test]
    fn timeout_is_anchored_before_create_and_comparison_is_strict() {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        driver.create_elapsed = Duration::from_millis(2);
        driver.commit_elapsed = Duration::from_millis(3);
        driver.commits.push_back(Err(CONFLICT));
        let opts = RetryOptions {
            retry_limit: Some(4),
            time_out: Some(Duration::from_millis(5)),
            is_idempotent: false,
        };
        assert_eq!(
            ready(run_transaction(&mut driver, opts, &recording)),
            Err(CONFLICT)
        );
        assert_eq!(driver.dispatches.len(), 3);
    }
    #[test]
    fn maybe_committed_guard_short_circuits_retry_clock() {
        let recording = Recording::default();
        let mut driver = Driver::new(&recording);
        driver.commits.push_back(Err(UNKNOWN));
        let opts = RetryOptions {
            retry_limit: Some(4),
            time_out: Some(Duration::from_secs(1)),
            is_idempotent: false,
        };
        assert_eq!(
            ready(run_transaction(&mut driver, opts, &recording)),
            Err(UNKNOWN)
        );
        assert_eq!(
            driver
                .clock_reads
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct RangeOptions {
        begin: u64,
        end: u64,
        limit: Option<usize>,
        reverse: bool,
        snapshot: bool,
        target_bytes: usize,
        mode: u8,
    }
    #[derive(Clone, Debug)]
    struct Page {
        keys: Vec<u64>,
        bytes: u64,
        more: bool,
    }
    #[derive(Clone)]
    struct Pages {
        recording: Recording,
        values: Arc<Mutex<VecDeque<Result<Page, Error>>>>,
        dispatches: Arc<Mutex<Vec<(RangeOptions, usize, u64)>>>,
        pending: bool,
    }
    impl RangeDriver for Pages {
        type Options = RangeOptions;
        type Page = Page;
        type Error = Error;
        fn get_range(
            &self,
            options: &RangeOptions,
            iteration: usize,
        ) -> impl Future<Output = Result<Page, Error>> + Send {
            self.dispatches.lock().unwrap().push((
                options.clone(),
                iteration,
                self.recording.row(Kind::RangePage).in_flight,
            ));
            let result = self
                .values
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted page");
            ModeledFuture {
                result: (!self.pending).then_some(result),
            }
        }
        fn next_range(&self, mut options: RangeOptions, page: &Page) -> Option<RangeOptions> {
            if !page.more {
                return None;
            }
            let last = *page.keys.last()?;
            if let Some(limit) = &mut options.limit {
                *limit = limit.saturating_sub(page.keys.len());
                if *limit == 0 {
                    return None;
                }
            }
            if options.reverse {
                options.end = last;
            } else {
                options.begin = last + 1;
            }
            Some(options)
        }
        fn successful_bytes(page: &Page) -> u64 {
            page.bytes
        }
    }
    fn pages(recording: &Recording, values: Vec<Result<Page, Error>>) -> Pages {
        Pages {
            recording: recording.clone(),
            values: Arc::new(Mutex::new(values.into())),
            dispatches: Arc::new(Mutex::new(Vec::new())),
            pending: false,
        }
    }
    fn range_options(reverse: bool) -> RangeOptions {
        RangeOptions {
            begin: 1,
            end: 100,
            limit: Some(3),
            reverse,
            snapshot: false,
            target_bytes: 4096,
            mode: 7,
        }
    }
    #[test]
    fn pager_counts_pages_not_rows_and_passes_exact_continuation() {
        for reverse in [false, true] {
            let recording = Recording::default();
            let first_keys = if reverse { vec![90, 80] } else { vec![10, 20] };
            let driver = pages(
                &recording,
                vec![
                    Ok(Page {
                        keys: first_keys,
                        bytes: 40,
                        more: true,
                    }),
                    Ok(Page {
                        keys: vec![30],
                        bytes: 20,
                        more: true,
                    }),
                ],
            );
            let (_, state) = ready(range_step(
                driver.clone(),
                &recording,
                (1, Some(range_options(reverse))),
            ))
            .unwrap();
            let (_, state) = ready(range_step(driver.clone(), &recording, state)).unwrap();
            assert!(ready(range_step(driver.clone(), &recording, state)).is_none());
            let calls = driver.dispatches.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert_eq!((calls[0].1, calls[1].1), (1, 2));
            assert_eq!(calls[1].0.limit, Some(1));
            assert_eq!(
                (
                    calls[1].0.target_bytes,
                    calls[1].0.mode,
                    calls[1].0.snapshot
                ),
                (4096, 7, false)
            );
            assert_eq!(
                (calls[1].0.begin, calls[1].0.end),
                if reverse { (1, 80) } else { (21, 100) }
            );
            assert!(calls.iter().all(|call| call.2 == 1));
            assert_row(&recording, Kind::RangePage, 2, 0, 0);
            assert_eq!(recording.row(Kind::RangePage).bytes, 60);
        }
    }
    #[test]
    fn range_error_stops_without_inventing_next_page_or_payload() {
        let recording = Recording::default();
        let driver = pages(&recording, vec![Err(CONFLICT)]);
        let (result, state) = ready(range_step(
            driver.clone(),
            &recording,
            (1, Some(range_options(false))),
        ))
        .unwrap();
        assert!(matches!(result, Err(CONFLICT)));
        assert!(ready(range_step(driver.clone(), &recording, state)).is_none());
        assert_eq!(driver.dispatches.lock().unwrap().len(), 1);
        assert_row(&recording, Kind::RangePage, 0, 1, 0);
    }
    #[test]
    fn empty_final_page_is_one_success_and_no_extra_dispatch() {
        let recording = Recording::default();
        let driver = pages(
            &recording,
            vec![Ok(Page {
                keys: vec![],
                bytes: 0,
                more: false,
            })],
        );
        let (_, state) = ready(range_step(
            driver.clone(),
            &recording,
            (1, Some(range_options(false))),
        ))
        .unwrap();
        assert!(ready(range_step(driver.clone(), &recording, state)).is_none());
        assert_eq!(driver.dispatches.lock().unwrap().len(), 1);
        assert_row(&recording, Kind::RangePage, 1, 0, 0);
    }
    #[test]
    fn dropping_pager_before_next_step_does_not_prefetch() {
        let recording = Recording::default();
        let driver = pages(
            &recording,
            vec![Ok(Page {
                keys: vec![10],
                bytes: 20,
                more: true,
            })],
        );
        let (_, _) = ready(range_step(
            driver.clone(),
            &recording,
            (1, Some(range_options(false))),
        ))
        .unwrap();
        assert_eq!(driver.dispatches.lock().unwrap().len(), 1);
    }
    #[test]
    fn actual_pager_pending_page_drop_is_abandoned_not_empty() {
        let recording = Recording::default();
        let mut driver = pages(
            &recording,
            vec![Ok(Page {
                keys: vec![10],
                bytes: 20,
                more: false,
            })],
        );
        driver.pending = true;
        {
            let mut future = std::pin::pin!(range_step(
                driver.clone(),
                &recording,
                (1, Some(range_options(false)))
            ));
            assert!(
                future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
            assert_eq!(driver.dispatches.lock().unwrap().len(), 1);
        }
        assert_row(&recording, Kind::RangePage, 0, 0, 1);
        assert_eq!(recording.row(Kind::RangePage).bytes, 0);
    }
}
