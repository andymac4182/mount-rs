//! Incremental AWS `aws-chunked` framing and SigV4 streaming helpers.
//!
//! The decoder copies every input run before retaining or returning it. Signed
//! chunks are held until their chain signature verifies; unsigned chunks are
//! returned as copied runs because no signature can make buffering them safer.
//! Header, trailer, signed-frame, and decoded-body limits keep the public
//! helper bounded without changing the HTTP server boundary.

use std::{error::Error, fmt};

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::sigv4::{
    CredentialScope, EMPTY_PAYLOAD_SHA256, STREAMING_PAYLOAD, STREAMING_PAYLOAD_TRAILER,
    STREAMING_UNSIGNED_PAYLOAD_TRAILER, credential_scope, sha256_hex, signatures_match,
    signing_key,
};

/// The chunk extension carrying a chunk's signature.
pub const CHUNK_SIGNATURE_PARAMETER: &str = "chunk-signature";

/// The trailing header carrying the trailer block's signature.
pub const TRAILER_SIGNATURE_HEADER: &str = "x-amz-trailer-signature";

/// Algorithm line of a chunk's string to sign.
pub const CHUNK_ALGORITHM: &str = "AWS4-HMAC-SHA256-PAYLOAD";

/// Algorithm line of a trailer block's string to sign.
pub const TRAILER_ALGORITHM: &str = "AWS4-HMAC-SHA256-TRAILER";

/// Largest signed frame, in bytes.
pub const CHUNKED_MAX_FRAME: usize = 8 * 1024 * 1024;

/// Largest exact unsigned declared size.
pub const CHUNKED_MAX_DECLARED_SIZE: u64 = 9_007_199_254_740_991;

/// Largest chunk header line, excluding CRLF.
pub const CHUNKED_MAX_HEADER_BYTES: usize = 256;

/// Largest trailing-header block, counting normalized LF bytes.
pub const CHUNKED_MAX_TRAILER_BYTES: usize = 16 * 1024;

/// Largest number of hexadecimal size digits.
pub const CHUNKED_MAX_HEX_DIGITS: usize = 16;

/// Which streaming shape a content hash sentinel names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamingPayloadKind {
    pub signed: bool,
    pub trailers: bool,
}

/// Read one of the three signed-streaming sentinels.
pub fn streaming_payload_kind(value: &str) -> Option<StreamingPayloadKind> {
    match value {
        STREAMING_PAYLOAD => Some(StreamingPayloadKind {
            signed: true,
            trailers: false,
        }),
        STREAMING_PAYLOAD_TRAILER => Some(StreamingPayloadKind {
            signed: true,
            trailers: true,
        }),
        STREAMING_UNSIGNED_PAYLOAD_TRAILER => Some(StreamingPayloadKind {
            signed: false,
            trailers: true,
        }),
        _ => None,
    }
}

/// Why a chunked body was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkedRefusal {
    BadSize,
    TooLarge,
    Malformed,
    MissingSignature,
    SignatureMismatch,
    Truncated,
    TrailingBytes,
    LengthMismatch,
    Trailer,
    Internal,
}

impl ChunkedRefusal {
    /// The stable upstream refusal spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BadSize => "bad-size",
            Self::TooLarge => "too-large",
            Self::Malformed => "malformed",
            Self::MissingSignature => "missing-signature",
            Self::SignatureMismatch => "signature-mismatch",
            Self::Truncated => "truncated",
            Self::TrailingBytes => "trailing-bytes",
            Self::LengthMismatch => "length-mismatch",
            Self::Trailer => "trailer",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for ChunkedRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A named, sticky refusal from the chunked codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkedError {
    /// Stable error-code spelling for callers that classify transport errors.
    pub code: &'static str,
    pub reason: ChunkedRefusal,
    pub message: String,
    /// Encoded bytes consumed when the refusal was detected.
    pub offset: usize,
}

impl ChunkedError {
    fn new(reason: ChunkedRefusal, message: impl Into<String>, offset: usize) -> Self {
        Self {
            code: "ERR_S3_CHUNKED",
            reason,
            message: message.into(),
            offset,
        }
    }
}

impl fmt::Display for ChunkedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.reason, self.message)
    }
}

impl Error for ChunkedError {}

/// Return whether a standard error is a chunked-codec refusal.
pub fn is_chunked_error(error: &(dyn Error + 'static)) -> bool {
    error.downcast_ref::<ChunkedError>().is_some()
}

/// Optional memory limits for an [`AwsChunkedDecoder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkedLimits {
    /// Largest signed frame. Unsigned frames are not subject to this buffer cap.
    pub max_frame: usize,
    /// Largest normalized trailing-header block.
    pub max_trailer_bytes: usize,
    /// Largest decoded body, independent of `decoded_length`.
    pub max_decoded_bytes: Option<u64>,
}

impl Default for ChunkedLimits {
    fn default() -> Self {
        Self {
            max_frame: CHUNKED_MAX_FRAME,
            max_trailer_bytes: CHUNKED_MAX_TRAILER_BYTES,
            max_decoded_bytes: None,
        }
    }
}

/// SigV4 chain material for signed chunks and trailers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkedSignature {
    /// The request's own SigV4 signature.
    pub seed: String,
    /// The request's `x-amz-date` value.
    pub amz_date: String,
    pub scope: CredentialScope,
    pub secret_access_key: String,
    /// An optional precomputed signing key.
    pub key: Option<Vec<u8>>,
}

/// Return the signing key for a chunk chain.
pub fn chunked_signing_key(signature: &ChunkedSignature) -> Vec<u8> {
    signature
        .key
        .clone()
        .unwrap_or_else(|| signing_key(&signature.secret_access_key, &signature.scope))
}

/// Construct one chunk's string to sign.
pub fn chunk_string_to_sign(
    signature: &ChunkedSignature,
    previous: &str,
    payload_hash: &str,
) -> String {
    [
        CHUNK_ALGORITHM,
        signature.amz_date.as_str(),
        credential_scope(&signature.scope).as_str(),
        previous,
        EMPTY_PAYLOAD_SHA256,
        payload_hash,
    ]
    .join("\n")
}

/// Construct a trailing-header block's string to sign.
pub fn trailer_string_to_sign(
    signature: &ChunkedSignature,
    previous: &str,
    trailer_hash: &str,
) -> String {
    [
        TRAILER_ALGORITHM,
        signature.amz_date.as_str(),
        credential_scope(&signature.scope).as_str(),
        previous,
        trailer_hash,
    ]
    .join("\n")
}

fn hmac_hex(key: &[u8], data: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts every key length");
    mac.update(data.as_bytes());
    hex_lower(&mac.finalize().into_bytes())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// Sign one chunk in a SigV4 streaming chain.
pub fn sign_chunk(signature: &ChunkedSignature, previous: &str, payload_hash: &str) -> String {
    hmac_hex(
        &chunked_signing_key(signature),
        &chunk_string_to_sign(signature, previous, payload_hash),
    )
}

/// Sign a trailing-header block in a SigV4 streaming chain.
pub fn sign_trailer(signature: &ChunkedSignature, previous: &str, trailer_hash: &str) -> String {
    hmac_hex(
        &chunked_signing_key(signature),
        &trailer_string_to_sign(signature, previous, trailer_hash),
    )
}

/// One verified trailing header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkedTrailer {
    pub name: String,
    pub value: String,
}

/// Configuration for an [`AwsChunkedDecoder`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AwsChunkedParams {
    pub signature: Option<ChunkedSignature>,
    pub trailers: Vec<String>,
    pub decoded_length: Option<u64>,
    pub limits: ChunkedLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecoderState {
    Header,
    HeaderLf,
    Payload,
    PayloadCr,
    PayloadLf,
    Trailer,
    TrailerLf,
    Epilogue,
}

/// Incremental AWS chunked decoder.
pub struct AwsChunkedDecoder {
    signature: Option<ChunkedSignature>,
    key: Option<Vec<u8>>,
    declared: Vec<String>,
    decoded_length: Option<u64>,
    limits: ChunkedLimits,
    state: DecoderState,
    line: Vec<u8>,
    chunk: Option<Vec<u8>>,
    filled: usize,
    size: u64,
    remaining: u64,
    chunk_signature: String,
    previous: String,
    trailer_block: Vec<u8>,
    trailers: Vec<ChunkedTrailer>,
    offset: usize,
    decoded: u64,
    epilogue: u8,
    terminated: bool,
    ended: bool,
    failure: Option<ChunkedError>,
}

impl AwsChunkedDecoder {
    /// Build a decoder, validating the seed before any body bytes are accepted.
    pub fn new(params: AwsChunkedParams) -> Result<Self, ChunkedError> {
        if let Some(signature) = params.signature.as_ref()
            && !is_signature_hex(&signature.seed)
        {
            return Err(ChunkedError::new(
                ChunkedRefusal::Malformed,
                "seed signature is not 64 hex characters",
                0,
            ));
        }
        let declared = params
            .trailers
            .into_iter()
            .map(|name| name.to_ascii_lowercase())
            .fold(Vec::new(), |mut names, name| {
                if !names.iter().any(|existing| existing == &name) {
                    names.push(name);
                }
                names
            });
        let previous = params
            .signature
            .as_ref()
            .map(|signature| signature.seed.clone())
            .unwrap_or_default();
        let key = params.signature.as_ref().map(chunked_signing_key);
        Ok(Self {
            signature: params.signature,
            key,
            declared,
            decoded_length: params.decoded_length,
            limits: params.limits,
            state: DecoderState::Header,
            line: Vec::with_capacity(128),
            chunk: None,
            filled: 0,
            size: 0,
            remaining: 0,
            chunk_signature: String::new(),
            previous,
            trailer_block: Vec::new(),
            trailers: Vec::new(),
            offset: 0,
            decoded: 0,
            epilogue: 0,
            terminated: false,
            ended: false,
            failure: None,
        })
    }

    /// Decoded bytes released or held for the current signed frame.
    pub fn decoded_bytes(&self) -> u64 {
        self.decoded
    }

    /// Verified trailing headers accumulated so far.
    pub fn trailers(&self) -> &[ChunkedTrailer] {
        &self.trailers
    }

    /// Whether the terminal zero-sized chunk has been verified.
    pub fn terminated(&self) -> bool {
        self.terminated
    }

    /// Feed encoded bytes and return copied decoded payload runs.
    pub fn write(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, ChunkedError> {
        if let Some(failure) = self.failure.clone() {
            return Err(failure);
        }
        if self.ended && !bytes.is_empty() {
            return Err(self.remember(ChunkedError::new(
                ChunkedRefusal::TrailingBytes,
                "bytes arrived after the body ended",
                self.offset,
            )));
        }
        match self.consume(bytes) {
            Ok(output) => Ok(output),
            Err(error) => Err(self.remember(error)),
        }
    }

    /// Finish the stream, refusing truncation or a decoded-length mismatch.
    pub fn end(&mut self) -> Result<(), ChunkedError> {
        if let Some(failure) = self.failure.clone() {
            return Err(failure);
        }
        if self.ended {
            return Ok(());
        }
        match self.finish() {
            Ok(()) => {
                self.ended = true;
                Ok(())
            }
            Err(error) => Err(self.remember(error)),
        }
    }

    fn fail(&self, reason: ChunkedRefusal, message: impl Into<String>) -> ChunkedError {
        ChunkedError::new(reason, message, self.offset)
    }

    fn remember(&mut self, error: ChunkedError) -> ChunkedError {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
        self.failure.clone().expect("failure was just stored")
    }

    fn consume(&mut self, input: &[u8]) -> Result<Vec<Vec<u8>>, ChunkedError> {
        let mut output = Vec::new();
        let mut index = 0;
        while index < input.len() {
            match self.state {
                DecoderState::Header => {
                    index = self.scan_line(
                        input,
                        index,
                        CHUNKED_MAX_HEADER_BYTES,
                        ChunkedRefusal::Malformed,
                        "chunk header",
                    )?;
                }
                DecoderState::HeaderLf => {
                    index = self.expect_lf(input, index, "chunk header")?;
                    self.begin_chunk()?;
                }
                DecoderState::Payload => {
                    let take = self.remaining.min((input.len() - index) as u64) as usize;
                    if let Some(chunk) = self.chunk.as_mut() {
                        chunk.extend_from_slice(&input[index..index + take]);
                        self.filled += take;
                    } else {
                        output.push(input[index..index + take].to_vec());
                    }
                    self.remaining -= take as u64;
                    self.decoded += take as u64;
                    self.offset += take;
                    index += take;
                    if self.remaining == 0 {
                        self.state = DecoderState::PayloadCr;
                    }
                }
                DecoderState::PayloadCr => {
                    index = self.expect_cr(input, index, "chunk payload")?;
                }
                DecoderState::PayloadLf => {
                    index = self.expect_lf(input, index, "chunk payload")?;
                    self.finish_chunk(&mut output)?;
                }
                DecoderState::Trailer => {
                    let limit = self
                        .limits
                        .max_trailer_bytes
                        .saturating_sub(self.trailer_block.len());
                    index = self.scan_line(
                        input,
                        index,
                        limit,
                        ChunkedRefusal::TooLarge,
                        "trailing header",
                    )?;
                }
                DecoderState::TrailerLf => {
                    index = self.expect_lf(input, index, "trailing header")?;
                    self.finish_trailer_line()?;
                }
                DecoderState::Epilogue => {
                    index = self.epilogue_byte(input, index)?;
                }
            }
        }
        Ok(output)
    }

    fn scan_line(
        &mut self,
        input: &[u8],
        mut index: usize,
        limit: usize,
        reason: ChunkedRefusal,
        what: &str,
    ) -> Result<usize, ChunkedError> {
        while index < input.len() {
            let byte = input[index];
            index += 1;
            self.offset += 1;
            if byte == b'\r' {
                self.state = if self.state == DecoderState::Header {
                    DecoderState::HeaderLf
                } else {
                    DecoderState::TrailerLf
                };
                return Ok(index);
            }
            if self.line.len() >= limit {
                return Err(self.fail(reason, format!("{what} is longer than {limit} bytes")));
            }
            self.line.push(byte);
        }
        Ok(index)
    }

    fn expect_cr(&mut self, input: &[u8], index: usize, what: &str) -> Result<usize, ChunkedError> {
        let byte = input[index];
        self.offset += 1;
        if byte != b'\r' {
            return Err(self.fail(
                ChunkedRefusal::Malformed,
                format!("{what} is not followed by CRLF"),
            ));
        }
        self.state = DecoderState::PayloadLf;
        Ok(index + 1)
    }

    fn expect_lf(&mut self, input: &[u8], index: usize, what: &str) -> Result<usize, ChunkedError> {
        let byte = input[index];
        self.offset += 1;
        if byte != b'\n' {
            return Err(self.fail(
                ChunkedRefusal::Malformed,
                format!("{what} has a CR that is not followed by LF"),
            ));
        }
        Ok(index + 1)
    }

    fn begin_chunk(&mut self) -> Result<(), ChunkedError> {
        let text = latin1(&self.line);
        self.line.clear();
        let separator = text.find(';');
        let size_text = separator.map_or(text.as_str(), |index| &text[..index]);
        let size = self.parse_size(size_text)?;
        self.chunk_signature =
            self.parse_chunk_signature(separator.map_or("", |index| &text[index + 1..]))?;
        let next = self
            .decoded
            .checked_add(size)
            .ok_or_else(|| self.fail(ChunkedRefusal::TooLarge, "decoded body length overflow"))?;
        if self.decoded_length.is_some_and(|length| next > length) {
            return Err(self.fail(
                ChunkedRefusal::LengthMismatch,
                format!(
                    "chunk of {size} bytes takes the body past the declared {} bytes",
                    self.decoded_length.unwrap_or_default()
                ),
            ));
        }
        if self
            .limits
            .max_decoded_bytes
            .is_some_and(|length| next > length)
        {
            return Err(self.fail(
                ChunkedRefusal::TooLarge,
                "body is larger than the decoded-byte cap",
            ));
        }
        self.size = size;
        self.remaining = size;
        self.filled = 0;
        self.chunk = if size == 0 || self.signature.is_none() {
            None
        } else {
            Some(Vec::with_capacity(usize::try_from(size).map_err(|_| {
                self.fail(
                    ChunkedRefusal::TooLarge,
                    "signed chunk does not fit in memory",
                )
            })?))
        };
        if size == 0 {
            self.finish_chunk(&mut Vec::new())?;
        } else {
            self.state = DecoderState::Payload;
        }
        Ok(())
    }

    fn parse_size(&self, text: &str) -> Result<u64, ChunkedError> {
        if text.is_empty() {
            return Err(self.fail(ChunkedRefusal::BadSize, "chunk header has no size"));
        }
        if text.len() > CHUNKED_MAX_HEX_DIGITS {
            return Err(self.fail(
                ChunkedRefusal::BadSize,
                format!("chunk size is {} hex digits", text.len()),
            ));
        }
        let cap = if self.signature.is_some() {
            self.limits.max_frame as u64
        } else {
            CHUNKED_MAX_DECLARED_SIZE
        };
        let mut size = 0u64;
        for byte in text.bytes() {
            let digit = hex_digit(byte).ok_or_else(|| {
                self.fail(ChunkedRefusal::BadSize, "chunk size is not hexadecimal")
            })?;
            size = size
                .checked_mul(16)
                .and_then(|value| value.checked_add(u64::from(digit)))
                .ok_or_else(|| self.fail(ChunkedRefusal::BadSize, "chunk size overflows u64"))?;
            if size > cap {
                return Err(self.fail(
                    ChunkedRefusal::TooLarge,
                    format!("chunk is larger than the {cap}-byte cap"),
                ));
            }
        }
        Ok(size)
    }

    fn parse_chunk_signature(&self, extensions: &str) -> Result<String, ChunkedError> {
        if self.signature.is_none() {
            return Ok(String::new());
        }
        for parameter in extensions.split(';') {
            let Some((name, value)) = parameter.split_once('=') else {
                continue;
            };
            if !name.trim().eq_ignore_ascii_case(CHUNK_SIGNATURE_PARAMETER) {
                continue;
            }
            let value = value.trim();
            if !is_signature_hex(value) {
                return Err(self.fail(
                    ChunkedRefusal::Malformed,
                    format!("{CHUNK_SIGNATURE_PARAMETER} is not 64 hex characters"),
                ));
            }
            return Ok(value.to_owned());
        }
        Err(self.fail(
            ChunkedRefusal::MissingSignature,
            format!("chunk header has no {CHUNK_SIGNATURE_PARAMETER}"),
        ))
    }

    fn finish_chunk(&mut self, output: &mut Vec<Vec<u8>>) -> Result<(), ChunkedError> {
        let payload = self.chunk.take();
        if let (Some(signature), Some(key)) = (&self.signature, &self.key) {
            let payload_hash = payload
                .as_deref()
                .map(sha256_hex)
                .unwrap_or_else(|| sha256_hex([]));
            let expected = hmac_hex(
                key,
                &chunk_string_to_sign(signature, &self.previous, &payload_hash),
            );
            if !signatures_match(&expected, &self.chunk_signature) {
                return Err(self.fail(
                    ChunkedRefusal::SignatureMismatch,
                    "chunk signature does not match",
                ));
            }
            self.previous = expected;
        }
        if self.size == 0 {
            self.terminated = true;
            self.state = if self.declared.is_empty() {
                DecoderState::Epilogue
            } else {
                DecoderState::Trailer
            };
        } else {
            if let Some(payload) = payload {
                output.push(payload);
            }
            self.state = DecoderState::Header;
        }
        self.filled = 0;
        self.size = 0;
        self.remaining = 0;
        self.chunk_signature.clear();
        Ok(())
    }

    fn finish_trailer_line(&mut self) -> Result<(), ChunkedError> {
        let raw = std::mem::take(&mut self.line);
        let text = latin1(&raw);
        if text.is_empty() {
            if self.signature.is_some() {
                return Err(self.fail(
                    ChunkedRefusal::Trailer,
                    format!("trailer block ends before {TRAILER_SIGNATURE_HEADER}"),
                ));
            }
            self.close_trailers()?;
            self.state = DecoderState::Epilogue;
            return Ok(());
        }
        let lower = text.to_ascii_lowercase();
        if lower.starts_with(&format!("{TRAILER_SIGNATURE_HEADER}:")) {
            self.verify_trailer_signature(text[TRAILER_SIGNATURE_HEADER.len() + 1..].trim())?;
            self.close_trailers()?;
            self.state = DecoderState::Epilogue;
            return Ok(());
        }
        let Some(colon) = text.find(':') else {
            return Err(self.fail(ChunkedRefusal::Trailer, "trailing header has no name"));
        };
        if colon == 0 {
            return Err(self.fail(ChunkedRefusal::Trailer, "trailing header has no name"));
        }
        let name = text[..colon].trim().to_ascii_lowercase();
        if !self.declared.iter().any(|declared| declared == &name) {
            return Err(self.fail(
                ChunkedRefusal::Trailer,
                format!("trailing header {name} was not declared in x-amz-trailer"),
            ));
        }
        if self.trailers.iter().any(|trailer| trailer.name == name) {
            return Err(self.fail(
                ChunkedRefusal::Trailer,
                format!("trailing header {name} arrived twice"),
            ));
        }
        if self.trailer_block.len() + raw.len() + 1 > self.limits.max_trailer_bytes {
            return Err(self.fail(
                ChunkedRefusal::TooLarge,
                "trailer block exceeds its byte limit",
            ));
        }
        self.trailer_block.extend_from_slice(&raw);
        self.trailer_block.push(b'\n');
        self.trailers.push(ChunkedTrailer {
            name,
            value: text[colon + 1..].trim().to_owned(),
        });
        self.state = DecoderState::Trailer;
        Ok(())
    }

    fn verify_trailer_signature(&mut self, value: &str) -> Result<(), ChunkedError> {
        let (Some(signature), Some(key)) = (&self.signature, &self.key) else {
            return Err(self.fail(
                ChunkedRefusal::Trailer,
                format!("an unsigned body carries a {TRAILER_SIGNATURE_HEADER}"),
            ));
        };
        if !is_signature_hex(value) {
            return Err(self.fail(
                ChunkedRefusal::Malformed,
                format!("{TRAILER_SIGNATURE_HEADER} is not 64 hex characters"),
            ));
        }
        let expected = hmac_hex(
            key,
            &trailer_string_to_sign(signature, &self.previous, &sha256_hex(&self.trailer_block)),
        );
        if !signatures_match(&expected, value) {
            return Err(self.fail(
                ChunkedRefusal::SignatureMismatch,
                "trailer signature does not match",
            ));
        }
        self.previous = expected;
        Ok(())
    }

    fn close_trailers(&self) -> Result<(), ChunkedError> {
        if let Some(missing) = self
            .declared
            .iter()
            .find(|name| !self.trailers.iter().any(|trailer| &trailer.name == *name))
        {
            return Err(self.fail(
                ChunkedRefusal::Trailer,
                format!("declared trailing header {missing} never arrived"),
            ));
        }
        Ok(())
    }

    fn epilogue_byte(&mut self, input: &[u8], index: usize) -> Result<usize, ChunkedError> {
        let byte = input[index];
        self.offset += 1;
        let expected = if self.epilogue == 0 { b'\r' } else { b'\n' };
        if self.epilogue >= 2 || byte != expected {
            return Err(self.fail(
                ChunkedRefusal::TrailingBytes,
                "bytes follow the terminal chunk",
            ));
        }
        self.epilogue += 1;
        Ok(index + 1)
    }

    fn finish(&mut self) -> Result<(), ChunkedError> {
        if !self.terminated {
            return Err(self.fail(ChunkedRefusal::Truncated, "body ends inside a chunk"));
        }
        if self.state == DecoderState::Trailer {
            if !self.line.is_empty() {
                return Err(self.fail(ChunkedRefusal::Truncated, "trailing header has no CRLF"));
            }
            if self.signature.is_some() {
                return Err(self.fail(
                    ChunkedRefusal::Truncated,
                    format!("body ends before {TRAILER_SIGNATURE_HEADER}"),
                ));
            }
            self.close_trailers()?;
            self.state = DecoderState::Epilogue;
        }
        if self.state != DecoderState::Epilogue {
            return Err(self.fail(
                ChunkedRefusal::Truncated,
                "body ends inside the trailer block",
            ));
        }
        if self.epilogue == 1 {
            return Err(self.fail(
                ChunkedRefusal::Truncated,
                "body ends between the final CR and its LF",
            ));
        }
        if self
            .decoded_length
            .is_some_and(|length| self.decoded != length)
        {
            return Err(self.fail(
                ChunkedRefusal::LengthMismatch,
                format!(
                    "body decoded to {} bytes, not the declared {} bytes",
                    self.decoded,
                    self.decoded_length.unwrap_or_default()
                ),
            ));
        }
        Ok(())
    }
}

/// Decode a complete encoded body using the incremental public codec.
pub fn decode_aws_chunked(body: &[u8], params: AwsChunkedParams) -> Result<Vec<u8>, ChunkedError> {
    let mut decoder = AwsChunkedDecoder::new(params)?;
    let pieces = decoder.write(body)?;
    decoder.end()?;
    let mut output = Vec::new();
    for piece in pieces {
        output.extend_from_slice(&piece);
    }
    Ok(output)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_signature_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| hex_digit(byte).is_some())
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}
