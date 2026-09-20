use serde::{Deserialize, Serialize};

pub const S_IFMT: u32 = 0o170000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFSOCK: u32 = 0o140000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileType {
    File,
    Directory,
    Symlink,
    BlockDevice,
    CharacterDevice,
    Fifo,
    Socket,
}

impl FileType {
    pub const fn mode_bits(self) -> u32 {
        match self {
            Self::File => S_IFREG,
            Self::Directory => S_IFDIR,
            Self::Symlink => S_IFLNK,
            Self::BlockDevice => S_IFBLK,
            Self::CharacterDevice => S_IFCHR,
            Self::Fifo => S_IFIFO,
            Self::Socket => S_IFSOCK,
        }
    }

    pub const fn from_mode(mode: u32) -> Self {
        match mode & S_IFMT {
            S_IFDIR => Self::Directory,
            S_IFLNK => Self::Symlink,
            S_IFBLK => Self::BlockDevice,
            S_IFCHR => Self::CharacterDevice,
            S_IFIFO => Self::Fifo,
            S_IFSOCK => Self::Socket,
            _ => Self::File,
        }
    }

    pub const fn is_special(self) -> bool {
        matches!(
            self,
            Self::BlockDevice | Self::CharacterDevice | Self::Fifo | Self::Socket
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub nlink: u64,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
    pub size: u64,
    pub blksize: u64,
    pub blocks: u64,
    pub atime_ms: i64,
    pub mtime_ms: i64,
    pub ctime_ms: i64,
    pub birthtime_ms: i64,
}

impl Stats {
    pub const fn file_type(&self) -> FileType {
        FileType::from_mode(self.mode)
    }

    pub const fn is_file(&self) -> bool {
        matches!(self.file_type(), FileType::File)
    }

    pub const fn is_directory(&self) -> bool {
        matches!(self.file_type(), FileType::Directory)
    }

    pub const fn is_symbolic_link(&self) -> bool {
        matches!(self.file_type(), FileType::Symlink)
    }

    pub const fn is_block_device(&self) -> bool {
        matches!(self.file_type(), FileType::BlockDevice)
    }

    pub const fn is_character_device(&self) -> bool {
        matches!(self.file_type(), FileType::CharacterDevice)
    }

    pub const fn is_fifo(&self) -> bool {
        matches!(self.file_type(), FileType::Fifo)
    }

    pub const fn is_socket(&self) -> bool {
        matches!(self.file_type(), FileType::Socket)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub parent_path: String,
    pub file_type: FileType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsFs {
    pub filesystem_type: u64,
    pub block_size: u64,
    pub blocks: u64,
    pub blocks_free: u64,
    pub blocks_available: u64,
    pub files: u64,
    pub files_free: u64,
}

impl DirEntry {
    pub const fn is_file(&self) -> bool {
        matches!(self.file_type, FileType::File)
    }
    pub const fn is_directory(&self) -> bool {
        matches!(self.file_type, FileType::Directory)
    }
    pub const fn is_symbolic_link(&self) -> bool {
        matches!(self.file_type, FileType::Symlink)
    }
    pub const fn is_block_device(&self) -> bool {
        matches!(self.file_type, FileType::BlockDevice)
    }
    pub const fn is_character_device(&self) -> bool {
        matches!(self.file_type, FileType::CharacterDevice)
    }
    pub const fn is_fifo(&self) -> bool {
        matches!(self.file_type, FileType::Fifo)
    }
    pub const fn is_socket(&self) -> bool {
        matches!(self.file_type, FileType::Socket)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub handles: bool,
    pub hardlinks: bool,
    pub symlinks: bool,
    pub permissions: bool,
    pub times: bool,
    pub truncate: bool,
    pub atomic_rename: bool,
    pub case_sensitive: bool,
    pub statfs: bool,
    pub read_only: bool,
    pub durable_writes: bool,
    pub mknod: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MkdirOptions {
    pub recursive: bool,
    pub mode: Option<u32>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn file_type_mode(mode: u32) -> FileType {
    FileType::from_mode(mode)
}
