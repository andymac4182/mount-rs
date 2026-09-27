use super::{
    observe_checkout_future, observe_first_future, observe_result_future, observe_rows_future,
};
use mount_rs_core::diagnostics::storage;
use mount_rs_core::diagnostics::storage::Operation;
use std::cell::Cell;
use std::future::{Future, pending, poll_fn, ready};
use std::task::{Context, Poll, Waker};

fn isolated(test: &str, expected_enabled: bool) -> bool {
    const CHILD: &str = "MOUNT_RS_TIDB_METRICS_TEST_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test) {
        assert_eq!(storage::enabled(), expected_enabled);
        return true;
    }
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("storage::storage_metrics_tests::{test}"),
                "--nocapture",
            ])
            .env(CHILD, test)
            .env(
                "MOUNT_RS_PROFILE_IO",
                if expected_enabled { "1" } else { "0" },
            )
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("running 1 test"), "{stdout}\n{stderr}");
        assert!(output.status.success(), "{stdout}\n{stderr}");
    }
    false
}

fn poll_ready<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("fixture-free ready future unexpectedly pending"),
    }
}

#[test]
fn checkout_cancellation_is_recorded() {
    if !isolated("checkout_cancellation_is_recorded", true) {
        return;
    }

    let before = storage::snapshot();
    let mut checkout = Box::pin(observe_checkout_future(pending::<Result<(), ()>>()));
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(
        checkout.as_mut().poll(&mut context),
        Poll::Pending
    ));
    drop(checkout);
    let delta = storage::snapshot().delta(&before).unwrap();
    let checkout = delta
        .entries
        .iter()
        .find(|entry| entry.name == "tidb.pool.checkout")
        .expect("the fixed TiDB checkout metric must be exported");
    assert_eq!(checkout.calls, 1);
    assert_eq!(checkout.cancelled, 1);
    assert_eq!(checkout.success, 0);
    assert_eq!(checkout.error, 0);
    assert_eq!(delta.in_flight, 0);
}

#[test]
fn sql_first_and_vector_results_count_only_returned_rows_and_known_bytes() {
    if !isolated(
        "sql_first_and_vector_results_count_only_returned_rows_and_known_bytes",
        true,
    ) {
        return;
    }
    let before = storage::snapshot();
    let body = poll_ready(observe_first_future(
        Operation::TidbSqlBlockRead,
        ready(Ok::<Option<Vec<u8>>, &'static str>(Some(vec![1, 2, 3]))),
        |row: &Option<Vec<u8>>| row.as_ref().map_or(0, |bytes| bytes.len() as u64),
    ));
    assert_eq!(body.unwrap(), Some(vec![1, 2, 3]));
    let absent = poll_ready(observe_first_future(
        Operation::TidbSqlBlockRead,
        ready(Ok::<Option<Vec<u8>>, &'static str>(None)),
        |row: &Option<Vec<u8>>| row.as_ref().map_or(0, |bytes| bytes.len() as u64),
    ));
    assert_eq!(absent.unwrap(), None);
    let rows = poll_ready(observe_rows_future(
        Operation::TidbSqlMetadataRead,
        ready(Ok::<Vec<u8>, &'static str>(vec![7, 8, 9])),
    ));
    assert_eq!(rows.unwrap(), vec![7, 8, 9]);
    let delta = storage::snapshot().delta(&before).unwrap();
    let block = delta
        .entries
        .iter()
        .find(|entry| entry.name == "tidb.sql.block_read")
        .unwrap();
    assert_eq!((block.calls, block.success, block.returned_rows), (2, 2, 1));
    assert_eq!(block.returned_row_observations, 2);
    assert_eq!(block.bytes, 3);
    let metadata = delta
        .entries
        .iter()
        .find(|entry| entry.name == "tidb.sql.metadata_read")
        .unwrap();
    assert_eq!((metadata.calls, metadata.returned_rows), (1, 3));
    assert_eq!(metadata.returned_row_observations, 1);
    assert_eq!(metadata.bytes, 0);
}

#[test]
fn sql_error_preserves_original_error_without_row_observation() {
    if !isolated(
        "sql_error_preserves_original_error_without_row_observation",
        true,
    ) {
        return;
    }
    let before = storage::snapshot();
    let result = poll_ready(observe_first_future(
        Operation::TidbSqlBlockRead,
        ready(Err::<Option<Vec<u8>>, &'static str>("original error")),
        |_: &Option<Vec<u8>>| 0,
    ));
    assert_eq!(result, Err("original error"));
    let delta = storage::snapshot().delta(&before).unwrap();
    let block = delta
        .entries
        .iter()
        .find(|entry| entry.name == "tidb.sql.block_read")
        .unwrap();
    assert_eq!(
        (block.calls, block.error, block.returned_row_observations),
        (1, 1, 0)
    );
    assert_eq!((block.bytes, delta.in_flight), (0, 0));
}

#[test]
fn polled_sql_cancellation_and_unpolled_future_are_distinct() {
    if !isolated(
        "polled_sql_cancellation_and_unpolled_future_are_distinct",
        true,
    ) {
        return;
    }
    let before = storage::snapshot();
    let unpolled =
        observe_result_future(Operation::TidbSqlMetadataWrite, ready(Ok::<(), ()>(())), 0);
    drop(unpolled);
    let mut pending = Box::pin(observe_first_future(
        Operation::TidbSqlBlockRead,
        pending::<Result<Option<Vec<u8>>, ()>>(),
        |_: &Option<Vec<u8>>| 0,
    ));
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(pending.as_mut().poll(&mut context), Poll::Pending));
    drop(pending);
    let delta = storage::snapshot().delta(&before).unwrap();
    let block = delta
        .entries
        .iter()
        .find(|entry| entry.name == "tidb.sql.block_read")
        .unwrap();
    assert_eq!(
        (
            block.calls,
            block.cancelled,
            block.returned_row_observations
        ),
        (1, 1, 0)
    );
    let write = delta
        .entries
        .iter()
        .find(|entry| entry.name == "tidb.sql.metadata_write")
        .unwrap();
    assert_eq!(write.calls, 0);
    assert_eq!(delta.in_flight, 0);
}

#[test]
fn disabled_helpers_forward_without_recording() {
    if !isolated("disabled_helpers_forward_without_recording", false) {
        return;
    }
    let before = storage::snapshot();
    let body = poll_ready(observe_first_future(
        Operation::TidbSqlBlockRead,
        ready(Ok::<Option<Vec<u8>>, &'static str>(Some(vec![1, 2, 3]))),
        |row: &Option<Vec<u8>>| row.as_ref().map_or(0, |bytes| bytes.len() as u64),
    ));
    assert_eq!(body, Ok(Some(vec![1, 2, 3])));
    let rows = poll_ready(observe_rows_future(
        Operation::TidbSqlMetadataRead,
        ready(Ok::<Vec<u8>, &'static str>(vec![4, 5])),
    ));
    assert_eq!(rows, Ok(vec![4, 5]));
    let error = poll_ready(observe_result_future(
        Operation::TidbSqlMetadataWrite,
        ready(Err::<(), &'static str>("original error")),
        0,
    ));
    assert_eq!(error, Err("original error"));

    let polled = Cell::new(0);
    let mut pending = Box::pin(observe_result_future(
        Operation::TidbSqlMetadataWrite,
        poll_fn(|_| {
            polled.set(polled.get() + 1);
            Poll::<Result<(), &'static str>>::Pending
        }),
        0,
    ));
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(pending.as_mut().poll(&mut context), Poll::Pending));
    drop(pending);
    assert_eq!(polled.get(), 1);

    let unpolled = Cell::new(0);
    let future = observe_checkout_future(poll_fn(|_| {
        unpolled.set(unpolled.get() + 1);
        Poll::<Result<(), &'static str>>::Ready(Ok(()))
    }));
    drop(future);
    assert_eq!(unpolled.get(), 0);

    let delta = storage::snapshot().delta(&before).unwrap();
    assert_eq!(delta.in_flight, 0);
    assert_eq!(delta.forwarding_boxes.calls, 0);
    assert_eq!(delta.forwarding_boxes.requested_object_bytes, 0);
    for entry in &delta.entries {
        assert_eq!(
            (
                entry.in_flight,
                entry.returned_rows,
                entry.returned_row_observations,
                entry.calls,
                entry.success,
                entry.error,
                entry.cancelled,
                entry.bytes,
                entry.elapsed_ns,
            ),
            (0, 0, 0, 0, 0, 0, 0, 0, 0),
            "{}",
            entry.name,
        );
        assert!(
            entry.latency_log2_us.iter().all(|&count| count == 0),
            "{}",
            entry.name
        );
    }
}
