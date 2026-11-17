//! Reloads a forwarding server's trusted membership file while it runs.
//!
//! The file is read again on SIGHUP and, unless polling is turned off, on a
//! timer. A file that cannot be read, parsed or accepted is reported once
//! and the membership in force stays as it was.
use crate::runtime::CliError;
use mount_rs_cluster::{IngressForwarder, MembershipChange, MembershipError, PeerDirectory};
use std::{future::Future, io::Read, path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;

/// Largest membership file read. The most a file may list is 1,024 records
/// of a few hundred bytes each.
const MAX_MEMBERSHIP_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Read the membership file, refusing one too large to be a membership.
pub(crate) fn read_membership(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let fail = |error: &dyn std::fmt::Display| format!("cannot read {}: {error}", path.display());
    let file = std::fs::File::open(path).map_err(|e| fail(&e))?;
    let mut bytes = Vec::new();
    file.take(MAX_MEMBERSHIP_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| fail(&e))?;
    if bytes.len() as u64 > MAX_MEMBERSHIP_FILE_BYTES {
        return Err(fail(&format_args!(
            "larger than {MAX_MEMBERSHIP_FILE_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The file is as it was when last looked at; nothing was done.
    Unseen,
    /// The file was read and describes the membership already in force.
    Same,
    Reloaded(MembershipChange),
    /// The file was refused; the membership in force is unchanged.
    Refused(String),
}

/// Remembers the file as last looked at, so that a poll does nothing for a
/// file that has not changed and reports a bad file once, not every poll.
struct Watch {
    path: PathBuf,
    seen: Result<Vec<u8>, String>,
}

impl Watch {
    /// Look at the file. `forced` (SIGHUP) evaluates it even if it has not
    /// changed, so the operator always gets an answer.
    async fn look<A, F>(&mut self, forced: bool, apply: A) -> Outcome
    where
        A: FnOnce(PeerDirectory) -> F,
        F: Future<Output = Result<MembershipChange, MembershipError>>,
    {
        let path = self.path.clone();
        let read = tokio::task::spawn_blocking(move || read_membership(&path))
            .await
            .unwrap_or_else(|error| Err(format!("membership read task failed: {error}")));
        // Comparing the bytes, not the modification time, also catches a
        // file replaced within the timestamp's resolution.
        if !forced && read == self.seen {
            return Outcome::Unseen;
        }
        self.seen = read;
        let bytes = match &self.seen {
            Ok(bytes) => bytes,
            Err(error) => return Outcome::Refused(error.clone()),
        };
        let candidate = match PeerDirectory::parse(bytes.as_slice()) {
            Ok(candidate) => candidate,
            Err(error) => return Outcome::Refused(error.to_string()),
        };
        match apply(candidate).await {
            Ok(change) if change.is_empty() => Outcome::Same,
            Ok(change) => Outcome::Reloaded(change),
            Err(error) => Outcome::Refused(error.to_string()),
        }
    }
}

fn report(path: &std::path::Path, members: usize, outcome: &Outcome) {
    let path = path.display();
    match outcome {
        Outcome::Unseen => {}
        Outcome::Same => eprintln!(
            "mount-rs: membership reload: {path} matches the membership in use ({members} servers)"
        ),
        Outcome::Reloaded(change) => eprintln!(
            "mount-rs: membership reload: now {members} servers from {path}; added {:?}, removed {:?}, changed {:?}",
            change.added, change.removed, change.changed
        ),
        Outcome::Refused(reason) => eprintln!(
            "mount-rs: membership reload refused, keeping the current {members} servers: {reason}"
        ),
    }
}

/// The background task that reloads the membership. Dropping it stops it.
pub(crate) struct MembershipReloader {
    stop: CancellationToken,
    task: crate::joined::JoinedTask<String>,
}

impl MembershipReloader {
    /// Watch `path`. `loaded` is the file as read at startup. `poll` is how
    /// often to look for a change; `None` reloads on SIGHUP only.
    pub(crate) fn start(
        path: PathBuf,
        loaded: Vec<u8>,
        poll: Option<Duration>,
        forwarder: IngressForwarder,
    ) -> Result<Self, CliError> {
        #[cfg(unix)]
        let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
            .map_err(|error| CliError::runtime(format!("cannot listen for SIGHUP: {error}")))?;
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let task = async move {
            let mut watch = Watch {
                path,
                seen: Ok(loaded),
            };
            loop {
                let tick = async {
                    match poll {
                        Some(interval) => tokio::time::sleep(interval).await,
                        None => std::future::pending().await,
                    }
                };
                #[cfg(unix)]
                let signalled = hangup.recv();
                #[cfg(not(unix))]
                let signalled = std::future::pending::<Option<()>>();
                let forced = tokio::select! {
                    _ = stopped.cancelled() => return Ok(()),
                    _ = signalled => true,
                    _ = tick => false,
                };
                let outcome = watch
                    .look(forced, |candidate| forwarder.reload_membership(candidate))
                    .await;
                report(
                    &watch.path,
                    forwarder.membership().directory().len(),
                    &outcome,
                );
            }
        };
        let task = crate::joined::JoinedTask::spawn(task, || {
            "membership reload task ended without a result".to_owned()
        });
        Ok(Self { stop, task })
    }

    /// Stop reloading and wait for a reload in progress to finish.
    pub(crate) async fn stop(&self) -> Result<(), CliError> {
        self.stop.cancel();
        self.task.join().await.map_err(CliError::runtime)
    }
}

impl Drop for MembershipReloader {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_cluster::PeerMembership;
    use std::sync::Arc;

    fn file(nodes: &[&str]) -> Vec<u8> {
        let records: Vec<_> = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                serde_json::json!({
                    "node": node, "boot": "boot-a",
                    "address": format!("127.0.0.1:{}", 4000 + index),
                    "tls_server_name": "localhost",
                    "spki_sha256": format!("{:02x}", index + 1).repeat(32),
                })
            })
            .collect();
        serde_json::to_vec(&records).unwrap()
    }
    struct Fixture {
        _root: tempfile::TempDir,
        membership: Arc<PeerMembership>,
        watch: Watch,
    }
    impl Fixture {
        fn new(nodes: &[&str]) -> Self {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("peers.json");
            let loaded = file(nodes);
            std::fs::write(&path, &loaded).unwrap();
            let directory = PeerDirectory::parse(loaded.as_slice()).unwrap();
            Self {
                _root: root,
                membership: Arc::new(PeerMembership::new(Arc::new(directory), 4).unwrap()),
                watch: Watch {
                    path,
                    seen: Ok(loaded),
                },
            }
        }
        fn write(&self, bytes: &[u8]) {
            std::fs::write(&self.watch.path, bytes).unwrap();
        }
        async fn look(&mut self, forced: bool) -> Outcome {
            let membership = self.membership.clone();
            self.watch
                .look(
                    forced,
                    |candidate| async move { membership.apply(candidate) },
                )
                .await
        }
        fn nodes(&self) -> Vec<String> {
            self.membership
                .directory()
                .members()
                .map(|(node, _)| node.to_owned())
                .collect()
        }
    }

    #[tokio::test]
    async fn a_changed_file_is_applied_and_an_unchanged_one_is_left_alone() {
        let mut fixture = Fixture::new(&["a", "b"]);
        assert_eq!(fixture.look(false).await, Outcome::Unseen);
        // SIGHUP answers even when the file has not changed.
        assert_eq!(fixture.look(true).await, Outcome::Same);
        fixture.write(&file(&["a", "b", "c"]));
        let Outcome::Reloaded(change) = fixture.look(false).await else {
            panic!("a changed file was not applied");
        };
        assert_eq!(change.added, ["c"]);
        assert_eq!(fixture.nodes(), ["a", "b", "c"]);
        assert_eq!(fixture.look(false).await, Outcome::Unseen);
        // The same membership written differently is read but changes nothing.
        let mut spaced = file(&["a", "b", "c"]);
        spaced.push(b'\n');
        fixture.write(&spaced);
        assert_eq!(fixture.look(false).await, Outcome::Same);
    }

    #[tokio::test]
    async fn a_bad_file_keeps_the_membership_and_is_reported_once() {
        let mut fixture = Fixture::new(&["a", "b"]);
        for bad in [
            &b"[{\"node\":\"a\","[..],
            b"[]",
            b"",
            // More servers than this process has room for.
            &file(&["a", "b", "c", "d", "e"]),
        ] {
            fixture.write(bad);
            assert!(matches!(fixture.look(false).await, Outcome::Refused(_)));
            assert_eq!(fixture.nodes(), ["a", "b"]);
            // The next poll sees the same bad file and stays quiet.
            assert_eq!(fixture.look(false).await, Outcome::Unseen);
            // SIGHUP reports it again.
            assert!(matches!(fixture.look(true).await, Outcome::Refused(_)));
        }
        // A good file is picked up after a bad one.
        fixture.write(&file(&["b", "c"]));
        assert!(matches!(fixture.look(false).await, Outcome::Reloaded(_)));
        assert_eq!(fixture.nodes(), ["b", "c"]);
    }

    #[tokio::test]
    async fn a_missing_or_oversized_file_keeps_the_membership() {
        let mut fixture = Fixture::new(&["a", "b"]);
        std::fs::remove_file(&fixture.watch.path).unwrap();
        let Outcome::Refused(reason) = fixture.look(false).await else {
            panic!("a missing file was not refused");
        };
        assert!(reason.contains("cannot read"), "{reason}");
        assert_eq!(fixture.look(false).await, Outcome::Unseen);
        fixture.write(&vec![b' '; MAX_MEMBERSHIP_FILE_BYTES as usize + 1]);
        let Outcome::Refused(reason) = fixture.look(false).await else {
            panic!("an oversized file was not refused");
        };
        assert!(reason.contains("larger than"), "{reason}");
        assert_eq!(fixture.nodes(), ["a", "b"]);
        // Restoring the original file needs no reload and is not an error.
        fixture.write(&file(&["a", "b"]));
        assert_eq!(fixture.look(false).await, Outcome::Same);
    }
}
