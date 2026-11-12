//! The byte transport for a FUSE connection.
//!
//! Linux exposes one complete FUSE request per `read(2)` from `/dev/fuse` and
//! accepts one complete reply per `write(2)`.  `AsyncFd` lets the session use
//! that character device without parking a Tokio worker thread.  Because the
//! kernel takes a reply whole or not at all, concurrent replies on the device
//! need no lock.  A byte-stream descriptor, which could take a reply in
//! pieces, is refused by [`FuseDevice::from_owned_fd`].  The type is
//! still available on non-Linux targets, but its native operations return
//! `Unsupported`: macOS uses a different filesystem protocol (NFS in this
//! workspace), not Linux FUSE.

use std::io;
use std::path::Path;

/// The largest request accepted by [`crate::session::FuseSession`].
pub const DEFAULT_MAX_FRAME: usize = 1024 * 1024;

/// The extra space libfuse keeps above the negotiated write size.
pub const FUSE_BUFFER_HEADER_SIZE: usize = 4096;

/// Size of a receive buffer for a session request limit.
pub const fn read_buffer_size(max_request: usize) -> usize {
    max_request.saturating_add(FUSE_BUFFER_HEADER_SIZE)
}

/// A single FUSE device connection.
pub struct FuseDevice {
    // Declared before `io` so the descriptor leaves the registry before it
    // is closed and its number can be reused.
    #[cfg(target_os = "linux")]
    _registered: RegisteredDevice,
    #[cfg(target_os = "linux")]
    io: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
    max_frame: usize,
}

/// Check that one `write(2)` on the descriptor is one whole message,
/// accepted atomically or refused: the kernel FUSE device, or a
/// `SOCK_SEQPACKET`/`SOCK_DGRAM` socket.
///
/// The session loop writes replies from several tasks at once (the loop
/// itself and up to sixteen positional-read workers) with no lock between
/// them.  That is only sound on such a descriptor, so anything else (a
/// `SOCK_STREAM` socket, a pipe, another character device) is refused here,
/// once, when the device is built.
#[cfg(target_os = "linux")]
fn require_message_framing(fd: std::os::fd::RawFd) -> io::Result<()> {
    // The FUSE device is misc character device 10:229 (`FUSE_MINOR` in
    // the kernel's miscdevice.h; fixed by Documentation/admin-guide/
    // devices.txt), whatever path it was opened through.
    const FUSE_MAJOR: libc::c_uint = 10;
    const FUSE_MINOR: libc::c_uint = 229;

    // SAFETY: `stat` is plain data and fstat initializes it on success.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let message = match stat.st_mode & libc::S_IFMT {
        libc::S_IFCHR => {
            libc::major(stat.st_rdev) == FUSE_MAJOR && libc::minor(stat.st_rdev) == FUSE_MINOR
        }
        libc::S_IFSOCK => {
            let mut kind: libc::c_int = 0;
            let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            // SAFETY: `kind` and `length` describe one writable c_int.
            let result = unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_TYPE,
                    (&raw mut kind).cast::<libc::c_void>(),
                    &raw mut length,
                )
            };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            kind == libc::SOCK_SEQPACKET || kind == libc::SOCK_DGRAM
        }
        _ => false,
    };
    if message {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a FUSE device must take each reply as one message; this descriptor is a byte stream",
        ))
    }
}

impl std::fmt::Debug for FuseDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FuseDevice")
            .field("max_frame", &self.max_frame)
            .finish_non_exhaustive()
    }
}

/// Descriptor numbers of the FUSE devices this process has open, so a child
/// can drop its inherited copies before `exec`.
///
/// `O_CLOEXEC` is not enough. `exec` closes close-on-exec descriptors one at
/// a time in ascending order, and closing a file that lives on a FUSE mount
/// sends `FUSE_FLUSH` and waits for the answer. If that mount's session has
/// stopped, the request can only end when the connection is aborted, which
/// happens when the last copy of its device descriptor is closed. A child
/// that inherited both the file and a higher-numbered device descriptor
/// would wait on the flush while holding the descriptor that keeps the
/// connection alive: it never reaches `exec`, and the unmount helper it was
/// meant to become never runs.
#[cfg(target_os = "linux")]
static OPEN_DEVICES: [std::sync::atomic::AtomicI32; MAX_REGISTERED_DEVICES] =
    [const { std::sync::atomic::AtomicI32::new(-1) }; MAX_REGISTERED_DEVICES];

/// Devices beyond this many are not registered and keep the old behavior.
#[cfg(target_os = "linux")]
const MAX_REGISTERED_DEVICES: usize = 256;

/// One slot of [`OPEN_DEVICES`], released on drop.
#[cfg(target_os = "linux")]
struct RegisteredDevice(Option<usize>);

#[cfg(target_os = "linux")]
impl RegisteredDevice {
    fn new(fd: std::os::fd::RawFd) -> Self {
        use std::sync::atomic::Ordering;
        Self(OPEN_DEVICES.iter().position(|slot| {
            slot.compare_exchange(-1, fd, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
        }))
    }
}

#[cfg(target_os = "linux")]
impl Drop for RegisteredDevice {
    fn drop(&mut self) {
        if let Some(slot) = self.0 {
            OPEN_DEVICES[slot].store(-1, std::sync::atomic::Ordering::Release);
        }
    }
}

/// Close this process's copies of every registered FUSE device.
///
/// Call it only in a forked child before `exec`: it closes descriptors the
/// parent still uses. It reads atomics and calls `close(2)`, both of which
/// are async-signal-safe.
#[cfg(target_os = "linux")]
pub(crate) fn close_inherited_devices() {
    for slot in &OPEN_DEVICES {
        let fd = slot.load(std::sync::atomic::Ordering::Acquire);
        if fd >= 0 {
            // SAFETY: the child owns its copy of the descriptor table, and
            // nothing in the child uses these descriptors before `exec`.
            unsafe { libc::close(fd) };
        }
    }
}

impl FuseDevice {
    /// Open a native FUSE device.
    ///
    /// On Linux this opens the supplied path read/write and changes the fd to
    /// non-blocking mode for [`tokio::io::unix::AsyncFd`].  On other platforms
    /// this returns [`io::ErrorKind::Unsupported`] without touching the path.
    pub fn open(path: impl AsRef<Path>, max_frame: usize) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::FromRawFd;

            validate_max_frame(max_frame)?;
            let path = path_to_cstring(path.as_ref())?;
            // FUSE is a Linux character device.  O_NONBLOCK is required by
            // AsyncFd; O_CLOEXEC prevents a helper or unrelated child from
            // keeping the kernel connection alive once it has run `exec`.
            // `close_inherited_devices` covers the time before that.
            let fd = unsafe {
                libc::open(
                    path.as_ptr(),
                    libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC,
                    0,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `fd` is a fresh descriptor returned by open and is now
            // owned by this value.
            let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
            Self::from_owned_fd(owned, max_frame)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (path, max_frame);
            Err(unsupported())
        }
    }

    /// Build a device from an already-open Linux FUSE descriptor.
    ///
    /// This is used by the rootless `fusermount3` path after it receives the
    /// descriptor with `SCM_RIGHTS`.  The descriptor is consumed even when
    /// configuring it fails, so callers never double-close it.
    ///
    /// The kernel FUSE device and message sockets (`SOCK_SEQPACKET`,
    /// `SOCK_DGRAM`) take each reply as one atomic write, with no lock.  Any
    /// other descriptor is refused with `InvalidInput`: it could take a
    /// reply in pieces, and concurrent replies would then interleave.
    #[cfg(target_os = "linux")]
    pub fn from_owned_fd(fd: std::os::fd::OwnedFd, max_frame: usize) -> io::Result<Self> {
        use std::os::fd::AsRawFd;

        require_message_framing(fd.as_raw_fd())?;
        Self::adopt(fd, max_frame)
    }

    /// Wrap a descriptor without asking what it is. Tests use this to hand
    /// the session a descriptor that fails in a chosen way.
    #[cfg(target_os = "linux")]
    pub(crate) fn adopt(fd: std::os::fd::OwnedFd, max_frame: usize) -> io::Result<Self> {
        use std::os::fd::AsRawFd;

        validate_max_frame(max_frame)?;
        set_nonblocking(fd.as_raw_fd())?;
        let registered = RegisteredDevice::new(fd.as_raw_fd());
        let io = tokio::io::unix::AsyncFd::new(fd)?;
        Ok(Self {
            _registered: registered,
            io,
            max_frame,
        })
    }

    /// Maximum number of bytes accepted from one device read.
    pub const fn max_frame(&self) -> usize {
        self.max_frame
    }

    /// Read one kernel request.
    ///
    /// `None` means that the FUSE connection has gone away.  A device read is
    /// intentionally not implemented with `read_exact`: FUSE is message
    /// framed by the character device, and waiting for a second read would
    /// turn a valid request into a deadlock.
    pub async fn read_frame(&self) -> io::Result<Option<Vec<u8>>> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;

            // One byte of sentinel space lets us report an oversized message
            // rather than silently handing a truncated request to the session.
            let mut buffer = vec![0_u8; self.max_frame.saturating_add(1)];
            loop {
                let mut ready = self.io.readable().await?;
                match ready.try_io(|inner| {
                    let fd = inner.get_ref().as_raw_fd();
                    let bytes = unsafe {
                        libc::read(fd, buffer.as_mut_ptr().cast::<libc::c_void>(), buffer.len())
                    };
                    if bytes < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(bytes as usize)
                    }
                }) {
                    Ok(Ok(0)) => return Ok(None),
                    Ok(Ok(bytes)) if bytes > self.max_frame => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "FUSE request exceeds the session frame limit",
                        ));
                    }
                    Ok(Ok(bytes)) => {
                        buffer.truncate(bytes);
                        return Ok(Some(buffer));
                    }
                    Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Ok(Err(error)) => return Err(error),
                    Err(_would_block) => continue,
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported())
        }
    }

    /// Write one complete FUSE reply or notification.
    ///
    /// Safe to call from several tasks at once: replies never interleave.
    /// It is one `write(2)` and no lock; a short write is an error, because
    /// the rest could only be sent as a second, malformed message.
    pub async fn write_frame(&self, frame: &[u8]) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.write_message(frame).await
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = frame;
            Err(unsupported())
        }
    }

    /// One non-blocking `write(2)` once the descriptor reports writable.
    /// `Ok(None)` means the readiness was stale and the caller should wait
    /// again.
    #[cfg(target_os = "linux")]
    async fn write_once(&self, bytes: &[u8]) -> io::Result<Option<usize>> {
        use std::os::fd::AsRawFd;

        let mut ready = self.io.writable().await?;
        match ready.try_io(|inner| {
            let fd = inner.get_ref().as_raw_fd();
            let written =
                unsafe { libc::write(fd, bytes.as_ptr().cast::<libc::c_void>(), bytes.len()) };
            if written < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(written as usize)
            }
        }) {
            Ok(Ok(written)) => Ok(Some(written)),
            Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => Ok(None),
            Ok(Err(error)) => Err(error),
            Err(_would_block) => Ok(None),
        }
    }

    /// Message framing: the whole frame in one write, retried only while
    /// nothing has been sent.  No state is carried between attempts, so
    /// concurrent callers need no mutual exclusion.
    #[cfg(target_os = "linux")]
    async fn write_message(&self, frame: &[u8]) -> io::Result<()> {
        if frame.is_empty() {
            return Ok(());
        }
        loop {
            match self.write_once(frame).await? {
                None => continue,
                Some(written) if written == frame.len() => return Ok(()),
                Some(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "FUSE device accepted only part of a reply",
                    ));
                }
            }
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn validate_max_frame(max_frame: usize) -> io::Result<()> {
    if max_frame < crate::IN_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "FUSE frame limit is smaller than the request header",
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "native FUSE devices are supported only on Linux; macOS uses the NFS transport",
    )
}

#[cfg(target_os = "linux")]
fn path_to_cstring(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "FUSE device path contains an embedded NUL",
        )
    })
}

#[cfg(target_os = "linux")]
fn set_nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receive_buffer_keeps_protocol_slack() {
        assert_eq!(read_buffer_size(crate::IN_HEADER_SIZE), 4136);
        assert_eq!(
            read_buffer_size(DEFAULT_MAX_FRAME),
            DEFAULT_MAX_FRAME + 4096
        );
    }

    #[test]
    fn frame_limit_must_fit_a_header() {
        assert!(validate_max_frame(crate::IN_HEADER_SIZE - 1).is_err());
        assert!(validate_max_frame(crate::IN_HEADER_SIZE).is_ok());
    }

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn native_device_is_explicitly_unsupported() {
        let error = FuseDevice::open("/dev/fuse", DEFAULT_MAX_FRAME)
            .expect_err("macOS must not attempt a Linux FUSE open");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn async_device_preserves_one_message_per_read_and_write() {
        use std::os::fd::{FromRawFd, OwnedFd};

        let mut descriptors = [-1; 2];
        let result = unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                descriptors.as_mut_ptr(),
            )
        };
        assert_eq!(result, 0, "socketpair: {}", io::Error::last_os_error());
        // SAFETY: socketpair initialized both descriptors and each is owned by
        // exactly one side of this test.
        let device_fd = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
        let peer_fd = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
        let device = FuseDevice::from_owned_fd(device_fd, 4096).unwrap();

        let incoming = b"one complete FUSE frame".to_vec();
        let expected_reply = b"one complete FUSE reply".to_vec();
        let peer = std::thread::spawn(move || {
            let written = unsafe {
                libc::write(
                    std::os::fd::AsRawFd::as_raw_fd(&peer_fd),
                    incoming.as_ptr().cast::<libc::c_void>(),
                    incoming.len(),
                )
            };
            assert_eq!(written as usize, incoming.len());
            let mut reply = vec![0_u8; expected_reply.len()];
            let read = unsafe {
                libc::read(
                    std::os::fd::AsRawFd::as_raw_fd(&peer_fd),
                    reply.as_mut_ptr().cast::<libc::c_void>(),
                    reply.len(),
                )
            };
            assert_eq!(read as usize, expected_reply.len());
            assert_eq!(reply, expected_reply);
        });

        assert_eq!(
            device.read_frame().await.unwrap(),
            Some(b"one complete FUSE frame".to_vec())
        );
        device
            .write_frame(b"one complete FUSE reply")
            .await
            .unwrap();
        peer.join().unwrap();
    }

    #[cfg(target_os = "linux")]
    fn test_socket_pair(kind: libc::c_int) -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
        use std::os::fd::FromRawFd;

        let mut descriptors = [-1; 2];
        let result = unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                kind | libc::SOCK_CLOEXEC,
                0,
                descriptors.as_mut_ptr(),
            )
        };
        assert_eq!(result, 0, "socketpair: {}", io::Error::last_os_error());
        // SAFETY: socketpair initialized both descriptors and each is owned
        // by exactly one value.
        unsafe {
            (
                std::os::fd::OwnedFd::from_raw_fd(descriptors[0]),
                std::os::fd::OwnedFd::from_raw_fd(descriptors[1]),
            )
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn only_message_framed_descriptors_are_accepted() {
        use std::os::fd::FromRawFd;

        for (kind, message) in [
            (libc::SOCK_STREAM, false),
            (libc::SOCK_SEQPACKET, true),
            (libc::SOCK_DGRAM, true),
        ] {
            let (device, _peer) = test_socket_pair(kind);
            let built = FuseDevice::from_owned_fd(device, 4096);
            assert_eq!(built.is_ok(), message, "socket type {kind}");
            if let Err(error) = built {
                assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            }
        }

        let mut pipe = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        // SAFETY: pipe2 initialized both descriptors.
        let (_read, write) = unsafe {
            (
                std::os::fd::OwnedFd::from_raw_fd(pipe[0]),
                std::os::fd::OwnedFd::from_raw_fd(pipe[1]),
            )
        };
        let error = FuseDevice::from_owned_fd(write, 4096).expect_err("a pipe is a byte stream");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        // A character device that is not the FUSE device (a tty would accept
        // a reply in pieces) must not be taken for message framed.
        let null = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .unwrap();
        let error = require_message_framing(std::os::fd::AsRawFd::as_raw_fd(&null)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    /// The production path: the kernel device is message framed, so replies
    /// to it take no lock. Needs `/dev/fuse`; required when the native gate
    /// is on, skipped where the device cannot be opened otherwise.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn kernel_fuse_device_is_message_framed() {
        match FuseDevice::open("/dev/fuse", DEFAULT_MAX_FRAME) {
            Ok(device) => drop(device),
            Err(error) => {
                assert!(
                    std::env::var_os("MOUNT_RS_RUN_NATIVE_FUSE").is_none(),
                    "the native gate is on but /dev/fuse did not open: {error}"
                );
                eprintln!("skipping: /dev/fuse did not open: {error}");
            }
        }
    }

    /// A test reply: a little-endian length, then `tag` in every other byte.
    #[cfg(target_os = "linux")]
    fn tagged_frame(tag: u8, length: usize) -> Vec<u8> {
        let mut frame = vec![tag; length];
        frame[..4].copy_from_slice(&(length as u32).to_le_bytes());
        frame
    }

    #[cfg(target_os = "linux")]
    fn assert_tagged(frame: &[u8]) {
        assert_eq!(
            u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize,
            frame.len()
        );
        let tag = frame[4];
        assert!(
            frame[4..].iter().all(|byte| *byte == tag),
            "two replies were interleaved"
        );
    }

    #[cfg(target_os = "linux")]
    fn shrink_send_buffer(fd: &std::os::fd::OwnedFd) {
        let size: libc::c_int = 4096;
        let result = unsafe {
            libc::setsockopt(
                std::os::fd::AsRawFd::as_raw_fd(fd),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&raw const size).cast::<libc::c_void>(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        assert_eq!(result, 0, "SO_SNDBUF: {}", io::Error::last_os_error());
    }

    #[cfg(target_os = "linux")]
    const RACE_WRITERS: usize = 16;
    #[cfg(target_os = "linux")]
    const RACE_FRAMES_PER_WRITER: usize = 2_000;

    /// Sixteen tasks write replies at once, as the session loop and its read
    /// workers do. The send buffer is small so writers also meet a full
    /// socket and have to wait and retry.
    #[cfg(target_os = "linux")]
    async fn hammer_concurrent_writers(kind: libc::c_int, max_length: usize) {
        use std::os::fd::AsRawFd;

        let (device_fd, peer_fd) = test_socket_pair(kind);
        shrink_send_buffer(&device_fd);
        let device = std::sync::Arc::new(FuseDevice::from_owned_fd(device_fd, 4096).unwrap());

        let total = RACE_WRITERS * RACE_FRAMES_PER_WRITER;
        let peer = std::thread::spawn(move || {
            let receive = |target: &mut [u8], flags: libc::c_int| -> usize {
                let bytes = unsafe {
                    libc::recv(
                        peer_fd.as_raw_fd(),
                        target.as_mut_ptr().cast::<libc::c_void>(),
                        target.len(),
                        flags,
                    )
                };
                assert!(bytes > 0, "receive: {}", io::Error::last_os_error());
                bytes as usize
            };
            let mut buffer = vec![0_u8; max_length + 1];
            let mut seen = [0_usize; RACE_WRITERS];
            for _ in 0..total {
                // One receive is one message: a reply split in two, or
                // two replies merged, fails the length check.
                let length = receive(&mut buffer, 0);
                assert_tagged(&buffer[..length]);
                seen[buffer[4] as usize] += 1;
            }
            assert_eq!(seen, [RACE_FRAMES_PER_WRITER; RACE_WRITERS]);
        });

        let mut writers = tokio::task::JoinSet::new();
        for writer in 0..RACE_WRITERS {
            let device = std::sync::Arc::clone(&device);
            writers.spawn(async move {
                for index in 0..RACE_FRAMES_PER_WRITER {
                    let length = 8 + (index * 131 + writer * 17) % (max_length - 7);
                    device
                        .write_frame(&tagged_frame(writer as u8, length))
                        .await
                        .expect("concurrent reply write");
                }
            });
        }
        while let Some(result) = writers.join_next().await {
            result.expect("writer task");
        }
        tokio::task::spawn_blocking(move || peer.join().expect("peer thread"))
            .await
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_replies_stay_whole_on_a_message_socket_without_a_lock() {
        hammer_concurrent_writers(libc::SOCK_SEQPACKET, 2048).await;
    }

    /// A message descriptor that takes only part of a reply cannot be
    /// repaired by writing the rest: that must fail, not loop.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn message_write_larger_than_the_socket_allows_is_an_error() {
        let (device_fd, _peer) = test_socket_pair(libc::SOCK_SEQPACKET);
        shrink_send_buffer(&device_fd);
        let device = FuseDevice::from_owned_fd(device_fd, 4096).unwrap();
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            device.write_frame(&tagged_frame(1, 4 * 1024 * 1024)),
        )
        .await
        .expect("an oversized message must fail, not wait for ever")
        .expect_err("an oversized message cannot be sent whole");
        assert_eq!(error.raw_os_error(), Some(libc::EMSGSIZE));
    }
}
