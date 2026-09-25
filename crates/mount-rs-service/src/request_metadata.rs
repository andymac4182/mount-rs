//! Borrowed request policy and bounded streaming audit metadata.
use crate::{catalog::CatalogSnapshot, dispatch::SessionIdentity};
use mount_rs_remote_protocol::OperationName;
use serde::{Serialize, Serializer, ser::SerializeSeq};
use serde_json::Value;
use std::io::Write;

/// Resolve a bounded catalog claim pointer without String unescaping allocations.
/// Matches serde_json pointer semantics, including literal unknown tilde escapes.
pub(crate) fn claim_pointer<'a>(claims: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer.is_empty() {
        return Some(claims);
    }
    if !pointer.starts_with('/') || pointer.len() > 512 {
        return None;
    }
    let mut current = claims;
    for token in pointer.split('/').skip(1) {
        let mut scratch = [0u8; 512];
        let key = if token.contains('~') {
            let input = token.as_bytes();
            let mut read = 0;
            let mut written = 0;
            while read < input.len() {
                if input[read] == b'~'
                    && read + 1 < input.len()
                    && matches!(input[read + 1], b'0' | b'1')
                {
                    scratch[written] = if input[read + 1] == b'0' { b'~' } else { b'/' };
                    read += 2;
                } else {
                    scratch[written] = input[read];
                    read += 1;
                }
                written += 1;
            }
            std::str::from_utf8(&scratch[..written]).ok()?
        } else {
            token
        };
        current = match current {
            Value::Object(map) => map.get(key)?,
            Value::Array(values) => {
                if key.starts_with('+') || (key.starts_with('0') && key.len() != 1) {
                    return None;
                }
                values.get(key.parse::<usize>().ok()?)?
            }
            _ => return None,
        };
    }
    Some(current)
}

pub(crate) fn policy_matches(policy: &Value, identity: &SessionIdentity) -> bool {
    let Some(object) = policy.as_object() else {
        return false;
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "issuer" | "audiences" | "algorithms"))
    {
        return false;
    }
    if object.get("issuer").and_then(Value::as_str) != Some(identity.issuer.as_str()) {
        return false;
    }
    let Some(audiences) = object.get("audiences").and_then(Value::as_array) else {
        return false;
    };
    if audiences.iter().any(|value| !value.is_string()) {
        return false;
    }
    let audience_matches = |audience: &str| {
        audiences
            .iter()
            .any(|value| value.as_str() == Some(audience))
    };
    let audience_ok = match identity.claims.get("aud") {
        Some(Value::String(value)) => audience_matches(value),
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .any(audience_matches),
        _ => false,
    };
    let algorithm_ok = match object.get("algorithms") {
        None => matches!(identity.signing_algorithm.as_str(), "RS256" | "ES256"),
        Some(Value::Array(values)) if values.iter().all(Value::is_string) => values
            .iter()
            .any(|value| value.as_str() == Some(identity.signing_algorithm.as_str())),
        _ => false,
    };
    audience_ok && algorithm_ok
}

pub(crate) struct MatchingGrants<'a> {
    pub catalog: &'a CatalogSnapshot,
    pub identity: &'a SessionIdentity,
    pub drive_id: &'a str,
}
impl Serialize for MatchingGrants<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(None)?;
        for (id, grant) in &self.catalog.grants {
            if grant.partition_id == self.identity.partition_id
                && grant.policy_id == self.identity.policy_id
                && grant.drives.contains_key(self.drive_id)
                && grant.claim_conditions.iter().all(|(pointer, expected)| {
                    claim_pointer(&self.identity.claims, pointer).and_then(Value::as_str)
                        == Some(expected.as_str())
                })
            {
                sequence.serialize_element(id)?;
            }
        }
        sequence.end()
    }
}
#[derive(Serialize)]
pub(crate) struct AuditRecord<'a> {
    pub event: &'static str,
    pub partition_id: &'a str,
    pub drive_id: &'a str,
    pub grant_ids: MatchingGrants<'a>,
    pub operation: OperationName,
    pub request_id: u64,
    pub outcome: &'a str,
}
// One caller holds its writer lock for a whole event. Stack buffering bounds
// serialization fragments without an owned JSON tree, grant vector or String.
struct Buffered<'a, W> {
    writer: &'a mut W,
    bytes: [u8; 4096],
    len: usize,
}
impl<W: Write> Write for Buffered<'_, W> {
    fn write(&mut self, mut input: &[u8]) -> std::io::Result<usize> {
        let length = input.len();
        while !input.is_empty() {
            let count = input.len().min(self.bytes.len() - self.len);
            self.bytes[self.len..self.len + count].copy_from_slice(&input[..count]);
            self.len += count;
            input = &input[count..];
            if self.len == self.bytes.len() {
                self.flush()?;
            }
        }
        Ok(length)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.write_all(&self.bytes[..self.len])?;
        self.len = 0;
        Ok(())
    }
}
pub(crate) fn write_audit<W: Write>(
    writer: &mut W,
    record: &AuditRecord<'_>,
) -> Result<(), serde_json::Error> {
    let mut buffered = Buffered {
        writer,
        bytes: [0; 4096],
        len: 0,
    };
    serde_json::to_writer(&mut buffered, record)?;
    buffered.write_all(b"\n").map_err(serde_json::Error::io)?;
    buffered.flush().map_err(serde_json::Error::io)
}
