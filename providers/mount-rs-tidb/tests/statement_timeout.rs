//! What TiDB's own statement limit covers, checked against a real server.
//!
//! The provider sets `max_execution_time` on every session and also enforces
//! a client deadline. This test records why both are needed: TiDB interrupts
//! a read-only statement itself, and does not interrupt a write.

use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Error as MysqlError, Pool};
use std::time::{Duration, Instant};

fn tidb_url() -> String {
    std::env::var("MOUNT_RS_TIDB_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| {
            panic!("MOUNT_RS_TIDB_URL must be set when explicitly running the ignored TiDB test")
        })
}

fn server_code(error: &MysqlError) -> Option<u16> {
    match error {
        MysqlError::Server(server) => Some(server.code),
        _ => None,
    }
}

async fn counter(connection: &mut Conn, table: &str) -> i64 {
    connection
        .query_first(format!("SELECT v FROM {table} WHERE id=1"))
        .await
        .expect("read the probe row")
        .expect("the probe row exists")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_max_execution_time_interrupts_reads_but_not_writes() {
    let pool = Pool::from_url(tidb_url()).expect("the TiDB URL must be parseable");
    let mut connection = pool.get_conn().await.expect("connect to TiDB");
    let version: String = connection
        .query_first("SELECT VERSION()")
        .await
        .expect("read the server version")
        .expect("the server reports a version");
    assert!(
        version.to_ascii_lowercase().contains("tidb"),
        "this test is only meaningful against TiDB, not {version:?}"
    );
    let table = format!("mount_rs_tidb_timeout_probe_{}", std::process::id());
    connection
        .query_drop(format!(
            "CREATE TABLE {table} (id INT PRIMARY KEY, v INT NOT NULL)"
        ))
        .await
        .expect("create the probe table");
    connection
        .query_drop(format!("INSERT INTO {table} VALUES (1, 0)"))
        .await
        .expect("insert the probe row");

    // Each statement below runs for three seconds unless TiDB stops it.
    const LIMIT: Duration = Duration::from_millis(500);
    const WORK: Duration = Duration::from_secs(3);
    connection
        .query_drop(format!(
            "SET SESSION max_execution_time={}",
            LIMIT.as_millis()
        ))
        .await
        .expect("set the session statement limit");

    // A read-only statement is interrupted by the server and the connection
    // stays usable. TiDB ends an interrupted SLEEP early and reports it
    // either as the MySQL-compatible result 1 or as a server error.
    let started = Instant::now();
    let read: Result<Option<i64>, _> = connection
        .query_first(format!("SELECT SLEEP({})", WORK.as_secs()))
        .await;
    let read_elapsed = started.elapsed();
    println!(
        "TIDB_STATEMENT_LIMIT version={version} read_only=interrupted result={read:?} elapsed_ms={}",
        read_elapsed.as_millis()
    );
    match &read {
        Ok(interrupted) => assert_eq!(*interrupted, Some(1), "SLEEP was not interrupted"),
        Err(error) => assert!(
            matches!(server_code(error), Some(1317 | 3024)),
            "unexpected interruption error: {error}"
        ),
    }
    assert!(
        read_elapsed < WORK,
        "the read ran to completion: {read_elapsed:?}"
    );
    assert_eq!(counter(&mut connection, &table).await, 0);

    // An autocommit write is not interrupted: it runs past the limit and
    // commits. Only the client deadline can bound it.
    let started = Instant::now();
    connection
        .query_drop(format!(
            "UPDATE {table} SET v=v+1 WHERE id=1 AND SLEEP({})=0",
            WORK.as_secs()
        ))
        .await
        .expect("TiDB does not apply max_execution_time to an UPDATE");
    let write_elapsed = started.elapsed();
    println!(
        "TIDB_STATEMENT_LIMIT version={version} update=not_interrupted elapsed_ms={}",
        write_elapsed.as_millis()
    );
    assert!(
        write_elapsed >= WORK,
        "the write was cut short: {write_elapsed:?}"
    );
    assert_eq!(counter(&mut connection, &table).await, 1);

    // The locking read the provider uses inside its transactions is still a
    // SELECT. Reported as an observation only: either answer is covered by
    // the client deadline.
    connection
        .query_drop("BEGIN PESSIMISTIC")
        .await
        .expect("begin");
    let started = Instant::now();
    let locking: Result<Option<i64>, _> = connection
        .query_first(format!(
            "SELECT v FROM {table} WHERE id=1 AND SLEEP({})=0 FOR UPDATE",
            WORK.as_secs()
        ))
        .await;
    let locking_elapsed = started.elapsed();
    println!(
        "TIDB_STATEMENT_LIMIT version={version} select_for_update={} result={locking:?} elapsed_ms={}",
        if locking_elapsed < WORK {
            "interrupted"
        } else {
            "not_interrupted"
        },
        locking_elapsed.as_millis()
    );
    connection.query_drop("ROLLBACK").await.expect("rollback");

    connection
        .query_drop(format!("DROP TABLE {table}"))
        .await
        .expect("drop the probe table");
    drop(connection);
    pool.disconnect().await.expect("close the probe pool");
}
