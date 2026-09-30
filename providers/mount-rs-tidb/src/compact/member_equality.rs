//! Fresh exact equality for contiguous member sets in scoped publications.
use super::*;
use std::borrow::Cow;

const SQL: &str = "SELECT COUNT(*),COUNT(CASE WHEN inode BETWEEN ? AND ? THEN 1 END) FROM mount_rs_tidb_compact_members WHERE volume_key=?";

fn contiguous_bounds(members: &[u64]) -> Option<(i64, i64)> {
    let first = *members.first()?;
    let last = *members.last()?;
    if first == 0
        || last > i64::MAX as u64
        || members
            .windows(2)
            .any(|pair| pair[0].checked_add(1) != Some(pair[1]))
    {
        return None;
    }
    Some((first as i64, last as i64))
}

fn fits_packet(volume_len: usize, budget: usize) -> bool {
    // Bound PREPARE and EXECUTE separately, including packet headers, null
    // bitmap, integer types/values and the length-encoded volume parameter.
    SQL.len().checked_add(64).is_some_and(|n| n <= budget)
        && volume_len.checked_add(288).is_some_and(|n| n <= budget)
}

fn equal_counts(total: u64, matched: u64, expected: usize) -> bool {
    total == expected as u64 && matched == expected as u64
}

pub(super) async fn structural_anchor<'a>(
    tx: &mut Transaction<'_>,
    volume: &str,
    delta: &'a CompactStructuralDelta,
) -> Result<(Cow<'a, CompactAnchor>, Option<usize>)> {
    let base = delta.base_anchor();
    if delta.scope() == StructuralScope::Full {
        return Ok((Cow::Owned(anchor(tx, volume, base.backing).await?), None));
    }
    let authority = authority(tx, volume, base.backing).await?;
    let mut budget = None;
    if authority.matches_anchor(base)
        && let Some(bounds) = contiguous_bounds(&base.members)
    {
        let packet = packet_budget(tx).await?;
        budget = Some(packet);
        if fits_packet(volume.len(), packet) {
            let (total, matched): (u64, u64) = tx
                .exec_first_observed(
                    StorageOperation::TidbSqlInodeRead,
                    SQL,
                    (bounds.0, bounds.1, volume),
                )
                .await
                .map_err(|e| db_error("compare indexed TiDB compact membership", e))?
                .ok_or_else(stale)?;
            // The validated primary key makes actual IDs unique. The closed
            // interval contains exactly N expected IDs. Total=N and matched=N
            // therefore proves exact set equality in this fresh statement.
            // Full audits still enumerate actual IDs; authority equality alone
            // never establishes membership freshness.
            if equal_counts(total, matched, base.members.len()) {
                return Ok((Cow::Borrowed(base), budget));
            }
        }
    }
    // Gaps, mismatches and packet refusals retain actual enumeration and the
    // original corruption/error precedence, without cloning a cached proof.
    Ok((
        Cow::Owned(authority.into_anchor(members(tx, volume).await?)?),
        budget,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_counts_prove_exact_equality_for_contiguous_sets() {
        for first in 1u64..=6 {
            for last in first..=6 {
                let expected: Vec<_> = (first..=last).collect();
                assert_eq!(
                    contiguous_bounds(&expected),
                    Some((first as i64, last as i64))
                );
                // Include equal-cardinality substitutions, missing members,
                // negatives/zero and extras on both sides of the interval.
                for actual_bits in 0u32..256 {
                    let actual: Vec<_> = (-1i64..=6)
                        .filter(|id| actual_bits & (1 << (id + 1)) != 0)
                        .collect();
                    let matched = actual
                        .iter()
                        .filter(|&&id| id >= first as i64 && id <= last as i64)
                        .count();
                    assert_eq!(
                        equal_counts(actual.len() as u64, matched as u64, expected.len()),
                        actual == expected.iter().map(|&id| id as i64).collect::<Vec<_>>()
                    );
                }
            }
        }
    }

    #[test]
    fn bounds_reject_gaps_invalid_ids_and_signed_overflow() {
        for members in [
            vec![],
            vec![0],
            vec![1, 1],
            vec![2, 1],
            vec![1, 3],
            vec![u64::MAX],
            vec![i64::MAX as u64, i64::MAX as u64 + 1],
        ] {
            assert_eq!(contiguous_bounds(&members), None);
        }
        assert_eq!(
            contiguous_bounds(&[i64::MAX as u64]),
            Some((i64::MAX, i64::MAX))
        );
        assert_eq!(
            contiguous_bounds(&(1..=10_000).collect::<Vec<_>>()),
            Some((1, 10_000))
        );
        assert_eq!(SQL.matches('?').count(), 3);
    }

    #[test]
    fn both_prepared_packets_must_fit_before_submission() {
        assert!(fits_packet(12, 300));
        assert!(!fits_packet(12, 299));
        assert!(!fits_packet(usize::MAX, usize::MAX));
        assert!(!fits_packet(0, SQL.len() + 63));
    }
}
