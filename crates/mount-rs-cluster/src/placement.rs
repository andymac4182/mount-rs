//! Rendezvous (highest-random-weight) ranking of a volume's candidate owners.
//!
//! The ranking is advice about which node should try to open a volume first.
//! It grants nothing: only the storage writer lease decides who may write.
//! Every node must compute the same ranking from the same candidates, so the
//! hash is fixed here and must never change between releases.

/// The weight of `node` for `volume`. Stable across processes, platforms and
/// releases: FNV-1a over `volume`, a separator no identifier contains, and
/// `node`, finished with the SplitMix64 mixer so that similar names spread.
pub fn weight(volume: &str, node: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in volume.bytes().chain([0xff]).chain(node.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash = (hash ^ (hash >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    hash = (hash ^ (hash >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    hash ^ (hash >> 31)
}

/// `candidates` from most to least preferred for `volume`, without
/// duplicates. Equal weights (not expected) are ordered by node id, so the
/// result never depends on the order the candidates were given in.
pub fn ranking<'a>(volume: &str, candidates: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut ranked: Vec<(u64, &str)> = candidates
        .into_iter()
        .map(|node| (weight(volume, node), node))
        .collect();
    ranked.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    ranked.dedup();
    ranked.into_iter().map(|(_, node)| node).collect()
}

/// The position of `node` in the ranking of `candidates` for `volume`, with
/// `node` counted as a candidate whether or not it is listed. 0 is the
/// preferred owner.
pub fn rank<'a>(volume: &str, node: &str, candidates: impl IntoIterator<Item = &'a str>) -> usize {
    let own = weight(volume, node);
    let mut above: Vec<&str> = candidates
        .into_iter()
        .filter(|other| {
            *other != node && {
                let theirs = weight(volume, other);
                theirs > own || (theirs == own && *other < node)
            }
        })
        .collect();
    above.sort_unstable();
    above.dedup();
    above.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("node-{index}")).collect()
    }
    fn refs(nodes: &[String]) -> Vec<&str> {
        nodes.iter().map(String::as_str).collect()
    }

    #[test]
    fn the_hash_is_pinned() {
        // Changing these values changes which node every running cluster
        // prefers for every drive. They must stay as they are.
        assert_eq!(weight("drive", "node-a"), 0xa401_d7b8_d08b_5929);
        assert_eq!(weight("drive", "node-b"), 0xfa64_08e9_996f_4ae7);
        assert_eq!(weight("", ""), 0x4f2d_b50f_1240_40cd);
        assert_ne!(weight("ab", "c"), weight("a", "bc"));
    }

    #[test]
    fn ranking_ignores_input_order_and_duplicates() {
        let all = nodes(7);
        let forward = ranking("volume-1", refs(&all));
        let mut reversed = refs(&all);
        reversed.reverse();
        reversed.push("node-3");
        assert_eq!(ranking("volume-1", reversed), forward);
        assert_eq!(forward.len(), 7);
        for (position, node) in forward.iter().enumerate() {
            assert_eq!(rank("volume-1", node, refs(&all)), position);
        }
    }

    #[test]
    fn every_node_agrees_on_the_preferred_owner() {
        let all = nodes(5);
        for volume in 0..200 {
            let volume = format!("volume-{volume}");
            let preferred = ranking(&volume, refs(&all))[0];
            let zero: Vec<&str> = all
                .iter()
                .map(String::as_str)
                .filter(|node| rank(&volume, node, refs(&all)) == 0)
                .collect();
            assert_eq!(zero, [preferred]);
        }
    }

    #[test]
    fn a_node_counts_itself_even_when_it_is_not_listed() {
        let all = nodes(4);
        let others: Vec<&str> = refs(&all).into_iter().filter(|n| *n != "node-2").collect();
        for volume in ["a", "b", "c", "d", "e"] {
            assert_eq!(
                rank(volume, "node-2", others.clone()),
                rank(volume, "node-2", refs(&all))
            );
        }
        assert_eq!(rank("a", "alone", []), 0);
    }

    #[test]
    fn a_node_leaving_moves_only_the_volumes_it_was_preferred_for() {
        let all = nodes(6);
        let remaining: Vec<&str> = refs(&all).into_iter().filter(|n| *n != "node-4").collect();
        let (mut moved, mut owned_by_leaver) = (0, 0);
        for volume in 0..2000 {
            let volume = format!("volume-{volume}");
            let before = ranking(&volume, refs(&all));
            let after = ranking(&volume, remaining.clone());
            // The survivors keep their relative order.
            let survivors: Vec<&str> = before.iter().copied().filter(|n| *n != "node-4").collect();
            assert_eq!(after, survivors);
            if before[0] == "node-4" {
                owned_by_leaver += 1;
                assert_eq!(after[0], before[1]);
            }
            if before[0] != after[0] {
                moved += 1;
            }
        }
        assert_eq!(moved, owned_by_leaver);
        // One node of six is preferred for about a sixth of the volumes.
        assert!((233..=433).contains(&owned_by_leaver), "{owned_by_leaver}");
    }

    #[test]
    fn preference_is_spread_evenly() {
        let all = nodes(4);
        let mut counts = std::collections::BTreeMap::new();
        for volume in 0..4000 {
            let volume = format!("drive-{volume}");
            *counts.entry(ranking(&volume, refs(&all))[0]).or_insert(0) += 1;
        }
        assert_eq!(counts.len(), 4);
        for (node, count) in counts {
            assert!((850..=1150).contains(&count), "{node}: {count}");
        }
    }
}
