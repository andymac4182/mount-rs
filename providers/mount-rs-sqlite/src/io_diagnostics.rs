//! Optional SQLite engine counters. These are pager operations, not device IOPS.
use mount_rs_core::turn_cell::TurnCell;
use mount_rs_core::{Result, backend_error};
use rusqlite::{Connection, ffi};
use std::{
    collections::BTreeMap,
    ffi::{CStr, c_void},
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicU16, AtomicU64, Ordering},
    },
};

/// The statement categories, in the order they are reported.
const CATEGORIES: [&str; 9] = [
    "BEGIN", "COMMIT", "DELETE", "INSERT", "OTHER", "PRAGMA", "ROLLBACK", "SELECT", "UPDATE",
];
const OTHER: usize = 4;
const PRAGMA: usize = 5;

// Statements run, and samples are taken, with the connection held, so a trace
// and a sample of one connection never overlap: these need no lock of their own.
#[derive(Default)]
pub(crate) struct Counts {
    sql: [AtomicU64; CATEGORIES.len()],
    /// One bit per category traced since the last reset: a category is
    /// reported once seen, even at a count of zero.
    seen: AtomicU16,
}
struct Entry {
    id: u64,
    connection: Weak<TurnCell<Connection>>,
    counts: Weak<Counts>,
}
static REGISTRY: OnceLock<TurnCell<Vec<Entry>>> = OnceLock::new();
static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);
unsafe extern "C" fn trace(
    mask: u32,
    context: *mut c_void,
    statement: *mut c_void,
    _: *mut c_void,
) -> i32 {
    if mask != ffi::SQLITE_TRACE_STMT {
        return 0;
    }
    // Counts is held by every Database clone until after its connection drops.
    let counts = unsafe { &*context.cast::<Counts>() };
    let sql = unsafe { ffi::sqlite3_sql(statement.cast()) };
    let category = if sql.is_null() {
        OTHER
    } else {
        let text = unsafe { CStr::from_ptr(sql) }.to_bytes();
        let token = text
            .split(|byte| byte.is_ascii_whitespace())
            .next()
            .unwrap_or_default();
        CATEGORIES
            .iter()
            .position(|category| category.as_bytes() == token)
            .unwrap_or(OTHER)
    };
    // No SQL text, bound values, namespaces, paths, or credentials are retained.
    counts.sql[category].fetch_add(1, Ordering::Relaxed);
    counts.seen.fetch_or(1 << category, Ordering::Relaxed);
    0
}
pub(crate) fn register(connection: &Arc<TurnCell<Connection>>) -> Result<Option<Arc<Counts>>> {
    if std::env::var("MOUNT_RS_PROFILE_IO").as_deref() != Ok("1") {
        return Ok(None);
    }
    let counts = Arc::new(Counts::default());
    let mut registry = REGISTRY
        .get_or_init(TurnCell::default)
        .lock()
        .map_err(|()| backend_error("SQLite diagnostic registry poisoned"))?;
    // Registration must stay bounded even when diagnostics are never sampled.
    registry
        .retain(|entry| entry.connection.strong_count() != 0 && entry.counts.strong_count() != 0);
    {
        let connection = connection
            .lock()
            .map_err(|()| backend_error("SQLite storage lock poisoned"))?;
        let rc = unsafe {
            ffi::sqlite3_trace_v2(
                connection.handle(),
                ffi::SQLITE_TRACE_STMT,
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

/// Sample/reset SQLite connection pager counters. Byte estimates exclude journal
/// headers, WAL/checkpoint traffic, syncs, filesystem/cache and physical storage.
/// Must run at a quiescent phase boundary for precise attribution.
pub fn connection_page_diagnostics(
    connection: &Connection,
    reset: bool,
) -> Result<serde_json::Value> {
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
    let page_size: u64 = connection
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .map_err(backend_error)?;
    Ok(serde_json::json!({"pager":pager,"page_size":page_size,
        "pager_read_bytes_estimate":pager["cache_misses"]*page_size,"pager_write_bytes_estimate":pager["page_writes"]*page_size}))
}

/// Sample/reset all live provider connections opened with MOUNT_RS_PROFILE_IO=1.
/// SQL counts contain statement categories only; they never contain SQL values.
/// Reset is a phase boundary operation and requires callers to drain workloads.
/// Match connection_id across snapshots; aggregate totals can decrease when
/// connections close. Missing end IDs cannot provide complete stage attribution.
pub fn sqlite_io_diagnostics(reset: bool) -> serde_json::Value {
    let Some(registry) = REGISTRY.get() else {
        return serde_json::json!({"connections":[],"sql_statements":0});
    };
    let Ok(mut entries) = registry.lock() else {
        return serde_json::json!({"error":"SQLite diagnostic registry poisoned"});
    };
    let mut samples = Vec::new();
    entries.retain(|entry| {
        let (Some(connection), Some(counts)) = (entry.connection.upgrade(), entry.counts.upgrade())
        else {
            return false;
        };
        let Ok(connection) = connection.lock() else {
            samples.push(serde_json::json!({"error":"SQLite connection poisoned"}));
            return true;
        };
        let sample = connection_page_diagnostics(&connection, reset);
        let seen = counts.seen.load(Ordering::Relaxed);
        // Exclude the observer's own PRAGMA page_size statement.
        if sample.is_ok() && seen & (1 << PRAGMA) != 0 {
            let count = counts.sql[PRAGMA].load(Ordering::Relaxed);
            counts.sql[PRAGMA].store(count.saturating_sub(1), Ordering::Relaxed);
        }
        let sql: BTreeMap<&'static str, u64> = CATEGORIES
            .iter()
            .zip(&counts.sql)
            .enumerate()
            .filter(|(category, _)| seen & (1 << category) != 0)
            .map(|(_, (name, count))| (*name, count.load(Ordering::Relaxed)))
            .collect();
        let mut value = match sample {
            Ok(value) => value,
            Err(error) => serde_json::json!({"error":error.to_string()}),
        };
        value["sql_statements"] = serde_json::json!(sql.values().sum::<u64>());
        value["connection_id"] = serde_json::json!(entry.id);
        value["sql_categories"] = serde_json::json!(sql);
        if reset {
            for count in &counts.sql {
                count.store(0, Ordering::Relaxed);
            }
            counts.seen.store(0, Ordering::Relaxed);
        }
        samples.push(value);
        true
    });
    let total: u64 = samples
        .iter()
        .filter_map(|v| v["sql_statements"].as_u64())
        .sum();
    serde_json::json!({"connections":samples,"sql_statements":total})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteBlockStore;

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
