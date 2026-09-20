use crate::error::{ErrorCode, FsError, Result};

/// The normalized meaning of the string form accepted by `open(2)` callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFlags {
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub truncate: bool,
    pub append: bool,
    pub exclusive: bool,
}

impl OpenFlags {
    pub const READ_ONLY: Self = Self {
        read: true,
        write: false,
        create: false,
        truncate: false,
        append: false,
        exclusive: false,
    };

    pub fn parse(value: &str, path: &str) -> Result<Self> {
        let normalized = value.replace('s', "");
        let flags = match normalized.as_str() {
            "r" => Self {
                read: true,
                write: false,
                create: false,
                truncate: false,
                append: false,
                exclusive: false,
            },
            "r+" => Self {
                read: true,
                write: true,
                create: false,
                truncate: false,
                append: false,
                exclusive: false,
            },
            "w" => Self {
                read: false,
                write: true,
                create: true,
                truncate: true,
                append: false,
                exclusive: false,
            },
            "wx" => Self {
                read: false,
                write: true,
                create: true,
                truncate: true,
                append: false,
                exclusive: true,
            },
            "w+" => Self {
                read: true,
                write: true,
                create: true,
                truncate: true,
                append: false,
                exclusive: false,
            },
            "wx+" => Self {
                read: true,
                write: true,
                create: true,
                truncate: true,
                append: false,
                exclusive: true,
            },
            "a" => Self {
                read: false,
                write: true,
                create: true,
                truncate: false,
                append: true,
                exclusive: false,
            },
            "ax" => Self {
                read: false,
                write: true,
                create: true,
                truncate: false,
                append: true,
                exclusive: true,
            },
            "a+" => Self {
                read: true,
                write: true,
                create: true,
                truncate: false,
                append: true,
                exclusive: false,
            },
            "ax+" => Self {
                read: true,
                write: true,
                create: true,
                truncate: false,
                append: true,
                exclusive: true,
            },
            _ => {
                return Err(FsError::new(ErrorCode::Einval)
                    .with_syscall("open")
                    .with_path(path)
                    .with_message(format!("EINVAL: invalid open flags: '{value}'")));
            }
        };
        Ok(flags)
    }

    /// Parse the Linux numeric `O_*` namespace used by Linux wire protocols.
    /// This is not the macOS/Node native flag namespace. Native callers must
    /// decode their host constants before passing `OpenFlags` to a driver.
    pub fn from_bits(bits: u64) -> Self {
        const O_WRONLY: u64 = 1;
        const O_RDWR: u64 = 2;
        const O_CREAT: u64 = 0o100;
        const O_EXCL: u64 = 0o200;
        const O_TRUNC: u64 = 0o1000;
        const O_APPEND: u64 = 0o2000;
        let access = bits & 3;
        Self {
            read: access == 0 || access == O_RDWR,
            write: access == O_WRONLY || access == O_RDWR,
            create: bits & O_CREAT != 0,
            truncate: bits & O_TRUNC != 0,
            append: bits & O_APPEND != 0,
            exclusive: bits & O_EXCL != 0,
        }
    }
}

/// Validate a file position before converting it to an allocation index.
pub fn checked_position(position: u64) -> Result<usize> {
    usize::try_from(position).map_err(|_| FsError::new(ErrorCode::Efbig).with_syscall("seek"))
}
