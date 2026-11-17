//! The version of the server-to-server protocol, and the rule for refusing
//! a peer that speaks another one.
//!
//! "The protocol" is everything one server of a cluster reads that another
//! wrote: a forwarded request and its response (the `x-mount-*` headers of
//! the peer hop and what they mean), and the route record in Redis.
//!
//! The rule has two halves, and neither guesses:
//!
//! * An owner publishes, in its route record, the range of versions it
//!   accepts. A forwarding server picks the highest version both speak and
//!   says so on every forwarded request ([`PEER_PROTOCOL_HEADER`]). If there
//!   is none, it sends nothing and answers its client `503` with
//!   `x-mount-dispatch: not-dispatched`.
//! * An owner reads that header before anything else. A version outside
//!   [`PeerProtocolRange::SUPPORTED`], or a header it cannot read, is
//!   answered `426` with `x-mount-dispatch: not-dispatched` and the request
//!   is not run; the forwarding server turns that into the same `503`.
//!
//! A request with no header, and a record with no range, are version 1:
//! that is what every release before the marker sent. Version 1 therefore
//! puts nothing new in the record, so those releases still read it.
//!
//! Changing the version: raise [`PEER_PROTOCOL_VERSION`] when a server
//! starts sending something an older owner would misread, and keep
//! [`MIN_PEER_PROTOCOL_VERSION`] one behind for a release, sending the old
//! form to an owner whose record does not accept the new one. That is what
//! lets a cluster be upgraded one server at a time between adjacent
//! releases.
use serde::{Deserialize, Serialize};

/// The newest protocol version this build speaks.
pub const PEER_PROTOCOL_VERSION: u32 = 1;
/// The oldest protocol version this build still speaks.
pub const MIN_PEER_PROTOCOL_VERSION: u32 = 1;
/// Request header of the peer hop: the version the request is written in.
/// Absent means version 1.
pub const PEER_PROTOCOL_HEADER: &str = "x-mount-peer-protocol";

/// The protocol versions a server accepts, both ends included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerProtocolRange {
    pub min: u32,
    pub max: u32,
}
impl PeerProtocolRange {
    /// What a release from before the marker speaks, and what an absent
    /// range means.
    pub const V1: Self = Self { min: 1, max: 1 };
    /// What this build speaks.
    pub const SUPPORTED: Self = Self {
        min: MIN_PEER_PROTOCOL_VERSION,
        max: PEER_PROTOCOL_VERSION,
    };
    pub(crate) fn is_v1(&self) -> bool {
        *self == Self::V1
    }
    pub(crate) fn valid(&self) -> bool {
        self.min >= 1 && self.min <= self.max
    }
    pub fn contains(&self, version: u32) -> bool {
        (self.min..=self.max).contains(&version)
    }
    /// The highest version in both ranges, if they overlap.
    pub fn common(&self, other: &Self) -> Option<u32> {
        let version = self.max.min(other.max);
        (self.contains(version) && other.contains(version)).then_some(version)
    }
}
impl Default for PeerProtocolRange {
    fn default() -> Self {
        Self::V1
    }
}
impl std::fmt::Display for PeerProtocolRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.min, self.max)
    }
}

/// A request that was not run, or not sent, because the two servers share
/// no protocol version. Reported to the observer the application installs;
/// nothing of it is sent to the public client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerProtocolRefusal {
    /// As an owner: `peer` sent a request in a version this server does
    /// not speak. `offered` is the header as received, shortened.
    Request { peer: String, offered: String },
    /// As a forwarder: `owner` publishes, for `volume`, a range that shares
    /// no version with this server's. Nothing was sent.
    Route {
        volume: String,
        owner: String,
        accepts: PeerProtocolRange,
    },
    /// As a forwarder: `owner` refused the version this server sent for
    /// `volume`. The request was not run.
    Refused { volume: String, owner: String },
}

/// Called for every refusal, on the request's own task: it must not block.
pub type PeerProtocolObserver = std::sync::Arc<dyn Fn(&PeerProtocolRefusal) + Send + Sync>;

/// The version a forwarded request is written in, or the header as it was
/// received if this server cannot speak it or cannot read it.
#[cfg(feature = "http3")]
pub(crate) fn request_version(headers: &http::HeaderMap) -> Result<u32, String> {
    let mut values = headers.get_all(PEER_PROTOCOL_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(1);
    };
    let shown = || String::from_utf8_lossy(&value.as_bytes()[..value.len().min(32)]).into_owned();
    if values.next().is_some() {
        return Err(format!("{} (repeated)", shown()));
    }
    value
        .to_str()
        .ok()
        // Decimal digits only: `u32::from_str` would also take a sign.
        .filter(|text| {
            !text.is_empty() && text.len() <= 10 && text.bytes().all(|b| b.is_ascii_digit())
        })
        .and_then(|text| text.parse::<u32>().ok())
        .filter(|version| PeerProtocolRange::SUPPORTED.contains(*version))
        .ok_or_else(shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(min: u32, max: u32) -> PeerProtocolRange {
        PeerProtocolRange { min, max }
    }

    #[test]
    fn the_highest_shared_version_is_chosen_and_disjoint_ranges_share_none() {
        assert_eq!(range(1, 1).common(&range(1, 1)), Some(1));
        assert_eq!(range(1, 2).common(&range(1, 1)), Some(1));
        assert_eq!(range(1, 1).common(&range(1, 2)), Some(1));
        assert_eq!(range(1, 3).common(&range(2, 5)), Some(3));
        assert_eq!(range(2, 5).common(&range(1, 3)), Some(3));
        assert_eq!(range(1, 1).common(&range(2, 2)), None);
        assert_eq!(range(2, 3).common(&range(1, 1)), None);
        assert!(!range(0, 1).valid());
        assert!(!range(2, 1).valid());
        assert!(PeerProtocolRange::SUPPORTED.valid());
        // Every build must still speak what it advertises by default.
        assert!(PeerProtocolRange::SUPPORTED.contains(PEER_PROTOCOL_VERSION));
        assert_eq!(PeerProtocolRange::default(), PeerProtocolRange::V1);
    }

    #[cfg(feature = "http3")]
    #[test]
    fn a_request_without_the_header_is_version_one_and_an_unreadable_one_is_refused() {
        let headers = |values: &[&str]| {
            let mut map = http::HeaderMap::new();
            for value in values {
                map.append(PEER_PROTOCOL_HEADER, value.parse().unwrap());
            }
            map
        };
        assert_eq!(request_version(&headers(&[])), Ok(1));
        assert_eq!(request_version(&headers(&["1"])), Ok(1));
        let next = (PEER_PROTOCOL_VERSION + 1).to_string();
        assert_eq!(request_version(&headers(&[&next])), Err(next.clone()));
        for refused in [
            "0",
            "",
            "+1",
            "1.0",
            "one",
            " 1x",
            "4294967296",
            "99999999999",
        ] {
            assert!(
                request_version(&headers(&[refused])).is_err(),
                "accepted {refused:?}"
            );
        }
        assert_eq!(
            request_version(&headers(&["1", "1"])),
            Err("1 (repeated)".into())
        );
        let long = "9".repeat(200);
        assert_eq!(request_version(&headers(&[&long])).unwrap_err().len(), 32);
    }
}
