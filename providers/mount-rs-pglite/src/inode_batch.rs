//! Bounded typed guard INSERTs; caller retains its authority transaction.
use super::*;
use std::fmt::Write as _;
use tokio_postgres::types::ToSql;

const MAX_ROWS: usize = 64;
const MAX_BYTES: usize = 256 * 1024;
const PREFIX: &str =
    "INSERT INTO mount_rs_inode_guards(volume_key,inode,generation,revision,node) VALUES";
const SINGLE: &str = "INSERT INTO mount_rs_inode_guards(volume_key,inode,generation,revision,node) VALUES($1,$2,$3,0,$4)";

/// Full frontend Parse+Bind+Describe+Execute+Sync request. Placeholder indexes
/// are at most 256 (four bytes each); using that maximum also bounds short tails.
pub(super) fn encoded_bound(rows: usize, volume_bytes: usize, node_bytes: usize) -> Option<usize> {
    let params = rows.checked_mul(4)?;
    let sql = PREFIX.len().checked_add(rows.checked_mul(24)?)?;
    // 46 fixed framing bytes includes the seven-byte unnamed Describe.
    // Per parameter: four OID, two format, four length-prefix bytes.
    sql.checked_add(46)?
        .checked_add(params.checked_mul(10)?)?
        .checked_add(rows.checked_mul(volume_bytes.checked_add(16)?)?)?
        .checked_add(node_bytes)
}

async fn flush(
    tx: &tokio_postgres::Transaction<'_>,
    volume: &str,
    generation: i64,
    rows: &mut Vec<(i64, String)>,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    #[cfg(test)]
    tests::before_batch(rows).await;
    let mut sql = String::with_capacity(PREFIX.len() + rows.len() * 24);
    sql.push_str(PREFIX);
    let mut params: Vec<(&(dyn ToSql + Sync), Type)> = Vec::with_capacity(rows.len() * 4);
    for (index, (inode, node)) in rows.iter().enumerate() {
        if index != 0 {
            sql.push(',');
        }
        let p = index * 4 + 1;
        write!(sql, "(${p},${},${},0,${})", p + 1, p + 2, p + 3)
            .expect("String formatting is infallible");
        params.extend([
            (&volume as &(dyn ToSql + Sync), Type::TEXT),
            (inode, Type::INT8),
            (&generation, Type::INT8),
            (node, Type::TEXT),
        ]);
    }
    tx.execute_typed(&sql, &params)
        .await
        .map_err(postgres_error)?;
    rows.clear();
    Ok(())
}

pub(super) async fn insert(
    tx: &tokio_postgres::Transaction<'_>,
    volume: &str,
    generation: i64,
    namespace: &Namespace,
) -> Result<()> {
    let mut rows = Vec::with_capacity(MAX_ROWS);
    let mut bytes = 0_usize;
    for (&inode, node) in &namespace.nodes {
        let inode = inode_signed(inode)?;
        let node = encode_inode_node(node)?;
        let candidate = bytes.checked_add(node.len());
        if rows.len() == MAX_ROWS
            || candidate
                .and_then(|n| encoded_bound(rows.len() + 1, volume.len(), n))
                .is_none_or(|n| n > MAX_BYTES)
        {
            flush(tx, volume, generation, &mut rows).await?;
            bytes = 0;
        }
        if encoded_bound(1, volume.len(), node.len()).is_none_or(|n| n > MAX_BYTES) {
            // Preserve oversized single rows before sending, never retry a
            // sent multirow statement or change transaction/COMMIT semantics.
            #[cfg(test)]
            let inode = tests::before_single(inode).await;
            tx.execute_typed(
                SINGLE,
                &[
                    (&volume, Type::TEXT),
                    (&inode, Type::INT8),
                    (&generation, Type::INT8),
                    (&node, Type::TEXT),
                ],
            )
            .await
            .map_err(postgres_error)?;
        } else {
            bytes += node.len();
            rows.push((inode, node));
        }
    }
    flush(tx, volume, generation, &mut rows).await
}

#[cfg(test)]
#[path = "inode_batch_tests.rs"]
mod tests;
