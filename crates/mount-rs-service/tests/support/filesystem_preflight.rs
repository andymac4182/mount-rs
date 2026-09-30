//! Fixture-only local filesystem ownership checks and anchored Drive preparation.
use mount_rs_sdk::StoreConfig;
use serde_json::{Value, json};
use std::{
    ffi::CString,
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
};

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    dev: String,
    ino: String,
    uid: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerMarker {
    schema: String,
    owner: String,
    root: Identity,
}

struct Root {
    path: PathBuf,
    identity: Identity,
    owner: String,
    file: File,
}

fn number(value: &str) -> Result<u64, String> {
    if value.is_empty()
        || value.bytes().any(|byte| !byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err("filesystem owner identity requires canonical unsigned decimal".into());
    }
    value
        .parse()
        .map_err(|_| "filesystem owner identity overflow".into())
}

fn valid_owner(owner: &str) -> bool {
    owner
        .strip_prefix("mount-rs-filesystem-")
        .is_some_and(|suffix| {
            suffix.len() == 24
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn validate_marker(marker: &OwnerMarker, owner: &str, expected: &Identity) -> Result<(), String> {
    if !valid_owner(owner)
        || marker.schema != "mount-rs-filesystem-owner-v1"
        || marker.owner != owner
    {
        return Err("filesystem fixture owner marker mismatch".into());
    }
    for (observed, expected) in [
        (&marker.root.dev, &expected.dev),
        (&marker.root.ino, &expected.ino),
        (&marker.root.uid, &expected.uid),
    ] {
        number(observed)?;
        number(expected)?;
        if observed != expected {
            return Err("filesystem fixture owner marker identity mismatch".into());
        }
    }
    if number(&expected.dev)? == 0 || number(&expected.ino)? == 0 {
        return Err("filesystem fixture requires nonzero device and inode identity".into());
    }
    Ok(())
}

fn same_identity(metadata: &std::fs::Metadata, identity: &Identity) -> Result<(), String> {
    if metadata.dev() != number(&identity.dev)?
        || metadata.ino() != number(&identity.ino)?
        || u64::from(metadata.uid()) != number(&identity.uid)?
    {
        return Err("filesystem fixture root identity changed".into());
    }
    Ok(())
}

fn open_at(parent: &File, name: &CString, flags: i32) -> Result<File, String> {
    // SAFETY: parent is live, name is NUL terminated and flags do not create files.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err("filesystem anchored open failed".into());
    }
    // SAFETY: successful openat returned a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_root(path: &Path) -> Result<File, String> {
    // SAFETY: the static C string is valid and a successful descriptor is owned here.
    let fd = unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err("filesystem root anchor unavailable".into());
    }
    // SAFETY: successful open returned a fresh owned descriptor.
    let mut file = unsafe { File::from_raw_fd(fd) };
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(part) => {
                let name = CString::new(part.as_bytes())
                    .map_err(|_| "filesystem root component contains NUL")?;
                file = open_at(&file, &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
            }
            _ => return Err("filesystem root requires canonical absolute components".into()),
        }
    }
    Ok(file)
}

fn private_directory(file: &File, dev: Option<u64>) -> Result<std::fs::Metadata, String> {
    let metadata = file
        .metadata()
        .map_err(|_| "filesystem directory descriptor metadata unavailable")?;
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.mode() & 0o7777 != 0o700
        || dev.is_some_and(|dev| metadata.dev() != dev)
    {
        return Err(
            "filesystem fixture requires a private current-owner directory on its root device"
                .into(),
        );
    }
    Ok(metadata)
}

fn root_path_still_matches(root: &Root) -> Result<(), String> {
    let path =
        std::fs::symlink_metadata(&root.path).map_err(|_| "filesystem root path unavailable")?;
    let fd = private_directory(&root.file, None)?;
    same_identity(&path, &root.identity)?;
    same_identity(&fd, &root.identity)?;
    if path.file_type().is_symlink() || !path.is_dir() || path.mode() != fd.mode() {
        return Err("filesystem root path no longer matches held directory descriptor".into());
    }
    Ok(())
}

#[allow(clippy::unnecessary_cast)] // stat field widths differ on Linux and Darwin.
fn named_identity(parent: &File, name: &CString, file: &File) -> Result<(), String> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: parent is live, name valid, stat has room for the no-follow observation.
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err("filesystem anchored path identity unavailable".into());
    }
    // SAFETY: successful fstatat initialized the returned stat.
    let stat = unsafe { stat.assume_init() };
    let fd = file
        .metadata()
        .map_err(|_| "filesystem anchored descriptor identity unavailable")?;
    if stat.st_dev as u64 != fd.dev()
        || stat.st_ino as u64 != fd.ino()
        || stat.st_uid != fd.uid()
        || stat.st_mode as u32 != fd.mode()
        || stat.st_nlink as u64 != fd.nlink()
    {
        return Err("filesystem anchored path no longer matches its held descriptor".into());
    }
    Ok(())
}

fn owner_root(blocks: &StoreConfig) -> Result<(Root, PathBuf), String> {
    let StoreConfig::Filesystem {
        root: blocks_root,
        durable: true,
    } = blocks
    else {
        return Err("owned durable filesystem blocks required".into());
    };
    let required = |name: &str| {
        std::env::var(name).map_err(|_| format!("{name} required for filesystem ownership"))
    };
    let path = super::filesystem_root(&required("MOUNT_RS_FILESYSTEM_ROOT")?)?;
    let owner = required("MOUNT_RS_BACKING_FILESYSTEM_OWNER")?;
    let identity = Identity {
        dev: required("MOUNT_RS_BACKING_FILESYSTEM_DEV")?,
        ino: required("MOUNT_RS_BACKING_FILESYSTEM_INO")?,
        uid: required("MOUNT_RS_BACKING_FILESYSTEM_UID")?,
    };
    if !valid_owner(&owner)
        || path.file_name().and_then(|name| name.to_str()) != Some(owner.as_str())
    {
        return Err("filesystem fixture requires the exact owner-issued root".into());
    }
    let suffix = blocks_root
        .strip_prefix(&path)
        .map_err(|_| "filesystem drive root mismatch")?;
    super::filesystem_namespace(
        suffix
            .to_str()
            .ok_or("filesystem namespace requires Unicode")?,
    )?;
    let suffix = suffix.to_path_buf();
    let file = open_root(&path)?;
    same_identity(&private_directory(&file, None)?, &identity)?;
    let root = Root {
        path,
        identity,
        owner,
        file,
    };
    root_path_still_matches(&root)?;
    read_marker(&root)?;
    Ok((root, suffix))
}

fn read_marker(root: &Root) -> Result<(), String> {
    let name = CString::new("owner.json").unwrap();
    // O_NONBLOCK prevents an unexpected FIFO from blocking before regular-file admission.
    let mut marker = open_at(&root.file, &name, libc::O_RDONLY | libc::O_NONBLOCK)?;
    let before = marker
        .metadata()
        .map_err(|_| "filesystem owner marker metadata unavailable")?;
    if !before.is_file()
        || before.nlink() != 1
        || before.mode() & 0o7777 != 0o600
        || u64::from(before.uid()) != number(&root.identity.uid)?
        || before.len() == 0
        || before.len() > 4096
    {
        return Err("filesystem owner marker must be one private bounded regular file".into());
    }
    named_identity(&root.file, &name, &marker)?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    marker
        .by_ref()
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| "filesystem owner marker read failed")?;
    if bytes.len() as u64 != before.len() {
        return Err("filesystem owner marker size changed".into());
    }
    let parsed: OwnerMarker =
        serde_json::from_slice(&bytes).map_err(|_| "filesystem owner marker invalid")?;
    validate_marker(&parsed, &root.owner, &root.identity)?;
    let after = marker
        .metadata()
        .map_err(|_| "filesystem owner marker final identity unavailable")?;
    if after.dev() != before.dev()
        || after.ino() != before.ino()
        || after.uid() != before.uid()
        || after.mode() != before.mode()
        || after.nlink() != 1
        || after.len() != before.len()
        || after.mtime() != before.mtime()
        || after.mtime_nsec() != before.mtime_nsec()
        || after.ctime() != before.ctime()
        || after.ctime_nsec() != before.ctime_nsec()
    {
        return Err("filesystem owner marker changed during observation".into());
    }
    named_identity(&root.file, &name, &marker)?;
    root_path_still_matches(root)
}

fn local_persistent_filesystem(root: &File) -> Result<Value, String> {
    let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: root is a live directory descriptor and info has room for statfs.
    if unsafe { libc::fstatfs(root.as_raw_fd(), info.as_mut_ptr()) } != 0 {
        return Err("filesystem fixture filesystem observation failed".into());
    }
    // SAFETY: successful fstatfs initialized all fields.
    let info = unsafe { info.assume_init() };
    #[cfg(target_os = "macos")]
    {
        // SAFETY: Darwin statfs returns a NUL-terminated filesystem type name.
        let kind = unsafe { std::ffi::CStr::from_ptr(info.f_fstypename.as_ptr()) }
            .to_str()
            .map_err(|_| "filesystem fixture type name invalid")?;
        if info.f_flags & libc::MNT_LOCAL as u32 == 0 || !matches!(kind, "apfs" | "hfs") {
            return Err("filesystem fixture requires a known persistent local filesystem".into());
        }
        Ok(
            json!({"source":"darwin_fstatfs","local":true,"persistent_type":kind,
            "scope":"held directory local filesystem type; no power-loss or physical-device proof"}),
        )
    }
    #[cfg(target_os = "linux")]
    {
        let name = match info.f_type as u64 {
            0xef53 => "ext",
            0x58465342 => "xfs",
            0x9123683e => "btrfs",
            0xf2f52010 => "f2fs",
            0x2fc12fc1 => "zfs",
            _ => {
                return Err(
                    "filesystem fixture requires a known persistent local filesystem".into(),
                );
            }
        };
        Ok(
            json!({"source":"linux_fstatfs","local":true,"persistent_type":name,
            "scope":"held directory local filesystem type; no power-loss or physical-device proof"}),
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = info;
        Err("filesystem fixture requires Linux or macOS filesystem observation".into())
    }
}

pub(super) fn preflight(blocks: &StoreConfig) -> Result<Value, String> {
    let (root, _) = owner_root(blocks)?;
    let filesystem = local_persistent_filesystem(&root.file)?;
    root_path_still_matches(&root)?;
    Ok(
        json!({"provider":"filesystem","complete":true,"qualified":true,"owner":root.owner,
        "root_identity_verified":true,"root_identity_redacted":true,"private_directory_mode":"0700",
        "owner_marker_mode":"0600","no_symlink_components":true,"anchored_directory_descriptors":true,
        "persistent_local_filesystem":filesystem,"durable":true,
        "scope":"owner-issued local shared filesystem root and configured sync barriers; no cross-host availability, process-crash or power-loss proof"}),
    )
}

pub(super) fn prepare(blocks: &StoreConfig) -> Result<(), String> {
    if !matches!(blocks, StoreConfig::Filesystem { .. }) {
        return Ok(());
    }
    let (root, suffix) = owner_root(blocks)?;
    local_persistent_filesystem(&root.file)?;
    let dev = number(&root.identity.dev)?;
    let mut directories = vec![
        root.file
            .try_clone()
            .map_err(|_| "filesystem root descriptor clone failed")?,
    ];
    let mut names = Vec::new();
    for component in suffix.components() {
        let Component::Normal(component) = component else {
            return Err("filesystem drive namespace requires normal components".into());
        };
        let name = CString::new(component.as_bytes())
            .map_err(|_| "filesystem drive component contains NUL")?;
        let parent = directories.last().unwrap();
        // SAFETY: parent is held, component is canonical and mode is a private directory.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err("filesystem anchored Drive directory creation failed".into());
        }
        let child = open_at(parent, &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
        private_directory(&child, Some(dev))?;
        named_identity(parent, &name, &child)?;
        child
            .sync_all()
            .map_err(|_| "filesystem new Drive directory sync failed")?;
        parent
            .sync_all()
            .map_err(|_| "filesystem Drive parent directory sync failed")?;
        names.push(name);
        directories.push(child);
    }
    for (index, name) in names.iter().enumerate() {
        private_directory(&directories[index + 1], Some(dev))?;
        named_identity(&directories[index], name, &directories[index + 1])?;
    }
    read_marker(&root)?;
    root_path_still_matches(&root)
}

#[cfg(test)]
#[path = "filesystem_preflight_tests.rs"]
mod tests;
