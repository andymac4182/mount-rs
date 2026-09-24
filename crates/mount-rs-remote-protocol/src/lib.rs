//! Wire contract for remote mount-rs Drives.

mod frame;
mod messages;

pub use frame::{FrameError, read_frame, write_frame};
pub use messages::{Message, Operation, OperationName, Permission, WireError};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
