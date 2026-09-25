//! Bounded guard INSERTs within the caller's existing authority transaction.
use super::*;
use mysql_async::Value;

const MAX_ROWS: usize = 64;
const MAX_BYTES: usize = 256 * 1024;
const SINGLE: &str = "INSERT INTO mount_rs_tidb_inodes (volume_key,inode,generation,revision,node) VALUES (?,?,?,0,?)";
#[path = "inode_batch_size.rs"]
mod size;
use size::{PREFIX, ROW, encoded_bound};

/// The provider owns no-reset sessions. TiDB's SESSION cap is read-only and
/// mysql_async negotiates it on each fresh connection. An explicit client cap
/// can be lower, so neither optional opts nor a default alone is sufficient.
pub(super) async fn budget(tx: &mut Transaction<'_>) -> Result<usize> {
    let configured = tx.opts().max_allowed_packet().unwrap_or(usize::MAX);
    let session: Option<u64> = tx
        .query_first("SELECT @@SESSION.max_allowed_packet")
        .await
        .map_err(|e| db_error("read TiDB guard packet budget", e))?;
    Ok(session
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(0)
        .min(configured)
        .min(MAX_BYTES))
}

async fn flush(
    tx: &mut Transaction<'_>,
    volume: &str,
    generation: i64,
    rows: &mut Vec<(i64, String)>,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    #[cfg(test)]
    tests::before_batch(rows).await;
    let mut sql = String::with_capacity(PREFIX.len() + rows.len() * (ROW.len() + 1));
    sql.push_str(PREFIX);
    let mut params = Vec::with_capacity(rows.len() * 4);
    for (index, (inode, node)) in rows.drain(..).enumerate() {
        if index != 0 {
            sql.push(',');
        }
        sql.push_str(ROW);
        params.extend([
            Value::Bytes(volume.as_bytes().to_vec()),
            Value::Int(inode),
            Value::Int(generation),
            Value::Bytes(node.into_bytes()),
        ]);
    }
    tx.exec_drop(sql, Params::Positional(params))
        .await
        .map_err(|e| db_error("insert TiDB inode guards", e))
}

pub(super) async fn insert(
    tx: &mut Transaction<'_>,
    volume: &str,
    generation: u64,
    namespace: &Namespace,
    budget: usize,
) -> Result<()> {
    let generation = signed(generation, "inode generation")?;
    let mut rows = Vec::with_capacity(MAX_ROWS);
    let mut bytes = 0_usize;
    for (&inode, node) in &namespace.nodes {
        let inode = signed(inode, "inode ID")?;
        let json = serde_json::to_string(node).map_err(backend_error)?;
        profile::add(Event::InodeSerialized, json.len() as u64);
        let candidate = bytes.checked_add(json.len());
        if rows.len() == MAX_ROWS
            || candidate
                .and_then(|n| encoded_bound(rows.len() + 1, volume.len(), n))
                .is_none_or(|n| n > budget)
        {
            flush(tx, volume, generation, &mut rows).await?;
            bytes = 0;
        }
        if encoded_bound(1, volume.len(), json.len()).is_none_or(|n| n > budget) {
            // Preserve the original accepted single-row path. Decide before
            // sending; never replay a failed bulk statement at a smaller size.
            #[cfg(test)]
            let inode = tests::before_single(inode).await;
            tx.exec_drop(SINGLE, (volume, inode, generation, json))
                .await
                .map_err(|e| db_error("insert TiDB inode guard", e))?;
        } else {
            bytes += json.len();
            rows.push((inode, json));
        }
    }
    flush(tx, volume, generation, &mut rows).await
}

#[cfg(test)]
#[path = "inode_batch_tests.rs"]
mod tests;
