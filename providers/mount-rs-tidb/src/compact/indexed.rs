//! Physical indexed compact metadata; logical proofs remain in mount-rs-core.
use super::*;
use mount_rs_core::storage::DirectoryEntry;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

pub(super) const MEMBERS_TABLE: &str = "mount_rs_tidb_compact_members";
pub(super) const DENTRIES_TABLE: &str = "mount_rs_tidb_compact_dentries";
pub(super) const MEMBERS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_tidb_compact_members (
    volume_key VARBINARY(1020) NOT NULL,
    inode BIGINT NOT NULL,
    PRIMARY KEY(volume_key,inode)
)";
pub(super) const DENTRIES_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS mount_rs_tidb_compact_dentries (
    volume_key VARBINARY(1020) NOT NULL,
    parent BIGINT NOT NULL,
    ordinal BIGINT NOT NULL,
    name_hash BINARY(32) NOT NULL,
    name LONGBLOB NOT NULL,
    inode BIGINT NOT NULL,
    PRIMARY KEY(volume_key,parent,ordinal),
    KEY name_lookup(volume_key,parent,name_hash)
)";

const AUTHORITY_TAG: &str = "mount-rs-tidb-indexed-compact";
const DIRECTORY_TAG: &str = "mount-rs-tidb-indexed-directory";
const AUTHORITY_PREFIX: &[u8] =
    b"{\"layout\":\"mount-rs-tidb-indexed-compact\",\"version\":1,\"authority\":";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityEnvelope<T> {
    layout: String,
    version: u32,
    authority: T,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryEnvelope<T> {
    layout: String,
    version: u32,
    header: T,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteNode {
    stats: mount_rs_core::Stats,
    data: NodeData,
}

pub(super) fn encode_authority(anchor: &CompactAnchor) -> Result<Vec<u8>> {
    let authority = CompactAuthority::from_anchor(anchor)?;
    signed(authority.generation, "compact generation")?;
    signed(authority.next_inode, "compact next inode")?;
    signed(authority.root, "compact root")?;
    signed(authority.members_count, "compact member count")?;
    for &inode in &anchor.members {
        signed(inode, "compact member")?;
    }
    serde_json::to_vec(&AuthorityEnvelope {
        layout: AUTHORITY_TAG.into(),
        version: 1,
        authority,
    })
    .map_err(backend_error)
}

pub(super) fn decode_authority(bytes: &[u8]) -> Result<CompactAuthority> {
    let envelope: AuthorityEnvelope<CompactAuthority> =
        serde_json::from_slice(bytes).map_err(backend_error)?;
    if envelope.layout != AUTHORITY_TAG || envelope.version != 1 {
        return Err(backend_error("unsupported indexed TiDB compact authority"));
    }
    envelope.authority.validate()?;
    signed(envelope.authority.next_inode, "compact next inode")?;
    Ok(envelope.authority)
}

/// Only canonical bytes admit a borrowed streaming fast path. Legal alternate
/// JSON representations use the strict owned decoder instead.
pub(super) fn authority_inner(bytes: &[u8]) -> Option<&[u8]> {
    bytes.strip_prefix(AUTHORITY_PREFIX)?.strip_suffix(b"}")
}

pub(super) enum StoredBody {
    Node(NodeMetadata),
    Directory(CompactDirectoryHeader),
}

pub(super) fn decode_body(inode: u64, json: &str) -> Result<StoredBody> {
    if let Ok(envelope) = serde_json::from_str::<DirectoryEnvelope<CompactDirectoryHeader>>(json) {
        if envelope.layout != DIRECTORY_TAG || envelope.version != 1 {
            return Err(backend_error("unsupported indexed TiDB directory"));
        }
        envelope.header.validate(inode)?;
        signed(
            envelope.header.next_ordinal,
            "compact next directory ordinal",
        )?;
        return Ok(StoredBody::Directory(envelope.header));
    }
    let node: CompleteNode = serde_json::from_str(json).map_err(backend_error)?;
    let node = NodeMetadata {
        stats: node.stats,
        data: node.data,
    };
    if matches!(node.data, NodeData::Directory { .. }) {
        return Err(backend_error("directory must use indexed TiDB header"));
    }
    validate_node_kind(&node)?;
    Ok(StoredBody::Node(node))
}

pub(super) fn encode_body(node: &NodeMetadata, next_ordinal: u64) -> Result<String> {
    validate_node_kind(node)?;
    match &node.data {
        NodeData::Directory { entries } => {
            let header = CompactDirectoryHeader {
                stats: node.stats.clone(),
                entry_count: entries.len() as u64,
                next_ordinal,
            };
            header.validate(node.stats.ino)?;
            signed(next_ordinal, "compact next directory ordinal")?;
            serde_json::to_string(&DirectoryEnvelope {
                layout: DIRECTORY_TAG.into(),
                version: 1,
                header,
            })
            .map_err(backend_error)
        }
        _ => serde_json::to_string(node).map_err(backend_error),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoredEntry {
    pub ordinal: u64,
    pub name: String,
    pub inode: u64,
}

pub(super) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\0']) {
        return Err(backend_error("invalid indexed TiDB directory name"));
    }
    Ok(())
}

pub(super) fn name_hash(name: &str) -> [u8; 32] {
    Sha256::digest(name.as_bytes()).into()
}

pub(super) fn materialize_directory(
    inode: u64,
    header: &CompactDirectoryHeader,
    entries: &[StoredEntry],
) -> Result<NodeMetadata> {
    header.validate(inode)?;
    validate_entries(entries, header.next_ordinal)?;
    if entries.len() as u64 != header.entry_count {
        return Err(backend_error("indexed TiDB directory count mismatch"));
    }
    Ok(NodeMetadata {
        stats: header.stats.clone(),
        data: NodeData::Directory {
            entries: entries
                .iter()
                .map(|e| DirectoryEntry {
                    name: e.name.clone(),
                    inode: e.inode,
                })
                .collect(),
        },
    })
}

fn validate_entries(entries: &[StoredEntry], next_ordinal: u64) -> Result<()> {
    signed(next_ordinal, "compact next directory ordinal")?;
    let mut names = BTreeSet::new();
    let mut previous = None;
    for entry in entries {
        validate_name(&entry.name)?;
        signed(entry.inode, "compact directory child")?;
        signed(entry.ordinal, "compact directory ordinal")?;
        if entry.inode == 0
            || entry.ordinal >= next_ordinal
            || previous.is_some_and(|old| old >= entry.ordinal)
            || !names.insert(&entry.name)
        {
            return Err(backend_error("invalid indexed TiDB directory rows"));
        }
        previous = Some(entry.ordinal);
    }
    Ok(())
}

pub(super) struct DirectoryPlan {
    pub rewrite: bool,
    pub removed: Vec<u64>,
    pub upserts: Vec<StoredEntry>,
    pub next_ordinal: u64,
}

impl DirectoryPlan {
    pub fn new(old: &[StoredEntry], next_ordinal: u64, next: &[DirectoryEntry]) -> Result<Self> {
        validate_entries(old, next_ordinal)?;
        let by_name: HashMap<&str, &StoredEntry> =
            old.iter().map(|e| (e.name.as_str(), e)).collect();
        let mut seen = BTreeSet::new();
        let mut previous = None;
        let mut appended = false;
        let mut retain_order = true;
        for entry in next {
            validate_name(&entry.name)?;
            signed(entry.inode, "compact directory child")?;
            if entry.inode == 0 || !seen.insert(entry.name.as_str()) {
                return Err(backend_error("duplicate indexed TiDB directory name"));
            }
            if let Some(old) = by_name.get(entry.name.as_str()) {
                if appended || previous.is_some_and(|p| p >= old.ordinal) {
                    retain_order = false;
                }
                previous = Some(old.ordinal);
            } else {
                appended = true;
            }
        }
        if !retain_order {
            let next_ordinal = next.len() as u64;
            signed(next_ordinal, "compact next directory ordinal")?;
            return Ok(Self {
                rewrite: true,
                removed: Vec::new(),
                upserts: next
                    .iter()
                    .enumerate()
                    .map(|(i, e)| StoredEntry {
                        ordinal: i as u64,
                        name: e.name.clone(),
                        inode: e.inode,
                    })
                    .collect(),
                next_ordinal,
            });
        }
        let mut ordinal = next_ordinal;
        let mut upserts = Vec::new();
        for entry in next {
            if let Some(old) = by_name.get(entry.name.as_str()) {
                if old.inode != entry.inode {
                    upserts.push(StoredEntry {
                        ordinal: old.ordinal,
                        name: entry.name.clone(),
                        inode: entry.inode,
                    });
                }
            } else {
                let allocated = ordinal;
                ordinal = ordinal
                    .checked_add(1)
                    .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
                signed(ordinal, "compact next directory ordinal")?;
                upserts.push(StoredEntry {
                    ordinal: allocated,
                    name: entry.name.clone(),
                    inode: entry.inode,
                });
            }
        }
        Ok(Self {
            rewrite: false,
            removed: old
                .iter()
                .filter(|e| !seen.contains(e.name.as_str()))
                .map(|e| e.ordinal)
                .collect(),
            upserts,
            next_ordinal: ordinal,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ordinal: u64, name: &str, inode: u64) -> StoredEntry {
        StoredEntry {
            ordinal,
            name: name.into(),
            inode,
        }
    }

    fn anchor(count: u64) -> CompactAnchor {
        use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
        CompactAnchor {
            backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
            generation: 2,
            root: 1,
            next_inode: count + 1,
            default_uid: 0,
            default_gid: 0,
            umask: 0o022,
            default_chunker: FixedSizeChunker::new(4096).unwrap().config(),
            members: (1..=count).collect(),
        }
    }

    fn directory(entries: Vec<DirectoryEntry>) -> NodeMetadata {
        NodeMetadata {
            stats: mount_rs_core::Stats {
                dev: 0,
                ino: 1,
                mode: mount_rs_core::S_IFDIR | 0o755,
                nlink: 2,
                uid: 0,
                gid: 0,
                rdev: 0,
                size: 0,
                blksize: 4096,
                blocks: 0,
                atime_ms: 0,
                mtime_ms: 0,
                ctime_ms: 0,
                birthtime_ms: 0,
            },
            data: NodeData::Directory { entries },
        }
    }

    #[test]
    fn directory_body_is_bounded_distinct_and_reconstructs_real_rows() {
        let node = directory(vec![DirectoryEntry {
            name: "a".into(),
            inode: 2,
        }]);
        let body = encode_body(&node, 4).unwrap();
        let StoredBody::Directory(header) = decode_body(1, &body).unwrap() else {
            panic!("header required")
        };
        assert_eq!(
            materialize_directory(1, &header, &[entry(3, "a", 2)]).unwrap(),
            node
        );
        assert!(materialize_directory(1, &header, &[]).is_err());
        assert!(materialize_directory(1, &header, &[entry(4, "a", 2)]).is_err());
        assert!(decode_body(1, &serde_json::to_string(&node).unwrap()).is_err());
        let large = directory(
            (0..10_000)
                .map(|i| DirectoryEntry {
                    name: format!("file-{i}"),
                    inode: i + 2,
                })
                .collect(),
        );
        assert!(encode_body(&large, 10_000).unwrap().len() < body.len() + 16);
        assert!(!body.contains("\"entries\""));
        let mut value: serde_json::Value = serde_json::from_str(&body).unwrap();
        value["stats"] = serde_json::to_value(&node.stats).unwrap();
        value["data"] = serde_json::to_value(&node.data).unwrap();
        assert!(decode_body(1, &serde_json::to_string(&value).unwrap()).is_err());
    }

    #[test]
    fn authority_is_distinct_strict_and_bounded_without_members() {
        let a = anchor(1);
        let bytes = encode_authority(&a).unwrap();
        assert_eq!(
            decode_authority(&bytes)
                .unwrap()
                .into_anchor(vec![1])
                .unwrap(),
            a
        );
        assert!(decode_authority(&encode_compact_anchor(&a).unwrap()).is_err());
        let many = encode_authority(&anchor(10_000)).unwrap();
        assert!(many.len() < bytes.len() + 24);
        assert!(!std::str::from_utf8(&many).unwrap().contains("\"members\":"));
        assert!(decode_authority(authority_inner(&many).unwrap()).is_err());
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["extra"] = serde_json::json!(true);
        assert!(decode_authority(&serde_json::to_vec(&value).unwrap()).is_err());
        value.as_object_mut().unwrap().remove("extra");
        value["version"] = serde_json::json!(2);
        assert!(decode_authority(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn directory_plan_retains_survivors_and_appends_rename() {
        let old = vec![entry(2, "source", 2), entry(9, "stay", 3)];
        let next = vec![
            DirectoryEntry {
                name: "stay".into(),
                inode: 3,
            },
            DirectoryEntry {
                name: "renamed".into(),
                inode: 2,
            },
        ];
        let plan = DirectoryPlan::new(&old, 10, &next).unwrap();
        assert!(!plan.rewrite);
        assert_eq!(plan.removed, vec![2]);
        assert_eq!(plan.upserts, vec![entry(10, "renamed", 2)]);
        assert_eq!(plan.next_ordinal, 11);
    }

    #[test]
    fn directory_plan_preserves_explicit_reorder() {
        let old = vec![entry(2, "a", 2), entry(9, "b", 3)];
        let next = vec![
            DirectoryEntry {
                name: "b".into(),
                inode: 3,
            },
            DirectoryEntry {
                name: "a".into(),
                inode: 2,
            },
        ];
        let plan = DirectoryPlan::new(&old, 10, &next).unwrap();
        assert!(plan.rewrite);
        assert_eq!(plan.upserts, vec![entry(0, "b", 3), entry(1, "a", 2)]);
        assert_eq!(plan.next_ordinal, 2);
    }

    #[test]
    fn directory_plan_keeps_long_full_names_and_rejects_bad_names() {
        let name = "x".repeat(8192);
        let plan = DirectoryPlan::new(
            &[],
            0,
            &[DirectoryEntry {
                name: name.clone(),
                inode: 2,
            }],
        )
        .unwrap();
        assert_eq!(plan.upserts[0].name, name);
        for name in ["", ".", "..", "a/b", "a\0b"] {
            assert!(
                DirectoryPlan::new(
                    &[],
                    0,
                    &[DirectoryEntry {
                        name: name.into(),
                        inode: 2
                    }]
                )
                .is_err()
            );
        }
    }

    #[test]
    fn directory_plan_rejects_duplicate_names_and_ordinal_corruption() {
        let next = vec![
            DirectoryEntry {
                name: "a".into(),
                inode: 2,
            },
            DirectoryEntry {
                name: "a".into(),
                inode: 3,
            },
        ];
        assert!(DirectoryPlan::new(&[], 0, &next).is_err());
        assert!(DirectoryPlan::new(&[entry(1, "a", 2), entry(1, "b", 3)], 3, &[]).is_err());
        assert!(DirectoryPlan::new(&[entry(3, "a", 2)], 3, &[]).is_err());
        assert!(
            DirectoryPlan::new(
                &[],
                i64::MAX as u64,
                &[DirectoryEntry {
                    name: "a".into(),
                    inode: 2
                }]
            )
            .is_err()
        );
    }
}
