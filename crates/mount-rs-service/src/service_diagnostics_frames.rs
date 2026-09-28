//! Lossless bounded frames for one already-captured service diagnostic object.
//! The original JSON bytes remain unchanged, including integer values. This
//! codec captures no bank, identity or clock. Stream consumers own capture
//! identity, duplicate-capture and measurement-completeness assertions.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ring::digest;
use serde::{
    Deserialize, Serialize,
    de::{IgnoredAny, MapAccess, Visitor},
};
use std::fmt;

/// Maximum bytes in the original JSON object, before framing.
pub const RECORD_LIMIT: usize = 1024 * 1024;
/// Maximum bytes in a physical frame, including its prefix and final LF.
pub const LINE_LIMIT: usize = 16 * 1024;
pub const PREFIX: &[u8] = b"service_diagnostics_frame ";
const PREFIX_TEXT: &str = "service_diagnostics_frame ";
const PREFIX_NAME: &str = "service_diagnostics_frame";
const SCHEMA: &str = "mount-rs.service-diagnostic-frame.v1";
const CHUNK_BYTES: usize = 8 * 1024;
const MAX_FRAMES: usize = RECORD_LIMIT / CHUNK_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    InvalidRecord,
    InvalidFrame,
    IncompleteRecord,
    Poisoned,
}

impl fmt::Display for CodecError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::InvalidRecord => "invalid service diagnostic JSON object",
            Self::InvalidFrame => "invalid service diagnostic frame",
            Self::IncompleteRecord => "incomplete service diagnostic frame group",
            Self::Poisoned => "service diagnostic decoder is poisoned",
        })
    }
}
impl std::error::Error for CodecError {}

#[derive(Serialize)]
struct OutgoingFrame<'a> {
    schema: &'static str,
    index: usize,
    count: usize,
    bytes: usize,
    digest: &'a str,
    payload_base64: &'a str,
}

#[derive(Deserialize)]
enum Schema {
    #[serde(rename = "mount-rs.service-diagnostic-frame.v1")]
    V1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IncomingFrame {
    schema: Schema,
    index: usize,
    count: usize,
    bytes: usize,
    digest: String,
    payload_base64: String,
}

fn valid_object(bytes: &[u8]) -> bool {
    if bytes.len() > RECORD_LIMIT || std::str::from_utf8(bytes).is_err() {
        return false;
    }
    struct Object;
    impl<'de> Visitor<'de> for Object {
        type Value = ();
        fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
            output.write_str("one JSON object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(())
        }
    }
    let mut parser = serde_json::Deserializer::from_slice(bytes);
    serde::Deserializer::deserialize_map(&mut parser, Object).is_ok() && parser.end().is_ok()
}

fn digest_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let hash = digest::digest(&digest::SHA256, bytes);
    let mut output = String::with_capacity(64);
    for byte in hash.as_ref() {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 15)]));
    }
    output
}

fn incoming_digest(value: &str) -> Result<[u8; 32], CodecError> {
    fn nibble(byte: u8) -> Result<u8, CodecError> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => Err(CodecError::InvalidFrame),
        }
    }
    if value.len() != 64 {
        return Err(CodecError::InvalidFrame);
    }
    let mut digest = [0; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        digest[index] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Ok(digest)
}

/// Stage the complete group before the caller writes anything to its sink.
/// Input is one logical JSON object; log prefixes belong to the outer frames.
pub fn encode(record: &[u8]) -> Result<Vec<u8>, CodecError> {
    if !valid_object(record) {
        return Err(CodecError::InvalidRecord);
    }
    let count = record.len().div_ceil(CHUNK_BYTES);
    let hash = digest_hex(record);
    let mut output = Vec::new();
    for (index, chunk) in record.chunks(CHUNK_BYTES).enumerate() {
        let payload = BASE64.encode(chunk);
        crate::startup::write_bounded::<LINE_LIMIT>(
            &mut output,
            PREFIX,
            &OutgoingFrame {
                schema: SCHEMA,
                index,
                count,
                bytes: record.len(),
                digest: &hash,
                payload_base64: &payload,
            },
        )
        .map_err(|_| CodecError::InvalidFrame)?;
    }
    Ok(output)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Metadata {
    count: usize,
    bytes: usize,
    digest: [u8; 32],
}
struct Group {
    metadata: Metadata,
    next: usize,
    payload: Vec<u8>,
}

/// One bounded group at a time. Any malformed frame permanently poisons this
/// decoder; it cannot recover by discarding that group and accepting a later one.
#[derive(Default)]
pub struct Decoder {
    group: Option<Group>,
    poisoned: bool,
}

impl Decoder {
    /// Accept a physical line, with or without its final LF. Other log lines
    /// are ignored. Framed groups cannot interleave or reorder their indices.
    /// A returned value contains the exact complete original JSON bytes.
    pub fn push_line(&mut self, line: &str) -> Result<Option<Vec<u8>>, CodecError> {
        if self.poisoned {
            return Err(CodecError::Poisoned);
        }
        let result = self.push(line);
        if result.is_err() {
            self.group = None;
            self.poisoned = true;
        }
        result
    }

    fn push(&mut self, line: &str) -> Result<Option<Vec<u8>>, CodecError> {
        let Some(body) = line.strip_prefix(PREFIX_TEXT) else {
            return if line.starts_with(PREFIX_NAME) {
                Err(CodecError::InvalidFrame)
            } else {
                Ok(None)
            };
        };
        let without_lf = line.strip_suffix('\n').unwrap_or(line);
        if without_lf.len() >= LINE_LIMIT || without_lf.contains('\n') {
            return Err(CodecError::InvalidFrame);
        }
        let frame: IncomingFrame =
            serde_json::from_str(body).map_err(|_| CodecError::InvalidFrame)?;
        let Schema::V1 = frame.schema;
        if frame.count == 0
            || frame.count > MAX_FRAMES
            || frame.bytes < 2
            || frame.bytes > RECORD_LIMIT
            || frame.count != frame.bytes.div_ceil(CHUNK_BYTES)
            || frame.index >= frame.count
        {
            return Err(CodecError::InvalidFrame);
        }
        let metadata = Metadata {
            count: frame.count,
            bytes: frame.bytes,
            digest: incoming_digest(&frame.digest)?,
        };
        let offset = frame
            .index
            .checked_mul(CHUNK_BYTES)
            .ok_or(CodecError::InvalidFrame)?;
        let expected = frame
            .bytes
            .checked_sub(offset)
            .ok_or(CodecError::InvalidFrame)?
            .min(CHUNK_BYTES);
        if frame.payload_base64.len() != expected.div_ceil(3) * 4 {
            return Err(CodecError::InvalidFrame);
        }
        // Base64's decoded-size estimate can include padding bytes. Only the
        // exact expected raw chunk is accepted or retained in the group.
        let mut chunk = [0; CHUNK_BYTES + 3];
        let decoded = BASE64
            .decode_slice(&frame.payload_base64, &mut chunk)
            .map_err(|_| CodecError::InvalidFrame)?;
        if decoded != expected {
            return Err(CodecError::InvalidFrame);
        }
        if self.group.is_none() {
            if frame.index != 0 {
                return Err(CodecError::InvalidFrame);
            }
            self.group = Some(Group {
                metadata,
                next: 0,
                payload: Vec::with_capacity(metadata.bytes),
            });
        }
        let group = self.group.as_mut().ok_or(CodecError::InvalidFrame)?;
        if group.metadata != metadata || group.next != frame.index {
            return Err(CodecError::InvalidFrame);
        }
        let length = group
            .payload
            .len()
            .checked_add(decoded)
            .ok_or(CodecError::InvalidFrame)?;
        if length > metadata.bytes {
            return Err(CodecError::InvalidFrame);
        }
        group.payload.extend_from_slice(&chunk[..decoded]);
        group.next = group.next.checked_add(1).ok_or(CodecError::InvalidFrame)?;
        if group.next != metadata.count {
            return Ok(None);
        }
        let complete = self.group.take().ok_or(CodecError::InvalidFrame)?;
        if complete.payload.len() != metadata.bytes
            || digest::digest(&digest::SHA256, &complete.payload).as_ref() != &metadata.digest[..]
            || !valid_object(&complete.payload)
        {
            return Err(CodecError::InvalidRecord);
        }
        Ok(Some(complete.payload))
    }

    /// Check EOF without treating a missing or partial group as a record.
    /// Callers independently require the captures their workload needs.
    pub fn finish(&self) -> Result<(), CodecError> {
        if self.poisoned {
            Err(CodecError::Poisoned)
        } else if self.group.is_some() {
            Err(CodecError::IncompleteRecord)
        } else {
            Ok(())
        }
    }
}

/// Decode exactly one complete LF-terminated group. More complete groups or
/// any incomplete group are rejected; use Decoder for a stream of captures.
pub fn decode(bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
    if bytes.len() > MAX_FRAMES * LINE_LIMIT || !bytes.ends_with(b"\n") {
        return Err(CodecError::InvalidFrame);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| CodecError::InvalidFrame)?;
    let mut decoder = Decoder::default();
    let mut record = None;
    for line in text.split_inclusive('\n') {
        if let Some(complete) = decoder.push_line(line)?
            && record.replace(complete).is_some()
        {
            return Err(CodecError::InvalidFrame);
        }
    }
    decoder.finish()?;
    record.ok_or(CodecError::IncompleteRecord)
}

#[cfg(test)]
#[path = "service_diagnostics_frames_tests.rs"]
mod tests;
