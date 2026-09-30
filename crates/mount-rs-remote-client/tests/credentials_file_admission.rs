//! File credential admission regressions. FIFO probes run in owned child
//! processes so the unfixed blocking open cannot strand a test-runtime worker.

#![cfg(unix)]

use std::ffi::CString;
use std::future::Future;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::task::Poll;
use std::time::{Duration, Instant};

use mount_rs_remote_client::credentials::{CredentialError, CredentialSource};

const JWT: &str = "e30.e30.e30";
const PROBE_PATH_ENV: &str = "MOUNT_RS_FILE_CREDENTIAL_PROBE_PATH";
const PROBE_MARKER_ENV: &str = "MOUNT_RS_FILE_CREDENTIAL_PROBE_MARKER";
const PROBE_MODE_ENV: &str = "MOUNT_RS_FILE_CREDENTIAL_PROBE_MODE";
const PROBE_LIMIT: Duration = Duration::from_secs(3);
const READ_MARKER: &[u8] = b"file-fifo-io-refusal-v1\n";
const CANCEL_MARKER: &[u8] = b"file-fifo-cancel-runtime-drained-v1\n";

struct OwnedProbe {
    child: Child,
    reaped: bool,
}

impl OwnedProbe {
    fn wait_until(&mut self, deadline: Instant) -> Option<ExitStatus> {
        loop {
            if let Some(status) = self.child.try_wait().expect("inspect owned probe") {
                self.reaped = true;
                return Some(status);
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                self.child.wait().expect("reap timed-out owned probe");
                self.reaped = true;
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for OwnedProbe {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn create_fifo(path: &Path) {
    let path = CString::new(path.as_os_str().as_bytes()).expect("FIFO path has no NUL");
    // SAFETY: path is a valid NUL-terminated string for this call. The new FIFO
    // is created inside the parent test's private temporary directory.
    let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(result, 0, "create private FIFO");
}

fn check_owned_fifo_probe(follow_symlink: bool, cancel_after_first_poll: bool) {
    let dir = tempfile::tempdir().expect("private probe directory");
    let fifo = dir.path().join("fifo");
    create_fifo(&fifo);
    let path = if follow_symlink {
        let link = dir.path().join("token");
        std::os::unix::fs::symlink("fifo", &link).expect("FIFO token symlink");
        link
    } else {
        fifo
    };
    let marker = dir.path().join("complete");
    let mode = if cancel_after_first_poll {
        "cancel"
    } else {
        "read"
    };
    // The child runs only the file probe and creates no processes of its own.
    // Null stdio cannot leave an unread pipe or an inherited output writer.
    let mut probe = OwnedProbe {
        child: Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--ignored",
                "--exact",
                "isolated_fifo_probe",
                "--test-threads=1",
            ])
            .env(PROBE_PATH_ENV, &path)
            .env(PROBE_MARKER_ENV, &marker)
            .env(PROBE_MODE_ENV, mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn owned credential-file probe"),
        reaped: false,
    };
    let status = probe.wait_until(Instant::now() + PROBE_LIMIT);
    // Reap before any assertion or TempDir cleanup, including the RED timeout.
    drop(probe);
    let status = status.expect(
        "FIFO credential reader did not terminate within its test bound; owned child was killed and reaped",
    );
    assert!(status.success(), "owned credential-file probe failed");
    let expected = if cancel_after_first_poll {
        CANCEL_MARKER
    } else {
        READ_MARKER
    };
    assert_eq!(
        std::fs::read(&marker).expect("exact probe completion marker"),
        expected,
        "a successful zero-test selector is not a completed file probe",
    );
}

#[test]
fn fifo_without_a_writer_is_rejected() {
    check_owned_fifo_probe(false, false);
}

#[test]
fn symlink_to_fifo_without_a_writer_is_rejected() {
    check_owned_fifo_probe(true, false);
}

#[test]
fn cancelling_fifo_token_after_first_poll_does_not_strand_the_runtime() {
    check_owned_fifo_probe(false, true);
}

#[test]
fn cancelling_symlinked_fifo_token_after_first_poll_does_not_strand_the_runtime() {
    check_owned_fifo_probe(true, true);
}

#[test]
#[ignore = "invoked only by the parent-owned FIFO regression processes"]
fn isolated_fifo_probe() {
    let path = std::env::var_os(PROBE_PATH_ENV).expect("parent-owned probe path");
    let marker = std::env::var_os(PROBE_MARKER_ENV).expect("parent-owned marker path");
    let mode = std::env::var(PROBE_MODE_ENV).expect("parent-owned probe mode");
    assert!(
        matches!(mode.as_str(), "read" | "cancel"),
        "known probe mode"
    );
    assert!(
        std::fs::metadata(&path)
            .expect("owned FIFO metadata")
            .file_type()
            .is_fifo(),
        "the isolated probe admits only its FIFO fixture",
    );
    let source = CredentialSource::File(path.into());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("probe runtime");
    if mode == "read" {
        assert!(
            matches!(runtime.block_on(source.token()), Err(CredentialError::Io)),
            "a non-regular token source is an I/O error",
        );
    } else {
        // One poll starts the real Tokio file operation; dropping its future
        // cancels the caller. Runtime destruction must still finish. Started
        // blocking filesystem work is not canceled by dropping its join handle.
        let mut future = Box::pin(source.token());
        runtime.block_on(std::future::poll_fn(|cx| {
            if let Poll::Ready(result) = future.as_mut().poll(cx) {
                assert!(matches!(result, Err(CredentialError::Io)));
            }
            Poll::Ready(())
        }));
        drop(future);
    }
    drop(runtime);
    // The marker follows the Io assertion or cancellation plus runtime drain.
    // A misspelled exact selector cannot produce it. No token is printed.
    let mut complete = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)
        .expect("create completion marker");
    complete
        .write_all(if mode == "read" {
            READ_MARKER
        } else {
            CANCEL_MARKER
        })
        .expect("write completion marker");
}

#[tokio::test]
async fn device_file_is_rejected_as_io_before_token_parsing() {
    assert!(matches!(
        CredentialSource::File("/dev/null".into()).token().await,
        Err(CredentialError::Io),
    ));
}

#[tokio::test]
async fn directory_is_rejected_as_io() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        CredentialSource::File(dir.path().to_owned()).token().await,
        Err(CredentialError::Io),
    ));
}

#[tokio::test]
async fn projected_file_symlink_is_fresh_after_atomic_directory_link_replacement() {
    let dir = tempfile::tempdir().unwrap();
    for (name, token) in [("version-1", JWT), ("version-2", "YWJj.ZGVm.Z2hp")] {
        let version = dir.path().join(name);
        tokio::fs::create_dir(&version).await.unwrap();
        tokio::fs::write(version.join("token"), format!("{token}\n"))
            .await
            .unwrap();
    }
    std::os::unix::fs::symlink("version-1", dir.path().join("..data")).unwrap();
    std::os::unix::fs::symlink("..data/token", dir.path().join("token")).unwrap();
    let source = CredentialSource::File(dir.path().join("token"));
    assert_eq!(source.token().await.unwrap().expose(), JWT);

    std::os::unix::fs::symlink("version-2", dir.path().join("..data-next")).unwrap();
    tokio::fs::rename(dir.path().join("..data-next"), dir.path().join("..data"))
        .await
        .unwrap();
    assert_eq!(source.token().await.unwrap().expose(), "YWJj.ZGVm.Z2hp");
}
