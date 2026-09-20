//! Windows filesystem primitives matching Node/libuv's win/fs.c conventions.
//! ABI declarations are local so the host crate needs no new lockfile entries.

use std::ffi::c_void;
use std::fs::{self, File};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;

use mount_rs_core::{OpenFlags, Stats, StatsFs};

type Handle = *mut c_void;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const READ_ATTRIBUTES: u32 = 0x80;
const WRITE_ATTRIBUTES: u32 = 0x100;
const READONLY: u32 = 1;
const EPOCH_TICKS: i128 = 116_444_736_000_000_000;

#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}
#[repr(C)]
#[derive(Default)]
struct HandleInfo {
    attributes: u32,
    creation: FileTime,
    access: FileTime,
    write: FileTime,
    volume: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}
#[repr(C)]
#[derive(Default)]
struct BasicInfo {
    creation: i64,
    access: i64,
    write: i64,
    change: i64,
    attributes: u32,
}
#[repr(C)]
#[derive(Default)]
struct StandardInfo {
    allocation: i64,
    size: i64,
    links: u32,
    delete_pending: u8,
    directory: u8,
}
#[repr(C)]
#[derive(Default)]
struct IoStatus {
    status: usize,
    information: usize,
}
#[repr(C)]
#[derive(Default)]
struct FullSizeInfo {
    total: i64,
    caller_free: i64,
    actual_free: i64,
    sectors: u32,
    bytes_per_sector: u32,
}

// Catch accidental ABI layout changes even in cross-target cargo check.
const _: () = {
    assert!(size_of::<FileTime>() == 8);
    assert!(size_of::<HandleInfo>() == 52);
    assert!(size_of::<BasicInfo>() == 40);
    assert!(size_of::<StandardInfo>() == 24);
    assert!(size_of::<FullSizeInfo>() == 32);
    assert!(size_of::<IoStatus>() == 2 * size_of::<usize>());
};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        path: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        disposition: u32,
        attributes: u32,
        template: Handle,
    ) -> Handle;
    fn GetFileInformationByHandle(handle: Handle, info: *mut HandleInfo) -> i32;
    fn GetFileInformationByHandleEx(
        handle: Handle,
        class: i32,
        info: *mut c_void,
        size: u32,
    ) -> i32;
    fn SetFileTime(
        handle: Handle,
        creation: *const FileTime,
        access: *const FileTime,
        write: *const FileTime,
    ) -> i32;
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryVolumeInformationFile(
        handle: Handle,
        status: *mut IoStatus,
        info: *mut c_void,
        size: u32,
        class: i32,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
}

fn open_raw(path: &Path, access: u32, disposition: u32, attributes: u32) -> io::Result<File> {
    // Normalize separators before using extended-length paths (which disable
    // Win32 slash normalization). Preserve non-Unicode filenames as UTF-16.
    let absolute = std::path::absolute(path)?;
    let mut wide: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    for unit in &mut wide {
        if *unit == u16::from(b'/') {
            *unit = u16::from(b'\\');
        }
    }
    if !wide.starts_with(&[92, 92, 63, 92]) {
        let (prefix, rest) = if wide.starts_with(&[92, 92]) {
            (r"\\?\UNC\", &wide[2..])
        } else {
            (r"\\?\", wide.as_slice())
        };
        wide = prefix.encode_utf16().chain(rest.iter().copied()).collect();
    }
    wide.push(0);
    // SAFETY: NUL-terminated UTF-16 path and null optional arguments; all share
    // modes match libuv, including deleting/renaming a live handle.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            7,
            std::ptr::null(),
            disposition,
            attributes | BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == -1_isize as Handle {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful CreateFileW transferred one owned handle to us.
    Ok(unsafe { File::from_raw_handle(handle) })
}

pub(super) fn open(path: &Path, flags: OpenFlags, mode: u32) -> io::Result<File> {
    // Access and creation are independent in CreateFileW; OpenOptions rejects
    // read-only O_CREAT before reaching Windows, unlike Node/libuv.
    let mut access = if flags.read { 0x0012_0089 } else { 0 };
    if flags.write {
        access |= 0x0012_0116;
    }
    if access == 0 {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    if flags.append {
        access = (access & !2) | 4;
    }
    let disposition = match (flags.create, flags.exclusive, flags.truncate) {
        (true, true, _) => 1,      // CREATE_NEW
        (true, false, true) => 2,  // CREATE_ALWAYS
        (false, _, false) => 3,    // OPEN_EXISTING
        (true, false, false) => 4, // OPEN_ALWAYS
        (false, _, true) => 5,     // TRUNCATE_EXISTING
    };
    let attributes = 0x80
        | if flags.create && mode & 0o200 == 0 {
            READONLY
        } else {
            0
        }
        // O_CREAT|O_EXCL must examine the directory entry itself. Without
        // OPEN_REPARSE_POINT, CreateFileW follows a dangling symlink and
        // reports ERROR_FILE_NOT_FOUND instead of the POSIX EEXIST result.
        | if flags.create && flags.exclusive {
            OPEN_REPARSE_POINT
        } else {
            0
        };
    open_raw(path, access, disposition, attributes).map_err(|error| {
        if error.raw_os_error() == Some(80) && flags.create && !flags.exclusive {
            io::Error::from(io::ErrorKind::IsADirectory)
        } else {
            error
        }
    })
}

pub(super) fn stat(path: &Path, follow: bool) -> io::Result<Stats> {
    let file = open_raw(
        path,
        READ_ATTRIBUTES,
        3,
        if follow { 0 } else { OPEN_REPARSE_POINT },
    )?;
    let mut stats = fstat(&file)?;
    if !follow && fs::symlink_metadata(path)?.file_type().is_symlink() {
        // libuv reports the UTF-8 readlink length, not the reparse buffer size.
        stats.mode = (stats.mode & !0o170000) | 0o120000;
        stats.size = fs::read_link(path)?.to_string_lossy().len() as u64;
    }
    Ok(stats)
}

pub(super) fn fstat(file: &File) -> io::Result<Stats> {
    let mut info = HandleInfo::default();
    let mut basic = BasicInfo::default();
    let mut standard = StandardInfo::default();
    // SAFETY: the live File owns the handle; each output matches its Windows
    // ABI layout and size (FileBasicInfo=0, FileStandardInfo=1).
    unsafe {
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0
            || GetFileInformationByHandleEx(
                file.as_raw_handle(),
                0,
                (&mut basic as *mut BasicInfo).cast(),
                size_of::<BasicInfo>() as u32,
            ) == 0
            || GetFileInformationByHandleEx(
                file.as_raw_handle(),
                1,
                (&mut standard as *mut StandardInfo).cast(),
                size_of::<StandardInfo>() as u32,
            ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(Stats {
        dev: u64::from(info.volume),
        ino: (u64::from(info.index_high) << 32) | u64::from(info.index_low),
        mode: (if standard.directory != 0 {
            0o040000
        } else {
            0o100000
        }) | if basic.attributes & READONLY != 0 {
            0o444
        } else {
            0o666
        },
        nlink: u64::from(standard.links),
        uid: 0,
        gid: 0,
        rdev: 0,
        size: if standard.directory != 0 {
            0
        } else {
            standard.size as u64
        },
        // libuv intentionally reports 4096; blocks use actual allocated bytes.
        blksize: 4096,
        blocks: (standard.allocation as u64) >> 9,
        atime_ms: millis(basic.access),
        mtime_ms: millis(basic.write),
        ctime_ms: millis(basic.change),
        birthtime_ms: millis(basic.creation),
    })
}

fn millis(ticks: i64) -> i64 {
    ((i128::from(ticks) - EPOCH_TICKS).div_euclid(10_000)) as i64
}
fn filetime(ms: i64) -> io::Result<FileTime> {
    let ticks = u64::try_from(i128::from(ms) * 10_000 + EPOCH_TICKS)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    Ok(FileTime {
        low: ticks as u32,
        high: (ticks >> 32) as u32,
    })
}

pub(super) fn utimes(path: &Path, atime: i64, mtime: i64, follow: bool) -> io::Result<()> {
    let file = open_raw(
        path,
        WRITE_ATTRIBUTES,
        3,
        if follow { 0 } else { OPEN_REPARSE_POINT },
    )?;
    let access = filetime(atime)?;
    let write = filetime(mtime)?;
    // SAFETY: valid live handle and FILETIME pointers; creation is unchanged.
    if unsafe { SetFileTime(file.as_raw_handle(), std::ptr::null(), &access, &write) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn statfs(path: &Path) -> io::Result<StatsFs> {
    let file = open_raw(path, READ_ATTRIBUTES, 3, 0)?;
    let mut status = IoStatus::default();
    let mut info = FullSizeInfo::default();
    // SAFETY: FileFsFullSizeInformation=7, with correctly sized output buffers
    // alive until this synchronous NT call returns, as used by libuv.
    let result = unsafe {
        NtQueryVolumeInformationFile(
            file.as_raw_handle(),
            &mut status,
            (&mut info as *mut FullSizeInfo).cast(),
            size_of::<FullSizeInfo>() as u32,
            7,
        )
    };
    if result < 0 {
        // SAFETY: pure NTSTATUS-to-Win32-code translation.
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(result) } as i32,
        ));
    }
    Ok(StatsFs {
        // Windows/libuv does not expose inode capacity or a filesystem magic.
        filesystem_type: 0,
        files: 0,
        files_free: 0,
        block_size: u64::from(info.sectors) * u64::from(info.bytes_per_sector),
        blocks: info.total as u64,
        blocks_free: info.actual_free as u64,
        blocks_available: info.caller_free as u64,
    })
}
