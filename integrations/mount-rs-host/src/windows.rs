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
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const CREATE_NEW: u32 = 1;
const OPEN_EXISTING: u32 = 3;
const SYMBOLIC_LINK_FLAG_DIRECTORY: u32 = 0x1;
const SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE: u32 = 0x2;
const SYMLINK_FLAG_RELATIVE: u32 = 0x1;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const READ_ATTRIBUTES: u32 = 0x80;
const WRITE_DATA: u32 = 0x2;
const APPEND_DATA: u32 = 0x4;
const WRITE_ATTRIBUTES: u32 = 0x100;
const DELETE_ACCESS: u32 = 0x1_0000;
const READONLY: u32 = 1;
const FILE_DISPOSITION_INFORMATION: i32 = 13;
const FILE_DISPOSITION_INFORMATION_EX: i32 = 64;
const FILE_DISPOSITION_DELETE: u32 = 0x1;
const FILE_DISPOSITION_POSIX_SEMANTICS: u32 = 0x2;
const FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE: u32 = 0x10;
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
#[repr(C)]
#[derive(Default)]
struct DispositionInfo {
    delete_file: u8,
}
#[repr(C)]
#[derive(Default)]
struct DispositionInfoEx {
    flags: u32,
}

// Catch accidental ABI layout changes even in cross-target cargo check.
const _: () = {
    assert!(size_of::<FileTime>() == 8);
    assert!(size_of::<HandleInfo>() == 52);
    assert!(size_of::<BasicInfo>() == 40);
    assert!(size_of::<StandardInfo>() == 24);
    assert!(size_of::<FullSizeInfo>() == 32);
    assert!(size_of::<DispositionInfo>() == 1);
    assert!(size_of::<DispositionInfoEx>() == 4);
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
    fn CreateDirectoryW(path: *const u16, security: *const c_void) -> i32;
    fn CreateSymbolicLinkW(link: *const u16, target: *const u16, flags: u32) -> i32;
    fn GetShortPathNameW(path: *const u16, short_path: *mut u16, length: u32) -> u32;
    fn DeleteFileW(path: *const u16) -> i32;
    fn RemoveDirectoryW(path: *const u16) -> i32;
    fn DeviceIoControl(
        device: Handle,
        control_code: u32,
        input: *const c_void,
        input_size: u32,
        output: *mut c_void,
        output_size: u32,
        bytes_returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn GetFileInformationByHandle(handle: Handle, info: *mut HandleInfo) -> i32;
    fn GetFileInformationByHandleEx(
        handle: Handle,
        class: i32,
        info: *mut c_void,
        size: u32,
    ) -> i32;
    fn ReOpenFile(handle: Handle, access: u32, share: u32, attributes: u32) -> Handle;
    fn SetFileTime(
        handle: Handle,
        creation: *const FileTime,
        access: *const FileTime,
        write: *const FileTime,
    ) -> i32;
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSetInformationFile(
        handle: Handle,
        status: *mut IoStatus,
        info: *const c_void,
        size: u32,
        class: i32,
    ) -> i32;
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
    // `path` is normally already rooted by HostFs::secure. Avoid asking the
    // Windows path-normalization API to resolve an already absolute long path:
    // on hosts without long-path normalization enabled that API can fail with
    // ERROR_FILE_NOT_FOUND before the extended `\\?\\` namespace is applied.
    // Resolve only relative inputs, then add the extended prefix below.
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::absolute(path)?
    };
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

fn short_symbolic_link_path(path: &Path) -> io::Result<Option<Vec<u16>>> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let parent = wide_host_path(parent)?;

    // GetShortPathNameW accepts the extended namespace and returns the size
    // including the terminator when the output buffer is queried with zero.
    // A short parent lets CreateSymbolicLinkW stay in the unprivileged Win32
    // path used by Node/libuv, even when the ordinary path exceeds MAX_PATH.
    let required = unsafe { GetShortPathNameW(parent.as_ptr(), std::ptr::null_mut(), 0) };
    if required == 0 {
        return Ok(None);
    }
    let mut short = vec![0_u16; required as usize];
    let length = unsafe { GetShortPathNameW(parent.as_ptr(), short.as_mut_ptr(), required) };
    if length == 0 || (length as usize) >= short.len() {
        return Ok(None);
    }
    short.truncate(length as usize);
    if short.starts_with(&[92, 92, 63, 92, 85, 78, 67, 92]) {
        short = [92, 92]
            .into_iter()
            .chain(short[8..].iter().copied())
            .collect();
    } else if short.starts_with(&[92, 92, 63, 92]) {
        short = short[4..].to_vec();
    }

    let name: Vec<u16> = name.encode_wide().collect();
    if name.is_empty() || name.contains(&0) {
        return Ok(None);
    }
    if !short.ends_with(&[92]) {
        short.push(92);
    }
    short.extend(name);
    // Retain the ordinary namespace only when the short path is genuinely
    // within the Win32 limit; otherwise the extended/reparse fallback below
    // remains responsible for the operation.
    if short.len() + 1 > 260 {
        return Ok(None);
    }
    short.push(0);
    Ok(Some(short))
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

fn reparse_link_target(target: &str) -> io::Result<Vec<u16>> {
    let mut target = wide_link_target(target)?;
    target.pop();

    // The substitute name in a Windows symlink reparse buffer is an NT path
    // for absolute targets, while relative targets must remain relative so
    // rename/move semantics match CreateSymbolicLinkW and Node.
    let is_drive_absolute =
        target.len() >= 3 && target[1] == u16::from(b':') && target[2] == u16::from(b'\\');
    let is_unc = target.starts_with(&[u16::from(b'\\'), u16::from(b'\\')]);
    let is_extended = target.starts_with(&[92, 92, 63, 92]);
    if is_extended {
        target = [92, 63, 63, 92]
            .into_iter()
            .chain(target[4..].iter().copied())
            .collect();
    } else if is_drive_absolute {
        target = [92, 63, 63, 92]
            .into_iter()
            .chain(target.iter().copied())
            .collect();
    } else if is_unc {
        target = [92, 63, 63, 92, 85, 78, 67, 92]
            .into_iter()
            .chain(target[2..].iter().copied())
            .collect();
    }
    Ok(target)
}

fn write_u16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn remove_created_symlink_entry(path: &[u16], directory: bool) {
    // SAFETY: `path` is a live NUL-terminated UTF-16 path and the cleanup
    // operation is best effort after the creation handle has been closed.
    unsafe {
        if directory {
            let _ = RemoveDirectoryW(path.as_ptr());
        } else {
            let _ = DeleteFileW(path.as_ptr());
        }
    }
}

fn debug_symlink_error(stage: &str, path: &Path, error: &io::Error) {
    eprintln!(
        "mount-rs windows symlink stage={stage} raw={:?} kind={:?} path_units={}",
        error.raw_os_error(),
        error.kind(),
        path.as_os_str().encode_wide().count()
    );
}

fn create_long_symlink(path: &Path, target: &str, directory: bool) -> io::Result<()> {
    let link = wide_host_path(path)?;
    let target = reparse_link_target(target)?;
    let target_bytes = target
        .len()
        .checked_mul(2)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let target_bytes =
        u16::try_from(target_bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let data_length = 12_u32
        .checked_add(u32::from(target_bytes) * 2)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let data_length =
        u16::try_from(data_length).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let total_length = 8_usize
        .checked_add(data_length as usize)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut reparse = vec![0_u8; total_length];
    write_u32(&mut reparse, 0, IO_REPARSE_TAG_SYMLINK);
    write_u16(&mut reparse, 4, data_length);
    write_u16(&mut reparse, 8, target_bytes);
    write_u16(&mut reparse, 10, target_bytes);
    write_u16(&mut reparse, 12, 0);
    write_u16(&mut reparse, 14, target_bytes);
    write_u32(
        &mut reparse,
        16,
        if target.starts_with(&[92, 63, 63, 92]) {
            0
        } else {
            SYMLINK_FLAG_RELATIVE
        },
    );
    for (index, unit) in target.iter().chain(target.iter()).enumerate() {
        reparse[20 + index * 2..22 + index * 2].copy_from_slice(&unit.to_le_bytes());
    }

    let mut created_directory = false;
    if directory {
        // SAFETY: `link` is a live NUL-terminated UTF-16 path. The null
        // security descriptor requests the default directory security.
        if unsafe { CreateDirectoryW(link.as_ptr(), std::ptr::null()) } == 0 {
            let error = io::Error::last_os_error();
            debug_symlink_error("create-directory", path, &error);
            return Err(error);
        }
        created_directory = true;
    }

    let disposition = if directory { OPEN_EXISTING } else { CREATE_NEW };
    let attributes = if directory {
        BACKUP_SEMANTICS | OPEN_REPARSE_POINT
    } else {
        FILE_ATTRIBUTE_NORMAL | OPEN_REPARSE_POINT
    };
    // SAFETY: the path and arguments remain valid for the synchronous call;
    // successful creation transfers ownership of the returned handle.
    let handle = unsafe {
        CreateFileW(
            link.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            7,
            std::ptr::null(),
            disposition,
            attributes,
            std::ptr::null_mut(),
        )
    };
    if handle == -1_isize as Handle {
        let error = io::Error::last_os_error();
        debug_symlink_error("create-file", path, &error);
        if created_directory {
            remove_created_symlink_entry(&link, true);
        }
        return Err(error);
    }
    let file = unsafe { File::from_raw_handle(handle) };
    let mut bytes_returned = 0_u32;
    // SAFETY: `file` owns the handle, `reparse` is a correctly sized buffered
    // FSCTL_SET_REPARSE_POINT input, and no output buffer is requested.
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_REPARSE_POINT,
            reparse.as_ptr().cast(),
            reparse.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut bytes_returned,
            std::ptr::null_mut(),
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        debug_symlink_error("set-reparse-point", path, &error);
        drop(file);
        remove_created_symlink_entry(&link, directory);
        return Err(error);
    }
    Ok(())
}

pub(super) fn symlink(target: &str, path: &Path, directory: bool) -> io::Result<()> {
    let link = wide_symbolic_link_path(path)?;
    let extended_link = wide_host_path(path)?;
    let target_wide = wide_link_target(target)?;
    let type_flag = if directory {
        SYMBOLIC_LINK_FLAG_DIRECTORY
    } else {
        0
    };
    let mut flags = type_flag | SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE;
    let mut link_path = &link;
    let short_link = if link.len() > 260 {
        short_symbolic_link_path(path).ok().flatten()
    } else {
        None
    };
    let mut short_attempted = false;
    let mut extended_attempted = false;
    // Windows 10 Developer Mode supports this flag without elevation. Older
    // Windows versions reject the flag itself; libuv retries without it so
    // the ordinary elevated symlink behavior remains available.
    loop {
        // SAFETY: both buffers are NUL-terminated UTF-16 and remain alive for
        // the synchronous CreateSymbolicLinkW call.
        if unsafe { CreateSymbolicLinkW(link_path.as_ptr(), target_wide.as_ptr(), flags) } != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        debug_symlink_error("create-symbolic-link", path, &error);
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
            // ERROR_FILENAME_EXCED_RANGE: first try the short aliases of the
            // existing parent components. This keeps creation in the normal
            // unprivileged Win32 namespace on runners where 8.3 names are
            // available, while still supporting long-path-aware filesystems.
            if let Some(short_link) = short_link.as_ref() {
                link_path = short_link;
                short_attempted = true;
            } else {
                link_path = &extended_link;
                extended_attempted = true;
            }
            continue;
        }
        if short_attempted && matches!(error.raw_os_error(), Some(2) | Some(3)) {
            // The short-name lookup can succeed while the link API still
            // rejects that namespace. Give the explicit extended path one
            // chance before using the low-level reparse fallback.
            link_path = &extended_link;
            short_attempted = false;
            extended_attempted = true;
            continue;
        }
        if extended_attempted && matches!(error.raw_os_error(), Some(2) | Some(3)) {
            // ERROR_FILE_NOT_FOUND and ERROR_PATH_NOT_FOUND both occur when
            // the long-link fallback is needed on hosted Windows runners.
            return create_long_symlink(path, target, directory);
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
    // Match libuv's fs__unlink_rmdir path: open the directory entry itself
    // with DELETE sharing, then use the extended disposition API first. This
    // is important for an open readonly file with hard-link aliases: the
    // POSIX disposition removes only the requested name and preserves the
    // file's readonly metadata for the remaining aliases and live handle.
    let file = open_raw(path, READ_ATTRIBUTES | DELETE_ACCESS, 3, OPEN_REPARSE_POINT)?;
    let mut basic = BasicInfo::default();
    // SAFETY: `file` owns a live handle and `basic` is a correctly sized
    // FILE_BASIC_INFO-compatible output buffer.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            0,
            (&mut basic as *mut BasicInfo).cast(),
            size_of::<BasicInfo>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    // unlink must not remove an ordinary directory. A directory reparse point
    // is permitted here because Node/libuv treats a directory symlink as the
    // link entry, not as the target directory.
    if basic.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
        && basic.attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
    {
        return Err(io::Error::from_raw_os_error(5));
    }

    let mut status = IoStatus::default();
    let mut disposition_ex = DispositionInfoEx {
        flags: FILE_DISPOSITION_DELETE
            | FILE_DISPOSITION_POSIX_SEMANTICS
            | FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: the handle and disposition buffer remain live for this
    // synchronous ntdll call. FileDispositionInformationEx is class 21.
    let nt_status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut status,
            (&mut disposition_ex as *mut DispositionInfoEx).cast(),
            size_of::<DispositionInfoEx>() as u32,
            FILE_DISPOSITION_INFORMATION_EX,
        )
    };
    if nt_status >= 0 {
        return Ok(());
    }

    let error = nt_status_to_io(nt_status);
    // Older Windows versions and filesystems may not implement the extended
    // disposition class. Fall back to the legacy libuv sequence only for
    // those capability errors; a real access/sharing failure must propagate.
    if !matches!(error.raw_os_error(), Some(1 | 50 | 87)) {
        return Err(error);
    }

    if basic.attributes & READONLY != 0 {
        // ReOpenFile avoids asking the primary DELETE handle for write
        // attributes and follows libuv's Wine-compatible fallback.
        let writable = unsafe {
            ReOpenFile(
                file.as_raw_handle(),
                WRITE_ATTRIBUTES,
                7,
                OPEN_REPARSE_POINT | BACKUP_SEMANTICS,
            )
        };
        if writable == -1_isize as Handle {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ReOpenFile returned an owned handle on success.
        let writable = unsafe { File::from_raw_handle(writable) };
        let writable_basic = BasicInfo {
            attributes: (basic.attributes & !READONLY) | FILE_ATTRIBUTE_ARCHIVE,
            ..BasicInfo::default()
        };
        // SAFETY: the reopened handle and FILE_BASIC_INFO-compatible buffer
        // remain live for the synchronous ntdll call.
        let status = unsafe {
            NtSetInformationFile(
                writable.as_raw_handle(),
                &mut status,
                (&writable_basic as *const BasicInfo).cast(),
                size_of::<BasicInfo>() as u32,
                0,
            )
        };
        if status < 0 {
            return Err(nt_status_to_io(status));
        }
    }

    let mut disposition = DispositionInfo { delete_file: 1 };
    // SAFETY: the original handle and one-byte disposition buffer remain
    // live for the synchronous legacy ntdll call. Class 13 is the legacy
    // FileDispositionInformation operation used by libuv.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut status,
            (&mut disposition as *mut DispositionInfo).cast(),
            size_of::<DispositionInfo>() as u32,
            FILE_DISPOSITION_INFORMATION,
        )
    };
    if status >= 0 {
        Ok(())
    } else {
        Err(nt_status_to_io(status))
    }
}

fn nt_status_to_io(status: i32) -> io::Error {
    // SAFETY: RtlNtStatusToDosError is a pure conversion and accepts every
    // NTSTATUS returned by NtSetInformationFile.
    io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32)
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
