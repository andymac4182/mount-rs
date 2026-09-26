//! Process-wide SQLite heap observations, separate from the Rust allocator.

pub(super) fn heap() -> Result<(u64, u64), &'static str> {
    let mut current = 0;
    let mut peak = 0;
    // SQLite owns the counters and synchronizes access; reset=0 leaves its
    // lifetime high-water mark unchanged. Both output pointers are valid.
    let status = unsafe {
        rusqlite::ffi::sqlite3_status64(
            rusqlite::ffi::SQLITE_STATUS_MEMORY_USED,
            &mut current,
            &mut peak,
            0,
        )
    };
    if status != rusqlite::ffi::SQLITE_OK {
        return Err("SQLite memory counters unavailable");
    }
    Ok((
        u64::try_from(current).map_err(|_| "negative SQLite heap usage")?,
        u64::try_from(peak).map_err(|_| "negative SQLite heap peak")?,
    ))
}
