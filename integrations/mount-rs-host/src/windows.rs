//! Windows filesystem primitives matching Node/libuv's win/fs.c conventions.
//! ABI declarations are local so the host crate needs no new lockfile entries.

use std::ffi::{OsStr, c_void};
use std::fs::{self, File};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;

use mount_rs_core::{OpenFlags, Stats, StatsFs};

type Handle = *mut c_void;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const SYMBOLIC_LINK_FLAG_DIRECTORY: u32 = 0x1;
const SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE: u32 = 0x2;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const READ_ATTRIBUTES: u32 = 0x80;
const WRITE_DATA: u32 = 0x2;
const APPEND_DATA: u32 = 0x4;
const WRITE_ATTRIBUTES: u32 = 0x100;
const READONLY: u32 = 1;
const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;
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
    fn CreateHardLinkW(
        new_path: *const u16,
        existing_path: *const u16,
        security: *const c_void,
    ) -> i32;
    fn CreateSymbolicLinkW(link: *const u16, target: *const u16, flags: u32) -> i32;
    fn DeleteFileW(path: *const u16) -> i32;
    fn GetFileAttributesW(path: *const u16) -> u32;
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
    fn SetFileAttributesW(path: *const u16, attributes: u32) -> i32;
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

fn wide_path(path: &Path, extended: bool) -> io::Result<Vec<u16>> {
    // Normalize separators before calling Win32. Preserve non-Unicode
    // filenames as UTF-16. CreateFileW uses the extended-length namespace so
    // ordinary host operations do not regress at MAX_PATH; CreateSymbolicLinkW
    // follows Node/libuv's normal Win32 path form and only falls back to the
    // extended namespace when a link name is actually too long.
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
    if extended {
        if !wide.starts_with(&[92, 92, 63, 92]) {
            let (prefix, rest) = if wide.starts_with(&[92, 92]) {
                (r"\\?\UNC\", &wide[2..])
            } else {
                (r"\\?\", wide.as_slice())
            };
            wide = prefix.encode_utf16().chain(rest.iter().copied()).collect();
        }
    } else if wide.starts_with(&[92, 92, 63, 92]) {
        // CreateSymbolicLinkW is used with the same ordinary Win32 namespace
        // as Node/libuv. Convert an already extended absolute path back to
        // that namespace when it is safe to do so.
        if wide.starts_with(&[92, 92, 63, 92, 85, 78, 67, 92]) {
            wide = [92, 92]
                .into_iter()
                .chain(wide[8..].iter().copied())
                .collect();
        } else {
            wide = wide[4..].to_vec();
        }
    }
    wide.push(0);
    Ok(wide)
}

fn wide_host_path(path: &Path) -> io::Result<Vec<u16>> {
    wide_path(path, true)
}

fn wide_symbolic_link_path(path: &Path) -> io::Result<Vec<u16>> {
    wide_path(path, false)
}

fn wide_link_target(target: &str) -> io::Result<Vec<u16>> {
    // A symlink target is intentionally not made absolute: relative targets
    // are resolved by Windows relative to the link's parent. The host driver
    // rewrites targets during rooted traversal, just as mountx does.
    let mut wide: Vec<u16> = OsStr::new(target).encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    for unit in &mut wide {
        if *unit == u16::from(b'/') {
            *unit = u16::from(b'\\');
        }
    }
    wide.push(0);
    Ok(wide)
}

pub(super) fn symlink(target: &str, path: &Path, directory: bool) -> io::Result<()> {
    let link = wide_symbolic_link_path(path)?;
    let extended_link = wide_host_path(path)?;
    let target = wide_link_target(target)?;
    let type_flag = if directory {
        SYMBOLIC_LINK_FLAG_DIRECTORY
    } else {
        0
    };
    let mut flags = type_flag | SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE;
    let mut link_path = &link;
    // Windows 10 Developer Mode supports this flag without elevation. Older
    // Windows versions reject the flag itself; libuv retries without it so
    // the ordinary elevated symlink behavior remains available.
    loop {
        // SAFETY: both buffers are NUL-terminated UTF-16 and remain alive for
        // the synchronous CreateSymbolicLinkW call.
        if unsafe { CreateSymbolicLinkW(link.as_ptr(), target.as_ptr(), flags) } != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if flags & SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE != 0
            && error.raw_os_error() == Some(87)
        {
            flags &= !SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE;
            continue;
        }
        if std::ptr::eq(link_path, &link)
            && error.raw_os_error() == Some(206)
            && extended_link != link
        {
            // ERROR_FILENAME_EXCED_RANGE: retain the normal namespace for
            // short paths, but still support long link names when the host
            // process and filesystem have opted into long paths.
            link_path = &extended_link;
            continue;
        }
        return Err(error);
    }
}

fn open_raw(path: &Path, access: u32, disposition: u32, attributes: u32) -> io::Result<File> {
    let wide = wide_host_path(path)?;
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
        // FILE_APPEND_DATA is permitted without FILE_WRITE_DATA. This is the
        // same access transformation used by libuv's win/fs.c open path.
        access = (access & !WRITE_DATA) | APPEND_DATA;
    }
    let disposition = match (flags.create, flags.exclusive, flags.truncate) {
        (true, true, _) => 1,      // CREATE_NEW
        (true, false, true) => 2,  // CREATE_ALWAYS
        (false, _, false) => 3,    // OPEN_EXISTING
        (true, false, false) => 4, // OPEN_ALWAYS
        (false, _, true) => 5,     // TRUNCATE_EXISTING
    };
    let attributes = FILE_ATTRIBUTE_NORMAL
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

pub(super) fn hard_link(existing: &Path, new_path: &Path) -> io::Result<()> {
    let existing = wide_host_path(existing)?;
    let new_path = wide_host_path(new_path)?;
    // SAFETY: both paths are NUL-terminated UTF-16 buffers alive for the
    // synchronous CreateHardLinkW call. A null security descriptor requests
    // the default security attributes, matching libuv.
    if unsafe { CreateHardLinkW(new_path.as_ptr(), existing.as_ptr(), std::ptr::null()) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(super) fn unlink(path: &Path) -> io::Result<()> {
    let wide = wide_host_path(path)?;
    // DeleteFileW is deliberately used instead of std::fs::remove_file so
    // long Win32 paths and open handles with FILE_SHARE_DELETE follow the same
    // path as libuv. The first attempt also preserves ordinary error codes.
    // SAFETY: `wide` is a live NUL-terminated UTF-16 path buffer.
    if unsafe { DeleteFileW(wide.as_ptr()) } != 0 {
        return Ok(());
    }
    let initial_error = io::Error::last_os_error();
    if initial_error.raw_os_error() != Some(5) {
        return Err(initial_error);
    }

    // libuv's Windows unlink path uses FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE.
    // DeleteFileW has no equivalent flag, so mirror that behavior for the
    // fallback by clearing only the readonly bit, then restore it if deletion
    // still fails. This matters for files created with O_RDONLY|O_CREAT and a
    // mode without the owner-write bit.
    // SAFETY: the path buffer remains valid for both synchronous Win32 calls.
    let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES || attributes & READONLY == 0 {
        return Err(initial_error);
    }
    let writable_attributes = match attributes & !READONLY {
        0 => FILE_ATTRIBUTE_NORMAL,
        attributes => attributes,
    };
    if unsafe { SetFileAttributesW(wide.as_ptr(), writable_attributes) } == 0 {
        return Err(initial_error);
    }
    if unsafe { DeleteFileW(wide.as_ptr()) } != 0 {
        return Ok(());
    }
    let delete_error = io::Error::last_os_error();
    // Best-effort restoration avoids changing the host entry when the delete
    // failed for a reason unrelated to readonly protection.
    let _ = unsafe { SetFileAttributesW(wide.as_ptr(), attributes) };
    Err(delete_error)
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
