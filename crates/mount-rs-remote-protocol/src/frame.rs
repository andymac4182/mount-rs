use std::fmt;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::Message;

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

/// Generic messages use the same bounded binary envelope as typed I/O.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
) -> Result<(), FrameError> {
    crate::binary::write_control(writer, message).await
}
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Message, FrameError> {
    crate::binary::read_control(reader).await
}
