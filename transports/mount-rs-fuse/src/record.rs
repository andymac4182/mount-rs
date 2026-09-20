//! Deterministic FUSE traffic transcripts and replay.
//!
//! The transcript format is intentionally plain: an `UMFT` header followed by
//! fixed-size frame headers and copied payloads. Replay feeds only the
//! kernel-to-daemon frames into a [`crate::session::FuseSession`]; recorded
//! replies are checked for framing but not compared byte-for-byte because
//! inode numbers, handles, and file contents belong to the driver that made
//! the recording.

use std::{collections::BTreeMap, fmt, time::Instant};

use crate::{OUT_HEADER_SIZE, Request};

/// `"UMFT"`, the first four bytes of every transcript.
pub const TRANSCRIPT_MAGIC: u32 = 0x55_4d_46_54;
/// Transcript format version.
pub const TRANSCRIPT_VERSION: u8 = 1;

const HEADER_SIZE: usize = 8;
const FRAME_SIZE: usize = 16;
const DIRECTION_IN: u8 = 0;
const DIRECTION_OUT: u8 = 1;

/// Which way one frame crossed the FUSE device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptDirection {
    /// Kernel to daemon.
    In,
    /// Daemon to kernel.
    Out,
}

/// One copied message, exactly as it crossed the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptFrame {
    pub direction: TranscriptDirection,
    /// Nanoseconds since the first frame of the transcript.
    pub timestamp: u64,
    pub bytes: Vec<u8>,
}

/// Thrown for a non-transcript or truncated transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptError(pub String);

impl fmt::Display for TranscriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TranscriptError {}

/// Serialize transcript frames using the v1 wire format.
pub fn encode_transcript(frames: &[TranscriptFrame]) -> Vec<u8> {
    let total = frames.iter().fold(HEADER_SIZE, |total, frame| {
        total
            .checked_add(FRAME_SIZE)
            .and_then(|value| value.checked_add(frame.bytes.len()))
            .expect("transcript length overflow")
    });
    let mut out = vec![0_u8; total];
    out[..4].copy_from_slice(&TRANSCRIPT_MAGIC.to_be_bytes());
    out[4] = TRANSCRIPT_VERSION;
    let mut offset = HEADER_SIZE;
    for frame in frames {
        out[offset] = match frame.direction {
            TranscriptDirection::In => DIRECTION_IN,
            TranscriptDirection::Out => DIRECTION_OUT,
        };
        let length = u32::try_from(frame.bytes.len()).expect("transcript frame exceeds u32 length");
        out[offset + 4..offset + 8].copy_from_slice(&length.to_le_bytes());
        out[offset + 8..offset + 16].copy_from_slice(&frame.timestamp.to_le_bytes());
        out[offset + FRAME_SIZE..offset + FRAME_SIZE + frame.bytes.len()]
            .copy_from_slice(&frame.bytes);
        offset += FRAME_SIZE + frame.bytes.len();
    }
    out
}

/// Parse a transcript and copy every payload out of the input buffer.
pub fn decode_transcript(bytes: &[u8]) -> Result<Vec<TranscriptFrame>, TranscriptError> {
    if bytes.len() < HEADER_SIZE {
        return Err(TranscriptError(format!(
            "transcript is {} bytes, shorter than its header",
            bytes.len()
        )));
    }
    let magic = u32::from_be_bytes(bytes[..4].try_into().unwrap());
    if magic != TRANSCRIPT_MAGIC {
        return Err(TranscriptError("not a transcript: bad magic".to_owned()));
    }
    let version = bytes[4];
    if version != TRANSCRIPT_VERSION {
        return Err(TranscriptError(format!(
            "transcript version {version}, expected {TRANSCRIPT_VERSION}"
        )));
    }

    let mut frames = Vec::new();
    let mut offset = HEADER_SIZE;
    while offset < bytes.len() {
        if offset + FRAME_SIZE > bytes.len() {
            return Err(TranscriptError(format!(
                "truncated frame header at byte {offset}"
            )));
        }
        let direction = match bytes[offset] {
            DIRECTION_IN => TranscriptDirection::In,
            DIRECTION_OUT => TranscriptDirection::Out,
            value => {
                return Err(TranscriptError(format!(
                    "frame at byte {offset} has direction {value}"
                )));
            }
        };
        let length = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let timestamp = u64::from_le_bytes(bytes[offset + 8..offset + 16].try_into().unwrap());
        let start = offset + FRAME_SIZE;
        let end = start.checked_add(length).ok_or_else(|| {
            TranscriptError(format!(
                "truncated frame payload at byte {start}: wanted {length}"
            ))
        })?;
        if end > bytes.len() {
            return Err(TranscriptError(format!(
                "truncated frame payload at byte {start}: wanted {length}"
            )));
        }
        frames.push(TranscriptFrame {
            direction,
            timestamp,
            bytes: bytes[start..end].to_vec(),
        });
        offset = end;
    }
    Ok(frames)
}

/// Collect frames from a FUSE transport tap.
#[derive(Debug)]
pub struct TranscriptRecorder {
    pub frames: Vec<TranscriptFrame>,
    /// `true` after the byte limit stopped recording.
    pub truncated: bool,
    /// Payload bytes captured, excluding frame headers.
    pub bytes: usize,
    limit: Option<usize>,
    origin: Option<u64>,
    clock_start: Instant,
}

impl Default for TranscriptRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriptRecorder {
    /// Create an unlimited recorder.
    pub fn new() -> Self {
        Self::with_limit(None)
    }

    /// Create a recorder that stops before a frame would exceed `limit`.
    pub fn with_limit(limit: Option<usize>) -> Self {
        Self {
            frames: Vec::new(),
            truncated: false,
            bytes: 0,
            limit,
            origin: None,
            clock_start: Instant::now(),
        }
    }

    /// Capture a frame using a monotonic process-local clock.
    pub fn tap(&mut self, direction: TranscriptDirection, bytes: &[u8]) {
        let elapsed = self.clock_start.elapsed().as_nanos();
        let now = elapsed.min(u64::MAX as u128) as u64;
        self.tap_at(direction, bytes, now);
    }

    /// Capture a frame at an explicit nanosecond value.
    ///
    /// This is useful for deterministic fixtures and has the same prefix-stop
    /// behavior as [`Self::tap`].
    pub fn tap_at(&mut self, direction: TranscriptDirection, bytes: &[u8], now: u64) {
        if self.truncated {
            return;
        }
        let next = match self.bytes.checked_add(bytes.len()) {
            Some(value) => value,
            None => {
                self.truncated = true;
                return;
            }
        };
        if self.limit.is_some_and(|limit| next > limit) {
            self.truncated = true;
            return;
        }
        let origin = *self.origin.get_or_insert(now);
        self.bytes = next;
        self.frames.push(TranscriptFrame {
            direction,
            timestamp: now.saturating_sub(origin),
            bytes: bytes.to_vec(),
        });
    }

    /// Serialize everything recorded so far.
    pub fn encode(&self) -> Vec<u8> {
        encode_transcript(&self.frames)
    }
}

/// One request in a replay that did not behave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayFailure {
    pub index: usize,
    pub reason: String,
}

/// What replay observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayReport {
    pub requests: usize,
    pub replies: usize,
    pub no_reply: usize,
    pub opcodes: BTreeMap<String, usize>,
    pub failures: Vec<ReplayFailure>,
}

/// Feed a transcript's requests through a session in order.
pub async fn replay_transcript(
    session: &mut crate::session::FuseSession,
    frames: &[TranscriptFrame],
) -> ReplayReport {
    let mut report = ReplayReport {
        requests: 0,
        replies: 0,
        no_reply: 0,
        opcodes: BTreeMap::new(),
        failures: Vec::new(),
    };

    for (index, frame) in frames.iter().enumerate() {
        if frame.direction == TranscriptDirection::Out {
            match decode_out_header(&frame.bytes) {
                Ok(header) if header.len as usize == frame.bytes.len() => {}
                Ok(header) => report.failures.push(ReplayFailure {
                    index,
                    reason: format!(
                        "recorded reply says len {}, frame is {}",
                        header.len,
                        frame.bytes.len()
                    ),
                }),
                Err(error) => report.failures.push(ReplayFailure {
                    index,
                    reason: format!("recorded reply does not decode: {error}"),
                }),
            }
            continue;
        }

        report.requests += 1;
        let request = match Request::decode(&frame.bytes, session.max_request) {
            Ok(request) => request,
            Err(error) => {
                report.failures.push(ReplayFailure {
                    index,
                    reason: format!("request does not decode: {error}"),
                });
                continue;
            }
        };
        let name = opcode_name(request.header.opcode);
        *report.opcodes.entry(name.clone()).or_default() += 1;

        let reply = match session.handle(&frame.bytes).await {
            Ok(reply) => reply,
            Err(error) => {
                report.failures.push(ReplayFailure {
                    index,
                    reason: format!("{name} rejected: {error}"),
                });
                continue;
            }
        };
        let Some(reply) = reply else {
            report.no_reply += 1;
            if !is_no_reply(request.header.opcode) {
                report.failures.push(ReplayFailure {
                    index,
                    reason: format!("{name} was not answered"),
                });
            }
            continue;
        };
        report.replies += 1;
        if is_no_reply(request.header.opcode) {
            report.failures.push(ReplayFailure {
                index,
                reason: format!("{name} must not be answered, but was"),
            });
            continue;
        }
        match decode_out_header(&reply) {
            Ok(header) => {
                if header.unique != request.header.unique {
                    report.failures.push(ReplayFailure {
                        index,
                        reason: format!("{name} answered unique {}, not its own", header.unique),
                    });
                }
                if header.len as usize != reply.len() {
                    report.failures.push(ReplayFailure {
                        index,
                        reason: format!("{name} reply says len {}, is {}", header.len, reply.len()),
                    });
                }
            }
            Err(error) => report.failures.push(ReplayFailure {
                index,
                reason: format!("{name} reply does not decode: {error}"),
            }),
        }
    }
    report
}

#[derive(Debug, Clone, Copy)]
struct OutHeader {
    len: u32,
    unique: u64,
}

fn decode_out_header(bytes: &[u8]) -> Result<OutHeader, TranscriptError> {
    if bytes.len() < OUT_HEADER_SIZE {
        return Err(TranscriptError(format!(
            "truncated FUSE output header: {} byte(s)",
            bytes.len()
        )));
    }
    let len = u32::from_le_bytes(bytes[..4].try_into().unwrap());
    if len < OUT_HEADER_SIZE as u32 {
        return Err(TranscriptError(format!(
            "fuse_out_header.len is {len}, below the {OUT_HEADER_SIZE}-byte header"
        )));
    }
    Ok(OutHeader {
        len,
        unique: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
    })
}

fn is_no_reply(opcode: u32) -> bool {
    matches!(opcode, 2 | 41 | 42)
}

fn opcode_name(opcode: u32) -> String {
    let name = match opcode {
        1 => "LOOKUP",
        2 => "FORGET",
        3 => "GETATTR",
        4 => "SETATTR",
        5 => "READLINK",
        6 => "SYMLINK",
        8 => "MKNOD",
        9 => "MKDIR",
        10 => "UNLINK",
        11 => "RMDIR",
        12 => "RENAME",
        13 => "LINK",
        14 => "OPEN",
        15 => "READ",
        16 => "WRITE",
        17 => "STATFS",
        18 => "RELEASE",
        20 => "FSYNC",
        21 => "SETXATTR",
        22 => "GETXATTR",
        23 => "LISTXATTR",
        24 => "REMOVEXATTR",
        25 => "FLUSH",
        26 => "INIT",
        27 => "OPENDIR",
        28 => "READDIR",
        29 => "RELEASEDIR",
        30 => "FSYNCDIR",
        31 => "GETLK",
        32 => "SETLK",
        33 => "SETLKW",
        34 => "ACCESS",
        35 => "CREATE",
        36 => "INTERRUPT",
        37 => "BMAP",
        38 => "DESTROY",
        39 => "IOCTL",
        40 => "POLL",
        41 => "NOTIFY_REPLY",
        42 => "BATCH_FORGET",
        43 => "FALLOCATE",
        44 => "READDIRPLUS",
        45 => "RENAME2",
        46 => "LSEEK",
        47 => "COPY_FILE_RANGE",
        48 => "SETUPMAPPING",
        49 => "REMOVEMAPPING",
        50 => "SYNCFS",
        51 => "TMPFILE",
        52 => "STATX",
        4096 => "CUSE_INIT",
        _ => return format!("UNKNOWN({opcode})"),
    };
    name.to_owned()
}
