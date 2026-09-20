//! Bounded, little-endian 9P wire primitives.

use std::fmt;

use crate::constants::{P9_MAX_ITEM, P9_MAX_STRING};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P9Error {
    pub message: String,
    pub offset: Option<usize>,
}

impl P9Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            offset: None,
        }
    }

    pub fn at(message: impl Into<String>, offset: usize) -> Self {
        Self {
            message: message.into(),
            offset: Some(offset),
        }
    }
}

impl fmt::Display for P9Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(offset) = self.offset {
            write!(f, "{} at byte {offset}", self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl std::error::Error for P9Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct P9Qid {
    pub type_: u8,
    pub version: u32,
    pub path: u64,
}

pub struct P9Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> P9Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub fn with_offset(bytes: &'a [u8], offset: usize) -> Result<Self, P9Error> {
        if offset > bytes.len() {
            return Err(P9Error::at("reader offset is beyond the buffer", offset));
        }
        Ok(Self { bytes, offset })
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

    fn need(&mut self, count: usize, what: &str) -> Result<usize, P9Error> {
        if self.remaining() < count {
            return Err(P9Error::at(
                format!(
                    "truncated {what}: need {count} bytes, {} left",
                    self.remaining()
                ),
                self.offset,
            ));
        }
        let at = self.offset;
        self.offset += count;
        Ok(at)
    }

    pub fn u8(&mut self, what: &str) -> Result<u8, P9Error> {
        let at = self.need(1, what)?;
        Ok(self.bytes[at])
    }

    pub fn u16(&mut self, what: &str) -> Result<u16, P9Error> {
        let at = self.need(2, what)?;
        Ok(u16::from_le_bytes([self.bytes[at], self.bytes[at + 1]]))
    }

    pub fn u32(&mut self, what: &str) -> Result<u32, P9Error> {
        let at = self.need(4, what)?;
        Ok(u32::from_le_bytes([
            self.bytes[at],
            self.bytes[at + 1],
            self.bytes[at + 2],
            self.bytes[at + 3],
        ]))
    }

    pub fn u64(&mut self, what: &str) -> Result<u64, P9Error> {
        let at = self.need(8, what)?;
        Ok(u64::from_le_bytes([
            self.bytes[at],
            self.bytes[at + 1],
            self.bytes[at + 2],
            self.bytes[at + 3],
            self.bytes[at + 4],
            self.bytes[at + 5],
            self.bytes[at + 6],
            self.bytes[at + 7],
        ]))
    }

    pub fn string(&mut self, what: &str) -> Result<String, P9Error> {
        self.string_max(P9_MAX_STRING, what)
    }

    pub fn string_max(&mut self, max: usize, what: &str) -> Result<String, P9Error> {
        let length = self.u16(&format!("{what} length"))? as usize;
        if length > max {
            return Err(P9Error::at(
                format!("{what} is {length} bytes, over the {max}-byte limit"),
                self.offset.saturating_sub(2),
            ));
        }
        let bytes = self.raw(length, what)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn qid(&mut self, what: &str) -> Result<P9Qid, P9Error> {
        Ok(P9Qid {
            type_: self.u8(&format!("{what} type"))?,
            version: self.u32(&format!("{what} version"))?,
            path: self.u64(&format!("{what} path"))?,
        })
    }

    pub fn blob(&mut self, what: &str) -> Result<Vec<u8>, P9Error> {
        self.blob_max(P9_MAX_ITEM, what)
    }

    pub fn blob_max(&mut self, max: usize, what: &str) -> Result<Vec<u8>, P9Error> {
        let length = self.u32(&format!("{what} count"))? as usize;
        if length > max {
            return Err(P9Error::at(
                format!("{what} is {length} bytes, over the {max}-byte limit"),
                self.offset.saturating_sub(4),
            ));
        }
        self.raw(length, what)
    }

    pub fn raw(&mut self, count: usize, what: &str) -> Result<Vec<u8>, P9Error> {
        let at = self.need(count, what)?;
        Ok(self.bytes[at..at + count].to_vec())
    }

    pub fn rest(&mut self) -> Result<Vec<u8>, P9Error> {
        self.raw(self.remaining(), "rest")
    }

    pub fn end(&self, what: &str) -> Result<(), P9Error> {
        if self.remaining() != 0 {
            return Err(P9Error::at(
                format!("{what} has {} trailing bytes", self.remaining()),
                self.offset,
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct P9Writer {
    bytes: Vec<u8>,
}

impl P9Writer {
    pub fn new(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity.max(16)),
        }
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn string(&mut self, value: &str) -> Result<(), P9Error> {
        let encoded = value.as_bytes();
        if encoded.len() > P9_MAX_STRING {
            return Err(P9Error::new(format!(
                "string is {} bytes, over the 16-bit count",
                encoded.len()
            )));
        }
        self.u16(encoded.len() as u16);
        self.raw(encoded);
        Ok(())
    }

    pub fn qid(&mut self, value: P9Qid) {
        self.u8(value.type_);
        self.u32(value.version);
        self.u64(value.path);
    }

    pub fn blob(&mut self, value: &[u8]) {
        self.u32(value.len() as u32);
        self.raw(value);
    }

    pub fn raw(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    pub fn patch_u32(&mut self, at: usize, value: u32) -> Result<(), P9Error> {
        if at.checked_add(4).is_none_or(|end| end > self.bytes.len()) {
            return Err(P9Error::at(
                format!("cannot patch 4 bytes at {at} of {}", self.bytes.len()),
                at,
            ));
        }
        self.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

pub fn string_byte_length(value: &str) -> usize {
    value.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn little_endian_is_unaligned_and_payloads_are_copied() {
        let mut writer = P9Writer::new(1);
        writer.u8(0xaa);
        writer.u32(0x1122_3344);
        writer.u64(0x0102_0304_0506_0708);
        let bytes = writer.bytes();
        assert_eq!(&bytes[..5], &[0xaa, 0x44, 0x33, 0x22, 0x11]);
        let mut reader = P9Reader::new(&bytes);
        assert_eq!(reader.u8("byte").unwrap(), 0xaa);
        assert_eq!(reader.u32("word").unwrap(), 0x1122_3344);
        assert_eq!(reader.u64("wide").unwrap(), 0x0102_0304_0506_0708);
        reader.end("test").unwrap();
    }

    #[test]
    fn malformed_counts_are_rejected_before_copying() {
        let bytes = [0xff, 0xff, 0xff, 0xff, 0, 0];
        let mut reader = P9Reader::new(&bytes);
        let error = reader.blob("data").unwrap_err();
        assert!(error.message.contains("over the"));
    }

    #[test]
    fn invalid_utf8_is_lossy_like_the_oracle() {
        let bytes = [1, 0, 0xff];
        let mut reader = P9Reader::new(&bytes);
        assert_eq!(reader.string("name").unwrap(), "\u{fffd}");
    }
}
