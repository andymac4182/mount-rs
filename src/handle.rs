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

    /// A decoded truncation request must also grant write access to the handle.
    /// Other decoded combinations, including read-only creation, are allowed.
    pub const fn has_valid_truncate_access(self) -> bool {
        !self.truncate || self.write
    }

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

#[cfg(kani)]
mod verification {
    use super::OpenFlags;

    #[kani::proof]
    fn decoded_truncation_requires_write() {
        // Six symbolic booleans cover every one of the 2^6 public combinations.
        let flags = OpenFlags {
            read: kani::any(),
            write: kani::any(),
            create: kani::any(),
            truncate: kani::any(),
            append: kani::any(),
            exclusive: kani::any(),
        };
        let accepted = flags.has_valid_truncate_access();

        kani::cover!(accepted && flags.truncate && flags.write);
        kani::cover!(!accepted && flags.truncate && !flags.write);
        kani::cover!(accepted && flags.create && !flags.write && !flags.truncate);

        if accepted && flags.truncate {
            assert!(flags.write);
        }
        if !accepted {
            assert!(flags.truncate);
            assert!(!flags.write);
        }
    }

    #[kani::proof]
    fn linux_wire_open_bits_preserve_access_and_truncate_guard() {
        const O_CREAT: u64 = 0o100;
        const O_EXCL: u64 = 0o200;
        const O_TRUNC: u64 = 0o1000;
        const O_APPEND: u64 = 0o2000;
        const KNOWN: u64 = 3 | O_CREAT | O_EXCL | O_TRUNC | O_APPEND;

        // Transport callers pass arbitrary numeric wire bits. Unknown Linux
        // flags must not silently grant read/write or bypass truncate policy.
        let bits: u64 = kani::any();
        let access = bits & 3;
        let flags = OpenFlags::from_bits(bits);
        let without_unknown_bits = OpenFlags::from_bits(bits & KNOWN);

        assert_eq!(flags, without_unknown_bits);
        assert_eq!(flags.read, access == 0 || access == 2);
        assert_eq!(flags.write, access == 1 || access == 2);
        assert_eq!(flags.create, bits & O_CREAT != 0);
        assert_eq!(flags.exclusive, bits & O_EXCL != 0);
        assert_eq!(flags.truncate, bits & O_TRUNC != 0);
        assert_eq!(flags.append, bits & O_APPEND != 0);
        assert_eq!(
            flags.has_valid_truncate_access(),
            bits & O_TRUNC == 0 || access == 1 || access == 2
        );

        kani::cover!(access == 0 && flags.truncate && !flags.has_valid_truncate_access());
        kani::cover!(access == 2 && flags.truncate && flags.has_valid_truncate_access());
        kani::cover!(access == 3 && flags.truncate && !flags.has_valid_truncate_access());
        kani::cover!(bits & !KNOWN != 0 && flags == without_unknown_bits);
    }
}

#[cfg(test)]
mod tests {
    use super::OpenFlags;

    #[test]
    fn linux_wire_bits_preserve_access_and_truncate_guard() {
        for (access, read, write) in [
            (0, true, false),
            (1, false, true),
            (2, true, true),
            (3, false, false),
        ] {
            let flags = OpenFlags::from_bits(access | 0o1000);
            assert_eq!((flags.read, flags.write), (read, write));
            assert_eq!(flags.has_valid_truncate_access(), write);
            assert_eq!(OpenFlags::from_bits(access | 0o1000 | (1 << 63)), flags);
        }

        let flags = OpenFlags::from_bits(0o100 | 0o200 | 0o2000 | 2);
        assert!(flags.read && flags.write);
        assert!(flags.create && flags.exclusive && flags.append);
        assert!(!flags.truncate);
    }
}
