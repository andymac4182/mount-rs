//! Pure FUSE version and capability negotiation, independent of host OS.
pub const DEFAULT_FLAGS: u64 = (1 << 0)
    | (1 << 3)
    | (1 << 5)
    | (1 << 12)
    | (1 << 13)
    | (1 << 14)
    | (1 << 15)
    | (1 << 18)
    | (1 << 22)
    | (1 << 29);
const INIT_EXT: u64 = 1 << 30;
const MAX_PAGES: u64 = 1 << 22;

#[derive(Debug, Clone, Copy)]
pub struct KernelInit {
    pub major: u32,
    pub minor: u32,
    pub max_readahead: u32,
    pub flags: u32,
    pub flags2: u32,
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
            minor: 41,
            flags: DEFAULT_FLAGS,
            max_write: 1024 * 1024,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Negotiation {
    Ready(InitReply),
    Retry(InitReply),
    UnsupportedMajor,
}

impl InitReply {
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend(self.major.to_le_bytes());
        body.extend(self.minor.to_le_bytes());
        if self.minor < 5 {
            return body;
        }
        body.extend(self.max_readahead.to_le_bytes());
        body.extend((self.flags as u32).to_le_bytes());
        body.extend(self.max_background.to_le_bytes());
        body.extend(self.congestion_threshold.to_le_bytes());
        body.extend(self.max_write.to_le_bytes());
        if self.minor < 23 {
            return body;
        }
        body.extend(self.time_gran.to_le_bytes());
        body.extend(self.max_pages.to_le_bytes());
        body.extend(0u16.to_le_bytes());
        body.extend(((self.flags >> 32) as u32).to_le_bytes());
        body.extend(self.max_stack_depth.to_le_bytes());
        body.resize(64, 0);
        body
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
    if kernel.major > 7 {
        return Negotiation::Retry(reply);
    }
    if kernel.major < 7 {
        return Negotiation::UnsupportedMajor;
    }
    reply.minor = kernel.minor.min(preferences.minor);
    let high = if reply.minor >= 36 && u64::from(kernel.flags) & INIT_EXT != 0 {
        u64::from(kernel.flags2) << 32
    } else {
        0
    };
    reply.flags = preferences.flags & (u64::from(kernel.flags) | high);
    if reply.flags >> 32 != 0 {
        reply.flags |= INIT_EXT;
    } else {
        reply.flags &= !INIT_EXT;
    }
    reply.max_write = preferences.max_write;
    if reply.minor >= 28 && reply.flags & MAX_PAGES != 0 {
        reply.max_pages = u64::from(reply.max_write).div_ceil(4096).clamp(1, 256) as u16;
        reply.max_write = reply.max_write.min(u32::from(reply.max_pages) * 4096);
    } else {
        reply.flags &= !MAX_PAGES;
        reply.max_write = reply.max_write.min(128 * 1024);
    }
    reply.max_write = if reply.minor < 5 {
        4096
    } else {
        reply.max_write.max(4096)
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
