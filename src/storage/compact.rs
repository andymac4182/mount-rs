//! Shared validators and a pure transaction reference model for an optional
//! compact inode layout. MRC4 encoding and publication contracts are unchanged.
//!
//! Providers must negotiate support before I/O and explicitly initialize only
//! fresh, owned volumes under an atomic mode/lease fence. These types do not
//! enroll, migrate or publish a volume. A provider must persist a
//! distinct layout tag; it must never infer this layout from MRC4 records.
//!
//! All inputs to transaction validators must come from ONE transaction view:
//! anchor, exact membership and guarded records. Selected writes do not advance
//! the anchor generation, so generation-before/after checks cannot establish
//! snapshot coherence. Validation proves well-formedness, not read provenance.
//!
//! EAGAIN means a proven conflict before any commit. An unknown commit outcome
//! must retain the original publication identity/resources: do not replay or
//! switch to MRC4. Provider/core fault tests must enforce that I/O rule.

use super::*;
use crate::diagnostics::profile::{self, Event, Span};

mod root_file;
pub use root_file::{
    CompactRootFileCapability, CompactRootFileIntent, CompactRootFileRead, CompactRootFileTimes,
    CompactRootFileTransition, VerifiedCompactRootFile,
};

/// Capability advertisement only; it does not assert a volume is enrolled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompactInodeCapability {
    #[default]
    Unsupported,
    V1,
}

/// Exact sorted membership and namespace defaults. IDs remain O(N) to read,
/// copy and rewrite; changed directory entry arrays also retain their cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactAnchor {
    pub backing: ConcurrentBackingId,
    pub generation: u64,
    pub root: InodeId,
    pub next_inode: InodeId,
    pub default_uid: u32,
    pub default_gid: u32,
    pub umask: u32,
    pub default_chunker: ChunkerConfig,
    pub members: Vec<InodeId>,
}

/// An unconditional selected read carries the body and its physical identity
/// together with the anchor generation observed in the same transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedCompactInode {
    pub generation: u64,
    pub guard: CompactGuard,
}

impl LoadedCompactInode {
    /// Validate membership, body kind and physical identity without fetching
    /// unrelated guards. The anchor and guard must share a transaction view.
    pub fn from_guard(anchor: &CompactAnchor, inode: InodeId, guard: CompactGuard) -> Result<Self> {
        anchor.validate()?;
        if anchor.members.binary_search(&inode).is_err() {
            return Err(invalid_namespace(
                "selected guard absent from compact membership",
            ));
        }
        guard.validate(inode, anchor)?;
        Ok(Self {
            generation: anchor.generation,
            guard,
        })
    }
}

/// Encode only the distinct compact v1 anchor representation.
pub fn encode_compact_anchor(anchor: &CompactAnchor) -> Result<Vec<u8>> {
    anchor.validate()?;
    serde_json::to_vec(&CompactAnchorEnvelope {
        layout: "mount-rs-compact-inodes".into(),
        version: 1,
        anchor: anchor.clone(),
    })
    .map_err(crate::backend_error)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactAnchorEnvelope {
    layout: String,
    version: u32,
    anchor: CompactAnchor,
}

/// Reject legacy namespaces, MRC4 envelopes, malformed and future formats.
pub fn decode_compact_anchor(bytes: &[u8]) -> Result<CompactAnchor> {
    let envelope: CompactAnchorEnvelope =
        serde_json::from_slice(bytes).map_err(crate::backend_error)?;
    if envelope.layout != "mount-rs-compact-inodes" || envelope.version != 1 {
        return Err(invalid_namespace("unsupported compact anchor envelope"));
    }
    envelope.anchor.validate()?;
    Ok(envelope.anchor)
}

impl CompactAnchor {
    pub fn validate(&self) -> Result<()> {
        ConcurrentBackingId::from_bytes(self.backing.as_bytes())?;
        validate_chunker_config(&self.default_chunker)?;
        if self.generation == 0
            || self.root == 0
            || self.members.is_empty()
            || self.members[0] == 0
            || self.members.windows(2).any(|pair| pair[0] >= pair[1])
            || self.members.binary_search(&self.root).is_err()
        {
            return Err(invalid_namespace(
                "invalid compact anchor identity/membership",
            ));
        }
        let max_inode = *self.members.last().expect("nonempty membership checked");
        if max_inode == u64::MAX {
            return Err(overflow_namespace("compact inode allocation exhausted"));
        }
        if self.next_inode <= max_inode {
            return Err(invalid_namespace(
                "compact next_inode must exceed membership",
            ));
        }
        Ok(())
    }

    fn with_namespace(&self, namespace: &Namespace, generation: u64) -> Self {
        Self {
            backing: self.backing,
            generation,
            root: namespace.root,
            next_inode: namespace.next_inode,
            default_uid: namespace.default_uid,
            default_gid: namespace.default_gid,
            umask: namespace.umask,
            default_chunker: namespace.default_chunker.clone(),
            members: namespace.nodes.keys().copied().collect(),
        }
    }
}

/// Exact persisted identity, separate from the projected logical token.
/// `incarnation` is the generation at allocation and never changes. IDs are
/// never reused: allocations are at or above the captured next_inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalInodeIdentity {
    pub incarnation: u64,
    pub epoch: u64,
    pub revision: u64,
}

impl PhysicalInodeIdentity {
    /// This projects a token only. It does NOT prove the corresponding cached
    /// body is fresh; selected CAS/conditional reads must also compare physical
    /// identity. Old physical epochs remain persisted until that guard changes.
    pub fn logical_version(self, generation: u64) -> Result<InodeVersion> {
        if self.incarnation == 0 || self.incarnation > self.epoch || self.epoch > generation {
            return Err(invalid_namespace(
                "invalid compact physical epoch/incarnation",
            ));
        }
        Ok(InodeVersion {
            structural_generation: generation,
            inode_revision: if self.epoch < generation {
                0
            } else {
                self.revision
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactGuard {
    pub identity: PhysicalInodeIdentity,
    pub node: NodeMetadata,
}

impl CompactGuard {
    fn validate(&self, inode: InodeId, anchor: &CompactAnchor) -> Result<()> {
        self.identity.logical_version(anchor.generation)?;
        if inode == 0 || self.node.stats.ino != inode {
            return Err(invalid_namespace("compact guard inode mismatch"));
        }
        validate_node_kind(&self.node)?;
        if let NodeData::Directory { entries } = &self.node.data {
            let mut names = BTreeSet::new();
            for entry in entries {
                validate_entry_name(&entry.name)?;
                if !names.insert(&entry.name) || anchor.members.binary_search(&entry.inode).is_err()
                {
                    return Err(invalid_namespace("invalid compact directory entries"));
                }
            }
        }
        Ok(())
    }
}

/// Borrowed expected body and physical identity. This is a comparison input,
/// never a freshness assertion. Only an audited root can use directory checks.
#[derive(Clone, Copy)]
pub struct CompactInodeExpectation<'a> {
    generation: u64,
    identity: PhysicalInodeIdentity,
    node: &'a NodeMetadata,
    root: Option<&'a ValidatedCompactStructure>,
}

impl<'a> CompactInodeExpectation<'a> {
    pub fn selected(
        generation: u64,
        identity: PhysicalInodeIdentity,
        node: &'a NodeMetadata,
    ) -> Self {
        Self {
            generation,
            identity,
            node,
            root: None,
        }
    }
}

/// A provider read either returns an exact checked match or the existing owned
/// read. Changed or unsupported encodings always retain the owned decoder.
#[derive(Debug)]
pub enum CompactInodeRead {
    Unchanged(CheckedCompactInode),
    Loaded(LoadedCompactInode),
}

/// Constructed only by the complete streamed comparator. Providers must retain
/// the SQL row/view owner through this check; these fields do not establish I/O
/// provenance on their own. The filesystem rechecks its captured local revision.
#[derive(Debug)]
pub struct CheckedCompactInode {
    generation: u64,
    inode: InodeId,
    identity: PhysicalInodeIdentity,
    root: Option<VerifiedCompactRoot>,
}

impl CheckedCompactInode {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn inode(&self) -> InodeId {
        self.inode
    }
    pub fn identity(&self) -> PhysicalInodeIdentity {
        self.identity
    }
    pub fn into_verified_root(self) -> Option<VerifiedCompactRoot> {
        self.root
    }
}

/// Exact root equality inherits graph/name validity from this retained witness;
/// no directory body is copied into a selected-read receipt.
#[derive(Debug, Clone)]
pub struct VerifiedCompactRoot {
    structure: ValidatedCompactStructure,
}

impl VerifiedCompactRoot {
    pub fn generation(&self) -> u64 {
        self.structure.anchor.generation
    }
    pub fn guard(&self) -> &CompactGuard {
        &self.structure.root
    }
}

/// Conservative certifier for bytes read together at one provider view. None
/// asks the provider to run its ordinary decoder on THESE SAME bytes, preserving
/// Serde compatibility and existing error precedence. No hash substitutes for
/// parsing the entire fresh anchor and body.
pub fn check_compact_inode_unchanged(
    anchor_bytes: &[u8],
    backing: ConcurrentBackingId,
    generation: u64,
    inode: InodeId,
    identity: PhysicalInodeIdentity,
    node_bytes: &[u8],
    expected: CompactInodeExpectation<'_>,
) -> Option<CheckedCompactInode> {
    streamed::check(
        anchor_bytes,
        backing,
        generation,
        inode,
        identity,
        node_bytes,
        expected,
    )
}

mod streamed {
    use super::*;
    use serde::de::{
        self, DeserializeSeed, EnumAccess, IgnoredAny, MapAccess, SeqAccess, VariantAccess, Visitor,
    };
    use std::fmt;

    type JsonResult<T, E> = std::result::Result<T, E>;

    #[derive(Clone, Copy)]
    struct Field(&'static [&'static str]);
    impl<'de> DeserializeSeed<'de> for Field {
        type Value = usize;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> JsonResult<usize, D::Error> {
            struct Keys(&'static [&'static str]);
            impl Visitor<'_> for Keys {
                type Value = usize;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("a field name")
                }
                fn visit_str<E: de::Error>(self, value: &str) -> JsonResult<usize, E> {
                    Ok(self
                        .0
                        .iter()
                        .position(|key| *key == value)
                        .unwrap_or(self.0.len()))
                }
            }
            d.deserialize_identifier(Keys(self.0))
        }
    }

    fn field_once<E: de::Error>(
        seen: &mut u32,
        index: usize,
        names: &'static [&'static str],
    ) -> JsonResult<(), E> {
        if *seen & (1 << index) != 0 {
            return Err(E::duplicate_field(names[index]));
        }
        *seen |= 1 << index;
        Ok(())
    }
    fn complete<E: de::Error>(seen: u32, names: &'static [&'static str]) -> JsonResult<(), E> {
        for (index, name) in names.iter().enumerate() {
            if seen & (1 << index) == 0 {
                return Err(E::missing_field(name));
            }
        }
        Ok(())
    }

    struct TextEqual<'a>(&'a str);
    impl<'de> DeserializeSeed<'de> for TextEqual<'_> {
        type Value = bool;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> JsonResult<bool, D::Error> {
            struct Text<'a>(&'a str);
            impl Visitor<'_> for Text<'_> {
                type Value = bool;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("a string")
                }
                fn visit_str<E: de::Error>(self, value: &str) -> JsonResult<bool, E> {
                    Ok(value == self.0)
                }
            }
            d.deserialize_str(Text(self.0))
        }
    }

    struct FixedConfig {
        size: u64,
        supported: bool,
    }
    impl FixedConfig {
        fn valid(&self) -> bool {
            self.supported && usize::try_from(self.size).is_ok_and(|size| size != 0)
        }
    }
    struct Config;
    impl<'de> DeserializeSeed<'de> for Config {
        type Value = FixedConfig;
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            d: D,
        ) -> JsonResult<Self::Value, D::Error> {
            const NAMES: &[&str] = &["algorithm", "version", "parameters"];
            struct ConfigVisitor;
            impl<'de> Visitor<'de> for ConfigVisitor {
                type Value = FixedConfig;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("chunker configuration")
                }
                fn visit_map<M: MapAccess<'de>>(
                    self,
                    mut map: M,
                ) -> JsonResult<Self::Value, M::Error> {
                    let (mut seen, mut algorithm, mut version, mut parameters) =
                        (0, false, 0_u32, (0, false));
                    while let Some(key) = map.next_key_seed(Field(NAMES))? {
                        if key < NAMES.len() {
                            field_once(&mut seen, key, NAMES)?;
                        }
                        match key {
                            0 => algorithm = map.next_value_seed(TextEqual("fixed-size"))?,
                            1 => version = map.next_value()?,
                            2 => parameters = map.next_value_seed(Parameters)?,
                            _ => {
                                map.next_value::<IgnoredAny>()?;
                            }
                        }
                    }
                    complete(seen, NAMES)?;
                    Ok(FixedConfig {
                        size: parameters.0,
                        supported: algorithm && version == 1 && parameters.1,
                    })
                }
            }
            d.deserialize_struct("ChunkerConfig", NAMES, ConfigVisitor)
        }
    }
    struct Parameters;
    impl<'de> DeserializeSeed<'de> for Parameters {
        type Value = (u64, bool);
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            d: D,
        ) -> JsonResult<Self::Value, D::Error> {
            struct ParametersVisitor;
            impl<'de> Visitor<'de> for ParametersVisitor {
                type Value = (u64, bool);
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("fixed chunker parameters")
                }
                fn visit_map<M: MapAccess<'de>>(
                    self,
                    mut map: M,
                ) -> JsonResult<Self::Value, M::Error> {
                    let (mut size, mut unknown) = (None, false);
                    while let Some(key) = map.next_key_seed(Field(&["chunk_size"]))? {
                        if key == 0 {
                            // BTreeMap's derived decoder retains the last duplicate.
                            size = Some(map.next_value::<u64>()?);
                        } else {
                            unknown = true;
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                    Ok((size.unwrap_or(0), size.is_some() && !unknown))
                }
            }
            d.deserialize_map(ParametersVisitor)
        }
    }

    #[derive(Default)]
    struct Members {
        last: Option<u64>,
        sorted: bool,
        selected: bool,
        root: bool,
        children: bool,
    }
    #[derive(Clone, Copy)]
    struct MemberSeed<'a> {
        selected: u64,
        root: Option<u64>,
        children: &'a [u64],
    }
    impl<'de> DeserializeSeed<'de> for MemberSeed<'_> {
        type Value = Members;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> JsonResult<Members, D::Error> {
            struct MemberVisitor<'a>(MemberSeed<'a>);
            impl<'de> Visitor<'de> for MemberVisitor<'_> {
                type Value = Members;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("compact inode membership")
                }
                fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> JsonResult<Members, S::Error> {
                    let mut result = Members {
                        sorted: true,
                        children: true,
                        ..Members::default()
                    };
                    let mut child = 0;
                    while let Some(inode) = seq.next_element::<u64>()? {
                        result.sorted &= inode != 0 && result.last.is_none_or(|last| last < inode);
                        result.last = Some(inode);
                        result.selected |= inode == self.0.selected;
                        result.root |= Some(inode) == self.0.root;
                        while child < self.0.children.len() && self.0.children[child] < inode {
                            result.children = false;
                            child += 1;
                        }
                        if self.0.children.get(child) == Some(&inode) {
                            child += 1;
                        }
                    }
                    result.children &= child == self.0.children.len();
                    Ok(result)
                }
            }
            d.deserialize_seq(MemberVisitor(self))
        }
    }
    struct AnchorView {
        backing: ConcurrentBackingId,
        generation: u64,
        root: u64,
        next_inode: u64,
        chunker: FixedConfig,
        members: Members,
    }
    // AnchorSeed keeps the root-child witness borrow tied to this parse.
    #[derive(Clone, Copy)]
    struct AnchorSeed<'a>(MemberSeed<'a>);
    impl<'de> DeserializeSeed<'de> for AnchorSeed<'_> {
        type Value = AnchorView;
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            d: D,
        ) -> JsonResult<Self::Value, D::Error> {
            const NAMES: &[&str] = &[
                "backing",
                "generation",
                "root",
                "next_inode",
                "default_uid",
                "default_gid",
                "umask",
                "default_chunker",
                "members",
            ];
            struct AnchorVisitor<'a>(MemberSeed<'a>);
            impl<'de> Visitor<'de> for AnchorVisitor<'_> {
                type Value = AnchorView;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("compact anchor")
                }
                fn visit_map<M: MapAccess<'de>>(
                    self,
                    mut map: M,
                ) -> JsonResult<Self::Value, M::Error> {
                    let mut seen = 0;
                    let (
                        mut backing,
                        mut generation,
                        mut root,
                        mut next_inode,
                        mut chunker,
                        mut members,
                    ) = (None, 0, 0, 0, None, None);
                    while let Some(key) = map.next_key_seed(Field(NAMES))? {
                        if key == NAMES.len() {
                            return Err(de::Error::custom("unknown compact anchor field"));
                        }
                        field_once(&mut seen, key, NAMES)?;
                        match key {
                            0 => backing = Some(map.next_value()?),
                            1 => generation = map.next_value()?,
                            2 => root = map.next_value()?,
                            3 => next_inode = map.next_value()?,
                            4..=6 => {
                                map.next_value::<u32>()?;
                            }
                            7 => chunker = Some(map.next_value_seed(Config)?),
                            8 => members = Some(map.next_value_seed(self.0)?),
                            _ => unreachable!(),
                        }
                    }
                    complete(seen, NAMES)?;
                    Ok(AnchorView {
                        backing: backing.expect("field checked"),
                        generation,
                        root,
                        next_inode,
                        chunker: chunker.expect("field checked"),
                        members: members.expect("field checked"),
                    })
                }
            }
            d.deserialize_struct("CompactAnchor", NAMES, AnchorVisitor(self.0))
        }
    }
    struct Envelope<'a>(MemberSeed<'a>);
    impl<'de> DeserializeSeed<'de> for Envelope<'_> {
        type Value = AnchorView;
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            d: D,
        ) -> JsonResult<Self::Value, D::Error> {
            const NAMES: &[&str] = &["layout", "version", "anchor"];
            struct EnvelopeVisitor<'a>(MemberSeed<'a>);
            impl<'de> Visitor<'de> for EnvelopeVisitor<'_> {
                type Value = AnchorView;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("compact anchor envelope")
                }
                fn visit_map<M: MapAccess<'de>>(
                    self,
                    mut map: M,
                ) -> JsonResult<Self::Value, M::Error> {
                    let (mut seen, mut layout, mut version, mut anchor) = (0, false, 0_u32, None);
                    while let Some(key) = map.next_key_seed(Field(NAMES))? {
                        if key == NAMES.len() {
                            return Err(de::Error::custom("unknown compact envelope field"));
                        }
                        field_once(&mut seen, key, NAMES)?;
                        match key {
                            0 => {
                                layout =
                                    map.next_value_seed(TextEqual("mount-rs-compact-inodes"))?
                            }
                            1 => version = map.next_value()?,
                            2 => anchor = Some(map.next_value_seed(AnchorSeed(self.0))?),
                            _ => unreachable!(),
                        }
                    }
                    complete(seen, NAMES)?;
                    if !layout || version != 1 {
                        return Err(de::Error::custom("unsupported compact anchor envelope"));
                    }
                    Ok(anchor.expect("field checked"))
                }
            }
            d.deserialize_struct("CompactAnchorEnvelope", NAMES, EnvelopeVisitor(self.0))
        }
    }
    fn anchor(bytes: &[u8], members: MemberSeed<'_>) -> Option<AnchorView> {
        let mut decoder = serde_json::Deserializer::from_slice(bytes);
        let value = Envelope(members).deserialize(&mut decoder).ok()?;
        decoder.end().ok()?;
        Some(value)
    }

    struct Node<'a>(&'a NodeMetadata);
    impl<'de> DeserializeSeed<'de> for Node<'_> {
        type Value = bool;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> JsonResult<bool, D::Error> {
            const NAMES: &[&str] = &["stats", "data"];
            struct NodeVisitor<'a>(&'a NodeMetadata);
            impl<'de> Visitor<'de> for NodeVisitor<'_> {
                type Value = bool;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("inode metadata")
                }
                fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> JsonResult<bool, M::Error> {
                    let (mut seen, mut equal) = (0, true);
                    while let Some(key) = map.next_key_seed(Field(NAMES))? {
                        if key < NAMES.len() {
                            field_once(&mut seen, key, NAMES)?;
                        }
                        match key {
                            0 => equal &= map.next_value::<Stats>()? == self.0.stats,
                            1 => equal &= map.next_value_seed(Data(&self.0.data))?,
                            _ => {
                                map.next_value::<IgnoredAny>()?;
                            }
                        }
                    }
                    complete(seen, NAMES)?;
                    Ok(equal)
                }
            }
            d.deserialize_struct("NodeMetadata", NAMES, NodeVisitor(self.0))
        }
    }
    struct Data<'a>(&'a NodeData);
    impl<'de> DeserializeSeed<'de> for Data<'_> {
        type Value = bool;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> JsonResult<bool, D::Error> {
            const NAMES: &[&str] = &["Directory", "File", "Symlink", "Special"];
            struct DataVisitor<'a>(&'a NodeData);
            impl<'de> Visitor<'de> for DataVisitor<'_> {
                type Value = bool;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("inode data")
                }
                fn visit_enum<E: EnumAccess<'de>>(self, data: E) -> JsonResult<bool, E::Error> {
                    let (kind, variant) = data.variant_seed(Field(NAMES))?;
                    match (kind, self.0) {
                        (0, NodeData::Directory { entries }) => {
                            variant.struct_variant(&["entries"], Directory(entries))
                        }
                        (1, NodeData::File(layout)) => variant.newtype_variant_seed(Layout(layout)),
                        _ => Err(de::Error::custom("changed or unsupported inode kind")),
                    }
                }
            }
            d.deserialize_enum("NodeData", NAMES, DataVisitor(self.0))
        }
    }
    struct Directory<'a>(&'a [DirectoryEntry]);
    impl<'de> Visitor<'de> for Directory<'_> {
        type Value = bool;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("directory entries")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> JsonResult<bool, M::Error> {
            const NAMES: &[&str] = &["entries"];
            let (mut seen, mut equal) = (0, true);
            while let Some(key) = map.next_key_seed(Field(NAMES))? {
                if key == 0 {
                    field_once(&mut seen, key, NAMES)?;
                    equal &= map.next_value_seed(Entries(self.0))?;
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            complete(seen, NAMES)?;
            Ok(equal)
        }
    }
    struct Entries<'a>(&'a [DirectoryEntry]);
    impl<'de> DeserializeSeed<'de> for Entries<'_> {
        type Value = bool;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> JsonResult<bool, D::Error> {
            struct EntriesVisitor<'a>(&'a [DirectoryEntry]);
            impl<'de> Visitor<'de> for EntriesVisitor<'_> {
                type Value = bool;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("ordered directory entries")
                }
                fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> JsonResult<bool, S::Error> {
                    for entry in self.0 {
                        if seq.next_element_seed(Entry(entry))? != Some(true) {
                            return Err(de::Error::custom("changed directory entries"));
                        }
                    }
                    Ok(seq.next_element::<IgnoredAny>()?.is_none())
                }
            }
            d.deserialize_seq(EntriesVisitor(self.0))
        }
    }
    struct Entry<'a>(&'a DirectoryEntry);
    impl<'de> DeserializeSeed<'de> for Entry<'_> {
        type Value = bool;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> JsonResult<bool, D::Error> {
            const NAMES: &[&str] = &["name", "inode"];
            struct EntryVisitor<'a>(&'a DirectoryEntry);
            impl<'de> Visitor<'de> for EntryVisitor<'_> {
                type Value = bool;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("directory entry")
                }
                fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> JsonResult<bool, M::Error> {
                    let (mut seen, mut equal) = (0, true);
                    while let Some(key) = map.next_key_seed(Field(NAMES))? {
                        if key < NAMES.len() {
                            field_once(&mut seen, key, NAMES)?;
                        }
                        match key {
                            0 => equal &= map.next_value_seed(TextEqual(&self.0.name))?,
                            1 => equal &= map.next_value::<u64>()? == self.0.inode,
                            _ => {
                                map.next_value::<IgnoredAny>()?;
                            }
                        }
                    }
                    complete(seen, NAMES)?;
                    Ok(equal)
                }
            }
            d.deserialize_struct("DirectoryEntry", NAMES, EntryVisitor(self.0))
        }
    }
    struct Layout<'a>(&'a FileLayout);
    impl<'de> DeserializeSeed<'de> for Layout<'_> {
        type Value = bool;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> JsonResult<bool, D::Error> {
            const NAMES: &[&str] = &["chunker", "extents"];
            struct LayoutVisitor<'a>(&'a FileLayout);
            impl<'de> Visitor<'de> for LayoutVisitor<'_> {
                type Value = bool;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("file layout")
                }
                fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> JsonResult<bool, M::Error> {
                    let (mut seen, mut equal) = (0, true);
                    while let Some(key) = map.next_key_seed(Field(NAMES))? {
                        if key < NAMES.len() {
                            field_once(&mut seen, key, NAMES)?;
                        }
                        match key {
                            0 => {
                                let config = map.next_value_seed(Config)?;
                                equal &= config.valid()
                                    && self.0.chunker.parameters.get("chunk_size")
                                        == Some(&config.size);
                            }
                            1 => equal &= map.next_value_seed(Extents(&self.0.extents))?,
                            _ => {
                                map.next_value::<IgnoredAny>()?;
                            }
                        }
                    }
                    complete(seen, NAMES)?;
                    Ok(equal)
                }
            }
            d.deserialize_struct("FileLayout", NAMES, LayoutVisitor(self.0))
        }
    }
    struct Extents<'a>(&'a [BlockExtent]);
    impl<'de> DeserializeSeed<'de> for Extents<'_> {
        type Value = bool;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> JsonResult<bool, D::Error> {
            struct ExtentsVisitor<'a>(&'a [BlockExtent]);
            impl<'de> Visitor<'de> for ExtentsVisitor<'_> {
                type Value = bool;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("ordered file extents")
                }
                fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> JsonResult<bool, S::Error> {
                    for extent in self.0 {
                        if seq.next_element_seed(Extent(extent))? != Some(true) {
                            return Err(de::Error::custom("changed file extents"));
                        }
                    }
                    Ok(seq.next_element::<IgnoredAny>()?.is_none())
                }
            }
            d.deserialize_seq(ExtentsVisitor(self.0))
        }
    }
    struct Extent<'a>(&'a BlockExtent);
    impl<'de> DeserializeSeed<'de> for Extent<'_> {
        type Value = bool;
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> JsonResult<bool, D::Error> {
            const NAMES: &[&str] = &["file_offset", "block", "block_offset", "length"];
            struct ExtentVisitor<'a>(&'a BlockExtent);
            impl<'de> Visitor<'de> for ExtentVisitor<'_> {
                type Value = bool;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("file extent")
                }
                fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> JsonResult<bool, M::Error> {
                    let (mut seen, mut equal) = (0, true);
                    while let Some(key) = map.next_key_seed(Field(NAMES))? {
                        if key < NAMES.len() {
                            field_once(&mut seen, key, NAMES)?;
                        }
                        match key {
                            0 => equal &= map.next_value::<u64>()? == self.0.file_offset,
                            1 => equal &= map.next_value_seed(TextEqual(&self.0.block.0))?,
                            2 => equal &= map.next_value::<u64>()? == self.0.block_offset,
                            3 => equal &= map.next_value::<u64>()? == self.0.length,
                            _ => {
                                map.next_value::<IgnoredAny>()?;
                            }
                        }
                    }
                    complete(seen, NAMES)?;
                    Ok(equal)
                }
            }
            d.deserialize_struct("BlockExtent", NAMES, ExtentVisitor(self.0))
        }
    }

    pub(super) fn check(
        bytes: &[u8],
        backing: ConcurrentBackingId,
        generation: u64,
        inode: u64,
        identity: PhysicalInodeIdentity,
        node: &[u8],
        expected: CompactInodeExpectation<'_>,
    ) -> Option<CheckedCompactInode> {
        if expected.generation != generation
            || expected.identity != identity
            || expected.node.stats.ino != inode
            || inode == 0
        {
            return None;
        }
        identity.logical_version(generation).ok()?;
        match (&expected.node.data, expected.root) {
            (NodeData::File(_), None) => {
                validate_node_kind(expected.node).ok()?;
            }
            (NodeData::Directory { .. }, Some(root)) if root.anchor.root == inode => {}
            _ => return None,
        }
        let view = anchor(
            bytes,
            MemberSeed {
                selected: inode,
                root: None,
                children: &[],
            },
        )?;
        if view.backing != backing
            || ConcurrentBackingId::from_bytes(view.backing.as_bytes()).is_err()
            || view.generation != generation
            || generation == 0
            || view.root == 0
            || !view.chunker.valid()
            || !view.members.sorted
            || !view.members.selected
            || view
                .members
                .last
                .is_none_or(|max| max == u64::MAX || view.next_inode <= max)
        {
            return None;
        }
        // A second complete parse supports root/members in any object order and
        // merges the fresh membership with sorted unique audited child IDs.
        let children = expected
            .root
            .map_or(&[][..], |root| root.root_children.as_ref());
        let checked = anchor(
            bytes,
            MemberSeed {
                selected: inode,
                root: Some(view.root),
                children,
            },
        )?;
        if !checked.members.root || !checked.members.children {
            return None;
        }
        let mut decoder = serde_json::Deserializer::from_slice(node);
        if !Node(expected.node).deserialize(&mut decoder).ok()? {
            return None;
        }
        decoder.end().ok()?;
        Some(CheckedCompactInode {
            generation,
            inode,
            identity,
            root: expected.root.map(|structure| VerifiedCompactRoot {
                structure: structure.clone(),
            }),
        })
    }
}

/// Materialized reference state. A provider must supply a single transactional
/// view; a type/validator cannot establish whether separately read rows coexisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactSnapshot {
    pub anchor: CompactAnchor,
    pub guards: BTreeMap<InodeId, CompactGuard>,
}

/// Proof that a complete compact snapshot passed namespace graph validation.
/// It describes topology only; it cannot certify the freshness of guard bodies.
#[derive(Debug, Clone)]
pub struct ValidatedCompactStructure {
    anchor: std::sync::Arc<CompactAnchor>,
    // Retain only the audited root guard, never a Namespace Arc. Exact root
    // equality binds a root-child proposal to the graph that was audited;
    // selected file updates cannot change this directory or its identity.
    root: std::sync::Arc<CompactGuard>,
    // Prepared only after a complete graph audit. Entry order need not follow
    // inode order, and hard links may repeat a child ID.
    root_children: std::sync::Arc<[InodeId]>,
    // Exactly aligned with root_children, derived only from an audited graph.
    root_single_link_file_bits: std::sync::Arc<[u64]>,
}

impl ValidatedCompactStructure {
    pub fn anchor(&self) -> &CompactAnchor {
        &self.anchor
    }

    fn audited_root_children(
        namespace: &Namespace,
    ) -> (std::sync::Arc<[InodeId]>, std::sync::Arc<[u64]>) {
        let mut children = match &namespace.nodes[&namespace.root].data {
            NodeData::Directory { entries } => {
                entries.iter().map(|entry| entry.inode).collect::<Vec<_>>()
            }
            _ => Vec::new(),
        };
        children.sort_unstable();
        children.dedup();
        let mut bits = vec![0; children.len().div_ceil(64)];
        for (index, inode) in children.iter().enumerate() {
            let node = &namespace.nodes[inode];
            if matches!(node.data, NodeData::File(_)) && node.stats.nlink == 1 {
                bits[index / 64] |= 1_u64 << (index % 64);
            }
        }
        (children.into(), bits.into())
    }

    fn new(anchor: CompactAnchor, root: CompactGuard, namespace: &Namespace) -> Self {
        let (root_children, root_single_link_file_bits) = Self::audited_root_children(namespace);
        Self {
            anchor: std::sync::Arc::new(anchor),
            root: std::sync::Arc::new(root),
            root_children,
            root_single_link_file_bits,
        }
    }

    pub fn expect_root(&self) -> CompactInodeExpectation<'_> {
        CompactInodeExpectation {
            generation: self.anchor.generation,
            identity: self.root.identity,
            node: &self.root.node,
            root: Some(self),
        }
    }

    /// Bind an ordinary fresh provider result to this exact audited root.
    pub fn verify_loaded_root(&self, loaded: &LoadedCompactInode) -> Result<VerifiedCompactRoot> {
        if loaded.generation != self.anchor.generation
            || loaded.guard.identity != self.root.identity
        {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        if &loaded.guard != self.root.as_ref() {
            return Err(invalid_namespace(
                "compact root body differs from audited graph",
            ));
        }
        Ok(VerifiedCompactRoot {
            structure: self.clone(),
        })
    }
}

impl CompactSnapshot {
    /// Validate the complete graph, then move guard bodies into the runtime
    /// namespace without retaining a second complete guard-body mirror.
    pub fn into_validated_namespace(
        self,
    ) -> Result<(
        Namespace,
        BTreeMap<InodeId, PhysicalInodeIdentity>,
        ValidatedCompactStructure,
    )> {
        let CompactSnapshot { anchor, guards } = self;
        anchor.validate()?;
        if !anchor.members.iter().copied().eq(guards.keys().copied()) {
            return Err(invalid_namespace(
                "compact guard membership differs from anchor",
            ));
        }
        let mut namespace = Namespace {
            format_version: NAMESPACE_FORMAT_VERSION,
            root: anchor.root,
            next_inode: anchor.next_inode,
            default_uid: anchor.default_uid,
            default_gid: anchor.default_gid,
            umask: anchor.umask,
            default_chunker: anchor.default_chunker.clone(),
            nodes: BTreeMap::new(),
        };
        let mut identities = BTreeMap::new();
        for (inode, guard) in guards {
            guard.validate(inode, &anchor)?;
            identities.insert(inode, guard.identity);
            namespace.nodes.insert(inode, guard.node);
        }
        namespace.validate()?;
        let root = CompactGuard {
            identity: identities[&anchor.root],
            node: namespace.nodes[&anchor.root].clone(),
        };
        let structure = ValidatedCompactStructure::new(anchor, root, &namespace);
        Ok((namespace, identities, structure))
    }

    pub fn namespace(&self) -> Result<Namespace> {
        let _profile =
            Span::new(Event::CompactNamespaceMaterializeNodes).units(self.guards.len() as u64);
        self.anchor.validate()?;
        if !self
            .anchor
            .members
            .iter()
            .copied()
            .eq(self.guards.keys().copied())
        {
            return Err(invalid_namespace(
                "compact guard membership differs from anchor",
            ));
        }
        for (&inode, guard) in &self.guards {
            guard.validate(inode, &self.anchor)?;
        }
        let namespace = Namespace {
            format_version: NAMESPACE_FORMAT_VERSION,
            root: self.anchor.root,
            next_inode: self.anchor.next_inode,
            default_uid: self.anchor.default_uid,
            default_gid: self.anchor.default_gid,
            umask: self.anchor.umask,
            default_chunker: self.anchor.default_chunker.clone(),
            nodes: self
                .guards
                .iter()
                .map(|(&id, guard)| (id, guard.node.clone()))
                .collect(),
        };
        namespace.validate()?;
        Ok(namespace)
    }

    /// Pure reference transition, not an I/O implementation. Clones the full
    /// state for tests. Providers use `validate_selected_update` on locked rows.
    pub fn selected_update(
        &self,
        backing: ConcurrentBackingId,
        generation: u64,
        inode: InodeId,
        expected: PhysicalInodeIdentity,
        node: NodeMetadata,
    ) -> Result<Self> {
        self.namespace()?;
        let current = self
            .guards
            .get(&inode)
            .ok_or_else(|| invalid_namespace("missing compact guard"))?;
        let updated = validate_selected_update(
            &self.anchor,
            backing,
            generation,
            inode,
            current,
            expected,
            node,
        )?;
        let mut next = self.clone();
        next.guards.insert(inode, updated);
        Ok(next)
    }
}

/// Shared selected-write transaction check. Read anchor and this guard in one
/// transaction, persist only the returned guard, and protect the anchor read
/// against concurrent structure publication. No unrelated guard write is needed.
pub fn validate_selected_update(
    anchor: &CompactAnchor,
    backing: ConcurrentBackingId,
    generation: u64,
    inode: InodeId,
    current: &CompactGuard,
    expected: PhysicalInodeIdentity,
    node: NodeMetadata,
) -> Result<CompactGuard> {
    anchor.validate()?;
    current.validate(inode, anchor)?;
    if anchor.backing != backing {
        return Err(FsError::new(ErrorCode::Estale));
    }
    if anchor.generation != generation || current.identity != expected {
        return Err(FsError::new(ErrorCode::Eagain));
    }
    if anchor.members.binary_search(&inode).is_err() {
        return Err(invalid_namespace(
            "selected guard absent from compact membership",
        ));
    }
    validate_inode_publication(inode, &current.node, &node)?;
    let revision = current
        .identity
        .logical_version(anchor.generation)?
        .inode_revision
        .checked_add(1)
        .ok_or_else(|| overflow_namespace("compact inode revision exhausted"))?;
    Ok(CompactGuard {
        identity: PhysicalInodeIdentity {
            incarnation: current.identity.incarnation,
            epoch: anchor.generation,
            revision,
        },
        node,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralScope {
    /// Complete physical version map, including untouched guards.
    Full,
    /// One independent regular-file create; only its parent guard is expected.
    FileCreate,
    /// Sealed root-child regular-file rename; exactly root and file expected.
    RootFileRenameAbsent,
    /// Sealed last-link unlink retaining the same physical file as a tombstone.
    RootFileUnlinkLastLink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentEntryPrecondition {
    pub parent: InodeId,
    pub name: String,
    pub expected: Option<InodeId>,
}

/// Immutable intent captured from a validated base BEFORE I/O. Untouched bodies
/// are absent. Read-only accessors expose exact expected rows and planned writes.
#[derive(Debug, Clone)]
pub struct CompactStructuralDelta {
    base: CompactAnchor,
    next: CompactAnchor,
    expected: BTreeMap<InodeId, PhysicalInodeIdentity>,
    changed: BTreeMap<InodeId, NodeMetadata>,
    created: BTreeMap<InodeId, NodeMetadata>,
    removed: BTreeSet<InodeId>,
    entries: Vec<ParentEntryPrecondition>,
    parent_body: Option<NodeMetadata>,
    captured_bodies: BTreeMap<InodeId, NodeMetadata>,
    // Generic Full/candidate captures retain the audited classification so a
    // different valid candidate cannot supply receipt-time graph provenance.
    next_root_children: Option<std::sync::Arc<[InodeId]>>,
    next_root_file_bits: Option<std::sync::Arc<[u64]>>,
    scope: StructuralScope,
}

/// Exact write set/receipt for an acknowledged structural publication. Untouched
/// bodies are deliberately absent: core must invalidate/reload those bodies,
/// even though their logical tokens now project to generation/revision zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactPublication {
    pub anchor: CompactAnchor,
    pub upserts: BTreeMap<InodeId, CompactGuard>,
    pub removed: BTreeSet<InodeId>,
}

/// A root-child empty regular-file create constructed from an audited graph.
/// There is no arbitrary cached/candidate Namespace input: unchanged topology
/// is inherited from the opaque structural witness and the constructor builds
/// the only permitted parent/new-file delta. Membership and parent entries
/// still require O(N) copying in the current durable format.
#[derive(Debug, Clone)]
pub struct CompactRootFileCreate {
    delta: CompactStructuralDelta,
    structure: ValidatedCompactStructure,
}

impl CompactRootFileCreate {
    pub fn capture(
        structure: &ValidatedCompactStructure,
        parent: &LoadedCompactInode,
        expected_parent: PhysicalInodeIdentity,
        name: String,
        created: NodeMetadata,
        mtime_ms: i64,
        ctime_ms: i64,
    ) -> Result<Self> {
        Self::capture_parent(
            structure,
            parent.generation,
            &parent.guard,
            expected_parent,
            name,
            created,
            mtime_ms,
            ctime_ms,
        )
    }

    pub fn capture_verified(
        structure: &ValidatedCompactStructure,
        parent: &VerifiedCompactRoot,
        expected_parent: PhysicalInodeIdentity,
        name: String,
        created: NodeMetadata,
        mtime_ms: i64,
        ctime_ms: i64,
    ) -> Result<Self> {
        if !std::sync::Arc::ptr_eq(&structure.anchor, &parent.structure.anchor)
            || !std::sync::Arc::ptr_eq(&structure.root, &parent.structure.root)
        {
            return Err(invalid_namespace(
                "verified compact root differs from audited graph",
            ));
        }
        Self::capture_parent(
            structure,
            parent.generation(),
            parent.guard(),
            expected_parent,
            name,
            created,
            mtime_ms,
            ctime_ms,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn capture_parent(
        structure: &ValidatedCompactStructure,
        parent_generation: u64,
        parent: &CompactGuard,
        expected_parent: PhysicalInodeIdentity,
        name: String,
        created: NodeMetadata,
        mtime_ms: i64,
        ctime_ms: i64,
    ) -> Result<Self> {
        let _profile = Span::new(Event::CompactStructuralDeltaCaptureNodes).units(2);
        let fail = || invalid_namespace("proposal is not an audited root-child file create");
        let anchor = structure.anchor();
        anchor.validate()?;
        if parent_generation != anchor.generation
            || parent.identity != expected_parent
            || parent.identity != structure.root.identity
        {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        if parent != structure.root.as_ref() {
            return Err(invalid_namespace(
                "compact root body differs from audited graph",
            ));
        }
        parent.validate(anchor.root, anchor)?;
        validate_entry_name(&name)?;
        let NodeData::Directory { entries } = &parent.node.data else {
            return Err(fail());
        };
        if entries.iter().any(|entry| entry.name == name) {
            return Err(FsError::new(ErrorCode::Eexist));
        }
        validate_node_kind(&created)?;
        let NodeData::File(layout) = &created.data else {
            return Err(fail());
        };
        if created.stats.ino != anchor.next_inode
            || created.stats.mode & S_IFMT != S_IFREG
            || created.stats.nlink != 1
            || created.stats.uid != anchor.default_uid
            || created.stats.gid != anchor.default_gid
            || created.stats.size != 0
            || created.stats.blocks != 0
            || !layout.extents.is_empty()
            || layout.chunker != anchor.default_chunker
        {
            return Err(fail());
        }
        let minimum_mtime = parent
            .node
            .stats
            .mtime_ms
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact parent modification time exhausted"))?;
        let minimum_ctime = parent
            .node
            .stats
            .ctime_ms
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact parent change time exhausted"))?;
        if mtime_ms < minimum_mtime || ctime_ms < minimum_ctime {
            return Err(fail());
        }
        let generation = anchor
            .generation
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact structural generation exhausted"))?;
        let next_inode = anchor
            .next_inode
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact inode allocation exhausted"))?;
        let mut next = anchor.clone();
        next.generation = generation;
        next.next_inode = next_inode;
        next.members.push(anchor.next_inode);
        let mut changed = parent.node.clone();
        let NodeData::Directory { entries } = &mut changed.data else {
            return Err(fail());
        };
        entries.push(DirectoryEntry {
            name: name.clone(),
            inode: anchor.next_inode,
        });
        changed.stats.mtime_ms = mtime_ms;
        changed.stats.ctime_ms = ctime_ms;
        let delta = CompactStructuralDelta {
            base: anchor.clone(),
            next,
            expected: BTreeMap::from([(anchor.root, expected_parent)]),
            changed: BTreeMap::from([(anchor.root, changed)]),
            created: BTreeMap::from([(anchor.next_inode, created)]),
            removed: BTreeSet::new(),
            entries: vec![ParentEntryPrecondition {
                parent: anchor.root,
                name,
                expected: None,
            }],
            parent_body: Some(parent.node.clone()),
            captured_bodies: BTreeMap::new(),
            next_root_children: None,
            next_root_file_bits: None,
            scope: StructuralScope::FileCreate,
        };
        delta.validate_file_create_parent(&parent.node)?;
        profile::add(Event::CompactStructuralExpectedGuardNodes, 1);
        Ok(Self {
            delta,
            structure: structure.clone(),
        })
    }

    pub fn delta(&self) -> &CompactStructuralDelta {
        &self.delta
    }

    /// Exact acknowledgement of this constructor-built delta extends the
    /// audited graph inductively. No unrelated body is asserted fresh.
    pub fn validate_publication(
        &self,
        receipt: &CompactPublication,
    ) -> Result<ValidatedCompactStructure> {
        self.delta.validate_receipt(receipt)?;
        let root = receipt
            .upserts
            .get(&receipt.anchor.root)
            .ok_or_else(|| invalid_namespace("root-child receipt omits its parent"))?;
        self.structure
            .with_created_root_file(receipt.anchor.clone(), root.clone())
    }
}

impl CompactStructuralDelta {
    /// Derive structural provenance only after exact receipt validation and
    /// a complete candidate graph check. The witness says nothing about
    /// freshness of bodies that were not changed by this publication.
    pub fn validate_next_structure(
        &self,
        base: &ValidatedCompactStructure,
        receipt: &CompactPublication,
        candidate: &Namespace,
    ) -> Result<ValidatedCompactStructure> {
        if base.anchor() != &self.base {
            return Err(invalid_namespace(
                "compact structural witness differs from capture",
            ));
        }
        self.validate_receipt(receipt)?;
        candidate.validate()?;
        if candidate.root != receipt.anchor.root
            || candidate.next_inode != receipt.anchor.next_inode
            || candidate.default_uid != receipt.anchor.default_uid
            || candidate.default_gid != receipt.anchor.default_gid
            || candidate.umask != receipt.anchor.umask
            || candidate.default_chunker != receipt.anchor.default_chunker
            || !candidate
                .nodes
                .keys()
                .copied()
                .eq(receipt.anchor.members.iter().copied())
        {
            return Err(invalid_namespace(
                "compact structural receipt differs from candidate",
            ));
        }
        let root = receipt
            .upserts
            .get(&receipt.anchor.root)
            .unwrap_or(base.root.as_ref());
        if candidate.nodes.get(&receipt.anchor.root) != Some(&root.node) {
            return Err(invalid_namespace(
                "compact structural candidate root differs from receipt",
            ));
        }
        for (&inode, node) in self.changed.iter().chain(&self.created) {
            if candidate.nodes.get(&inode) != Some(node) {
                return Err(invalid_namespace(
                    "compact structural candidate body differs from receipt",
                ));
            }
        }
        let next = ValidatedCompactStructure::new(receipt.anchor.clone(), root.clone(), candidate);
        if self.next_root_children.as_deref() != Some(next.root_children.as_ref())
            || self.next_root_file_bits.as_deref() != Some(next.root_single_link_file_bits.as_ref())
        {
            return Err(invalid_namespace(
                "compact structural candidate eligibility differs from capture",
            ));
        }
        Ok(next)
    }

    /// Acknowledged providers may return only this exact write set. This
    /// validates the receipt before local installation or guard disarming.
    pub fn validate_receipt(&self, receipt: &CompactPublication) -> Result<()> {
        if receipt.anchor != self.next || receipt.removed != self.removed {
            return Err(invalid_namespace(
                "compact structural receipt anchor/removals differ",
            ));
        }
        if receipt.upserts.len() != self.changed.len() + self.created.len() {
            return Err(invalid_namespace(
                "compact structural receipt upserts differ",
            ));
        }
        for (&inode, node) in self.changed.iter().chain(&self.created) {
            let incarnation = self
                .expected
                .get(&inode)
                .map_or(self.next.generation, |identity| identity.incarnation);
            let expected = PhysicalInodeIdentity {
                incarnation,
                epoch: self.next.generation,
                revision: 0,
            };
            if receipt
                .upserts
                .get(&inode)
                .is_none_or(|guard| guard.identity != expected || &guard.node != node)
            {
                return Err(invalid_namespace(
                    "compact structural receipt body/identity differs",
                ));
            }
        }
        Ok(())
    }

    pub fn capture(
        base: &CompactSnapshot,
        candidate: &Namespace,
        scope: StructuralScope,
    ) -> Result<Self> {
        if matches!(
            scope,
            StructuralScope::RootFileRenameAbsent | StructuralScope::RootFileUnlinkLastLink
        ) {
            return Err(invalid_namespace(
                "root-file scopes require a sealed transition",
            ));
        }
        let _profile = Span::new(Event::CompactStructuralDeltaCaptureNodes)
            .units(candidate.nodes.len() as u64);
        base.namespace()?;
        candidate.validate()?;
        let (next_root_children, next_root_file_bits) =
            ValidatedCompactStructure::audited_root_children(candidate);
        let generation = base
            .anchor
            .generation
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact structural generation exhausted"))?;
        if candidate.root != base.anchor.root || candidate.next_inode < base.anchor.next_inode {
            return Err(invalid_namespace("compact root/allocation moved backwards"));
        }
        let mut delta = Self {
            base: base.anchor.clone(),
            next: base.anchor.with_namespace(candidate, generation),
            expected: BTreeMap::new(),
            changed: BTreeMap::new(),
            created: BTreeMap::new(),
            removed: BTreeSet::new(),
            entries: Vec::new(),
            parent_body: None,
            captured_bodies: BTreeMap::new(),
            next_root_children: Some(next_root_children),
            next_root_file_bits: Some(next_root_file_bits),
            scope,
        };
        for (&inode, old) in &base.guards {
            match candidate.nodes.get(&inode) {
                None => {
                    delta.removed.insert(inode);
                }
                Some(node) if node != &old.node => {
                    delta.changed.insert(inode, node.clone());
                }
                _ => {}
            }
            if scope == StructuralScope::Full
                || delta.changed.contains_key(&inode)
                || delta.removed.contains(&inode)
            {
                delta.expected.insert(inode, old.identity);
            }
            let old_entries = directory_entries(&old.node);
            let new_entries = candidate
                .nodes
                .get(&inode)
                .map(directory_entries)
                .unwrap_or_default();
            for name in old_entries
                .keys()
                .chain(new_entries.keys())
                .copied()
                .collect::<BTreeSet<_>>()
            {
                if old_entries.get(name) != new_entries.get(name) {
                    delta.entries.push(ParentEntryPrecondition {
                        parent: inode,
                        name: name.to_owned(),
                        expected: old_entries.get(name).copied(),
                    });
                }
            }
        }
        for (&inode, node) in &candidate.nodes {
            if !base.guards.contains_key(&inode) {
                if inode < base.anchor.next_inode {
                    return Err(invalid_namespace(
                        "compact allocation reuses old inode range",
                    ));
                }
                delta.created.insert(inode, node.clone());
            }
        }
        if scope == StructuralScope::FileCreate {
            delta.validate_file_create(base)?;
            delta.parent_body = Some(base.guards[&delta.entries[0].parent].node.clone());
        }
        profile::add(
            Event::CompactStructuralExpectedGuardNodes,
            delta.expected.len() as u64,
        );
        Ok(delta)
    }

    /// Capture one file create from retained topology and a fresh selected parent.
    /// The cached namespace supplies unchanged bodies, not their freshness. Only
    /// the parent is expected at publication; unrelated selected bodies remain
    /// outside the transaction read/write set.
    pub fn capture_file_create(
        structure: &ValidatedCompactStructure,
        cached: &Namespace,
        parent: &LoadedCompactInode,
        expected_parent: PhysicalInodeIdentity,
        candidate: &Namespace,
    ) -> Result<Self> {
        let _profile = Span::new(Event::CompactStructuralDeltaCaptureNodes)
            .units(candidate.nodes.len() as u64);
        let fail = || invalid_namespace("delta is not an independent file create");
        let anchor = structure.anchor();
        anchor.validate()?;
        candidate.validate()?;
        if cached.format_version != NAMESPACE_FORMAT_VERSION
            || cached.root != anchor.root
            || cached.next_inode != anchor.next_inode
            || cached.default_uid != anchor.default_uid
            || cached.default_gid != anchor.default_gid
            || cached.umask != anchor.umask
            || cached.default_chunker != anchor.default_chunker
            || !cached
                .nodes
                .keys()
                .copied()
                .eq(anchor.members.iter().copied())
        {
            return Err(fail());
        }
        if parent.generation != anchor.generation {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        let inode = parent.guard.node.stats.ino;
        parent.guard.validate(inode, anchor)?;
        if parent.guard.identity != expected_parent {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        let old_parent = cached.nodes.get(&inode).ok_or_else(fail)?;
        if old_parent != &parent.guard.node {
            return Err(invalid_namespace(
                "compact parent body differs from cached namespace",
            ));
        }
        // This legacy API accepts a complete cached candidate. Its unchanged
        // child classification must still come from the retained audit; a
        // separately valid graph cannot redefine an existing child's kind.
        if cached.nodes.get(&anchor.root) != Some(&structure.root.node) {
            return Err(invalid_namespace(
                "compact cached root differs from audited graph",
            ));
        }
        for (position, inode) in structure.root_children.iter().enumerate() {
            let node = cached.nodes.get(inode).ok_or_else(fail)?;
            let eligible = matches!(node.data, NodeData::File(_)) && node.stats.nlink == 1;
            let audited = structure.root_single_link_file_bits[position / 64]
                & (1_u64 << (position % 64))
                != 0;
            if eligible != audited {
                return Err(invalid_namespace(
                    "compact cached child eligibility differs from audited graph",
                ));
            }
        }
        let generation = anchor
            .generation
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact structural generation exhausted"))?;
        let next_inode = anchor
            .next_inode
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact inode allocation exhausted"))?;
        let mut next = anchor.clone();
        next.generation = generation;
        next.next_inode = next_inode;
        next.members.push(anchor.next_inode);
        if candidate.root != next.root
            || candidate.next_inode != next.next_inode
            || candidate.default_uid != next.default_uid
            || candidate.default_gid != next.default_gid
            || candidate.umask != next.umask
            || candidate.default_chunker != next.default_chunker
            || !candidate
                .nodes
                .keys()
                .copied()
                .eq(next.members.iter().copied())
        {
            return Err(fail());
        }
        for (&id, node) in &cached.nodes {
            if id != inode && candidate.nodes.get(&id) != Some(node) {
                return Err(fail());
            }
        }
        let changed_parent = candidate.nodes.get(&inode).ok_or_else(fail)?;
        let NodeData::Directory { entries } = &changed_parent.data else {
            return Err(fail());
        };
        let NodeData::Directory {
            entries: old_entries,
        } = &old_parent.data
        else {
            return Err(fail());
        };
        let mut remaining = old_entries.iter().peekable();
        let mut added_entry = None;
        for entry in entries {
            if remaining.peek().is_some_and(|old| *old == entry) {
                remaining.next();
            } else if added_entry.replace(entry).is_some() {
                return Err(fail());
            }
        }
        if remaining.next().is_some() {
            return Err(fail());
        }
        let added_entry = added_entry.ok_or_else(fail)?;
        let created = candidate.nodes.get(&anchor.next_inode).ok_or_else(fail)?;
        let (next_root_children, next_root_file_bits) =
            ValidatedCompactStructure::audited_root_children(candidate);
        let delta = Self {
            base: anchor.clone(),
            next,
            expected: BTreeMap::from([(inode, expected_parent)]),
            changed: BTreeMap::from([(inode, changed_parent.clone())]),
            created: BTreeMap::from([(anchor.next_inode, created.clone())]),
            removed: BTreeSet::new(),
            entries: vec![ParentEntryPrecondition {
                parent: inode,
                name: added_entry.name.clone(),
                expected: None,
            }],
            parent_body: Some(old_parent.clone()),
            captured_bodies: BTreeMap::new(),
            next_root_children: Some(next_root_children),
            next_root_file_bits: Some(next_root_file_bits),
            scope: StructuralScope::FileCreate,
        };
        delta.validate_file_create_parent(old_parent)?;
        profile::add(Event::CompactStructuralExpectedGuardNodes, 1);
        Ok(delta)
    }

    fn validate_file_create(&self, base: &CompactSnapshot) -> Result<()> {
        let old = self
            .entries
            .first()
            .and_then(|entry| base.guards.get(&entry.parent))
            .ok_or_else(|| invalid_namespace("delta is not an independent file create"))?;
        self.validate_file_create_parent(&old.node)
    }

    fn validate_file_create_parent(&self, old: &NodeMetadata) -> Result<()> {
        let fail = || invalid_namespace("delta is not an independent file create");
        let next_inode = self
            .base
            .next_inode
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact inode allocation exhausted"))?;
        let mut allowed_anchor = self.base.clone();
        allowed_anchor.generation = self.next.generation;
        allowed_anchor.next_inode = next_inode;
        allowed_anchor.members.push(self.base.next_inode);
        if self.next != allowed_anchor
            || self.created.len() != 1
            || self.changed.len() != 1
            || !self.removed.is_empty()
            || self.entries.len() != 1
        {
            return Err(fail());
        }
        let created = self.created.get(&self.base.next_inode).ok_or_else(fail)?;
        if !matches!(created.data, NodeData::File(_)) || created.stats.nlink != 1 {
            return Err(fail());
        }
        let entry = &self.entries[0];
        if entry.expected.is_some() || old.stats.ino != entry.parent {
            return Err(fail());
        }
        let changed = self.changed.get(&entry.parent).ok_or_else(fail)?;
        let (
            NodeData::Directory {
                entries: old_entries,
            },
            NodeData::Directory {
                entries: new_entries,
            },
        ) = (&old.data, &changed.data)
        else {
            return Err(fail());
        };
        let mut allowed_stats = old.stats.clone();
        allowed_stats.mtime_ms = changed.stats.mtime_ms;
        allowed_stats.ctime_ms = changed.stats.ctime_ms;
        if changed.stats != allowed_stats
            || new_entries.len() != old_entries.len() + 1
            || new_entries
                .iter()
                .filter(|item| item.name != entry.name)
                .ne(old_entries.iter())
            || !new_entries
                .iter()
                .any(|item| item.name == entry.name && item.inode == self.base.next_inode)
        {
            return Err(fail());
        }
        Ok(())
    }

    pub fn scope(&self) -> StructuralScope {
        self.scope
    }
    pub fn base_anchor(&self) -> &CompactAnchor {
        &self.base
    }
    pub fn next_anchor(&self) -> &CompactAnchor {
        &self.next
    }
    pub fn expected(&self) -> &BTreeMap<InodeId, PhysicalInodeIdentity> {
        &self.expected
    }
    pub fn changed(&self) -> &BTreeMap<InodeId, NodeMetadata> {
        &self.changed
    }
    pub fn created(&self) -> &BTreeMap<InodeId, NodeMetadata> {
        &self.created
    }
    pub fn removed(&self) -> &BTreeSet<InodeId> {
        &self.removed
    }
    pub fn parent_entries(&self) -> &[ParentEntryPrecondition] {
        &self.entries
    }

    /// Shared transaction validator. Supply EXACTLY expected guards from the
    /// same transaction as anchor. Full scope MUST enumerate the entire physical
    /// guard keyspace, including phantoms, with range/phantom protection; selecting
    /// only expected IDs cannot prove exact membership. Create scope requires
    /// only its affected parent. Lock/conflict-protect all
    /// those reads and the anchor through commit. Reopen separately audits full
    /// membership/body corruption; a targeted operation is not a complete audit.
    pub fn validate_current(
        &self,
        anchor: &CompactAnchor,
        guards: &BTreeMap<InodeId, CompactGuard>,
    ) -> Result<CompactPublication> {
        anchor.validate()?;
        if anchor.backing != self.base.backing {
            return Err(FsError::new(ErrorCode::Estale));
        }
        if anchor != &self.base {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        if !guards.keys().eq(self.expected.keys()) {
            return Err(invalid_namespace(
                "compact transaction guard set is not exact",
            ));
        }
        if self.scope == StructuralScope::Full {
            CompactSnapshot {
                anchor: anchor.clone(),
                guards: guards.clone(),
            }
            .namespace()?;
        }
        for (&inode, guard) in guards {
            guard.validate(inode, anchor)?;
            if guard.identity != self.expected[&inode] {
                return Err(FsError::new(ErrorCode::Eagain));
            }
        }
        if matches!(
            self.scope,
            StructuralScope::RootFileRenameAbsent | StructuralScope::RootFileUnlinkLastLink
        ) {
            self.validate_root_file_transition()?;
            for (&inode, captured) in &self.captured_bodies {
                if guards.get(&inode).map(|guard| &guard.node) != Some(captured) {
                    return Err(invalid_namespace(
                        "compact affected body differs from captured guard",
                    ));
                }
            }
        }
        for entry in &self.entries {
            let guard = guards
                .get(&entry.parent)
                .ok_or_else(|| invalid_namespace("missing parent precondition guard"))?;
            let NodeData::Directory { entries } = &guard.node.data else {
                return Err(invalid_namespace(
                    "parent precondition guard is not a directory",
                ));
            };
            if entries
                .iter()
                .find(|item| item.name == entry.name)
                .map(|item| item.inode)
                != entry.expected
            {
                return Err(FsError::new(ErrorCode::Eagain));
            }
        }
        if self.scope == StructuralScope::FileCreate {
            let entry = &self.entries[0];
            if self.parent_body.as_ref() != Some(&guards[&entry.parent].node) {
                return Err(invalid_namespace(
                    "compact parent body differs from captured guard",
                ));
            }
        }
        let mut upserts = BTreeMap::new();
        for (&inode, node) in self.changed.iter().chain(&self.created) {
            let incarnation = self
                .expected
                .get(&inode)
                .map_or(self.next.generation, |identity| identity.incarnation);
            upserts.insert(
                inode,
                CompactGuard {
                    identity: PhysicalInodeIdentity {
                        incarnation,
                        epoch: self.next.generation,
                        revision: 0,
                    },
                    node: node.clone(),
                },
            );
        }
        Ok(CompactPublication {
            anchor: self.next.clone(),
            upserts,
            removed: self.removed.clone(),
        })
    }

    /// Pure transaction reference evaluator. Reads/validates/clones complete
    /// state; it makes NO provider performance claim. The same shared validator
    /// above produces the minimal write set for provider implementations.
    pub fn evaluate(&self, current: &CompactSnapshot) -> Result<CompactSnapshot> {
        current.namespace()?;
        let read_set = current
            .guards
            .iter()
            .filter(|(id, _)| self.expected.contains_key(id))
            .map(|(&id, guard)| (id, guard.clone()))
            .collect();
        let writes = self.validate_current(&current.anchor, &read_set)?;
        let mut next = current.clone();
        next.anchor = writes.anchor;
        for inode in writes.removed {
            next.guards.remove(&inode);
        }
        next.guards.extend(writes.upserts);
        next.namespace()?;
        Ok(next)
    }
}

fn directory_entries(node: &NodeMetadata) -> BTreeMap<&str, InodeId> {
    match &node.data {
        NodeData::Directory { entries } => entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.inode))
            .collect(),
        _ => BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, not(kani)))]
mod streamed_tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    struct CountingAllocator;
    thread_local! {
        static ENABLED: Cell<bool> = const { Cell::new(false) };
        static CALLS: Cell<usize> = const { Cell::new(0) };
        static BYTES: Cell<usize> = const { Cell::new(0) };
    }
    fn count(bytes: usize) {
        let _ = ENABLED.try_with(|enabled| {
            if enabled.get() {
                CALLS.with(|calls| calls.set(calls.get() + 1));
                BYTES.with(|requested| requested.set(requested.get() + bytes));
            }
        });
    }
    // This allocator is test-only; no production unsafe or custom allocation
    // policy is introduced. Count the calling test thread, including realloc.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count(layout.size());
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count(layout.size());
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            count(size);
            unsafe { System.realloc(ptr, layout, size) }
        }
    }
    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    fn measured<T>(operation: impl FnOnce() -> T) -> (T, usize, usize) {
        CALLS.with(|calls| calls.set(0));
        BYTES.with(|bytes| bytes.set(0));
        ENABLED.with(|enabled| enabled.set(true));
        let value = operation();
        ENABLED.with(|enabled| enabled.set(false));
        (value, CALLS.with(Cell::get), BYTES.with(Cell::get))
    }

    fn fixture(siblings: usize) -> CompactSnapshot {
        let chunker = ChunkerConfig {
            algorithm: "fixed-size".into(),
            version: 1,
            parameters: BTreeMap::from([("chunk_size".into(), 4096)]),
        };
        let identity = PhysicalInodeIdentity {
            incarnation: 1,
            epoch: 3,
            revision: 4,
        };
        let node = |inode, mode, nlink, size, data| CompactGuard {
            identity,
            node: NodeMetadata {
                stats: Stats {
                    dev: 0,
                    ino: inode,
                    mode,
                    nlink,
                    uid: 1000,
                    gid: 1000,
                    rdev: 0,
                    size,
                    blksize: 4096,
                    blocks: u64::from(size != 0),
                    atime_ms: 0,
                    mtime_ms: 0,
                    ctime_ms: 0,
                    birthtime_ms: 0,
                },
                data,
            },
        };
        let mut guards = BTreeMap::new();
        let mut entries = Vec::new();
        for inode in (2..=siblings as u64 + 1).rev() {
            entries.push(DirectoryEntry {
                name: format!("child-{inode}"),
                inode,
            });
            guards.insert(
                inode,
                node(
                    inode,
                    S_IFREG | 0o644,
                    1,
                    4,
                    NodeData::File(FileLayout {
                        chunker: chunker.clone(),
                        extents: vec![BlockExtent {
                            file_offset: 0,
                            block: BlockId("block".into()),
                            block_offset: 0,
                            length: 4,
                        }],
                    }),
                ),
            );
        }
        guards.insert(
            1,
            node(1, S_IFDIR | 0o755, 2, 0, NodeData::Directory { entries }),
        );
        CompactSnapshot {
            anchor: CompactAnchor {
                backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
                generation: 3,
                root: 1,
                next_inode: siblings as u64 + 2,
                default_uid: 1000,
                default_gid: 1000,
                umask: 0o022,
                default_chunker: chunker,
                members: guards.keys().copied().collect(),
            },
            guards,
        }
    }
    fn certify(
        anchor: &[u8],
        body: &[u8],
        inode: u64,
        identity: PhysicalInodeIdentity,
        expected: CompactInodeExpectation<'_>,
    ) -> Option<CheckedCompactInode> {
        check_compact_inode_unchanged(
            anchor,
            ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
            3,
            inode,
            identity,
            body,
            expected,
        )
    }
    fn reference(
        anchor: &[u8],
        body: &[u8],
        inode: u64,
        identity: PhysicalInodeIdentity,
    ) -> Result<LoadedCompactInode> {
        let anchor = decode_compact_anchor(anchor)?;
        let node = serde_json::from_slice(body).map_err(crate::backend_error)?;
        LoadedCompactInode::from_guard(&anchor, inode, CompactGuard { identity, node })
    }

    #[test]
    fn streamed_meter_observes_owned_reference_allocations() {
        let snapshot = fixture(128);
        let guard = &snapshot.guards[&1];
        let anchor = encode_compact_anchor(&snapshot.anchor).unwrap();
        let body = serde_json::to_vec(&guard.node).unwrap();
        // Retain the allocated body until after the same meter used by the
        // zero-allocation controls has been disabled.
        let (loaded, calls, bytes) = measured(|| {
            std::hint::black_box(reference(&anchor, &body, 1, guard.identity).unwrap())
        });
        assert!(calls > 0, "the meter must observe owned decode allocations");
        assert!(
            bytes > 0,
            "the meter must observe requested allocation bytes"
        );
        assert_eq!(loaded.guard, *guard);
    }

    #[test]
    fn streamed_plain_root_and_file_allocate_nothing_at_128_and_1000_siblings() {
        for siblings in [128, 1000] {
            let snapshot = fixture(siblings);
            let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
            let anchor = encode_compact_anchor(&snapshot.anchor).unwrap();
            for inode in [1, 2] {
                let guard = &snapshot.guards[&inode];
                let body = serde_json::to_vec(&guard.node).unwrap();
                let expected = if inode == 1 {
                    structure.expect_root()
                } else {
                    CompactInodeExpectation::selected(3, guard.identity, &guard.node)
                };
                let (checked, calls, bytes) =
                    measured(|| certify(&anchor, &body, inode, guard.identity, expected));
                assert!(checked.is_some());
                assert_eq!((calls, bytes), (0, 0), "inode {inode}, siblings {siblings}");
                assert_eq!(
                    reference(&anchor, &body, inode, guard.identity)
                        .unwrap()
                        .guard,
                    *guard
                );
            }
        }
    }

    #[test]
    fn streamed_semantic_variants_match_reference_with_bounded_escape_scratch() {
        let mut snapshot = fixture(2);
        let NodeData::Directory { entries } = &mut snapshot.guards.get_mut(&1).unwrap().node.data
        else {
            panic!()
        };
        entries[0].name = "café雪😀 quote\" slash\\".into();
        let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
        let guard = &snapshot.guards[&1];
        let mut value = serde_json::to_value(&guard.node).unwrap();
        value["ignored"] = serde_json::json!({"nested":[{"escape":"\"\\"},null]});
        value["stats"]["ignored"] = serde_json::json!([1, 2, 3]);
        let body = format!(" \n {} \t", serde_json::to_string_pretty(&value).unwrap())
            .replace("\"stats\"", "\"\\u0073tats\"")
            .replace("café雪😀", "caf\\u00e9\\u96ea\\ud83d\\ude00");
        let anchor = String::from_utf8(encode_compact_anchor(&snapshot.anchor).unwrap())
            .unwrap()
            .replace("\"layout\"", "\"\\u006cayout\"")
            .replace("\"algorithm\"", "\"algo\\u0072ithm\"")
            .replace(
                "\"chunk_size\":4096",
                "\"chunk_size\":0,\"chunk_size\":4096",
            );
        assert_eq!(
            reference(anchor.as_bytes(), body.as_bytes(), 1, guard.identity)
                .unwrap()
                .guard,
            *guard
        );
        let (checked, calls, bytes) = measured(|| {
            certify(
                anchor.as_bytes(),
                body.as_bytes(),
                1,
                guard.identity,
                structure.expect_root(),
            )
        });
        assert!(checked.is_some());
        assert!(
            calls <= 32 && bytes <= 4096,
            "escape scratch: {calls} calls/{bytes} bytes"
        );
    }

    #[test]
    fn streamed_valid_unused_members_defaults_and_actual_root_remain_accepted() {
        let snapshot = fixture(2);
        let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
        let guard = &snapshot.guards[&1];
        let body = serde_json::to_vec(&guard.node).unwrap();
        let mut anchor = snapshot.anchor.clone();
        anchor.members.push(99);
        anchor.next_inode = 100;
        anchor.root = 99;
        anchor.default_uid = 55;
        anchor.default_gid = 66;
        anchor.umask = 0o077;
        anchor
            .default_chunker
            .parameters
            .insert("chunk_size".into(), 8192);
        let bytes = encode_compact_anchor(&anchor).unwrap();
        assert!(reference(&bytes, &body, 1, guard.identity).is_ok());
        assert!(certify(&bytes, &body, 1, guard.identity, structure.expect_root()).is_some());
        // Explicit field order avoids relying on serde_json's map ordering:
        // members-before-root must still validate the CURRENT root.
        let mut remaining = serde_json::to_value(&anchor).unwrap();
        let fields = remaining.as_object_mut().unwrap();
        let members = fields.remove("members").unwrap();
        let root = fields.remove("root").unwrap();
        let remaining = serde_json::to_string(&remaining).unwrap();
        let reordered = format!(
            "{{\"anchor\":{{\"members\":{members},\"root\":{root},{}}},\"version\":1,\"layout\":\"mount-rs-compact-inodes\"}}",
            &remaining[1..remaining.len() - 1]
        );
        assert!(reference(reordered.as_bytes(), &body, 1, guard.identity).is_ok());
        assert!(
            certify(
                reordered.as_bytes(),
                &body,
                1,
                guard.identity,
                structure.expect_root()
            )
            .is_some()
        );
    }

    #[test]
    fn streamed_sequence_structs_preserve_owned_reference_fallback() {
        let snapshot = fixture(2);
        let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
        let anchor = encode_compact_anchor(&snapshot.anchor).unwrap();
        // Derived Serde struct decoders accept sequence form. The conservative
        // comparator supports map form and must leave these valid bytes to the
        // same-view owned decoder.
        let value = &snapshot.anchor;
        let sequence_anchor = serde_json::to_vec(&serde_json::json!([
            "mount-rs-compact-inodes",
            1,
            [
                value.backing,
                value.generation,
                value.root,
                value.next_inode,
                value.default_uid,
                value.default_gid,
                value.umask,
                value.default_chunker,
                value.members
            ]
        ]))
        .unwrap();
        for inode in [1, 2] {
            let guard = &snapshot.guards[&inode];
            let body = serde_json::to_vec(&guard.node).unwrap();
            let sequence_body =
                serde_json::to_vec(&serde_json::json!([guard.node.stats, guard.node.data]))
                    .unwrap();
            let expected = if inode == 1 {
                structure.expect_root()
            } else {
                CompactInodeExpectation::selected(3, guard.identity, &guard.node)
            };
            for (anchor, body) in [
                (anchor.as_slice(), sequence_body.as_slice()),
                (sequence_anchor.as_slice(), body.as_slice()),
            ] {
                assert_eq!(
                    reference(anchor, body, inode, guard.identity)
                        .unwrap()
                        .guard,
                    *guard
                );
                assert!(certify(anchor, body, inode, guard.identity, expected).is_none());
            }
        }
    }

    #[test]
    fn streamed_root_checks_unrequested_names_entry_order_and_every_stat_field() {
        let snapshot = fixture(3);
        let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
        let guard = &snapshot.guards[&1];
        let anchor = encode_compact_anchor(&snapshot.anchor).unwrap();
        let original = serde_json::to_value(&guard.node).unwrap();
        let mut renamed = original.clone();
        renamed["data"]["Directory"]["entries"][2]["name"] =
            serde_json::json!("unrequested-rename");
        let mut reordered = original.clone();
        reordered["data"]["Directory"]["entries"]
            .as_array_mut()
            .unwrap()
            .swap(0, 2);
        for changed in [renamed, reordered] {
            let body = serde_json::to_vec(&changed).unwrap();
            assert!(reference(&anchor, &body, 1, guard.identity).is_ok());
            assert!(certify(&anchor, &body, 1, guard.identity, structure.expect_root()).is_none());
        }
        for key in original["stats"].as_object().unwrap().keys() {
            let mut changed = original.clone();
            let previous = changed["stats"][key].as_i64().unwrap();
            changed["stats"][key] = serde_json::json!(previous + 1);
            let body = serde_json::to_vec(&changed).unwrap();
            assert!(
                certify(&anchor, &body, 1, guard.identity, structure.expect_root()).is_none(),
                "stat {key}"
            );
        }
    }

    #[test]
    fn streamed_membership_scan_rejects_missing_children_unsorted_duplicates_and_overflow() {
        let snapshot = fixture(3);
        let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
        let guard = &snapshot.guards[&1];
        let body = serde_json::to_vec(&guard.node).unwrap();
        let original: serde_json::Value =
            serde_json::from_slice(&encode_compact_anchor(&snapshot.anchor).unwrap()).unwrap();
        for members in [
            vec![1, 2, 3],
            vec![1, 3, 2, 4],
            vec![1, 2, 2, 3, 4],
            vec![1, 2, 3, 4, u64::MAX],
            vec![],
        ] {
            let mut anchor = original.clone();
            anchor["anchor"]["members"] = serde_json::json!(members);
            let bytes = serde_json::to_vec(&anchor).unwrap();
            assert!(reference(&bytes, &body, 1, guard.identity).is_err());
            assert!(certify(&bytes, &body, 1, guard.identity, structure.expect_root()).is_none());
        }
    }

    #[test]
    fn streamed_file_duplicates_known_fields_and_extent_corruption_use_reference_fallback() {
        let snapshot = fixture(2);
        let guard = &snapshot.guards[&2];
        let anchor = encode_compact_anchor(&snapshot.anchor).unwrap();
        let body = serde_json::to_string(&guard.node).unwrap();
        let expected = CompactInodeExpectation::selected(3, guard.identity, &guard.node);
        let last_parameter = body.replace(
            "\"chunk_size\":4096",
            "\"chunk_size\":0,\"chunk_size\":4096",
        );
        assert_eq!(
            reference(&anchor, last_parameter.as_bytes(), 2, guard.identity)
                .unwrap()
                .guard,
            *guard
        );
        assert!(
            certify(
                &anchor,
                last_parameter.as_bytes(),
                2,
                guard.identity,
                expected
            )
            .is_some()
        );
        for malformed in [
            body.replace("\"stats\":", "\"stats\":{},\"stats\":"),
            body.replace(
                "\"algorithm\":\"fixed-size\"",
                "\"algorithm\":\"fixed-size\",\"algorithm\":\"fixed-size\"",
            ),
            body.replace("\"length\":4", "\"length\":0"),
            format!("{body} null"),
            "{".into(),
        ] {
            assert!(reference(&anchor, malformed.as_bytes(), 2, guard.identity).is_err());
            assert!(certify(&anchor, malformed.as_bytes(), 2, guard.identity, expected).is_none());
        }
        let changed = body.replace("\"block\":\"block\"", "\"block\":\"other\"");
        assert!(reference(&anchor, changed.as_bytes(), 2, guard.identity).is_ok());
        assert!(certify(&anchor, changed.as_bytes(), 2, guard.identity, expected).is_none());
        let newer = PhysicalInodeIdentity {
            revision: 5,
            ..guard.identity
        };
        assert!(certify(&anchor, body.as_bytes(), 2, newer, expected).is_none());
        let mut generation = snapshot.anchor.clone();
        generation.generation = 4;
        let next_anchor = encode_compact_anchor(&generation).unwrap();
        assert!(
            check_compact_inode_unchanged(
                &next_anchor,
                generation.backing,
                4,
                2,
                guard.identity,
                body.as_bytes(),
                expected
            )
            .is_none()
        );
    }

    #[test]
    fn streamed_root_child_membership_handles_hardlink_duplicates_without_inode_order_assumption() {
        let mut snapshot = fixture(2);
        snapshot.guards.remove(&3);
        snapshot.anchor.members = vec![1, 2];
        snapshot.guards.get_mut(&2).unwrap().node.stats.nlink = 2;
        let NodeData::Directory { entries } = &mut snapshot.guards.get_mut(&1).unwrap().node.data
        else {
            panic!()
        };
        entries[0].inode = 2;
        let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
        let guard = &snapshot.guards[&1];
        assert_eq!(structure.root_children.as_ref(), &[2]);
        assert!(
            certify(
                &encode_compact_anchor(&snapshot.anchor).unwrap(),
                &serde_json::to_vec(&guard.node).unwrap(),
                1,
                guard.identity,
                structure.expect_root()
            )
            .is_some()
        );
    }

    #[test]
    fn streamed_verified_root_receipt_cannot_graft_onto_another_audited_witness() {
        let snapshot = fixture(2);
        let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
        let (_, _, other) = snapshot.clone().into_validated_namespace().unwrap();
        let guard = &snapshot.guards[&1];
        let checked = certify(
            &encode_compact_anchor(&snapshot.anchor).unwrap(),
            &serde_json::to_vec(&guard.node).unwrap(),
            1,
            guard.identity,
            structure.expect_root(),
        )
        .unwrap()
        .into_verified_root()
        .unwrap();
        let mut created = snapshot.guards[&2].node.clone();
        created.stats.ino = snapshot.anchor.next_inode;
        created.stats.size = 0;
        created.stats.blocks = 0;
        let NodeData::File(layout) = &mut created.data else {
            panic!()
        };
        layout.extents.clear();
        assert!(
            CompactRootFileCreate::capture_verified(
                &other,
                &checked,
                guard.identity,
                "new".into(),
                created.clone(),
                1,
                1
            )
            .is_err()
        );
        assert!(
            CompactRootFileCreate::capture_verified(
                &structure,
                &checked,
                guard.identity,
                "new".into(),
                created,
                1,
                1
            )
            .is_ok()
        );
    }

    #[test]
    fn root_file_eligibility_allocations_are_packed_at_1000_siblings() {
        for siblings in [63_usize, 64, 65, 128, 1000, 1023, 1024, 1025] {
            let snapshot = fixture(siblings);
            let namespace = snapshot.namespace().unwrap();
            let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
            assert_eq!(
                structure.root_single_link_file_bits.len(),
                siblings.div_ceil(64)
            );
            if siblings == 1000 {
                assert_eq!(structure.root_single_link_file_bits.len(), 16);
            }
            if siblings % 64 != 0 {
                assert_eq!(
                    structure.root_single_link_file_bits.last().unwrap() >> (siblings % 64),
                    0
                );
            }
            for (&inode, node) in &namespace.nodes {
                assert_eq!(
                    structure.root_single_link_file(inode),
                    matches!(node.data, NodeData::File(_)) && node.stats.nlink == 1
                );
            }
            let (eligible, calls, bytes) = measured(|| {
                structure
                    .root_children
                    .iter()
                    .all(|inode| structure.root_single_link_file(*inode))
            });
            assert!(eligible);
            assert_eq!((calls, bytes), (0, 0));
            let ((children, bits), calls, bytes) = measured(|| structure.root_file_rename_index());
            assert_eq!((calls, bytes), (0, 0));
            assert!(std::sync::Arc::ptr_eq(&children, &structure.root_children));
            assert!(std::sync::Arc::ptr_eq(
                &bits,
                &structure.root_single_link_file_bits
            ));
            // These arrays and fixture assertions are outside the isolated bit
            // measurement. Full-operation observations below include ID copies.
            let mut created_children = structure.root_children.to_vec();
            created_children.push(snapshot.anchor.next_inode);
            let (created_bits, calls, bytes) = measured(|| {
                root_file::append_eligible_bit(&structure.root_single_link_file_bits, siblings)
            });
            let bound = if siblings == 1000 {
                512
            } else {
                2 * created_children.len().div_ceil(64) * std::mem::size_of::<u64>() + 128
            };
            eprintln!(
                "eligibility create siblings={siblings} calls={calls} requested_bytes={bytes}"
            );
            assert!(
                calls <= 2 && bytes <= bound,
                "calls={calls} requested_bytes={bytes} bound={bound}"
            );
            assert_eq!(created_bits.len(), created_children.len().div_ceil(64));
            assert_ne!(created_bits[siblings / 64] & (1_u64 << (siblings % 64)), 0);
            for position in [0, siblings / 2, siblings - 1] {
                let mut remaining_children = structure.root_children.to_vec();
                remaining_children.remove(position);
                let (removed_bits, calls, bytes) = measured(|| {
                    root_file::remove_eligible_bit(
                        &structure.root_single_link_file_bits,
                        siblings,
                        position,
                    )
                });
                let bound = if siblings == 1000 {
                    512
                } else {
                    2 * remaining_children.len().div_ceil(64) * std::mem::size_of::<u64>() + 128
                };
                eprintln!(
                    "eligibility unlink siblings={siblings} position={position} calls={calls} requested_bytes={bytes}"
                );
                assert!(
                    calls <= 2 && bytes <= bound,
                    "calls={calls} requested_bytes={bytes} bound={bound}"
                );
                assert_eq!(removed_bits.len(), remaining_children.len().div_ceil(64));
                for index in 0..remaining_children.len() {
                    assert_ne!(removed_bits[index / 64] & (1_u64 << (index % 64)), 0);
                }
                if remaining_children.len() % 64 != 0 {
                    assert_eq!(
                        removed_bits.last().unwrap() >> (remaining_children.len() % 64),
                        0
                    );
                }
            }
            // The old witness remains strongly referenced throughout successors.
            assert_eq!(structure.root_children.len(), siblings);
            assert_eq!(
                structure.root_single_link_file_bits.len(),
                siblings.div_ceil(64)
            );
        }
    }

    #[test]
    fn root_file_complete_transition_allocation_observations() {
        for siblings in [128, 1000] {
            let snapshot = fixture(siblings);
            let (namespace, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
            let namespace = std::sync::Arc::new(namespace);
            let source = 2;
            for pinned in [false, true] {
                // This is the real Namespace Arc storage representation. A
                // retained reader keeps its old complete view after installation.
                let mut installed = if pinned {
                    std::sync::Arc::clone(&namespace)
                } else {
                    std::sync::Arc::new(namespace.as_ref().clone())
                };
                let intent = CompactRootFileIntent::UnlinkLastLink {
                    name: format!("child-{source}"),
                };
                let affected = BTreeMap::from([
                    (1, snapshot.guards[&1].clone()),
                    (source, snapshot.guards[&source].clone()),
                ]);
                let ((proposal, receipt, next_structure), calls, bytes) = measured(|| {
                    let read = CompactRootFileRead::from_guards(
                        snapshot.anchor.clone(),
                        1,
                        source,
                        Some(snapshot.guards[&1].clone()),
                        Some(snapshot.guards[&source].clone()),
                    )
                    .unwrap();
                    let verified = structure
                        .verify_root_file(
                            read,
                            source,
                            &snapshot.guards[&source].node,
                            snapshot.guards[&source].identity,
                        )
                        .unwrap();
                    let proposal = CompactRootFileTransition::capture(
                        verified,
                        intent,
                        CompactRootFileTimes {
                            parent_mtime_ms: 1,
                            parent_ctime_ms: 1,
                            file_ctime_ms: 1,
                        },
                    )
                    .unwrap();
                    let receipt = proposal
                        .delta()
                        .validate_current(&snapshot.anchor, &affected)
                        .unwrap();
                    let next_structure = proposal.validate_publication(&receipt).unwrap();
                    let next = std::sync::Arc::make_mut(&mut installed);
                    for (&inode, guard) in &receipt.upserts {
                        next.nodes.insert(inode, guard.node.clone());
                    }
                    (proposal, receipt, next_structure)
                });
                eprintln!(
                    "root-file transition/install siblings={siblings} pinned_namespace={pinned} calls={calls} requested_bytes={bytes}"
                );
                installed.validate().unwrap();
                assert_eq!(installed.nodes[&source].stats.nlink, 0);
                assert_eq!(namespace.nodes[&source].stats.nlink, 1);
                assert_eq!(next_structure.anchor(), &receipt.anchor);
                proposal.delta().validate_receipt(&receipt).unwrap();
                assert!(calls > 0 && bytes > 0);
            }
        }
    }

    #[test]
    fn streamed_pure_chunker_validator_matches_existing_constructor_rules() {
        let base = fixture(1).anchor.default_chunker;
        let mut cases = vec![base.clone()];
        let mut unsupported = base.clone();
        unsupported.algorithm = "future".into();
        cases.push(unsupported);
        let mut version = base.clone();
        version.version = 2;
        cases.push(version);
        let mut extra = base.clone();
        extra.parameters.insert("unknown".into(), 1);
        cases.push(extra);
        let mut wrong = base.clone();
        wrong.parameters = BTreeMap::from([("other".into(), 4096)]);
        cases.push(wrong);
        let mut zero = base.clone();
        zero.parameters.insert("chunk_size".into(), 0);
        cases.push(zero);
        let mut maximum = base;
        maximum.parameters.insert("chunk_size".into(), u64::MAX);
        cases.push(maximum);
        for config in cases {
            match (validate_chunker_config(&config), from_config(&config)) {
                (Ok(_), Ok(_)) => {}
                (Err(pure), Err(owned)) => {
                    assert_eq!(pure.code, owned.code);
                    assert_eq!(pure.to_string(), owned.to_string());
                }
                _ => panic!("chunker validation differs from construction"),
            }
        }
    }
}
