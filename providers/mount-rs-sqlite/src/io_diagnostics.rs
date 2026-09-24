//! Optional SQLite engine counters. These are pager operations, not device IOPS.
use mount_rs_core::{Result, backend_error};
use rusqlite::{Connection, ffi};
use std::{
    collections::BTreeMap,
    ffi::{CStr, c_void},
    sync::{Arc, Mutex, OnceLock, Weak},
};

#[derive(Default)]
pub(crate) struct Counts {
    sql: Mutex<BTreeMap<&'static str, u64>>,
}
struct Entry {
    connection: Weak<Mutex<Connection>>,
    counts: Weak<Counts>,
}
static REGISTRY: OnceLock<Mutex<Vec<Entry>>> = OnceLock::new();
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
        "OTHER"
    } else {
        let text = unsafe { CStr::from_ptr(sql) }.to_bytes();
        let token = text
            .split(|byte| byte.is_ascii_whitespace())
            .next()
            .unwrap_or_default();
        match token {
            b"SELECT" => "SELECT",
            b"INSERT" => "INSERT",
            b"UPDATE" => "UPDATE",
            b"DELETE" => "DELETE",
            b"BEGIN" => "BEGIN",
            b"COMMIT" => "COMMIT",
            b"ROLLBACK" => "ROLLBACK",
            b"PRAGMA" => "PRAGMA",
            _ => "OTHER",
        }
    };
    // No SQL text, bound values, namespaces, paths, or credentials are retained.
    if let Ok(mut map) = counts.sql.lock() {
        *map.entry(category).or_default() += 1;
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
    // Registration must stay bounded even when diagnostics are never sampled.
    registry
        .retain(|entry| entry.connection.strong_count() != 0 && entry.counts.strong_count() != 0);
    {
        let connection = connection.lock().map_err(backend_error)?;
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
        let Ok(mut sql) = counts.sql.lock() else {
            samples.push(serde_json::json!({"error":"SQLite trace counters poisoned"}));
            return true;
        };
        // Exclude the observer's own PRAGMA page_size statement.
        if sample.is_ok()
            && let Some(count) = sql.get_mut("PRAGMA")
        {
            *count = count.saturating_sub(1);
        }
        let mut value = match sample {
            Ok(value) => value,
            Err(error) => serde_json::json!({"error":error.to_string()}),
        };
        value["sql_statements"] = serde_json::json!(sql.values().sum::<u64>());
        value["sql_categories"] = serde_json::json!(*sql);
        if reset {
            sql.clear();
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
        assert_eq!(sample["connections"].as_array().unwrap().len(), 1);
        assert_eq!(sample["sql_statements"], 5);
        assert_eq!(sample["connections"][0]["sql_categories"]["SELECT"], 2);
        assert!(!sample.to_string().contains("diagnostic-sensitive-payload"));
        assert_eq!(sqlite_io_diagnostics(false)["sql_statements"], 0);
        drop(store);
        assert!(
            sqlite_io_diagnostics(false)["connections"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let reopened = SqliteBlockStore::in_memory().unwrap();
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
