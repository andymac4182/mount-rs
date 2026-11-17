//! Trusted membership that can be replaced while the server runs.
//!
//! [`PeerMembership`] holds the current [`PeerDirectory`] and the SPKI pin
//! set derived from it. The forwarder, the peer listener's request policy and
//! both transports' pin checks read the current value on every handshake and
//! every request, so nothing keeps a copy of an older membership.
use crate::forwarding::{MAX_PEER_DIRECTORY_MEMBERS, PeerDirectory, TrustedPeer};
use crate::{VolumeRoute, transport::*};
use std::sync::Arc;

/// Why a membership file, or a reload of one, was refused. The text names
/// the record and field at fault; a membership file holds no secrets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MembershipError {
    /// The file is not a JSON array of membership records.
    Parse(String),
    /// The file lists no servers.
    Empty,
    /// The file lists more servers than a membership may hold.
    TooManyMembers { members: usize, maximum: usize },
    /// One record has a field that is not usable.
    InvalidRecord { index: usize, field: &'static str },
    /// Two records name the same node.
    DuplicateNode { index: usize, node: String },
    /// The file lists more servers than this process sized its peer
    /// transports for when it started.
    OverCapacity { members: usize, capacity: usize },
    /// This server's own certificate would no longer match a record.
    LocalMemberMissing { node: String },
    /// This server's own record changed a field its listener is bound to.
    LocalMemberChanged { node: String, field: &'static str },
    /// The pin set was refused by the transport.
    Pins(String),
}

impl std::fmt::Display for MembershipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(reason) => write!(f, "membership file is not valid: {reason}"),
            Self::Empty => write!(f, "membership file lists no servers"),
            Self::TooManyMembers { members, maximum } => write!(
                f,
                "membership file lists {members} servers; the most allowed is {maximum}"
            ),
            Self::InvalidRecord { index, field } => {
                write!(f, "membership record {index} has an invalid `{field}`")
            }
            Self::DuplicateNode { index, node } => write!(
                f,
                "membership record {index} repeats node `{node}`; each node may appear once"
            ),
            Self::OverCapacity { members, capacity } => write!(
                f,
                "membership file lists {members} servers but this server was started with \
                 room for {capacity}; raise forwarding.max_members and restart it"
            ),
            Self::LocalMemberMissing { node } => write!(
                f,
                "membership file has no record matching this server's own certificate \
                 (node `{node}`); it could no longer authenticate to its peers"
            ),
            Self::LocalMemberChanged { node, field } => write!(
                f,
                "membership file changes `{field}` in this server's own record \
                 (node `{node}`); restart the server to change it"
            ),
            Self::Pins(reason) => write!(f, "membership pins were refused: {reason}"),
        }
    }
}
impl std::error::Error for MembershipError {}

/// What a reload changed, by node name. Empty when the file describes the
/// membership already in use.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MembershipChange {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Nodes whose record changed any field.
    pub changed: Vec<String>,
    /// Where the removed and changed records pointed. Connections pooled to
    /// these are closed.
    pub(crate) retired: Vec<PeerDestination>,
}

impl MembershipChange {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

struct LocalMember {
    identity: TlsIdentity,
    /// The record a peer listener was started from, when this server has one.
    listener: Option<TrustedPeer>,
}

/// The membership a running server trusts, replaceable without a restart.
///
/// A reload is checked in full before anything changes; a refused reload
/// leaves the previous membership in force. An accepted one takes effect for
/// every later handshake and request, and closes the connections that the
/// new membership no longer covers.
pub struct PeerMembership {
    directory: arc_swap::ArcSwap<PeerDirectory>,
    pins: SharedPeerPins,
    capacity: usize,
    local: Option<LocalMember>,
}

impl PeerMembership {
    /// Start from a validated directory. `capacity` is the largest membership
    /// a later reload may install; the caller sizes its transports' connection
    /// limits from it, and those cannot grow while the process runs.
    pub fn new(directory: Arc<PeerDirectory>, capacity: usize) -> Result<Self, MembershipError> {
        if capacity > MAX_PEER_DIRECTORY_MEMBERS {
            return Err(MembershipError::TooManyMembers {
                members: capacity,
                maximum: MAX_PEER_DIRECTORY_MEMBERS,
            });
        }
        if directory.len() > capacity {
            return Err(MembershipError::OverCapacity {
                members: directory.len(),
                capacity,
            });
        }
        let pins = SharedPeerPins::new(approved_pins(&directory)?)
            .map_err(|error| MembershipError::Pins(error.to_string()))?;
        Ok(Self {
            directory: arc_swap::ArcSwap::new(directory),
            pins,
            capacity,
            local: None,
        })
    }

    /// Refuse any reload after which `identity`, this server's own
    /// certificate, would not match a record: its peers read the same file
    /// and would stop accepting it.
    pub fn with_local_identity(mut self, identity: TlsIdentity) -> Result<Self, MembershipError> {
        let directory = self.directory.load();
        if directory.authenticated_member(&identity).is_none() {
            return Err(MembershipError::LocalMemberMissing {
                node: identity.role_id().to_owned(),
            });
        }
        drop(directory);
        self.local = Some(LocalMember {
            identity,
            listener: None,
        });
        Ok(self)
    }

    /// As [`Self::with_local_identity`], for a server that also listens for
    /// peers. Its listener is bound to its record's address, and the routes
    /// it publishes carry the record's `boot`, `address` and
    /// `tls_server_name`, so a reload that changes any of them is refused
    /// too: the other servers would stop trusting its routes.
    pub fn with_local_listener(mut self, identity: TlsIdentity) -> Result<Self, MembershipError> {
        let directory = self.directory.load();
        let Some(record) = directory.authenticated_member(&identity).cloned() else {
            return Err(MembershipError::LocalMemberMissing {
                node: identity.role_id().to_owned(),
            });
        };
        drop(directory);
        self.local = Some(LocalMember {
            identity,
            listener: Some(record),
        });
        Ok(self)
    }

    /// The membership in force now.
    pub fn directory(&self) -> Arc<PeerDirectory> {
        self.directory.load_full()
    }

    /// The pin set both transports must be built with
    /// ([`PeerClient::new_with_pin_provider`],
    /// [`PeerServer::new_with_pin_provider`]). A reload replaces it, which
    /// makes each transport check new handshakes against the new pins and
    /// close established connections whose pin has gone.
    pub fn pins(&self) -> SharedPeerPins {
        self.pins.clone()
    }

    /// The largest membership a reload may install.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Authorize an incoming peer against the membership in force now. Use
    /// this as the peer listener's request policy.
    pub fn accepts(&self, identity: &TlsIdentity) -> bool {
        self.directory.load().accepts(identity)
    }

    /// The pin of the member `route` names, if the current membership has
    /// that exact member.
    pub(crate) fn route_pin(&self, route: &VolumeRoute) -> Option<SpkiSha256> {
        self.directory.load().resolve(route).map(|peer| peer.pin)
    }

    /// Check `candidate` against the reload rules without installing it.
    pub fn check(&self, candidate: &PeerDirectory) -> Result<MembershipChange, MembershipError> {
        self.check_against(&self.directory.load(), candidate)
    }

    /// [`Self::check`], against `current` as the membership in use.
    fn check_against(
        &self,
        current: &PeerDirectory,
        candidate: &PeerDirectory,
    ) -> Result<MembershipChange, MembershipError> {
        if candidate.len() > self.capacity {
            return Err(MembershipError::OverCapacity {
                members: candidate.len(),
                capacity: self.capacity,
            });
        }
        if let Some(local) = &self.local {
            let node = || local.identity.role_id().to_owned();
            let Some(record) = candidate.authenticated_member(&local.identity) else {
                return Err(MembershipError::LocalMemberMissing { node: node() });
            };
            if let Some(listener) = &local.listener {
                let field = if record.boot != listener.boot {
                    Some("boot")
                } else if record.address != listener.address {
                    Some("address")
                } else if record.tls_server_name != listener.tls_server_name {
                    Some("tls_server_name")
                } else {
                    None
                };
                if let Some(field) = field {
                    return Err(MembershipError::LocalMemberChanged {
                        node: node(),
                        field,
                    });
                }
            }
        }
        let mut change = MembershipChange::default();
        for (node, old) in current.members() {
            match candidate.member(node) {
                Some(new) if new == old => continue,
                Some(_) => change.changed.push(node.to_owned()),
                None => change.removed.push(node.to_owned()),
            }
            change.retired.push(PeerDestination::new(
                old.address,
                old.tls_server_name.clone(),
            ));
        }
        for (node, _) in candidate.members() {
            if current.member(node).is_none() {
                change.added.push(node.to_owned());
            }
        }
        Ok(change)
    }

    /// Install `candidate` if it passes [`Self::check`]; otherwise leave the
    /// membership as it was. Returns what changed.
    ///
    /// The directory is replaced before the pins. A reader that sees the new
    /// directory with the old pins applies both, so in that instant it can
    /// only refuse a peer the new membership would admit, never admit one
    /// either membership refuses.
    ///
    /// Reloads take no turns. Each replaces the directory only if it is
    /// still the one its change was worked out against, working it out
    /// again if not, so every reload reports what it really replaced. Each
    /// then sets the pins from whichever directory is installed, again if
    /// the directory moved while it did. Whichever reload sets the pins
    /// last therefore set them from the directory that stays, and the two
    /// agree once the reloads have returned. While they overlap a reader
    /// can see one reload's directory with another's pins, which is the
    /// case above: it applies both.
    ///
    /// This closes inbound and outbound connections whose pin left the pin
    /// set (the transports watch it). Connections pooled to a retired
    /// address whose pin is still listed are closed by
    /// [`crate::IngressForwarder::reload_membership`], which is the entry
    /// point a server with a forwarder should use.
    pub fn apply(&self, candidate: PeerDirectory) -> Result<MembershipChange, MembershipError> {
        let candidate = Arc::new(candidate);
        let change = loop {
            let current = self.directory.load_full();
            let change = self.check_against(&current, &candidate)?;
            if change.is_empty() {
                return Ok(change);
            }
            // Refused here, before anything changes, if the pins are
            // unusable.
            approved_pins(&candidate)?;
            let previous = self
                .directory
                .compare_and_swap(&current, Arc::clone(&candidate));
            if Arc::ptr_eq(&previous, &current) {
                break change;
            }
        };
        loop {
            let installed = self.directory.load_full();
            // Cannot fail: every installed directory passed the check
            // above, and its pin count is bounded by the membership size,
            // which is below the transport's pin limit.
            self.pins
                .replace_approved(approved_pins(&installed)?)
                .map_err(|error| MembershipError::Pins(error.to_string()))?;
            if Arc::ptr_eq(&installed, &self.directory.load()) {
                return Ok(change);
            }
        }
    }

    /// Parse a membership file and [`Self::apply`] it.
    pub fn reload_from_reader(
        &self,
        reader: impl std::io::Read,
    ) -> Result<MembershipChange, MembershipError> {
        self.apply(PeerDirectory::parse(reader)?)
    }
}

fn approved_pins(directory: &PeerDirectory) -> Result<ApprovedPeerPins, MembershipError> {
    ApprovedPeerPins::new(directory.members().map(|(_, peer)| peer.pin))
        .map_err(|error| MembershipError::Pins(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{
        BasicConstraints, Certificate, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose, SanType,
    };

    struct Authority {
        certificate: Certificate,
        key: KeyPair,
    }
    fn authority() -> Authority {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        Authority {
            certificate: params.self_signed(&key).unwrap(),
            key,
        }
    }
    /// A peer certificate for `node`, as the transport would present it.
    fn identity(ca: &Authority, node: &str) -> TlsIdentity {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec!["localhost".into()]).unwrap();
        params.subject_alt_names.push(SanType::URI(
            format!("urn:agent-tunnel:peer:{node}").try_into().unwrap(),
        ));
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ClientAuth,
            ExtendedKeyUsagePurpose::ServerAuth,
        ];
        let certificate = params.signed_by(&key, &ca.certificate, &ca.key).unwrap();
        StagedPeerIdentity::from_pem(
            certificate.pem().as_bytes(),
            key.serialize_pem().as_bytes(),
            ca.certificate.pem().as_bytes(),
        )
        .unwrap()
        .identity()
        .clone()
    }
    fn record(node: &str, port: u16, pin: SpkiSha256) -> TrustedPeer {
        TrustedPeer {
            node: node.into(),
            boot: "boot-a".into(),
            address: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            tls_server_name: "localhost".into(),
            pin,
        }
    }
    fn other(node: &str, port: u16) -> TrustedPeer {
        record(node, port, SpkiSha256::from_bytes([port as u8; 32]))
    }
    fn directory(peers: Vec<TrustedPeer>) -> PeerDirectory {
        PeerDirectory::validate(peers).unwrap()
    }
    fn membership(peers: Vec<TrustedPeer>, capacity: usize) -> PeerMembership {
        PeerMembership::new(Arc::new(directory(peers)), capacity).unwrap()
    }
    fn nodes(membership: &PeerMembership) -> Vec<String> {
        membership
            .directory()
            .members()
            .map(|(node, _)| node.to_owned())
            .collect()
    }

    #[test]
    fn a_reload_reports_what_changed_and_replaces_directory_and_pins_together() {
        let membership = membership(vec![other("a", 1), other("b", 2), other("c", 3)], 8);
        let before = membership.pins().snapshot();
        let mut moved = other("b", 2);
        moved.address.set_port(20);
        let change = membership
            .apply(directory(vec![other("a", 1), moved, other("d", 4)]))
            .unwrap();
        assert_eq!(change.added, ["d"]);
        assert_eq!(change.removed, ["c"]);
        assert_eq!(change.changed, ["b"]);
        // The removed and the moved record's old addresses are retired.
        assert_eq!(
            change.retired,
            [
                PeerDestination::new(other("b", 2).address, "localhost"),
                PeerDestination::new(other("c", 3).address, "localhost"),
            ]
        );
        assert_eq!(nodes(&membership), ["a", "b", "d"]);
        let after = membership.pins().snapshot();
        assert!(after.revision() > before.revision());
        assert!(after.contains(other("d", 4).pin));
        assert!(!after.contains(other("c", 3).pin));
    }

    /// Reloads from several threads at once, each of a different file:
    /// when they have returned, the pins are those of the directory that
    /// stayed, whichever it was, and each reload reported a real change.
    #[test]
    fn reloads_at_once_leave_the_pins_of_the_directory_that_stayed() {
        const THREADS: u16 = 8;
        const ROUNDS: u16 = 200;
        let membership = membership(vec![other("a", 1)], 8);
        for round in 0..ROUNDS {
            std::thread::scope(|scope| {
                for thread in 0..THREADS {
                    let membership = &membership;
                    scope.spawn(move || {
                        // A member no other reload lists, and one that
                        // every reload lists, under one of three pins.
                        let own = format!("n{thread}-{round}");
                        let candidate = directory(vec![
                            other("a", 1),
                            other(&own, 10 + thread),
                            other("shared", 100 + ((round + thread) % 3)),
                        ]);
                        let change = membership.apply(candidate).unwrap();
                        assert!(change.added.contains(&own), "{change:?}");
                    });
                }
            });
            let directory = membership.directory();
            let pins = membership.pins().snapshot();
            assert_eq!(directory.len(), 3, "round {round}");
            assert_eq!(pins.len(), 3, "round {round}");
            for (node, peer) in directory.members() {
                assert!(pins.contains(peer.pin), "round {round}: {node}");
            }
        }
    }

    #[test]
    fn a_reload_of_the_same_membership_changes_nothing() {
        let membership = membership(vec![other("a", 1), other("b", 2)], 8);
        let before = membership.pins().snapshot();
        let current = membership.directory();
        let change = membership
            .apply(directory(vec![other("b", 2), other("a", 1)]))
            .unwrap();
        assert!(change.is_empty());
        // Neither the directory nor the pin revision moved, so no transport
        // was woken to re-check its connections.
        assert!(Arc::ptr_eq(&current, &membership.directory()));
        assert_eq!(membership.pins().snapshot(), before);
    }

    #[test]
    fn a_rotated_pin_is_a_change_that_retires_the_old_pin() {
        let membership = membership(vec![other("a", 1), other("b", 2)], 8);
        let rotated = record("b", 2, SpkiSha256::from_bytes([0xbb; 32]));
        let change = membership
            .apply(directory(vec![other("a", 1), rotated.clone()]))
            .unwrap();
        assert_eq!(change.changed, ["b"]);
        assert!(change.added.is_empty() && change.removed.is_empty());
        let pins = membership.pins().snapshot();
        assert!(pins.contains(rotated.pin));
        assert!(!pins.contains(other("b", 2).pin));
    }

    #[test]
    fn a_reload_larger_than_the_startup_capacity_is_refused() {
        let membership = membership(vec![other("a", 1), other("b", 2)], 2);
        let error = membership
            .apply(directory(vec![other("a", 1), other("b", 2), other("c", 3)]))
            .unwrap_err();
        assert_eq!(
            error,
            MembershipError::OverCapacity {
                members: 3,
                capacity: 2
            }
        );
        assert_eq!(nodes(&membership), ["a", "b"]);
        // Replacing a member within the capacity is still allowed.
        membership
            .apply(directory(vec![other("a", 1), other("c", 3)]))
            .unwrap();
        assert!(
            PeerMembership::new(Arc::new(directory(vec![other("a", 1), other("b", 2)])), 1)
                .is_err()
        );
        assert!(
            PeerMembership::new(
                Arc::new(directory(vec![other("a", 1)])),
                MAX_PEER_DIRECTORY_MEMBERS + 1
            )
            .is_err()
        );
    }

    #[test]
    fn a_reload_that_drops_or_rekeys_the_local_member_is_refused() {
        let ca = authority();
        let local = identity(&ca, "self");
        let own = record("self", 9, local.spki_sha256());
        let membership = membership(vec![own.clone(), other("a", 1)], 8)
            .with_local_identity(local.clone())
            .unwrap();
        let before = membership.pins().snapshot();
        let missing = MembershipError::LocalMemberMissing {
            node: "self".into(),
        };
        // Removed outright.
        assert_eq!(
            membership.apply(directory(vec![other("a", 1)])),
            Err(missing.clone())
        );
        // Still listed, but under a pin this server's key does not match.
        assert_eq!(
            membership.apply(directory(vec![other("self", 9), other("a", 1)])),
            Err(missing.clone())
        );
        // This server's pin listed under another node's name.
        assert_eq!(
            membership.apply(directory(vec![
                record("someone-else", 9, local.spki_sha256()),
                other("a", 1)
            ])),
            Err(missing)
        );
        assert_eq!(nodes(&membership), ["a", "self"]);
        assert_eq!(membership.pins().snapshot(), before);
        // A server that only dials out may have its address changed.
        let mut moved = own.clone();
        moved.address.set_port(99);
        moved.boot = "boot-b".into();
        let change = membership
            .apply(directory(vec![moved, other("b", 2)]))
            .unwrap();
        assert_eq!(change.changed, ["self"]);
        assert_eq!(nodes(&membership), ["b", "self"]);
    }

    #[test]
    fn a_listening_member_also_refuses_a_change_to_its_published_record() {
        let ca = authority();
        let local = identity(&ca, "self");
        let own = record("self", 9, local.spki_sha256());
        let membership = membership(vec![own.clone(), other("a", 1)], 8)
            .with_local_listener(local)
            .unwrap();
        for (field, edit) in [
            (
                "boot",
                (|peer| peer.boot = "boot-b".into()) as fn(&mut TrustedPeer),
            ),
            ("address", |peer| peer.address.set_port(99)),
            ("tls_server_name", |peer| {
                peer.tls_server_name = "other.example".into()
            }),
        ] {
            let mut changed = own.clone();
            edit(&mut changed);
            assert_eq!(
                membership.apply(directory(vec![changed, other("a", 1)])),
                Err(MembershipError::LocalMemberChanged {
                    node: "self".into(),
                    field
                })
            );
        }
        assert_eq!(nodes(&membership), ["a", "self"]);
        // The rest of the membership may change around it.
        let change = membership
            .apply(directory(vec![own, other("b", 2)]))
            .unwrap();
        assert_eq!(
            (change.added, change.removed),
            (vec!["b".into()], vec!["a".into()])
        );
    }

    #[test]
    fn a_local_identity_absent_at_startup_is_refused() {
        let ca = authority();
        let local = identity(&ca, "self");
        assert!(matches!(
            membership(vec![other("a", 1)], 8).with_local_identity(local.clone()),
            Err(MembershipError::LocalMemberMissing { .. })
        ));
        assert!(matches!(
            membership(vec![other("self", 9)], 8).with_local_listener(local),
            Err(MembershipError::LocalMemberMissing { .. })
        ));
    }

    #[test]
    fn an_invalid_file_is_refused_with_its_reason_and_changes_nothing() {
        let membership = membership(vec![other("a", 1), other("b", 2)], 8);
        let before = membership.pins().snapshot();
        let valid = serde_json::json!({
            "node":"c", "boot":"boot-a", "address":"127.0.0.1:443",
            "tls_server_name":"localhost", "spki_sha256":"1a".repeat(32)
        });
        let edited = |field: &str, value: serde_json::Value| {
            let mut record = valid.clone();
            record[field] = value;
            serde_json::json!([record]).to_string()
        };
        let reload = |text: &str| membership.reload_from_reader(text.as_bytes()).unwrap_err();
        // A file cut short while it was being written.
        assert!(matches!(
            reload("[{\"node\":\"c\","),
            MembershipError::Parse(_)
        ));
        assert!(matches!(reload(""), MembershipError::Parse(_)));
        assert!(matches!(reload("{}"), MembershipError::Parse(_)));
        assert!(matches!(
            reload(&edited("spki_sha256", "abc".into())),
            MembershipError::Parse(_)
        ));
        let mut extra = valid.clone();
        extra["tls_name"] = "ignored".into();
        let unknown = reload(&serde_json::json!([extra]).to_string());
        assert!(unknown.to_string().contains("tls_name"), "{unknown}");
        // An empty membership would refuse every peer.
        assert_eq!(reload("[]"), MembershipError::Empty);
        for (field, value) in [
            ("node", serde_json::json!("")),
            ("boot", serde_json::json!("has space")),
            ("tls_server_name", serde_json::json!("")),
            ("address", serde_json::json!("127.0.0.1:0")),
        ] {
            assert_eq!(
                reload(&edited(field, value)),
                MembershipError::InvalidRecord { index: 0, field }
            );
        }
        assert_eq!(
            reload(&serde_json::json!([valid, valid]).to_string()),
            MembershipError::DuplicateNode {
                index: 1,
                node: "c".into()
            }
        );
        assert_eq!(nodes(&membership), ["a", "b"]);
        assert_eq!(membership.pins().snapshot(), before);
        // The same reader accepts a good file afterwards.
        let change = membership
            .reload_from_reader(serde_json::json!([valid]).to_string().as_bytes())
            .unwrap();
        assert_eq!(change.added, ["c"]);
        assert_eq!(nodes(&membership), ["c"]);
    }
}
