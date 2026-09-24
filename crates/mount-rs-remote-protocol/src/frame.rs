use std::fmt;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{MAX_FRAME_BYTES, Message};

#[derive(Debug)]
pub enum FrameError {
    Io(std::io::Error),
    Json(serde_json::Error),
    InvalidLength,
}

impl fmt::Display for FrameError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => output.write_str("remote stream I/O failed"),
            Self::Json(_) => output.write_str("invalid remote frame"),
            Self::InvalidLength => output.write_str("remote frame size exceeds limit"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<std::io::Error> for FrameError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for FrameError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
) -> Result<(), FrameError> {
    let body = serde_json::to_vec(message)?;
    if body.is_empty() || body.len() > MAX_FRAME_BYTES {
        return Err(FrameError::InvalidLength);
    }
    let length = u32::try_from(body.len()).map_err(|_| FrameError::InvalidLength)?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Message, FrameError> {
    let mut prefix = [0_u8; 4];
    reader.read_exact(&mut prefix).await?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(FrameError::InvalidLength);
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}
