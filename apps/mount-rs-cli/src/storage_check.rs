//! Whether the storage under a `serve-http` drive is still there.
//!
//! A split-store drive knows when it has lost its storage: its lease worker
//! talks to it all the time. A `host` drive and a `sqlite` drive talk to
//! theirs only when a request does, so an idle one whose directory or
//! database file has gone would stay ready. One thread per such drive looks
//! every [`INTERVAL`], off the request path, and `/readyz` reads what it
//! last found. A look still out after [`TIMEOUT`] (a mount that has stopped
//! answering) counts as a failure for as long as it stays out.
//!
//! The thread is a plain one, not a task of the runtime's blocking pool:
//! the runtime waits for that pool when the process ends, and a look that
//! never returns must not keep the process from exiting.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use mount_rs_core::{ErrorCode, FsError};
use mount_rs_http::HealthCheck;

/// How often the storage is looked at.
pub(crate) const INTERVAL: Duration = Duration::from_secs(2);
/// How long one look may take before the storage counts as unreachable.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(5);

/// What is looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Storage {
    /// The root of a `host` drive: it must be a directory that can be opened.
    Directory(PathBuf),
    /// The database of a `sqlite` drive: it must be a file that can be opened.
    File(PathBuf),
}

impl Storage {
    fn look(&self) -> std::io::Result<()> {
        match self {
            Self::Directory(path) => std::fs::read_dir(path).map(drop),
            Self::File(path) => {
                if std::fs::File::open(path)?.metadata()?.is_file() {
                    Ok(())
                } else {
                    Err(std::io::Error::other("not a regular file"))
                }
            }
        }
    }
}

struct Found {
    /// Counted from here, so that a time fits an atomic.
    origin: Instant,
    unreachable: AtomicBool,
    /// When the look now out began, in milliseconds after `origin` plus
    /// one; zero while none is out.
    looking_since: AtomicU64,
    timeout: Duration,
    stop: AtomicBool,
}

impl Found {
    fn health(&self) -> Result<(), FsError> {
        let since = self.looking_since.load(Ordering::Acquire);
        if since != 0 {
            let began = self.origin + Duration::from_millis(since - 1);
            if began.elapsed() > self.timeout {
                return Err(
                    FsError::new(ErrorCode::Eio).with_message("the drive's storage did not answer")
                );
            }
        }
        if self.unreachable.load(Ordering::Acquire) {
            return Err(
                FsError::new(ErrorCode::Eio).with_message("the drive's storage is unreachable")
            );
        }
        Ok(())
    }
}

/// The thread that looks, and what it last found. Dropping it stops the
/// thread at its next look.
pub(crate) struct StorageCheck {
    found: Arc<Found>,
    thread: std::thread::Thread,
}

impl StorageCheck {
    /// Start looking at `storage` for the drive `drive`. Each change of
    /// what is found, and each look that took longer than [`TIMEOUT`], is
    /// written to stderr as one `http_drive_storage` line.
    pub(crate) fn start(drive: &str, storage: Storage) -> std::io::Result<Self> {
        let drive = drive.to_owned();
        Self::start_with(storage, INTERVAL, TIMEOUT, move |reachable, error, took| {
            let record = serde_json::json!({
                "event": "http_drive_storage",
                "drive": drive,
                "reachable": reachable,
                "error": error,
                "check_ms": took.as_millis() as u64,
            });
            let _ = writeln!(std::io::stderr().lock(), "{record}");
        })
    }

    fn start_with(
        storage: Storage,
        interval: Duration,
        timeout: Duration,
        report: impl Fn(bool, Option<String>, Duration) + Send + 'static,
    ) -> std::io::Result<Self> {
        let found = Arc::new(Found {
            origin: Instant::now(),
            unreachable: AtomicBool::new(false),
            looking_since: AtomicU64::new(0),
            timeout,
            stop: AtomicBool::new(false),
        });
        let state = Arc::clone(&found);
        let thread = std::thread::Builder::new()
            .name("http-storage-check".to_owned())
            .spawn(move || {
                while !state.stop.load(Ordering::Acquire) {
                    let began = Instant::now();
                    let since = began.duration_since(state.origin).as_millis() as u64 + 1;
                    state.looking_since.store(since, Ordering::Release);
                    let looked = storage.look();
                    let took = began.elapsed();
                    let unreachable = looked.is_err();
                    let changed =
                        state.unreachable.swap(unreachable, Ordering::AcqRel) != unreachable;
                    state.looking_since.store(0, Ordering::Release);
                    if changed || took > state.timeout {
                        report(
                            !unreachable,
                            looked.err().map(|error| error.kind().to_string()),
                            took,
                        );
                    }
                    std::thread::park_timeout(interval);
                }
            })?
            .thread()
            .clone();
        Ok(Self { found, thread })
    }

    /// The check `/readyz` runs: what was last found, read from memory.
    pub(crate) fn health_check(&self) -> HealthCheck {
        let found = Arc::clone(&self.found);
        Arc::new(move || found.health())
    }
}

impl Drop for StorageCheck {
    fn drop(&mut self) {
        self.found.stop.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const QUICK: Duration = Duration::from_millis(10);
    const PATIENCE: Duration = Duration::from_secs(10);

    type Changes = mpsc::Receiver<(bool, Duration)>;

    fn watch(storage: Storage, timeout: Duration) -> (StorageCheck, Changes) {
        let (changes, seen) = mpsc::channel();
        let check = StorageCheck::start_with(storage, QUICK, timeout, move |reachable, _, took| {
            let _ = changes.send((reachable, took));
        })
        .expect("thread");
        (check, seen)
    }

    fn quiet(seen: &Changes) {
        assert!(seen.recv_timeout(QUICK * 10).is_err(), "nothing changed");
    }

    #[test]
    fn a_directory_that_goes_away_is_unreachable_until_it_is_back() {
        let parent = tempfile::tempdir().expect("tempdir");
        let root = parent.path().join("root");
        std::fs::create_dir(&root).expect("root");
        let (check, seen) = watch(Storage::Directory(root.clone()), TIMEOUT);
        let health = check.health_check();
        assert!(health().is_ok(), "a drive that has just opened is ready");
        quiet(&seen);
        assert!(health().is_ok());

        std::fs::remove_dir(&root).expect("remove");
        assert!(!seen.recv_timeout(PATIENCE).expect("a change").0);
        assert_eq!(health().expect_err("gone").code, ErrorCode::Eio);

        // A file where the directory was is not the directory.
        std::fs::write(&root, b"").expect("file");
        quiet(&seen);
        assert!(health().is_err());
        std::fs::remove_file(&root).expect("remove");

        std::fs::create_dir(&root).expect("root");
        assert!(seen.recv_timeout(PATIENCE).expect("a change").0);
        assert!(health().is_ok());
        quiet(&seen);
    }

    #[test]
    fn a_database_file_that_goes_away_is_unreachable_until_it_is_back() {
        let directory = tempfile::tempdir().expect("tempdir");
        let database = directory.path().join("drive.db");
        std::fs::write(&database, b"x").expect("database");
        let (check, seen) = watch(Storage::File(database.clone()), TIMEOUT);
        let health = check.health_check();
        quiet(&seen);
        assert!(health().is_ok());

        std::fs::remove_file(&database).expect("remove");
        assert!(!seen.recv_timeout(PATIENCE).expect("a change").0);
        assert!(health().is_err());

        // A directory of that name is not a database.
        std::fs::create_dir(&database).expect("directory");
        quiet(&seen);
        assert!(health().is_err());
        std::fs::remove_dir(&database).expect("remove");

        std::fs::write(&database, b"x").expect("database");
        assert!(seen.recv_timeout(PATIENCE).expect("a change").0);
        assert!(health().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_look_that_does_not_return_is_a_failure_while_it_is_out() {
        // Opening a FIFO for reading waits for a writer: storage that
        // does not answer.
        let directory = tempfile::tempdir().expect("tempdir");
        let fifo = directory.path().join("fifo");
        let name = std::ffi::CString::new(fifo.to_str().expect("path")).expect("name");
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);

        let timeout = QUICK * 10;
        let (check, seen) = watch(Storage::File(fifo.clone()), timeout);
        let health = check.health_check();
        // Not yet: the look has not been out for long enough.
        assert!(health().is_ok());
        std::thread::sleep(timeout * 3);
        let stalled = health().expect_err("stalled");
        assert!(stalled.to_string().contains("did not answer"), "{stalled}");
        assert!(seen.try_recv().is_err(), "the look has not returned");

        // The storage answers. The look returns, late, and with the news
        // that a FIFO is not a database.
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&fifo)
            .expect("writer");
        let (reachable, took) = seen.recv_timeout(PATIENCE).expect("the look returned");
        assert!(!reachable);
        assert!(took > timeout, "{took:?}");
        let refused = health().expect_err("not a file");
        assert!(refused.to_string().contains("unreachable"), "{refused}");

        // Stop the thread while its looks still return.
        drop(check);
        std::thread::sleep(QUICK * 10);
        drop(writer);
    }

    #[test]
    fn dropping_the_check_stops_its_thread() {
        let directory = tempfile::tempdir().expect("tempdir");
        let (changes, seen) = mpsc::channel::<()>();
        let check = StorageCheck::start_with(
            Storage::Directory(directory.path().to_owned()),
            Duration::from_secs(3600),
            TIMEOUT,
            move |_, _, _| {
                // Held by the thread: the channel closes when it ends.
                let _ = &changes;
            },
        )
        .expect("thread");
        std::thread::sleep(QUICK * 5);
        drop(check);
        assert_eq!(
            seen.recv_timeout(PATIENCE),
            Err(mpsc::RecvTimeoutError::Disconnected),
            "the thread ended although it was an hour from its next look"
        );
    }
}
