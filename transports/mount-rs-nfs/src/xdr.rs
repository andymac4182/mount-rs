//! Bounds-checked XDR (RFC 4506) primitives used by RPC and NFSv3.

use std::fmt;

pub const XDR_MAX_ITEM: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XdrError {
    pub message: String,
    pub offset: usize,
}

impl XdrError {
    pub fn new(message: impl Into<String>, offset: usize) -> Self {
        Self {
            message: message.into(),
            offset,
        }
    }
}

impl fmt::Display for XdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

impl std::error::Error for XdrError {}

pub fn xdr_pad(length: usize) -> usize {
    (4 - (length % 4)) % 4
}

pub fn xdr_align(length: usize) -> usize {
    length
        .checked_add(xdr_pad(length))
        .expect("XDR length overflow")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpaqueFrame {
    OverLimit,
    Truncated { span: usize },
    Complete { span: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpaqueWireFrame {
    MissingPrefix,
    OverLimit { length: usize },
    Truncated { length: usize, span: usize },
    Complete { length: usize, span: usize },
}

fn opaque_frame(length: usize, max: usize, remaining: usize) -> OpaqueFrame {
    if length > max || length > XDR_MAX_ITEM {
        return OpaqueFrame::OverLimit;
    }

    let span = xdr_align(length);
    if span > remaining {
        OpaqueFrame::Truncated { span }
    } else {
        OpaqueFrame::Complete { span }
    }
}

fn opaque_wire_frame(bytes: &[u8], max: usize) -> OpaqueWireFrame {
    if bytes.len() < 4 {
        return OpaqueWireFrame::MissingPrefix;
    }

    let length = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    match opaque_frame(length, max, bytes.len() - 4) {
        OpaqueFrame::OverLimit => OpaqueWireFrame::OverLimit { length },
        OpaqueFrame::Truncated { span } => OpaqueWireFrame::Truncated { length, span },
        OpaqueFrame::Complete { span } => OpaqueWireFrame::Complete { length, span },
    }
}

#[derive(Clone, Copy)]
pub struct XdrReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> XdrReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    pub fn at_end(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn need(&mut self, count: usize, what: &str) -> Result<usize, XdrError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or_else(|| XdrError::new(format!("{what} length overflow"), self.offset))?;
        if end > self.bytes.len() {
            return Err(XdrError::new(
                format!(
                    "truncated {what}: need {count} bytes, {} left",
                    self.remaining()
                ),
                self.offset,
            ));
        }
        let start = self.offset;
        self.offset = end;
        Ok(start)
    }

    pub fn u32(&mut self, what: &str) -> Result<u32, XdrError> {
        let at = self.need(4, what)?;
        Ok(u32::from_be_bytes(
            self.bytes[at..at + 4].try_into().unwrap(),
        ))
    }

    pub fn i32(&mut self, what: &str) -> Result<i32, XdrError> {
        Ok(self.u32(what)? as i32)
    }

    pub fn u64(&mut self, what: &str) -> Result<u64, XdrError> {
        let at = self.need(8, what)?;
        Ok(u64::from_be_bytes(
            self.bytes[at..at + 8].try_into().unwrap(),
        ))
    }

    pub fn i64(&mut self, what: &str) -> Result<i64, XdrError> {
        Ok(self.u64(what)? as i64)
    }

    pub fn bool(&mut self, what: &str) -> Result<bool, XdrError> {
        match self.u32(what)? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(XdrError::new(
                format!("invalid {what}: {value} is not 0 or 1"),
                self.offset.saturating_sub(4),
            )),
        }
    }

    pub fn fixed_opaque(&mut self, length: usize, what: &str) -> Result<Vec<u8>, XdrError> {
        let span = xdr_align(length);
        let at = self.need(span, what)?;
        Ok(self.bytes[at..at + length].to_vec())
    }

    fn read_validated_opaque(&mut self, length: usize, span: usize) -> Vec<u8> {
        let at = self.offset;
        self.offset += span;
        self.bytes[at..at + length].to_vec()
    }

    pub fn var_opaque(&mut self, max: usize, what: &str) -> Result<Vec<u8>, XdrError> {
        let frame = opaque_wire_frame(&self.bytes[self.offset..], max);
        if frame == OpaqueWireFrame::MissingPrefix {
            return Err(XdrError::new(
                format!(
                    "truncated {what} length: need 4 bytes, {} left",
                    self.remaining()
                ),
                self.offset,
            ));
        }

        let prefix_offset = self.offset;
        self.offset += 4;
        let (length, span) = match frame {
            OpaqueWireFrame::OverLimit { length } => {
                return Err(XdrError::new(
                    format!("{what} is {length} bytes, over the {max}-byte limit"),
                    prefix_offset,
                ));
            }
            OpaqueWireFrame::Truncated { span, .. } => {
                return Err(XdrError::new(
                    format!(
                        "truncated {what}: need {span} bytes, {} left",
                        self.remaining()
                    ),
                    self.offset,
                ));
            }
            OpaqueWireFrame::Complete { length, span } => (length, span),
            OpaqueWireFrame::MissingPrefix => unreachable!(),
        };
        Ok(self.read_validated_opaque(length, span))
    }

    pub fn string(&mut self, max: usize, what: &str) -> Result<String, XdrError> {
        let bytes = self.var_opaque(max, what)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn optional<T>(
        &mut self,
        what: &str,
        read: impl FnOnce(&mut Self) -> Result<T, XdrError>,
    ) -> Result<Option<T>, XdrError> {
        if self.bool(&format!("{what} present"))? {
            read(self).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn array<T>(
        &mut self,
        max: usize,
        what: &str,
        mut read: impl FnMut(&mut Self) -> Result<T, XdrError>,
    ) -> Result<Vec<T>, XdrError> {
        let count = self.u32(&format!("{what} count"))? as usize;
        if count > max || count > self.remaining() / 4 {
            return Err(XdrError::new(
                format!("{what} claims {count} items, which cannot fit"),
                self.offset.saturating_sub(4),
            ));
        }
        let mut result = Vec::with_capacity(count);
        for _ in 0..count {
            result.push(read(self)?);
        }
        Ok(result)
    }

    pub fn list<T>(
        &mut self,
        max: usize,
        what: &str,
        mut read: impl FnMut(&mut Self) -> Result<T, XdrError>,
    ) -> Result<Vec<T>, XdrError> {
        let mut result = Vec::new();
        while self.bool(&format!("{what} next"))? {
            if result.len() >= max {
                return Err(XdrError::new(
                    format!("{what} is longer than {max} items"),
                    self.offset,
                ));
            }
            result.push(read(self)?);
        }
        Ok(result)
    }

    pub fn rest(&mut self) -> Vec<u8> {
        let result = self.bytes[self.offset..].to_vec();
        self.offset = self.bytes.len();
        result
    }

    pub fn end(&self, what: &str) -> Result<(), XdrError> {
        if self.remaining() != 0 {
            Err(XdrError::new(
                format!("{what} has {} trailing bytes", self.remaining()),
                self.offset,
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct XdrWriter {
    bytes: Vec<u8>,
}

impl XdrWriter {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity.max(16)),
        }
    }

    pub fn new() -> Self {
        Self::with_capacity(512)
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn ensure(&mut self, additional: usize) {
        self.bytes.reserve(additional);
    }

    pub fn truncate(&mut self, length: usize) -> Result<(), XdrError> {
        if length > self.bytes.len() {
            return Err(XdrError::new(
                format!(
                    "cannot truncate to {length} bytes of {} written",
                    self.bytes.len()
                ),
                self.bytes.len(),
            ));
        }
        self.bytes.truncate(length);
        Ok(())
    }

    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn i32(&mut self, value: i32) {
        self.u32(value as u32);
    }

    pub fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn i64(&mut self, value: i64) {
        self.u64(value as u64);
    }

    pub fn bool(&mut self, value: bool) {
        self.u32(u32::from(value));
    }

    pub fn fixed_opaque(&mut self, value: &[u8], length: usize) {
        let span = xdr_align(length);
        let start = self.bytes.len();
        self.bytes.resize(start + span, 0);
        let copied = value.len().min(length);
        self.bytes[start..start + copied].copy_from_slice(&value[..copied]);
    }

    pub fn var_opaque(&mut self, value: &[u8]) {
        self.u32(value.len() as u32);
        self.fixed_opaque(value, value.len());
    }

    pub fn string(&mut self, value: &str) {
        self.var_opaque(value.as_bytes());
    }

    pub fn optional<T>(&mut self, value: Option<&T>, write: impl FnOnce(&mut Self, &T)) {
        self.bool(value.is_some());
        if let Some(value) = value {
            write(self, value);
        }
    }

    pub fn array<T>(&mut self, values: &[T], mut write: impl FnMut(&mut Self, &T)) {
        self.u32(values.len() as u32);
        for value in values {
            write(self, value);
        }
    }

    pub fn list<T>(&mut self, values: &[T], mut write: impl FnMut(&mut Self, &T)) {
        for value in values {
            self.bool(true);
            write(self, value);
        }
        self.bool(false);
    }

    pub fn raw(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

pub fn encode_xdr(write: impl FnOnce(&mut XdrWriter)) -> Vec<u8> {
    let mut writer = XdrWriter::new();
    write(&mut writer);
    writer.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_endian_scalars_and_padding_round_trip() {
        let mut writer = XdrWriter::new();
        writer.u32(0x0102_0304);
        writer.u64(0x1122_3344_5566_7788);
        writer.var_opaque(&[1, 2, 3]);
        let bytes = writer.into_bytes();
        assert_eq!(
            bytes,
            vec![
                1, 2, 3, 4, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0, 0, 0, 3, 1, 2, 3, 0,
            ]
        );
        let mut reader = XdrReader::new(&bytes);
        assert_eq!(reader.u32("u32").unwrap(), 0x0102_0304);
        assert_eq!(reader.u64("u64").unwrap(), 0x1122_3344_5566_7788);
        assert_eq!(reader.var_opaque(16, "opaque").unwrap(), vec![1, 2, 3]);
        reader.end("message").unwrap();
    }

    #[test]
    fn malformed_bool_and_truncation_are_xdr_errors() {
        let mut reader = XdrReader::new(&[0, 0, 0, 2]);
        assert!(reader.bool("bool").is_err());
        let mut reader = XdrReader::new(&[0, 0]);
        assert!(reader.u32("u32").is_err());
    }

    #[test]
    fn var_opaque_requires_the_length_payload_and_padding() {
        for prefix_size in 0..4 {
            let mut reader = XdrReader::new(&[0, 0, 0][..prefix_size]);
            assert_eq!(reader.var_opaque(8, "opaque").unwrap_err().offset, 0);
            assert_eq!(reader.offset(), 0);
        }

        for length in 0..=8usize {
            let mut encoded = (length as u32).to_be_bytes().to_vec();
            encoded.extend((0..length).map(|byte| byte as u8 + 1));
            encoded.extend(std::iter::repeat_n(0xa5, xdr_pad(length)));

            for size in 4..encoded.len() {
                let mut reader = XdrReader::new(&encoded[..size]);
                assert_eq!(reader.var_opaque(8, "opaque").unwrap_err().offset, 4);
                assert_eq!(reader.offset(), 4);
            }

            let mut reader = XdrReader::new(&encoded);
            let payload = reader.var_opaque(8, "opaque").unwrap();
            assert_eq!(payload, encoded[4..4 + length]);
            assert_eq!(reader.offset(), encoded.len());
            assert!(reader.at_end());
        }
    }

    #[test]
    fn var_opaque_limit_rejects_before_consuming_payload() {
        let bytes = [0, 0, 0, 5, 1, 2, 3, 4, 5, 0, 0, 0];
        for max in 0..5 {
            let mut reader = XdrReader::new(&bytes);
            assert_eq!(reader.var_opaque(max, "opaque").unwrap_err().offset, 0);
            assert_eq!(reader.offset(), 4);
        }

        let mut reader = XdrReader::new(&[1, 0, 0, 1]);
        assert_eq!(
            reader.var_opaque(usize::MAX, "opaque").unwrap_err().offset,
            0
        );
        assert_eq!(reader.offset(), 4);
    }

    #[test]
    fn var_opaque_short_wire_reports_exact_offsets_for_all_small_lengths_and_limits() {
        for length in 0..=4u8 {
            let bytes = [0, 0, 0, length, 0x11, 0x22, 0x33, 0xa5];
            for max in 0..=4usize {
                for wire_size in 0..=bytes.len() {
                    for start_offset in [0, 4] {
                        let mut input = vec![0xa5; start_offset];
                        input.extend_from_slice(&bytes[..wire_size]);
                        let mut reader = XdrReader::new(&input);
                        if start_offset == 4 {
                            reader.u32("preceding field").unwrap();
                        }
                        let actual = reader.var_opaque(max, "opaque");
                        let span = if length == 0 { 0 } else { 4 };

                        if wire_size < 4 {
                            let error = actual.unwrap_err();
                            assert_eq!(error.offset, start_offset);
                            assert_eq!(
                                error.message,
                                format!("truncated opaque length: need 4 bytes, {wire_size} left")
                            );
                            assert_eq!(reader.offset(), start_offset);
                        } else if usize::from(length) > max {
                            let error = actual.unwrap_err();
                            assert_eq!(error.offset, start_offset);
                            assert_eq!(
                                error.message,
                                format!("opaque is {length} bytes, over the {max}-byte limit")
                            );
                            assert_eq!(reader.offset(), start_offset + 4);
                        } else if wire_size - 4 < span {
                            let error = actual.unwrap_err();
                            assert_eq!(error.offset, start_offset + 4);
                            assert_eq!(
                                error.message,
                                format!(
                                    "truncated opaque: need {span} bytes, {} left",
                                    wire_size - 4
                                )
                            );
                            assert_eq!(reader.offset(), start_offset + 4);
                        } else {
                            assert_eq!(actual.unwrap(), bytes[4..4 + usize::from(length)]);
                            assert_eq!(reader.offset(), start_offset + 4 + span);
                            assert_eq!(reader.remaining(), wire_size - (4 + span));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(kani)]
mod verification {
    use super::*;

    /// Checks all decoded u32 lengths and caller limits; the finite wire tail
    /// includes enough room to distinguish missing payload from missing pad.
    #[kani::proof]
    fn var_opaque_frame_bounds() {
        let length = kani::any::<u32>() as usize;
        let max: usize = kani::any();
        let remaining: usize = kani::any();
        kani::assume(remaining <= 12);

        match opaque_frame(length, max, remaining) {
            OpaqueFrame::OverLimit => {
                assert!(length > max || length > XDR_MAX_ITEM);
                kani::cover!(length == 5 && max == 4);
                kani::cover!(length == XDR_MAX_ITEM + 1 && max == usize::MAX);
            }
            OpaqueFrame::Truncated { span } => {
                assert!(length <= max && length <= XDR_MAX_ITEM);
                assert_eq!(span, length + xdr_pad(length));
                assert!(span > remaining);
                kani::cover!(length == 3 && remaining == 2);
                kani::cover!(length == 3 && remaining == 3);
            }
            OpaqueFrame::Complete { span } => {
                assert!(length <= max && length <= XDR_MAX_ITEM);
                assert_eq!(span, length + xdr_pad(length));
                assert!(span <= remaining);
                kani::cover!(length == 4 && remaining == 4);
            }
        }
    }

    /// Proves the payload extraction called after the frame check. The wire
    /// prefix was already consumed, so the reader starts at byte four.
    #[kani::proof]
    #[kani::unwind(10)]
    fn var_opaque_short_wire_payload() {
        let bytes: [u8; 12] = kani::any();
        let wire_size: usize = kani::any();
        kani::assume(wire_size >= 4 && wire_size <= bytes.len());
        let length: usize = kani::any();
        kani::assume(length <= 8);
        let span = length + xdr_pad(length);
        kani::assume(4 + span <= wire_size);

        let wire = &bytes[..wire_size];
        let mut reader = XdrReader::new(wire);
        reader.offset = 4;
        let payload = reader.read_validated_opaque(length, span);
        assert_eq!(payload.as_slice(), &wire[4..4 + length]);
        assert_eq!(reader.offset(), 4 + span);
        assert_eq!(reader.remaining(), wire_size - (4 + span));
        kani::cover!(length == 4 && wire_size == 8);
        kani::cover!(length == 3 && wire_size == 8 && bytes[7] != 0);
    }

    /// Proves the production wire decision on every short prefix/limit/frame
    /// combination. Public-reader tests check the error offsets and extraction
    /// that this allocation-free decision selects.
    #[kani::proof]
    #[kani::unwind(6)]
    fn var_opaque_short_wire_decision() {
        let length: u8 = kani::any();
        kani::assume(length <= 4);
        let max: u8 = kani::any();
        kani::assume(max <= 4);
        let wire_size: u8 = kani::any();
        kani::assume(wire_size <= 8);
        let bytes = [
            0u8,
            0,
            0,
            length,
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any(),
        ];
        let wire = &bytes[..usize::from(wire_size)];
        let result = opaque_wire_frame(wire, usize::from(max));

        if wire_size < 4 {
            assert_eq!(result, OpaqueWireFrame::MissingPrefix);
            kani::cover!(wire_size == 3);
        } else if length > max {
            assert_eq!(
                result,
                OpaqueWireFrame::OverLimit {
                    length: usize::from(length)
                }
            );
            kani::cover!(length == 4 && max == 3);
        } else {
            let span = if length == 0 { 0 } else { 4 };
            if usize::from(wire_size) - 4 < span {
                assert_eq!(
                    result,
                    OpaqueWireFrame::Truncated {
                        length: usize::from(length),
                        span
                    }
                );
                kani::cover!(length == 4 && wire_size == 6);
                kani::cover!(length == 3 && wire_size == 7);
            } else {
                assert_eq!(
                    result,
                    OpaqueWireFrame::Complete {
                        length: usize::from(length),
                        span
                    }
                );
                kani::cover!(length == 0 && wire_size == 4);
                kani::cover!(length == 3 && wire_size == 8 && bytes[7] != 0);
            }
        }
    }

    /// Connects every four-byte wire length and every caller limit to the
    /// production frame decision. Only the tail size is bounded to 0..=4.
    #[kani::proof]
    #[kani::unwind(6)]
    fn var_opaque_all_wire_prefixes() {
        let prefix: [u8; 4] = kani::any();
        let remaining: u8 = kani::any();
        kani::assume(remaining <= 4);
        let max: usize = kani::any();
        let wire = [prefix[0], prefix[1], prefix[2], prefix[3], 0, 0, 0, 0];
        let length = (usize::from(prefix[0]) << 24)
            | (usize::from(prefix[1]) << 16)
            | (usize::from(prefix[2]) << 8)
            | usize::from(prefix[3]);
        let decision = opaque_wire_frame(&wire[..4 + usize::from(remaining)], max);

        if length > max || length > XDR_MAX_ITEM {
            assert_eq!(decision, OpaqueWireFrame::OverLimit { length });
            kani::cover!(length == 5 && max == 4);
            kani::cover!(length == XDR_MAX_ITEM + 1 && max == usize::MAX);
        } else {
            let span = ((length + 3) / 4) * 4;
            if span > usize::from(remaining) {
                assert_eq!(decision, OpaqueWireFrame::Truncated { length, span });
                kani::cover!(length == 3 && remaining == 3);
                kani::cover!(length == XDR_MAX_ITEM && remaining == 4);
            } else {
                assert_eq!(decision, OpaqueWireFrame::Complete { length, span });
                kani::cover!(length == 0 && remaining == 0);
                kani::cover!(length == 4 && remaining == 4);
            }
        }
    }
}
