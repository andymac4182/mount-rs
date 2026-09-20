use std::fmt;

/// Linux/POSIX error codes used by the mountx driver contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    Eperm,
    Enoent,
    Eintr,
    Eio,
    Enxio,
    Ebadf,
    Eagain,
    Enomem,
    Eacces,
    Ebusy,
    Eexist,
    Exdev,
    Enodev,
    Enotdir,
    Eisdir,
    Einval,
    Enfile,
    Emfile,
    Efbig,
    Enospc,
    Espipe,
    Erofs,
    Emlink,
    Erange,
    Enametoolong,
    Enosys,
    Enotempty,
    Eloop,
    Enodata,
    Eproto,
    Eoverflow,
    Enotsup,
    Estale,
    Edquot,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eperm => "EPERM",
            Self::Enoent => "ENOENT",
            Self::Eintr => "EINTR",
            Self::Eio => "EIO",
            Self::Enxio => "ENXIO",
            Self::Ebadf => "EBADF",
            Self::Eagain => "EAGAIN",
            Self::Enomem => "ENOMEM",
            Self::Eacces => "EACCES",
            Self::Ebusy => "EBUSY",
            Self::Eexist => "EEXIST",
            Self::Exdev => "EXDEV",
            Self::Enodev => "ENODEV",
            Self::Enotdir => "ENOTDIR",
            Self::Eisdir => "EISDIR",
            Self::Einval => "EINVAL",
            Self::Enfile => "ENFILE",
            Self::Emfile => "EMFILE",
            Self::Efbig => "EFBIG",
            Self::Enospc => "ENOSPC",
            Self::Espipe => "ESPIPE",
            Self::Erofs => "EROFS",
            Self::Emlink => "EMLINK",
            Self::Erange => "ERANGE",
            Self::Enametoolong => "ENAMETOOLONG",
            Self::Enosys => "ENOSYS",
            Self::Enotempty => "ENOTEMPTY",
            Self::Eloop => "ELOOP",
            Self::Enodata => "ENODATA",
            Self::Eproto => "EPROTO",
            Self::Eoverflow => "EOVERFLOW",
            Self::Enotsup => "ENOTSUP",
            Self::Estale => "ESTALE",
            Self::Edquot => "EDQUOT",
        }
    }

    pub const fn errno(self) -> i32 {
        match self {
            Self::Eperm => 1,
            Self::Enoent => 2,
            Self::Eintr => 4,
            Self::Eio => 5,
            Self::Enxio => 6,
            Self::Ebadf => 9,
            Self::Eagain => 11,
            Self::Enomem => 12,
            Self::Eacces => 13,
            Self::Ebusy => 16,
            Self::Eexist => 17,
            Self::Exdev => 18,
            Self::Enodev => 19,
            Self::Enotdir => 20,
            Self::Eisdir => 21,
            Self::Einval => 22,
            Self::Enfile => 23,
            Self::Emfile => 24,
            Self::Efbig => 27,
            Self::Enospc => 28,
            Self::Espipe => 29,
            Self::Erofs => 30,
            Self::Emlink => 31,
            Self::Erange => 34,
            Self::Enametoolong => 36,
            Self::Enosys => 38,
            Self::Enotempty => 39,
            Self::Eloop => 40,
            Self::Enodata => 61,
            Self::Eproto => 71,
            Self::Eoverflow => 75,
            Self::Enotsup => 95,
            Self::Estale => 116,
            Self::Edquot => 122,
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Eperm => "operation not permitted",
            Self::Enoent => "no such file or directory",
            Self::Eintr => "interrupted system call",
            Self::Eio => "i/o error",
            Self::Enxio => "no such device or address",
            Self::Ebadf => "bad file descriptor",
            Self::Eagain => "resource temporarily unavailable",
            Self::Enomem => "not enough memory",
            Self::Eacces => "permission denied",
            Self::Ebusy => "resource busy or locked",
            Self::Eexist => "file already exists",
            Self::Exdev => "cross-device link not permitted",
            Self::Enodev => "no such device",
            Self::Enotdir => "not a directory",
            Self::Eisdir => "illegal operation on a directory",
            Self::Einval => "invalid argument",
            Self::Enfile => "file table overflow",
            Self::Emfile => "too many open files",
            Self::Efbig => "file too large",
            Self::Enospc => "no space left on device",
            Self::Espipe => "invalid seek",
            Self::Erofs => "read-only file system",
            Self::Emlink => "too many links",
            Self::Erange => "result too large",
            Self::Enametoolong => "name too long",
            Self::Enosys => "function not implemented",
            Self::Enotempty => "directory not empty",
            Self::Eloop => "too many symbolic links encountered",
            Self::Enodata => "no data available",
            Self::Eproto => "protocol error",
            Self::Eoverflow => "value too large for defined data type",
            Self::Enotsup => "operation not supported",
            Self::Estale => "stale file handle",
            Self::Edquot => "disk quota exceeded",
        }
    }
}

/// Filesystem error carrying the same observable POSIX code information as
/// the TypeScript implementation.
#[derive(Debug, Clone)]
pub struct FsError {
    pub code: ErrorCode,
    pub syscall: Option<String>,
    pub path: Option<String>,
    pub dest: Option<String>,
    message: Option<String>,
}

impl FsError {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            syscall: None,
            path: None,
            dest: None,
            message: None,
        }
    }

    pub fn with_syscall(mut self, syscall: impl Into<String>) -> Self {
        self.syscall = Some(syscall.into());
        self
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_dest(mut self, dest: impl Into<String>) -> Self {
        self.dest = Some(dest.into());
        self
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    pub fn backend(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Eio).with_message(message)
    }

    pub fn enosys(syscall: impl Into<String>) -> Self {
        Self::new(ErrorCode::Enosys).with_syscall(syscall)
    }

    pub fn enotsup(syscall: impl Into<String>) -> Self {
        Self::new(ErrorCode::Enotsup).with_syscall(syscall)
    }

    pub fn is(&self, code: ErrorCode) -> bool {
        self.code == code
    }
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(message) = &self.message {
            return f.write_str(message);
        }
        write!(f, "{}: {}", self.code.as_str(), self.code.description())?;
        if let Some(syscall) = &self.syscall {
            write!(f, ", {syscall}")?;
        }
        if let Some(path) = &self.path {
            write!(f, " '{path}'")?;
        }
        if let Some(dest) = &self.dest {
            write!(f, " -> '{dest}'")?;
        }
        Ok(())
    }
}

impl std::error::Error for FsError {}

pub type Result<T> = std::result::Result<T, FsError>;

/// Convert an arbitrary backend error to a safe filesystem error.
pub fn backend_error(error: impl fmt::Display) -> FsError {
    FsError::backend(error.to_string())
}
