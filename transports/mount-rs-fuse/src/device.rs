//! The byte transport for a FUSE connection.
//!
//! Linux exposes one complete FUSE request per `read(2)` from `/dev/fuse` and
//! accepts one complete reply per `write(2)`.  `AsyncFd` lets the session use
//! that character device without parking a Tokio worker thread.  The type is
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
    #[cfg(target_os = "linux")]
    io: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
    max_frame: usize,
}

impl std::fmt::Debug for FuseDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FuseDevice")
            .field("max_frame", &self.max_frame)
            .finish_non_exhaustive()
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
            // keeping the kernel connection alive.
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
    #[cfg(target_os = "linux")]
    pub fn from_owned_fd(fd: std::os::fd::OwnedFd, max_frame: usize) -> io::Result<Self> {
        use std::os::fd::AsRawFd;

        validate_max_frame(max_frame)?;
        set_nonblocking(fd.as_raw_fd())?;
        let io = tokio::io::unix::AsyncFd::new(fd)?;
        Ok(Self { io, max_frame })
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
    pub async fn write_frame(&self, frame: &[u8]) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;

            let mut written = 0;
            while written < frame.len() {
                let mut ready = self.io.writable().await?;
                match ready.try_io(|inner| {
                    let fd = inner.get_ref().as_raw_fd();
                    let bytes = unsafe {
                        libc::write(
                            fd,
                            frame[written..].as_ptr().cast::<libc::c_void>(),
                            frame.len() - written,
                        )
                    };
                    if bytes < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(bytes as usize)
                    }
                }) {
                    Ok(Ok(0)) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "FUSE device accepted no reply bytes",
                        ));
                    }
                    Ok(Ok(bytes)) => written += bytes,
                    Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Ok(Err(error)) => return Err(error),
                    Err(_would_block) => continue,
                }
            }
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = frame;
            Err(unsupported())
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
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
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
}
