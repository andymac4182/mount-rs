//! Opt-in SQLite diagnostics: pager work, statement execution and lock waits.
//! SQLite PROFILE is approximate wall time, not CPU time or a success/fsync oracle.
use mount_rs_core::{Result, backend_error};
use rusqlite::{Connection, ffi};
use std::{
    collections::BTreeMap,
    ffi::{CStr, c_void},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

const CATEGORIES: [&str; 9] = [
    "SELECT", "INSERT", "UPDATE", "DELETE", "BEGIN", "COMMIT", "ROLLBACK", "PRAGMA", "OTHER",
];
fn checked_add(counter: &AtomicU64, overflow: &AtomicBool, amount: u64) {
    if counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(amount)
        })
        .is_err()
    {
        overflow.store(true, Ordering::Relaxed);
    }
}
#[derive(Default)]
struct Timing {
    overflow: AtomicBool,
    completed: AtomicU64,
    elapsed_ns: AtomicU64,
    max_elapsed_ns: AtomicU64,
    invalid_elapsed: AtomicU64,
    histogram: [AtomicU64; 32],
}
impl Timing {
    fn record(&self, elapsed: Option<u64>) {
        checked_add(&self.completed, &self.overflow, 1);
        let Some(ns) = elapsed else {
            checked_add(&self.invalid_elapsed, &self.overflow, 1);
            return;
        };
        checked_add(&self.elapsed_ns, &self.overflow, ns);
        self.max_elapsed_ns.fetch_max(ns, Ordering::Relaxed);
        let us = ns / 1000;
        let bucket = (63 - us.max(1).leading_zeros()).min(31) as usize;
        checked_add(&self.histogram[bucket], &self.overflow, 1);
    }
    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "completed":self.completed.load(Ordering::Relaxed),
            "overflow":self.overflow.load(Ordering::Relaxed),
            "elapsed_ns":self.elapsed_ns.load(Ordering::Relaxed),
            "max_elapsed_ns":self.max_elapsed_ns.load(Ordering::Relaxed),
            "invalid_elapsed":self.invalid_elapsed.load(Ordering::Relaxed),
            "histogram_log2_us":self.histogram.iter().map(|v|v.load(Ordering::Relaxed)).collect::<Vec<_>>()
        })
    }
    fn reset(&self) {
        self.overflow.store(false, Ordering::Relaxed);
        for value in [
            &self.completed,
            &self.elapsed_ns,
            &self.max_elapsed_ns,
            &self.invalid_elapsed,
        ] {
            value.store(0, Ordering::Relaxed);
        }
        for value in &self.histogram {
            value.store(0, Ordering::Relaxed);
        }
    }
}
#[derive(Default)]
pub(crate) struct Counts {
    overflow: AtomicBool,
    statements: [AtomicU64; 9],
    profiles: [Timing; 9],
    lock_wait: Timing,
    lock_errors: AtomicU64,
    provider_commit: Timing,
    provider_commit_errors: AtomicU64,
    observer_suppressed: AtomicBool,
}
impl Counts {
    pub(crate) fn lock_finished(&self, elapsed_ns: u64, success: bool) {
        self.lock_wait.record(Some(elapsed_ns));
        if !success {
            checked_add(&self.lock_errors, &self.overflow, 1);
        }
    }
    pub(crate) fn commit_finished(&self, elapsed_ns: u64, success: bool) {
        self.provider_commit.record(Some(elapsed_ns));
        if !success {
            checked_add(&self.provider_commit_errors, &self.overflow, 1);
        }
    }
    fn suppress_observer(&self) -> ObserverGuard<'_> {
        ObserverGuard {
            counts: self,
            previous: self.observer_suppressed.swap(true, Ordering::Relaxed),
        }
    }
    fn reset(&self) {
        self.overflow.store(false, Ordering::Relaxed);
        for value in &self.statements {
            value.store(0, Ordering::Relaxed);
        }
        for value in &self.profiles {
            value.reset();
        }
        self.lock_wait.reset();
        self.lock_errors.store(0, Ordering::Relaxed);
        self.provider_commit.reset();
        self.provider_commit_errors.store(0, Ordering::Relaxed);
    }
}
struct ObserverGuard<'a> {
    counts: &'a Counts,
    previous: bool,
}
impl Drop for ObserverGuard<'_> {
    fn drop(&mut self) {
        self.counts
            .observer_suppressed
            .store(self.previous, Ordering::Relaxed);
    }
}
struct Entry {
    id: u64,
    connection: Weak<Mutex<Connection>>,
    counts: Weak<Counts>,
}
static REGISTRY: OnceLock<Mutex<Vec<Entry>>> = OnceLock::new();
static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);
unsafe extern "C" fn trace(
    mask: u32,
    context: *mut c_void,
    statement: *mut c_void,
    extra: *mut c_void,
) -> i32 {
    if context.is_null()
        || statement.is_null()
        || !matches!(mask, ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_PROFILE)
    {
        return 0;
    }
    // Every Database clone holds Counts through connection closure. Callbacks
    // run with that connection locked and never acquire a Rust lock or allocate.
    let counts = unsafe { &*context.cast::<Counts>() };
    if counts.observer_suppressed.load(Ordering::Relaxed) {
        return 0;
    }
    let sql = unsafe { ffi::sqlite3_sql(statement.cast()) };
    let category = if sql.is_null() {
        8
    } else {
        let text = unsafe { CStr::from_ptr(sql) }.to_bytes();
        let token = text
            .split(|byte| byte.is_ascii_whitespace())
            .next()
            .unwrap_or_default();
        match token {
            b"SELECT" => 0,
            b"INSERT" => 1,
            b"UPDATE" => 2,
            b"DELETE" => 3,
            b"BEGIN" => 4,
            b"COMMIT" => 5,
            b"ROLLBACK" => 6,
            b"PRAGMA" => 7,
            _ => 8,
        }
    };
    if mask == ffi::SQLITE_TRACE_STMT {
        checked_add(&counts.statements[category], &counts.overflow, 1);
    } else {
        // PROFILE X points to a signed sqlite3_int64, not SQL or an encoded
        // integer. Copy synchronously; a backwards VFS wall clock can be negative.
        let elapsed = if extra.is_null() {
            None
        } else {
            u64::try_from(unsafe { extra.cast::<ffi::sqlite3_int64>().read_unaligned() }).ok()
        };
        counts.profiles[category].record(elapsed);
    }
    0
}
pub(crate) fn register(connection: &Arc<Mutex<Connection>>) -> Result<Option<Arc<Counts>>> {
    if std::env::var("MOUNT_RS_PROFILE_IO").as_deref() != Ok("1") {
        return Ok(None);
    }
    let counts = Arc::new(Counts::default());
    let mut registry = REGISTRY
        .get_or_init(Mutex::default)
        .lock()
        .map_err(backend_error)?;
    registry
        .retain(|entry| entry.connection.strong_count() != 0 && entry.counts.strong_count() != 0);
    {
        let connection = connection.lock().map_err(backend_error)?;
        let rc = unsafe {
            ffi::sqlite3_trace_v2(
                connection.handle(),
                ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_PROFILE,
                Some(trace),
                Arc::as_ptr(&counts).cast_mut().cast(),
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(backend_error(format!(
                "SQLite trace registration failed: {rc}"
            )));
        }
    }
    registry.push(Entry {
        id: NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed),
        connection: Arc::downgrade(connection),
        counts: Arc::downgrade(&counts),
    });
    Ok(Some(counts))
}
fn pager_counters(connection: &Connection, reset: bool) -> Result<BTreeMap<&'static str, u64>> {
    let mut pager = BTreeMap::new();
    for (label, operation) in [
        ("cache_hits", ffi::SQLITE_DBSTATUS_CACHE_HIT),
        ("cache_misses", ffi::SQLITE_DBSTATUS_CACHE_MISS),
        ("page_writes", ffi::SQLITE_DBSTATUS_CACHE_WRITE),
        ("cache_spills", ffi::SQLITE_DBSTATUS_CACHE_SPILL),
    ] {
        let (mut current, mut high) = (0, 0);
        let rc = unsafe {
            ffi::sqlite3_db_status(
                connection.handle(),
                operation,
                &mut current,
                &mut high,
                i32::from(reset),
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(backend_error(format!("SQLite page counter failed: {rc}")));
        }
        pager.insert(label, u64::try_from(current).map_err(backend_error)?);
    }
    Ok(pager)
}
/// Sample/reset pager work without adding configuration queries to catalog
/// hot paths. Byte estimates exclude journals, syncs and physical storage.
pub fn connection_page_diagnostics(
    connection: &Connection,
    reset: bool,
) -> Result<serde_json::Value> {
    let pager = pager_counters(connection, false)?;
    let sample = (|| {
        let page_size: u64 = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .map_err(backend_error)?;
        Ok(serde_json::json!({"pager":pager,"page_size":page_size,
            "pager_read_bytes_estimate":pager["cache_misses"]*page_size,
            "pager_write_bytes_estimate":pager["page_writes"]*page_size}))
    })();
    if reset {
        pager_counters(connection, true)?;
    }
    sample
}
fn configuration(connection: &Connection) -> Result<serde_json::Value> {
    let journal_mode: String = connection
        .query_row("PRAGMA main.journal_mode", [], |row| row.get(0))
        .map_err(backend_error)?;
    let locking_mode: String = connection
        .query_row("PRAGMA main.locking_mode", [], |row| row.get(0))
        .map_err(backend_error)?;
    if !matches!(
        journal_mode.as_str(),
        "delete" | "truncate" | "persist" | "memory" | "wal" | "off"
    ) || !matches!(locking_mode.as_str(), "normal" | "exclusive")
    {
        return Err(backend_error(
            "SQLite diagnostic configuration enum unavailable",
        ));
    }
    let mut result = serde_json::json!({"journal_mode":journal_mode,"locking_mode":locking_mode,"is_autocommit":connection.is_autocommit()});
    for (name, sql) in [
        ("synchronous", "PRAGMA main.synchronous"),
        ("busy_timeout_ms", "PRAGMA busy_timeout"),
        ("fullfsync", "PRAGMA fullfsync"),
        ("checkpoint_fullfsync", "PRAGMA checkpoint_fullfsync"),
        ("wal_autocheckpoint_pages", "PRAGMA wal_autocheckpoint"),
        ("cache_size", "PRAGMA main.cache_size"),
    ] {
        let value: i64 = connection
            .query_row(sql, [], |row| row.get(0))
            .map_err(backend_error)?;
        result[name] = serde_json::json!(value);
    }
    Ok(result)
}
/// Sample/reset all live opt-in provider connections at a drained phase boundary.
/// No SQL text, values, paths, namespace names or credentials are retained.
/// Match connection_id; missing IDs cannot provide complete stage attribution.
pub fn sqlite_io_diagnostics(reset: bool) -> serde_json::Value {
    let Some(registry) = REGISTRY.get() else {
        return serde_json::json!({"connections":[],"sql_statements":0});
    };
    let Ok(mut entries) = registry.lock() else {
        return serde_json::json!({"error":"SQLite diagnostic registry poisoned"});
    };
    let mut samples = Vec::new();
    entries.retain(|entry| {
        // Upgrade context first; it must outlive the upgraded connection even
        // if another thread drops the last Database during this snapshot.
        let Some(counts) = entry.counts.upgrade() else { return false; };
        let Some(connection) = entry.connection.upgrade() else { return false; };
        let Ok(connection) = connection.lock() else { samples.push(serde_json::json!({"error":"SQLite connection poisoned"}));return true; };
        let _observer = counts.suppress_observer();
        let sample = (|| {
            let mut value = connection_page_diagnostics(&connection, false)?;
            value["configuration"] = configuration(&connection)?;
            Ok::<_, mount_rs_core::FsError>(value)
        })();
        // Reset after ALL observer queries, even when a query failed. Capture
        // workload pager counters before querying configuration, not afterward.
        let reset_result = if reset { pager_counters(&connection, true).map(|_| ()) } else { Ok(()) };
        let mut value = match sample.and_then(|value| reset_result.map(|()| value)) {
            Ok(value)=>value, Err(error)=>serde_json::json!({"error":error.to_string()}),
        };
        value["pager_sampling"] = serde_json::json!("before observer queries; reset after queries; repeated nonreset samples may include prior observer pager work");
        let sql:BTreeMap<_,_> = CATEGORIES.iter().zip(&counts.statements)
            .map(|(name,counter)|(*name,counter.load(Ordering::Relaxed))).filter(|(_,v)|*v!=0).collect();
        let profiles:BTreeMap<_,_> = CATEGORIES.iter().zip(&counts.profiles).map(|(name,timing)|(*name,timing.snapshot())).collect();
        let total = sql.values().try_fold(0u64, |total, value| total.checked_add(*value));
        value["sql_statements"] = serde_json::json!(total);
        value["counter_overflow"] = serde_json::json!(total.is_none() || counts.overflow.load(Ordering::Relaxed));
        value["connection_id"] = serde_json::json!(entry.id);
        value["sql_categories"] = serde_json::json!(sql);
        value["sql_profile"] = serde_json::json!(profiles);
        value["sql_profile_scope"] = serde_json::json!("SQLite PROFILE completion notifications, not successes; approximate VFS wall clock, bundled SQLite 1ms resolution; excludes post-PROFILE WAL callbacks");
        value["connection_lock"] = counts.lock_wait.snapshot();
        value["connection_lock"]["errors"] = serde_json::json!(counts.lock_errors.load(Ordering::Relaxed));
        value["provider_commit"] = counts.provider_commit.snapshot();
        value["provider_commit"]["errors"] = serde_json::json!(counts.provider_commit_errors.load(Ordering::Relaxed));
        value["provider_commit_scope"] = serde_json::json!("Instant wall time for Transaction::commit call including error Drop rollback; block_put and all MRC5 compact transactions only; encloses COMMIT SQL PROFILE interval");
        if reset { counts.reset(); }
        samples.push(value);true
    });
    let total = samples
        .iter()
        .filter_map(|v| v["sql_statements"].as_u64())
        .try_fold(0u64, |sum, value| sum.checked_add(value));
    serde_json::json!({"connections":samples,"sql_statements":total})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteBlockStore;

    #[test]
    fn profile_callback_rejects_invalid_durations_and_never_wraps() {
        let connection = Connection::open_in_memory().unwrap();
        let counts = Counts::default();
        let context = std::ptr::from_ref(&counts).cast_mut().cast();
        let mut statement = std::ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(
                connection.handle(),
                c"SELECT 1".as_ptr(),
                -1,
                &mut statement,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, ffi::SQLITE_OK);
        for elapsed in [-1i64, 0] {
            let mut elapsed = elapsed;
            assert_eq!(
                unsafe {
                    trace(
                        ffi::SQLITE_TRACE_PROFILE,
                        context,
                        statement.cast(),
                        std::ptr::from_mut(&mut elapsed).cast(),
                    )
                },
                0
            );
        }
        assert_eq!(
            unsafe {
                trace(
                    ffi::SQLITE_TRACE_PROFILE,
                    context,
                    statement.cast(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let profile = counts.profiles[0].snapshot();
        assert_eq!(profile["completed"], 3);
        assert_eq!(profile["invalid_elapsed"], 2);
        assert_eq!(profile["elapsed_ns"], 0);
        assert_eq!(profile["histogram_log2_us"][0], 1);
        counts.statements[0].store(u64::MAX, Ordering::Relaxed);
        unsafe {
            trace(
                ffi::SQLITE_TRACE_STMT,
                context,
                statement.cast(),
                std::ptr::null_mut(),
            );
        }
        assert_eq!(counts.statements[0].load(Ordering::Relaxed), u64::MAX);
        assert!(counts.overflow.load(Ordering::Relaxed));
        counts.profiles[0]
            .elapsed_ns
            .store(u64::MAX, Ordering::Relaxed);
        let mut elapsed = 1i64;
        unsafe {
            trace(
                ffi::SQLITE_TRACE_PROFILE,
                context,
                statement.cast(),
                std::ptr::from_mut(&mut elapsed).cast(),
            );
        }
        assert_eq!(
            counts.profiles[0].elapsed_ns.load(Ordering::Relaxed),
            u64::MAX
        );
        assert!(counts.profiles[0].overflow.load(Ordering::Relaxed));
        unsafe {
            trace(
                ffi::SQLITE_TRACE_ROW,
                context,
                statement.cast(),
                std::ptr::null_mut(),
            );
        }
        assert_eq!(counts.profiles[0].completed.load(Ordering::Relaxed), 4);
        assert_eq!(unsafe { ffi::sqlite3_finalize(statement) }, ffi::SQLITE_OK);
    }

    #[test]
    #[ignore = "requires MOUNT_RS_PROFILE_IO=1 and exclusive registry phase ownership"]
    fn commit_profile_includes_failed_attempt_and_fresh_success() {
        assert_eq!(std::env::var("MOUNT_RS_PROFILE_IO").as_deref(), Ok("1"));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private-contention.sqlite");
        let writer = Arc::new(Mutex::new(Connection::open(&path).unwrap()));
        writer.lock().unwrap().execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; CREATE TABLE contents(value BLOB); INSERT INTO contents VALUES(X'11');").unwrap();
        writer
            .lock()
            .unwrap()
            .busy_timeout(std::time::Duration::from_millis(40))
            .unwrap();
        let counts = register(&writer).unwrap().unwrap();
        let reader = Connection::open(&path).unwrap();
        reader
            .execute_batch("BEGIN; SELECT * FROM contents;")
            .unwrap();
        sqlite_io_diagnostics(true);
        {
            let writer = writer.lock().unwrap();
            writer
                .execute_batch("BEGIN IMMEDIATE; INSERT INTO contents VALUES(X'22');")
                .unwrap();
            let error = writer.execute_batch("COMMIT").unwrap_err();
            assert_eq!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy)
            );
            assert!(
                !writer.is_autocommit(),
                "failed COMMIT must remain uncommitted"
            );
            writer.execute_batch("ROLLBACK;").unwrap();
        }
        let failed = sqlite_io_diagnostics(true);
        let commit = &failed["connections"][0]["sql_profile"]["COMMIT"];
        assert_eq!(commit["completed"], 1);
        assert!(
            commit["elapsed_ns"].as_u64().unwrap() >= 20_000_000,
            "real busy COMMIT must have observed elapsed time"
        );
        reader.execute_batch("ROLLBACK;").unwrap();
        {
            let writer = writer.lock().unwrap();
            writer
                .execute_batch("BEGIN IMMEDIATE; INSERT INTO contents VALUES(X'33');")
                .unwrap();
            // Match the provider's standalone COMMIT text, avoiding the legacy
            // first-token classifier's OTHER label for a leading-space batch.
            writer.execute_batch("COMMIT").unwrap();
        }
        let success = sqlite_io_diagnostics(false);
        assert_eq!(
            success["connections"][0]["sql_profile"]["COMMIT"]["completed"],
            1
        );
        let fresh = Connection::open(&path).unwrap();
        let values: Vec<Vec<u8>> = fresh
            .prepare("SELECT value FROM contents ORDER BY rowid")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(values, [vec![0x11], vec![0x33]]);
        drop(fresh);
        drop(reader);
        writer
            .lock()
            .unwrap()
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .unwrap();
        let wal = sqlite_io_diagnostics(false);
        assert_eq!(
            wal["connections"][0]["configuration"]["journal_mode"],
            "wal"
        );
        assert_eq!(wal["connections"][0]["configuration"]["synchronous"], 2);
        drop(writer);
        drop(counts);
        assert!(
            sqlite_io_diagnostics(false)["connections"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    #[ignore = "requires MOUNT_RS_PROFILE_IO=1 and exclusive registry phase ownership"]
    fn diagnostics_separate_commit_profile_lock_wait_and_observer_queries() {
        assert_eq!(std::env::var("MOUNT_RS_PROFILE_IO").as_deref(), Ok("1"));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private-diagnostic-name.sqlite");
        let store = SqliteBlockStore::open(&path).unwrap();
        sqlite_io_diagnostics(true);
        futures_lite::future::block_on(async {
            use mount_rs_core::storage::BlockStore;
            let bytes = b"private-diagnostic-payload";
            let id = store.put(bytes).await.unwrap();
            assert_eq!(store.get(&id).await.unwrap(), bytes);
        });
        let sample = sqlite_io_diagnostics(false);
        let connection = &sample["connections"][0];
        assert_eq!(
            connection["configuration"]["journal_mode"], "delete",
            "actual SQLite configuration is missing from the diagnostic snapshot"
        );
        assert_eq!(connection["configuration"]["synchronous"], 2);
        assert_eq!(connection["configuration"]["busy_timeout_ms"], 5000);
        assert_eq!(connection["sql_statements"], 5);
        assert_eq!(connection["sql_profile"]["COMMIT"]["completed"], 1);
        assert_eq!(connection["provider_commit"]["completed"], 1);
        assert_eq!(connection["provider_commit"]["errors"], 0);
        assert_eq!(connection["sql_profile"]["SELECT"]["completed"], 2);
        assert_eq!(connection["sql_profile"]["PRAGMA"]["completed"], 0);
        assert_eq!(connection["sql_profile"]["COMMIT"]["invalid_elapsed"], 0);
        assert_eq!(
            connection["sql_profile"]["COMMIT"]["histogram_log2_us"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap())
                .sum::<u64>(),
            1
        );
        assert!(connection["connection_lock"]["completed"].as_u64().unwrap() >= 2);
        // Sampling must not become application SQL/profile/lock work itself.
        let repeated = sqlite_io_diagnostics(false);
        for field in [
            "sql_statements",
            "sql_categories",
            "sql_profile",
            "connection_lock",
            "provider_commit",
        ] {
            assert_eq!(
                connection[field], repeated["connections"][0][field],
                "observer polluted {field}"
            );
        }
        let encoded = sample.to_string();
        assert!(!encoded.contains("private-diagnostic"));
        sqlite_io_diagnostics(true);
        let reset = sqlite_io_diagnostics(false);
        assert_eq!(reset["connections"][0]["sql_statements"], 0);
        assert_eq!(
            reset["connections"][0]["sql_profile"]["COMMIT"]["completed"],
            0
        );
        assert_eq!(reset["connections"][0]["connection_lock"]["completed"], 0);
        drop(store);
        assert!(
            sqlite_io_diagnostics(false)["connections"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    #[ignore = "requires MOUNT_RS_PROFILE_IO=1 and exclusive registry phase ownership"]
    fn diagnostics_reset_drop_and_sql_privacy() {
        assert_eq!(std::env::var("MOUNT_RS_PROFILE_IO").as_deref(), Ok("1"));
        let store = SqliteBlockStore::in_memory().unwrap();
        sqlite_io_diagnostics(true);
        futures_lite::future::block_on(async {
            use mount_rs_core::storage::BlockStore;
            let id = store.put(b"diagnostic-sensitive-payload").await.unwrap();
            assert_eq!(
                store.get(&id).await.unwrap(),
                b"diagnostic-sensitive-payload"
            );
        });
        let sample = sqlite_io_diagnostics(true);
        assert_eq!(
            sample["connections"][0]["configuration"]["journal_mode"],
            "memory"
        );
        let first_id = sample["connections"][0]["connection_id"].as_u64().unwrap();
        assert_eq!(sample["connections"].as_array().unwrap().len(), 1);
        assert_eq!(sample["sql_statements"], 5);
        assert_eq!(sample["connections"][0]["sql_categories"]["SELECT"], 2);
        assert!(!sample.to_string().contains("diagnostic-sensitive-payload"));
        let unchanged = sqlite_io_diagnostics(false);
        assert_eq!(unchanged["sql_statements"], 0);
        assert_eq!(unchanged["connections"][0]["connection_id"], first_id);
        drop(store);
        assert!(
            sqlite_io_diagnostics(false)["connections"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let reopened = SqliteBlockStore::in_memory().unwrap();
        assert!(
            sqlite_io_diagnostics(false)["connections"][0]["connection_id"]
                .as_u64()
                .unwrap()
                > first_id
        );
        assert_eq!(
            sqlite_io_diagnostics(true)["connections"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        drop(reopened);
        assert!(
            sqlite_io_diagnostics(false)["connections"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        // Repeated short-lived connections must not accumulate weak entries
        // between samples; checking the raw registry avoids snapshot pruning.
        for _ in 0..64 {
            let transient = SqliteBlockStore::in_memory().unwrap();
            assert_eq!(REGISTRY.get().unwrap().lock().unwrap().len(), 1);
            drop(transient);
        }
    }
}
