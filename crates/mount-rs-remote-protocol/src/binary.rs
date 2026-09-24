//! Negotiated v2 envelope. Header admission is separate from body allocation.
//! All integers are big endian; reserved bytes must be zero. One envelope per stream.
use crate::{FrameError, Message, WireError};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u16 = crate::PROTOCOL_VERSION;
pub const MAX_IO_BYTES: usize = 1024 * 1024;
pub const MAX_CONTROL_BYTES: usize = crate::MAX_FRAME_BYTES;
pub const HEADER_BYTES: usize = 32;
pub const IO_PREFIX_BYTES: usize = 18;
pub const MAX_DRIVE_ID_BYTES: usize = 64;
pub const MIN_IO_CONTROL_BYTES: usize = IO_PREFIX_BYTES + 1;
pub const MAX_IO_CONTROL_BYTES: usize = IO_PREFIX_BYTES + MAX_DRIVE_ID_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Control = 0,
    Read = 1,
    Write = 2,
    ReadResult = 3,
    WriteResult = 4,
    Error = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: Kind,
    pub request_id: u64,
    pub control_len: usize,
    pub payload_len: usize,
    pub count: usize,
}
impl Header {
    pub fn new(
        kind: Kind,
        request_id: u64,
        control_len: usize,
        payload_len: usize,
        count: usize,
    ) -> Result<Self, FrameError> {
        let h = Self {
            kind,
            request_id,
            control_len,
            payload_len,
            count,
        };
        if !valid_lengths(kind, request_id, control_len, payload_len, count) {
            return Err(FrameError::InvalidLength);
        }
        Ok(h)
    }
    pub fn encode(self) -> Result<[u8; HEADER_BYTES], FrameError> {
        Self::new(
            self.kind,
            self.request_id,
            self.control_len,
            self.payload_len,
            self.count,
        )?;
        let mut out = [0; HEADER_BYTES];
        out[..4].copy_from_slice(b"MRB2");
        out[4] = self.kind as u8;
        out[8..16].copy_from_slice(&self.request_id.to_be_bytes());
        out[16..20].copy_from_slice(&(self.control_len as u32).to_be_bytes());
        out[20..24].copy_from_slice(&(self.payload_len as u32).to_be_bytes());
        out[24..28].copy_from_slice(&(self.count as u32).to_be_bytes());
        Ok(out)
    }
    pub fn decode(b: [u8; HEADER_BYTES]) -> Result<Self, FrameError> {
        if &b[..4] != b"MRB2" || b[5..8] != [0; 3] || b[28..] != [0; 4] {
            return Err(FrameError::InvalidLength);
        }
        let kind = match b[4] {
            0 => Kind::Control,
            1 => Kind::Read,
            2 => Kind::Write,
            3 => Kind::ReadResult,
            4 => Kind::WriteResult,
            5 => Kind::Error,
            _ => return Err(FrameError::InvalidLength),
        };
        Self::new(
            kind,
            u64::from_be_bytes(b[8..16].try_into().unwrap()),
            u32::from_be_bytes(b[16..20].try_into().unwrap()) as usize,
            u32::from_be_bytes(b[20..24].try_into().unwrap()) as usize,
            u32::from_be_bytes(b[24..28].try_into().unwrap()) as usize,
        )
    }
    pub async fn read<R: AsyncRead + Unpin>(r: &mut R) -> Result<Self, FrameError> {
        let mut b = [0; HEADER_BYTES];
        r.read_exact(&mut b).await?;
        Self::decode(b)
    }
}
fn valid_lengths(kind: Kind, id: u64, control: usize, payload: usize, count: usize) -> bool {
    if control > MAX_CONTROL_BYTES || payload > MAX_IO_BYTES || count > MAX_IO_BYTES {
        return false;
    }
    match kind {
        Kind::Control => id == 0 && control > 0 && payload == 0 && count == 0,
        Kind::Read => {
            id > 0
                && (MIN_IO_CONTROL_BYTES..=MAX_IO_CONTROL_BYTES).contains(&control)
                && payload == 0
        }
        Kind::Write => {
            id > 0 && (MIN_IO_CONTROL_BYTES..=MAX_IO_CONTROL_BYTES).contains(&control) && count == 0
        }
        Kind::ReadResult => id > 0 && control == 0 && count == payload,
        Kind::WriteResult => id > 0 && control == 0 && payload == 0,
        Kind::Error => id > 0 && (1..=64).contains(&control) && payload == 0 && count == 0,
    }
}

#[derive(Debug, Clone)]
pub struct IoRequest<D = String> {
    pub drive_id: D,
    pub handle: u64,
    pub position: Option<u64>,
}

/// An owned catalog Drive ID stored without a heap allocation.
#[derive(Clone, PartialEq, Eq)]
pub struct InlineDriveId {
    bytes: [u8; MAX_DRIVE_ID_BYTES],
    len: u8,
}
impl AsRef<str> for InlineDriveId {
    fn as_ref(&self) -> &str {
        // Construction validates ASCII before storing the ID.
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap()
    }
}
impl std::ops::Deref for InlineDriveId {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_ref()
    }
}
impl std::fmt::Display for InlineDriveId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_ref())
    }
}
impl std::fmt::Debug for InlineDriveId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_ref(), f)
    }
}
fn valid_drive_id(bytes: &[u8]) -> bool {
    (1..=MAX_DRIVE_ID_BYTES).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
#[derive(Debug)]
pub enum Incoming {
    Control(Message),
    Read {
        request_id: u64,
        request: IoRequest<InlineDriveId>,
        length: usize,
    },
    Write {
        request_id: u64,
        request: IoRequest<InlineDriveId>,
        data: Vec<u8>,
    },
}

// A capped writer rejects oversized JSON while serializing, without constructing
// a giant serialized body first. It applies to all generic v2 control operations.
struct Capped(Vec<u8>, usize);
impl std::io::Write for Capped {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if b.len() > self.1.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("control limit"));
        }
        self.0.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn json<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let mut out = Capped(Vec::new(), MAX_CONTROL_BYTES);
    serde_json::to_writer(&mut out, value)?;
    Ok(out.0)
}
async fn emit<W: AsyncWrite + Unpin>(
    w: &mut W,
    h: Header,
    control: &[u8],
    data: &[u8],
) -> Result<(), FrameError> {
    let encoded = h.encode()?;
    if control.len() != h.control_len || data.len() != h.payload_len {
        return Err(FrameError::InvalidLength);
    }
    w.write_all(&encoded).await?;
    w.write_all(control).await?;
    w.write_all(data).await?;
    Ok(())
}
pub async fn write_control<W: AsyncWrite + Unpin>(
    w: &mut W,
    message: &Message,
) -> Result<(), FrameError> {
    let b = json(message)?;
    emit(w, Header::new(Kind::Control, 0, b.len(), 0, 0)?, &b, &[]).await
}
pub async fn read_control<R: AsyncRead + Unpin>(r: &mut R) -> Result<Message, FrameError> {
    let h = Header::read(r).await?;
    match read_body(r, h).await? {
        Incoming::Control(m) => Ok(m),
        _ => Err(FrameError::InvalidLength),
    }
}
pub async fn write_request<W: AsyncWrite + Unpin, D: AsRef<str>>(
    w: &mut W,
    id: u64,
    request: &IoRequest<D>,
    length: usize,
    data: Option<&[u8]>,
) -> Result<(), FrameError> {
    let drive = request.drive_id.as_ref().as_bytes();
    if !valid_drive_id(drive) {
        return Err(FrameError::InvalidLength);
    }
    let (kind, payload, count) = match data {
        Some(b) => (Kind::Write, b.len(), 0),
        None => (Kind::Read, 0, length),
    };
    let control_len = IO_PREFIX_BYTES + drive.len();
    let header = Header::new(kind, id, control_len, payload, count)?;
    let mut metadata = [0; MAX_IO_CONTROL_BYTES];
    metadata[0] = drive.len() as u8;
    metadata[1] = u8::from(request.position.is_some());
    metadata[2..10].copy_from_slice(&request.handle.to_be_bytes());
    metadata[10..18].copy_from_slice(&request.position.unwrap_or(0).to_be_bytes());
    metadata[18..control_len].copy_from_slice(drive);
    emit(
        w,
        header,
        &metadata[..control_len],
        data.unwrap_or_default(),
    )
    .await
}

/// Read typed I/O metadata into bounded inline storage, leaving raw payload unread.
pub async fn read_io_metadata<R: AsyncRead + Unpin>(
    r: &mut R,
    h: Header,
) -> Result<IoRequest<InlineDriveId>, FrameError> {
    Header::new(h.kind, h.request_id, h.control_len, h.payload_len, h.count)?;
    if !matches!(h.kind, Kind::Read | Kind::Write) {
        return Err(FrameError::InvalidLength);
    }
    let mut metadata = [0; MAX_IO_CONTROL_BYTES];
    r.read_exact(&mut metadata[..h.control_len]).await?;
    let len = usize::from(metadata[0]);
    let flags = metadata[1];
    let position = u64::from_be_bytes(metadata[10..18].try_into().unwrap());
    if flags & !1 != 0
        || h.control_len != IO_PREFIX_BYTES + len
        || (flags == 0 && position != 0)
        || !valid_drive_id(&metadata[IO_PREFIX_BYTES..h.control_len])
    {
        return Err(FrameError::InvalidLength);
    }
    let mut bytes = [0; MAX_DRIVE_ID_BYTES];
    bytes[..len].copy_from_slice(&metadata[IO_PREFIX_BYTES..h.control_len]);
    Ok(IoRequest {
        drive_id: InlineDriveId {
            bytes,
            len: len as u8,
        },
        handle: u64::from_be_bytes(metadata[2..10].try_into().unwrap()),
        position: (flags == 1).then_some(position),
    })
}
pub async fn read_body<R: AsyncRead + Unpin>(r: &mut R, h: Header) -> Result<Incoming, FrameError> {
    Header::new(h.kind, h.request_id, h.control_len, h.payload_len, h.count)?;
    if !matches!(h.kind, Kind::Control | Kind::Read | Kind::Write) {
        return Err(FrameError::InvalidLength);
    }
    if h.kind == Kind::Control {
        let mut b = vec![0; h.control_len];
        r.read_exact(&mut b).await?;
        return Ok(Incoming::Control(serde_json::from_slice(&b)?));
    }
    let request = read_io_metadata(r, h).await?;
    if h.kind == Kind::Read {
        return Ok(Incoming::Read {
            request_id: h.request_id,
            request,
            length: h.count,
        });
    }
    let mut data = vec![0; h.payload_len];
    r.read_exact(&mut data).await?;
    Ok(Incoming::Write {
        request_id: h.request_id,
        request,
        data,
    })
}
pub async fn write_result<W: AsyncWrite + Unpin>(
    w: &mut W,
    id: u64,
    result: Result<IoResult, WireError>,
) -> Result<(), FrameError> {
    match result {
        Ok(IoResult::Read(data)) => {
            emit(
                w,
                Header::new(Kind::ReadResult, id, 0, data.len(), data.len())?,
                &[],
                &data,
            )
            .await
        }
        Ok(IoResult::Write(count)) => {
            emit(
                w,
                Header::new(Kind::WriteResult, id, 0, 0, count)?,
                &[],
                &[],
            )
            .await
        }
        Err(e) => {
            let code = e.code.as_bytes();
            if code.is_empty() || code.len() > 64 {
                return Err(FrameError::InvalidLength);
            }
            emit(
                w,
                Header::new(Kind::Error, id, code.len(), 0, 0)?,
                code,
                &[],
            )
            .await
        }
    }
}
#[derive(Debug)]
pub enum IoResult {
    Read(Vec<u8>),
    Write(usize),
}
/// Validate kind, ID and caller bounds before touching the caller buffer.
pub async fn read_result<R: AsyncRead + Unpin>(
    r: &mut R,
    id: u64,
    read: bool,
    buffer: &mut [u8],
    write_limit: usize,
) -> Result<Result<usize, WireError>, FrameError> {
    let h = Header::read(r).await?;
    if h.request_id != id {
        return Err(FrameError::InvalidLength);
    }
    if h.kind == Kind::Error {
        let mut b = vec![0; h.control_len];
        r.read_exact(&mut b).await?;
        let code = String::from_utf8(b).map_err(|_| FrameError::InvalidLength)?;
        if !code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            return Err(FrameError::InvalidLength);
        }
        return Ok(Err(WireError { code }));
    }
    if read {
        if h.kind != Kind::ReadResult || h.payload_len > buffer.len() {
            return Err(FrameError::InvalidLength);
        }
        r.read_exact(&mut buffer[..h.payload_len]).await?;
    } else if h.kind != Kind::WriteResult || h.count > write_limit {
        return Err(FrameError::InvalidLength);
    }
    Ok(Ok(h.count))
}

#[cfg(kani)]
mod proofs {
    use super::*;
    #[kani::proof]
    #[kani::unwind(7)]
    fn binary_io_lengths_bounded_before_allocation() {
        let payload: usize = kani::any();
        let count: usize = kani::any();
        let control: usize = kani::any();
        let id: u64 = kani::any();
        for kind in [
            Kind::Control,
            Kind::Read,
            Kind::Write,
            Kind::ReadResult,
            Kind::WriteResult,
            Kind::Error,
        ] {
            let accepted = valid_lengths(kind, id, control, payload, count);
            if accepted {
                assert!(control <= MAX_CONTROL_BYTES);
                assert!(payload <= MAX_IO_BYTES);
                assert!(count <= MAX_IO_BYTES);
                if kind != Kind::Control {
                    assert!(id != 0);
                }
                if kind == Kind::ReadResult {
                    assert_eq!(payload, count);
                    assert_eq!(control, 0);
                }
                if matches!(kind, Kind::Read | Kind::Write) {
                    assert!(control >= MIN_IO_CONTROL_BYTES);
                    assert!(control <= MAX_IO_CONTROL_BYTES);
                    assert!(control - IO_PREFIX_BYTES <= MAX_DRIVE_ID_BYTES);
                }
                if kind == Kind::Write {
                    assert_eq!(count, 0);
                }
            }
        }
        kani::cover!(
            valid_lengths(Kind::Write, id, control, payload, count) && payload == MAX_IO_BYTES
        );
        kani::cover!(
            payload > MAX_IO_BYTES && !valid_lengths(Kind::Write, id, control, payload, count)
        );
        kani::cover!(valid_lengths(Kind::ReadResult, id, control, payload, count) && payload == 0);
    }
}
