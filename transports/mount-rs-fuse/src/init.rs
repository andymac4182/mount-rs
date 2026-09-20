//! Pure FUSE version and capability negotiation, independent of host OS.
//!
//! The two `u32` flag words in `fuse_init_in`/`fuse_init_out` are exposed as a
//! joined `u64` in this module.  That keeps negotiation lossless while the
//! protocol codec remains faithful to the wire layout.
use crate::constants::{
    FUSE_ASYNC_DIO, FUSE_ASYNC_READ, FUSE_ATOMIC_O_TRUNC, FUSE_AUTO_INVAL_DATA, FUSE_BIG_WRITES,
    FUSE_DEFAULT_MAX_PAGES_PER_REQ, FUSE_DO_READDIRPLUS, FUSE_INIT_EXT, FUSE_KERNEL_MINOR_VERSION,
    FUSE_KERNEL_VERSION, FUSE_MAX_MAX_PAGES, FUSE_MAX_PAGES, FUSE_PAGE_SIZE, FUSE_PARALLEL_DIROPS,
    FUSE_READDIRPLUS_AUTO, FUSE_SETXATTR_EXT,
};
use crate::protocol::{FuseInitIn, FuseInitOut};

/// Flags requested by the native session's default negotiation.
pub const DEFAULT_WANTED_FLAGS: u64 = FUSE_ASYNC_READ
    | FUSE_ATOMIC_O_TRUNC
    | FUSE_BIG_WRITES
    | FUSE_AUTO_INVAL_DATA
    | FUSE_DO_READDIRPLUS
    | FUSE_READDIRPLUS_AUTO
    | FUSE_ASYNC_DIO
    | FUSE_PARALLEL_DIROPS
    | FUSE_MAX_PAGES
    | FUSE_SETXATTR_EXT;

/// Compatibility alias retained for the existing native session API.
pub const DEFAULT_FLAGS: u64 = DEFAULT_WANTED_FLAGS;

/// Default maximum write requested by the mountx init surface.
pub const DEFAULT_MAX_WRITE: u32 = 1024 * 1024;

/// Join the two little-endian `fuse_init_*` flag words into one lossless value.
pub const fn join_init_flags(flags: u32, flags2: u32) -> u64 {
    flags as u64 | ((flags2 as u64) << 32)
}

/// Split the public joined flag value back into the two wire words.
pub const fn split_init_flags(flags: u64) -> (u32, u32) {
    (flags as u32, (flags >> 32) as u32)
}

#[derive(Debug, Clone, Copy)]
pub struct KernelInit {
    pub major: u32,
    pub minor: u32,
    pub max_readahead: u32,
    pub flags: u32,
    pub flags2: u32,
}

impl From<FuseInitIn> for KernelInit {
    fn from(value: FuseInitIn) -> Self {
        Self {
            major: value.major,
            minor: value.minor,
            max_readahead: value.max_readahead,
            flags: value.flags,
            flags2: value.flags2,
        }
    }
}

impl From<&FuseInitIn> for KernelInit {
    fn from(value: &FuseInitIn) -> Self {
        (*value).into()
    }
}
#[derive(Debug, Clone)]
pub struct Preferences {
    pub minor: u32,
    pub flags: u64,
    pub max_write: u32,
    pub max_readahead: Option<u32>,
    pub max_background: u16,
    pub congestion_threshold: Option<u16>,
    pub time_gran: u32,
    pub max_stack_depth: u32,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            minor: FUSE_KERNEL_MINOR_VERSION,
            flags: DEFAULT_WANTED_FLAGS,
            max_write: DEFAULT_MAX_WRITE,
            max_readahead: None,
            max_background: 64,
            congestion_threshold: None,
            time_gran: 1,
            max_stack_depth: 0,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReply {
    pub major: u32,
    pub minor: u32,
    pub max_readahead: u32,
    pub flags: u64,
    pub max_background: u16,
    pub congestion_threshold: u16,
    pub max_write: u32,
    pub time_gran: u32,
    pub max_pages: u16,
    pub max_stack_depth: u32,
}

impl InitReply {
    /// Convert the negotiated reply to the public wire-codec representation.
    pub fn as_wire(&self) -> FuseInitOut {
        let (flags, flags2) = split_init_flags(self.flags);
        FuseInitOut {
            major: self.major,
            minor: self.minor,
            max_readahead: self.max_readahead,
            flags,
            max_background: self.max_background,
            congestion_threshold: self.congestion_threshold,
            max_write: self.max_write,
            time_gran: self.time_gran,
            max_pages: self.max_pages,
            map_alignment: 0,
            flags2,
            max_stack_depth: self.max_stack_depth,
        }
    }

    /// Convert a wire reply into the joined-flag negotiation representation.
    pub fn from_wire(value: FuseInitOut) -> Self {
        Self {
            major: value.major,
            minor: value.minor,
            max_readahead: value.max_readahead,
            flags: join_init_flags(value.flags, value.flags2),
            max_background: value.max_background,
            congestion_threshold: value.congestion_threshold,
            max_write: value.max_write,
            time_gran: value.time_gran,
            max_pages: value.max_pages,
            max_stack_depth: value.max_stack_depth,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Negotiation {
    Ready(InitReply),
    Retry(InitReply),
    UnsupportedMajor,
}

impl InitReply {
    pub fn encode(&self) -> Vec<u8> {
        crate::protocol::encode_init_out(&self.as_wire(), None)
            .expect("negotiated init reply has a valid protocol layout")
    }
}

pub fn negotiate(kernel: KernelInit, preferences: &Preferences) -> Negotiation {
    let mut reply = InitReply {
        major: 7,
        minor: preferences.minor,
        max_readahead: 0,
        flags: 0,
        max_background: 0,
        congestion_threshold: 0,
        max_write: 0,
        time_gran: 0,
        max_pages: 0,
        max_stack_depth: 0,
    };
    if kernel.major > FUSE_KERNEL_VERSION {
        return Negotiation::Retry(reply);
    }
    if kernel.major < FUSE_KERNEL_VERSION {
        return Negotiation::UnsupportedMajor;
    }
    reply.minor = kernel.minor.min(preferences.minor);
    let offered_flags = if reply.minor >= 36 && u64::from(kernel.flags) & FUSE_INIT_EXT != 0 {
        join_init_flags(kernel.flags, kernel.flags2)
    } else {
        u64::from(kernel.flags)
    };
    reply.flags = preferences.flags & offered_flags;
    if reply.flags >> 32 != 0 {
        reply.flags |= FUSE_INIT_EXT;
    } else {
        reply.flags &= !FUSE_INIT_EXT;
    }
    reply.max_write = preferences.max_write;
    if reply.minor >= 28 && reply.flags & FUSE_MAX_PAGES != 0 {
        let requested_pages = u64::from(reply.max_write).div_ceil(FUSE_PAGE_SIZE as u64);
        reply.max_pages = requested_pages.clamp(1, u64::from(FUSE_MAX_MAX_PAGES)) as u16;
        reply.max_write = reply
            .max_write
            .min(u32::from(reply.max_pages) * FUSE_PAGE_SIZE as u32);
    } else {
        reply.flags &= !FUSE_MAX_PAGES;
        reply.max_write = reply
            .max_write
            .min(u32::from(FUSE_DEFAULT_MAX_PAGES_PER_REQ) * FUSE_PAGE_SIZE as u32);
    }
    reply.max_write = if reply.minor < 5 {
        FUSE_PAGE_SIZE as u32
    } else {
        reply.max_write.max(FUSE_PAGE_SIZE as u32)
    };
    reply.max_readahead = preferences
        .max_readahead
        .unwrap_or(kernel.max_readahead)
        .min(kernel.max_readahead);
    if reply.minor >= 13 {
        reply.max_background = preferences.max_background;
        reply.congestion_threshold = preferences
            .congestion_threshold
            .unwrap_or(((u32::from(preferences.max_background) * 3 / 4).max(1)) as u16);
    }
    if reply.minor >= 23 {
        reply.time_gran = preferences.time_gran;
    }
    if reply.minor >= 40 {
        reply.max_stack_depth = preferences.max_stack_depth;
    }
    Negotiation::Ready(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::encode_init_out;

    fn kernel(flags: u64, minor: u32) -> KernelInit {
        let (flags, flags2) = split_init_flags(flags);
        KernelInit {
            major: FUSE_KERNEL_VERSION,
            minor,
            max_readahead: 131_072,
            flags,
            flags2,
        }
    }

    #[test]
    fn joins_and_splits_wire_flag_words() {
        let value = join_init_flags(0x8000_0000, 3);
        assert_eq!(value, 0x0000_0003_8000_0000);
        assert_eq!(split_init_flags(value), (0x8000_0000, 3));
        assert_eq!(
            split_init_flags(join_init_flags(u32::MAX, u32::MAX)),
            (u32::MAX, u32::MAX)
        );
    }

    #[test]
    fn negotiates_default_max_pages_and_wire_round_trip() {
        let result = negotiate(
            kernel(DEFAULT_WANTED_FLAGS, FUSE_KERNEL_MINOR_VERSION),
            &Preferences::default(),
        );
        let Negotiation::Ready(reply) = result else {
            panic!("expected a ready negotiation");
        };
        assert_eq!(reply.minor, FUSE_KERNEL_MINOR_VERSION);
        assert_eq!(reply.max_write, DEFAULT_MAX_WRITE);
        assert_eq!(reply.max_pages, FUSE_MAX_MAX_PAGES);
        assert_eq!(reply.max_background, 64);
        assert_eq!(reply.congestion_threshold, 48);
        assert_eq!(InitReply::from_wire(decode_wire(&reply)), reply);
    }

    fn decode_wire(reply: &InitReply) -> FuseInitOut {
        crate::protocol::decode_init_out(&encode_init_out(&reply.as_wire(), None).unwrap()).unwrap()
    }

    #[test]
    fn follows_old_minor_layout_and_major_retry_rules() {
        let old = negotiate(kernel(DEFAULT_WANTED_FLAGS, 4), &Preferences::default());
        let Negotiation::Ready(old) = old else {
            panic!("expected old kernel negotiation");
        };
        assert_eq!(old.max_write, FUSE_PAGE_SIZE as u32);
        assert_eq!(old.encode().len(), 8);

        let mut future = kernel(DEFAULT_WANTED_FLAGS, FUSE_KERNEL_MINOR_VERSION);
        future.major += 1;
        assert!(matches!(
            negotiate(future, &Preferences::default()),
            Negotiation::Retry(_)
        ));

        let mut ancient = kernel(DEFAULT_WANTED_FLAGS, FUSE_KERNEL_MINOR_VERSION);
        ancient.major -= 1;
        assert!(matches!(
            negotiate(ancient, &Preferences::default()),
            Negotiation::UnsupportedMajor
        ));
    }

    #[test]
    fn extended_flags_require_the_kernel_extension() {
        let high = crate::constants::FUSE_HAS_EXPIRE_ONLY;
        let offered = FUSE_INIT_EXT | high;
        let ready = negotiate(
            kernel(offered, FUSE_KERNEL_MINOR_VERSION),
            &Preferences {
                flags: high,
                ..Preferences::default()
            },
        );
        let Negotiation::Ready(reply) = ready else {
            panic!("expected extended negotiation");
        };
        assert_eq!(reply.flags & high, high);
        assert_eq!(reply.flags & FUSE_INIT_EXT, FUSE_INIT_EXT);

        let no_extension = negotiate(
            kernel(high, FUSE_KERNEL_MINOR_VERSION),
            &Preferences {
                flags: high,
                ..Preferences::default()
            },
        );
        let Negotiation::Ready(reply) = no_extension else {
            panic!("expected non-extended negotiation");
        };
        assert_eq!(reply.flags & high, 0);
    }
}
