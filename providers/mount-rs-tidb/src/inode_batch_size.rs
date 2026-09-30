pub(super) const PREFIX: &str =
    "INSERT INTO mount_rs_tidb_inodes (volume_key,inode,generation,revision,node) VALUES ";
pub(super) const ROW: &str = "(?,?,?,0,?)";

/// Sum of complete PREPARE and EXECUTE packets, conservatively counting every
/// string's length prefix as nine bytes. This also bounds each packet alone.
pub(super) fn encoded_bound(rows: usize, volume_bytes: usize, node_bytes: usize) -> Option<usize> {
    let params = rows.checked_mul(4)?;
    let sql = PREFIX.len().checked_add(rows.checked_mul(ROW.len() + 1)?)?;
    let values = rows
        .checked_mul(volume_bytes.checked_add(16 + 18)?)?
        .checked_add(node_bytes)?;
    // PREPARE: header+command. EXECUTE: header+10 fixed bytes, null bitmap,
    // new-params flag, two bytes/type, then binary values.
    sql.checked_add(5 + 4 + 10 + 1)?
        .checked_add(params.div_ceil(8))?
        .checked_add(params.checked_mul(2)?)?
        .checked_add(values)
}
