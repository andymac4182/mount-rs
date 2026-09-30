//! Run alone: another test's SQLite allocations can change process-wide gauges.
#[path = "support/resource_profile/sqlite_heap.rs"]
mod sqlite_memory;

#[test]
fn sqlite_foreign_heap_is_observed_separately_from_rust_allocations() {
    let before = sqlite_memory::heap().unwrap().0;
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE heap_probe (payload BLOB); INSERT INTO heap_probe VALUES (zeroblob(4096));",
        )
        .unwrap();
    let during = sqlite_memory::heap().unwrap();
    assert!(
        during.0 >= before + 4096,
        "SQLite heap growth: before={before}, during={during:?}"
    );
    assert!(
        during.1 >= during.0,
        "SQLite heap high-water: during={during:?}"
    );
    drop(connection);
    let after = sqlite_memory::heap().unwrap().0;
    assert!(
        after < during.0,
        "SQLite heap release: before={before}, during={during:?}, after={after}"
    );
}
