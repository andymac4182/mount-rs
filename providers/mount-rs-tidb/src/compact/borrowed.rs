//! Borrowed binary projection checks. A miss delegates errors to the owned
//! decoder; these functions never establish snapshot provenance themselves.

use super::{
    Params, Result, StorageOperation, StorageSpan, StoredBody, Transaction, authority, db_error,
    signed, stored_guards,
};
use mount_rs_core::storage::ConcurrentBackingId;
use mount_rs_core::storage::compact::{
    BorrowedCompactRootEntry, CheckedCompactInode, CompactInodeExpectation, CompactRootMemberCursor,
};
use mysql_async::prelude::Queryable;
use mysql_async::{
    Column,
    consts::{ColumnFlags, ColumnType},
};
use sha2::{Digest, Sha256};

const MEMBERS_SQL: &str =
    "SELECT inode FROM mount_rs_tidb_compact_members WHERE volume_key=? ORDER BY inode";
const ENTRIES_SQL: &str = "SELECT parent,ordinal,name_hash,name,inode FROM mount_rs_tidb_compact_dentries WHERE volume_key=? AND parent=? ORDER BY parent,ordinal";

/// Observe one entire prepared result, then settle its terminal driver state.
/// Domain misses stay inside the callback and cannot become transport retries.
async fn scan<F>(
    tx: &mut Transaction<'_>,
    sql: &str,
    parameters: Params,
    mut observe: F,
) -> std::result::Result<bool, mysql_async::Error>
where
    F: for<'row> FnMut(&'row [u8], &'row [Column]) + Send,
{
    let mut span = StorageSpan::new(StorageOperation::TidbSqlInodeRead);
    let outcome = async {
        let mut result = tx.exec_iter(sql, parameters).await?;
        let mut rows = 0_u64;
        let single_result = loop {
            let (row, more_at_boundary) = result
                .next_binary_row_with_boundary(|packet, columns| observe(packet, columns))
                .await?;
            if row.is_some() {
                rows = rows.saturating_add(1);
            } else {
                break !more_at_boundary && result.is_empty();
            }
        };
        // Includes any unexpected extra set, including empty OK sets. No
        // receipt or fallback query escapes while a response remains unread.
        result.drop_result().await?;
        Ok::<_, mysql_async::Error>((rows, single_result))
    }
    .await;
    match outcome {
        Ok((rows, single_result)) => {
            span.finish_success_with_rows(0, rows);
            Ok(single_result)
        }
        Err(error) => {
            span.finish_error();
            Err(error)
        }
    }
}

/// A successful hit retains the original sealed audit after comparing all fresh
/// rows. Every miss uses the caller's ordinary validator in this same RR view.
pub(super) async fn try_root(
    tx: &mut Transaction<'_>,
    volume: &str,
    backing: ConcurrentBackingId,
    inode: u64,
    expected: CompactInodeExpectation<'_>,
) -> Result<Option<CheckedCompactInode>> {
    if expected.audited_root_inode() != Some(inode) {
        return Ok(None);
    }
    let fresh = authority(tx, volume, backing).await?;
    let Some(mut members) = CompactRootMemberCursor::begin(&fresh, backing, inode, expected) else {
        return Ok(None);
    };
    let complete = scan(
        tx,
        MEMBERS_SQL,
        Params::from((volume,)),
        |packet, columns| {
            if let Some(inode) = parse_member(packet, columns) {
                members.observe_member(inode);
            } else {
                members.reject();
            }
        },
    )
    .await
    .map_err(|e| db_error("read indexed TiDB compact members", e))?;
    if !complete {
        members.reject();
    }
    let Some(members) = members.finish_members() else {
        return Ok(None);
    };

    // These are the existing owned O(1) guard/header decoders. Membership
    // validation precedes them, preserving the ordinary error precedence.
    let mut guards = stored_guards(tx, volume, Some(inode), false).await?;
    let Some(guard) = guards.remove(&inode) else {
        return Ok(None);
    };
    let StoredBody::Directory(header) = guard.body else {
        return Ok(None);
    };
    let Some(mut entries) = members.observe_guard(inode, guard.identity, &header) else {
        return Ok(None);
    };
    let complete = scan(
        tx,
        ENTRIES_SQL,
        Params::from((volume, signed(inode, "compact directory parent")?)),
        |packet, columns| {
            if let Some(actual) = parse_dentry(packet, columns) {
                entries.observe_entry(BorrowedCompactRootEntry {
                    parent: actual.parent,
                    ordinal: actual.ordinal,
                    name: actual.name,
                    inode: actual.inode,
                });
            } else {
                entries.reject();
            }
        },
    )
    .await
    .map_err(|e| db_error("read indexed TiDB directory entries", e))?;
    if !complete {
        entries.reject();
    }
    Ok(entries.finish_dentries())
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedDentry<'row> {
    parent: u64,
    ordinal: u64,
    name: &'row str,
    inode: u64,
}

fn signed_column(column: &Column, name: &[u8]) -> bool {
    column.name_ref() == name
        && column.column_type() == ColumnType::MYSQL_TYPE_LONGLONG
        && column.flags().contains(ColumnFlags::NOT_NULL_FLAG)
        && !column.flags().contains(ColumnFlags::UNSIGNED_FLAG)
}

fn bytes_column(column: &Column, name: &[u8]) -> bool {
    column.name_ref() == name
        && column
            .flags()
            .contains(ColumnFlags::NOT_NULL_FLAG | ColumnFlags::BINARY_FLAG)
        && matches!(
            column.column_type(),
            ColumnType::MYSQL_TYPE_STRING
                | ColumnType::MYSQL_TYPE_VAR_STRING
                | ColumnType::MYSQL_TYPE_VARCHAR
                | ColumnType::MYSQL_TYPE_TINY_BLOB
                | ColumnType::MYSQL_TYPE_MEDIUM_BLOB
                | ColumnType::MYSQL_TYPE_LONG_BLOB
                | ColumnType::MYSQL_TYPE_BLOB
        )
}

struct Packet<'row>(&'row [u8]);

impl<'row> Packet<'row> {
    fn take(&mut self, length: usize) -> Option<&'row [u8]> {
        let result = self.0.get(..length)?;
        self.0 = self.0.get(length..)?;
        Some(result)
    }

    fn integer(&mut self) -> Option<u64> {
        let value = i64::from_le_bytes(self.take(8)?.try_into().ok()?);
        u64::try_from(value).ok()
    }

    fn bytes(&mut self) -> Option<&'row [u8]> {
        let prefix = *self.take(1)?.first()?;
        let length = match prefix {
            0..=0xfa => u64::from(prefix),
            0xfc => u64::from(u16::from_le_bytes(self.take(2)?.try_into().ok()?)),
            0xfd => {
                let bytes = self.take(3)?;
                u64::from(bytes[0]) | (u64::from(bytes[1]) << 8) | (u64::from(bytes[2]) << 16)
            }
            0xfe => u64::from_le_bytes(self.take(8)?.try_into().ok()?),
            // NULL is carried by the binary bitmap, never a length marker.
            _ => return None,
        };
        self.take(usize::try_from(length).ok()?)
    }

    fn row(packet: &'row [u8], null_mask: u8) -> Option<Self> {
        let mut row = Self(packet);
        if row.take(1)? != [0] || row.take(1)?[0] & null_mask != 0 {
            return None;
        }
        Some(row)
    }
}

fn parse_member(packet: &[u8], columns: &[Column]) -> Option<u64> {
    let [column] = columns else { return None };
    if !signed_column(column, b"inode") {
        return None;
    }
    let mut row = Packet::row(packet, 1 << 2)?;
    let inode = row.integer()?;
    row.0.is_empty().then_some(inode)
}

fn parse_dentry<'row>(packet: &'row [u8], columns: &[Column]) -> Option<ParsedDentry<'row>> {
    let [parent, ordinal, hash, name, inode] = columns else {
        return None;
    };
    if !signed_column(parent, b"parent")
        || !signed_column(ordinal, b"ordinal")
        || !bytes_column(hash, b"name_hash")
        || !bytes_column(name, b"name")
        || !signed_column(inode, b"inode")
    {
        return None;
    }
    let mut row = Packet::row(packet, 0b0111_1100)?;
    let parent = row.integer()?;
    let ordinal = row.integer()?;
    let hash = row.bytes()?;
    let name = row.bytes()?;
    let inode = row.integer()?;
    if !row.0.is_empty() || hash.len() != 32 || hash != Sha256::digest(name).as_slice() {
        return None;
    }
    let name = std::str::from_utf8(name).ok()?;
    if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\0']) {
        return None;
    }
    Some(ParsedDentry {
        parent,
        ordinal,
        name,
        inode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mysql_async::consts::{ColumnFlags, ColumnType};
    use sha2::{Digest, Sha256};

    fn integer(name: &[u8]) -> Column {
        Column::new(ColumnType::MYSQL_TYPE_LONGLONG)
            .with_name(name)
            .with_flags(ColumnFlags::NOT_NULL_FLAG)
    }

    fn member_columns() -> [Column; 1] {
        [integer(b"inode")]
    }

    fn dentry_columns() -> [Column; 5] {
        [
            integer(b"parent"),
            integer(b"ordinal"),
            Column::new(ColumnType::MYSQL_TYPE_STRING)
                .with_name(b"name_hash")
                .with_flags(ColumnFlags::NOT_NULL_FLAG | ColumnFlags::BINARY_FLAG),
            Column::new(ColumnType::MYSQL_TYPE_LONG_BLOB)
                .with_name(b"name")
                .with_flags(ColumnFlags::NOT_NULL_FLAG | ColumnFlags::BINARY_FLAG),
            integer(b"inode"),
        ]
    }

    fn length(out: &mut Vec<u8>, bytes: &[u8], encoding: u8) {
        match encoding {
            0 => out.push(u8::try_from(bytes.len()).unwrap()),
            0xfc => {
                out.push(0xfc);
                out.extend_from_slice(&u16::try_from(bytes.len()).unwrap().to_le_bytes());
            }
            0xfd => {
                out.push(0xfd);
                out.extend_from_slice(&u32::try_from(bytes.len()).unwrap().to_le_bytes()[..3]);
            }
            0xfe => {
                out.push(0xfe);
                out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            }
            _ => unreachable!(),
        }
        out.extend_from_slice(bytes);
    }

    fn dentry(name: &[u8], encoding: u8) -> Vec<u8> {
        let mut packet = vec![0, 0];
        packet.extend_from_slice(&1_i64.to_le_bytes());
        packet.extend_from_slice(&9_i64.to_le_bytes());
        length(&mut packet, &Sha256::digest(name), encoding);
        length(&mut packet, name, encoding);
        packet.extend_from_slice(&42_i64.to_le_bytes());
        packet
    }

    #[test]
    fn borrowed_member_checks_projection_and_full_signed_payload() {
        let columns = member_columns();
        for value in [0_i64, 1, i64::MAX] {
            let mut packet = vec![0, 0];
            packet.extend_from_slice(&value.to_le_bytes());
            assert_eq!(parse_member(&packet, &columns), Some(value as u64));
            for end in 0..packet.len() {
                assert_eq!(parse_member(&packet[..end], &columns), None);
            }
            packet.push(0);
            assert_eq!(parse_member(&packet, &columns), None);
        }
        for value in [-1_i64, i64::MIN] {
            let mut packet = vec![0, 0];
            packet.extend_from_slice(&value.to_le_bytes());
            assert_eq!(parse_member(&packet, &columns), None);
        }
    }

    #[test]
    fn borrowed_dentry_retains_full_unicode_name_without_copying() {
        let columns = dentry_columns();
        for encoding in [0, 0xfc, 0xfd, 0xfe] {
            let name = "a-long-name-🍊-á";
            let packet = dentry(name.as_bytes(), encoding);
            let parsed = parse_dentry(&packet, &columns).expect("complete valid binary row");
            assert_eq!(
                parsed,
                ParsedDentry {
                    parent: 1,
                    ordinal: 9,
                    name,
                    inode: 42
                }
            );
            let start = packet.as_ptr() as usize;
            let pointer = parsed.name.as_ptr() as usize;
            assert!((start..start + packet.len()).contains(&pointer));
            for end in 0..packet.len() {
                assert!(parse_dentry(&packet[..end], &columns).is_none());
            }
        }
        let name = "x".repeat(70_000);
        for encoding in [0xfd, 0xfe] {
            let packet = dentry(name.as_bytes(), encoding);
            assert_eq!(parse_dentry(&packet, &columns).unwrap().name, name);
        }
    }

    #[test]
    fn borrowed_rows_reject_nulls_projection_drift_and_unsigned_integers() {
        let columns = dentry_columns();
        let original = dentry(b"valid", 0);
        for column in 0..5 {
            let mut packet = original.clone();
            packet[1] = 1 << (column + 2);
            assert!(parse_dentry(&packet, &columns).is_none());
            let mut wrong = columns.clone();
            wrong[column] = wrong[column].clone().with_name(b"wrong");
            assert!(parse_dentry(&original, &wrong).is_none());
        }
        for index in [0, 1, 4] {
            let mut wrong = columns.clone();
            wrong[index] = wrong[index].clone().with_flags(ColumnFlags::UNSIGNED_FLAG);
            assert!(parse_dentry(&original, &wrong).is_none());
            let mut wrong = columns.clone();
            wrong[index] =
                Column::new(ColumnType::MYSQL_TYPE_LONG).with_name(columns[index].name_ref());
            assert!(parse_dentry(&original, &wrong).is_none());
        }
        assert!(parse_dentry(&original, &columns[..4]).is_none());
        let mut packet = original.clone();
        packet[0] = 1;
        assert!(parse_dentry(&packet, &columns).is_none());
        packet = original.clone();
        packet.push(0);
        assert!(parse_dentry(&packet, &columns).is_none());
        let mut member = vec![0, 4];
        member.extend_from_slice(&1_i64.to_le_bytes());
        assert!(parse_member(&member, &member_columns()).is_none());
    }

    #[test]
    fn borrowed_dentry_rejects_bad_names_hashes_and_length_overflow() {
        let columns = dentry_columns();
        for name in [b"".as_slice(), b".", b"..", b"a/b", b"a\0b", &[0xff]] {
            assert!(parse_dentry(&dentry(name, 0), &columns).is_none());
        }
        let mut packet = dentry(b"valid", 0);
        packet[19] ^= 1;
        assert!(parse_dentry(&packet, &columns).is_none());
        for marker in [0xfb, 0xff] {
            let mut packet = dentry(b"valid", 0);
            packet[18] = marker;
            assert!(parse_dentry(&packet, &columns).is_none());
        }
        let mut packet = vec![0, 0];
        packet.extend_from_slice(&1_i64.to_le_bytes());
        packet.extend_from_slice(&0_i64.to_le_bytes());
        packet.push(0xfe);
        packet.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(parse_dentry(&packet, &columns).is_none());
        for offset in [2, 10, dentry(b"valid", 0).len() - 8] {
            let mut packet = dentry(b"valid", 0);
            packet[offset..offset + 8].copy_from_slice(&(-1_i64).to_le_bytes());
            assert!(parse_dentry(&packet, &columns).is_none());
        }
    }
}
