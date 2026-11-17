//! Rotates the bearer tokens of `serve-http` drives while the server runs.
//!
//! A drive whose token is a file (`"token": {"file": ...}`) accepts the
//! tokens that file lists: one per line, at most two, so that a new token
//! can be accepted beside the old one while clients move to it. The file is
//! read again on SIGHUP and, unless polling is turned off, on a timer. A
//! file that cannot be read or does not list valid tokens is reported once
//! and the tokens in force stay as they were.
//!
//! The set a file is loaded into is the one every check of the drive's
//! token reads: the file routes, the session routes of `remote_sessions`,
//! and the owner of the drive in a forwarding cluster, which is where a
//! forwarded request is authorized. A reload therefore reaches all of them
//! at once. On a server with `remote_sessions` every token of the file
//! must also be one the session routes accept, or the file is refused
//! whole.
//!
//! Nothing here writes token text anywhere: not to a log line and not into
//! an error.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use mount_rs_http::{DriveConfig, DriveTokens, MAX_DRIVE_TOKENS};
use tokio_util::sync::CancellationToken;

use crate::runtime::CliError;

/// Largest token file read. Two tokens are far smaller.
const MAX_TOKEN_FILE_BYTES: u64 = 64 * 1024;

/// Read a token file, refusing one too large to be one.
fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    let fail = |error: &dyn std::fmt::Display| format!("cannot read {}: {error}", path.display());
    let file = std::fs::File::open(path).map_err(|error| fail(&error))?;
    let mut bytes = Vec::new();
    file.take(MAX_TOKEN_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| fail(&error))?;
    if bytes.len() as u64 > MAX_TOKEN_FILE_BYTES {
        return Err(fail(&format_args!(
            "larger than {MAX_TOKEN_FILE_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

/// The tokens a file lists: one per line, blank lines and the white space
/// around a token ignored. The file is taken whole or not at all, and a
/// refusal says which line is wrong without quoting it.
///
/// `sessions` is whether the drive also serves the session routes, whose
/// tokens have a stricter form.
fn parse_tokens(bytes: &[u8], sessions: bool) -> Result<Vec<String>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "is not UTF-8 text".to_owned())?;
    let mut tokens: Vec<String> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let token = line.trim();
        if token.is_empty() {
            continue;
        }
        DriveConfig::validate_bearer_token(token)
            .map_err(|error| format!("line {} is not a token: {error}", index + 1))?;
        if sessions && !mount_rs_cluster::SessionCredentials::valid_token(token) {
            return Err(format!(
                "line {} is not a token remote_sessions accepts: 32 to 256 characters from A-Z a-z 0-9 - . _ ~ + / =",
                index + 1
            ));
        }
        if tokens.iter().any(|seen| seen == token) {
            continue;
        }
        if tokens.len() == MAX_DRIVE_TOKENS {
            return Err(format!(
                "lists more than {MAX_DRIVE_TOKENS} tokens; keep the current one and the one before"
            ));
        }
        tokens.push(token.to_owned());
    }
    if tokens.is_empty() {
        return Err("lists no token".to_owned());
    }
    Ok(tokens)
}

/// A token file as read when the server starts.
pub(crate) struct LoadedTokens {
    path: PathBuf,
    bytes: Vec<u8>,
    tokens: DriveTokens,
    sessions: bool,
}

impl LoadedTokens {
    /// Read `path`. Unlike a reload, a file that is not valid is an error:
    /// there are no earlier tokens to keep. `sessions` is whether the
    /// server has `remote_sessions`, now and at every reload.
    pub(crate) fn load(path: &Path, sessions: bool) -> Result<Self, String> {
        let bytes = read_file(path)?;
        let tokens = parse_tokens(&bytes, sessions)
            .and_then(|tokens| DriveTokens::new(tokens).map_err(|error| error.to_string()))
            .map_err(|error| format!("token file {} {error}", path.display()))?;
        Ok(Self {
            path: path.to_owned(),
            bytes,
            tokens,
            sessions,
        })
    }

    /// The tokens, to give to the drive.
    pub(crate) fn tokens(&self) -> DriveTokens {
        self.tokens.clone()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The file is as it was when last looked at; nothing was done.
    Unseen,
    /// The file was read and lists the tokens already in force.
    Same,
    Reloaded,
    /// The file was refused; the tokens in force are unchanged.
    Refused(String),
}

/// Remembers one drive's file as last looked at, so that a poll does
/// nothing for a file that has not changed and reports a bad file once,
/// not every poll.
struct Watch {
    drive: String,
    path: PathBuf,
    seen: Result<Vec<u8>, String>,
    tokens: DriveTokens,
    sessions: bool,
}

impl Watch {
    fn new(drive: &str, loaded: LoadedTokens) -> Self {
        Self {
            drive: drive.to_owned(),
            path: loaded.path,
            seen: Ok(loaded.bytes),
            tokens: loaded.tokens,
            sessions: loaded.sessions,
        }
    }

    /// Look at the file. `forced` (SIGHUP) evaluates it even if it has not
    /// changed, so the operator always gets an answer.
    async fn look(&mut self, forced: bool) -> Outcome {
        let path = self.path.clone();
        let read = tokio::task::spawn_blocking(move || read_file(&path))
            .await
            .unwrap_or_else(|error| Err(format!("token read task failed: {error}")));
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
        let replaced = parse_tokens(bytes, self.sessions)
            .and_then(|tokens| self.tokens.replace(tokens).map_err(|e| e.to_string()));
        match replaced {
            Ok(true) => Outcome::Reloaded,
            Ok(false) => Outcome::Same,
            Err(error) => Outcome::Refused(format!("token file {} {error}", self.path.display())),
        }
    }

    /// One `http_token_reload` line on stderr. It names the drive, the
    /// outcome and how many tokens are accepted, never a token.
    fn report(&self, outcome: &Outcome) {
        let (outcome, error) = match outcome {
            Outcome::Unseen => return,
            Outcome::Same => ("unchanged", None),
            Outcome::Reloaded => ("reloaded", None),
            Outcome::Refused(reason) => ("refused", Some(reason.as_str())),
        };
        let record = serde_json::json!({
            "event": "http_token_reload",
            "drive": self.drive,
            "outcome": outcome,
            "tokens": self.tokens.len(),
            "error": error,
        });
        let _ = writeln!(std::io::stderr().lock(), "{record}");
    }
}

/// The background task that reloads token files. Dropping it stops it.
pub(crate) struct TokenReloader {
    stop: CancellationToken,
}

impl TokenReloader {
    /// Watch the token file of each drive in `drives`. `poll` is how often
    /// to look for a change; `None` reloads on SIGHUP only.
    ///
    /// From here on SIGHUP reloads the files instead of ending the process,
    /// which is why a server with no token file does not start this.
    pub(crate) fn start(
        drives: Vec<(String, LoadedTokens)>,
        poll: Option<Duration>,
    ) -> Result<Self, CliError> {
        #[cfg(unix)]
        let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
            .map_err(|error| CliError::runtime(format!("cannot listen for SIGHUP: {error}")))?;
        let mut watches: Vec<Watch> = drives
            .into_iter()
            .map(|(drive, loaded)| Watch::new(&drive, loaded))
            .collect();
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        tokio::spawn(async move {
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
                    _ = stopped.cancelled() => return,
                    _ = signalled => true,
                    _ = tick => false,
                };
                for watch in &mut watches {
                    let outcome = watch.look(forced).await;
                    watch.report(&outcome);
                }
            }
        });
        Ok(Self { stop })
    }
}

impl Drop for TokenReloader {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(text: &str) -> Result<Vec<String>, String> {
        parse_tokens(text.as_bytes(), false)
    }

    #[test]
    fn a_token_file_lists_one_or_two_tokens_one_per_line() {
        assert_eq!(tokens("current"), Ok(vec!["current".to_owned()]));
        assert_eq!(tokens("current\n"), Ok(vec!["current".to_owned()]));
        assert_eq!(
            tokens("\n  current \r\n\nprevious\n\n"),
            Ok(vec!["current".to_owned(), "previous".to_owned()])
        );
        // The same token twice is one token.
        assert_eq!(
            tokens("current\ncurrent\nprevious\n"),
            Ok(vec!["current".to_owned(), "previous".to_owned()])
        );
    }

    #[test]
    fn a_token_file_that_is_not_valid_is_refused_without_quoting_it() {
        for (text, reason) in [
            ("", "lists no token"),
            ("\n \n", "lists no token"),
            ("first-secret\nsecond-secret\nthird-secret\n", "more than 2"),
            ("first-secret\nsecond secret\n", "line 2 is not a token"),
        ] {
            let error = tokens(text).expect_err(text);
            assert!(error.contains(reason), "{error}");
            assert!(!error.contains("secret"), "{error}");
        }
        assert_eq!(
            parse_tokens(&[b's', 0xff, b'\n'], false),
            Err("is not UTF-8 text".to_owned())
        );
    }

    struct Fixture {
        _root: tempfile::TempDir,
        watch: Watch,
        tokens: DriveTokens,
    }

    impl Fixture {
        fn new(text: &str) -> Self {
            Self::with_sessions(text, false)
        }

        fn with_sessions(text: &str, sessions: bool) -> Self {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("drive.token");
            std::fs::write(&path, text).unwrap();
            let loaded = LoadedTokens::load(&path, sessions).expect("token file");
            let tokens = loaded.tokens();
            Self {
                _root: root,
                watch: Watch::new("drive", loaded),
                tokens,
            }
        }

        fn write(&self, text: &str) {
            std::fs::write(&self.watch.path, text).unwrap();
        }
    }

    #[test]
    fn a_token_file_that_is_missing_or_not_valid_stops_startup() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("drive.token");
        let missing = LoadedTokens::load(&path, false).err().expect("missing");
        assert!(missing.contains("cannot read"), "{missing}");
        std::fs::write(&path, "a-secret with-a-space\n").unwrap();
        let invalid = LoadedTokens::load(&path, false).err().expect("invalid");
        assert!(invalid.contains("line 1 is not a token"), "{invalid}");
        assert!(!invalid.contains("secret"), "{invalid}");
        std::fs::write(&path, vec![b'x'; MAX_TOKEN_FILE_BYTES as usize + 1]).unwrap();
        let large = LoadedTokens::load(&path, false).err().expect("too large");
        assert!(large.contains("larger than"), "{large}");
    }

    /// With `remote_sessions` a token must also be one the session routes
    /// accept, at startup and at every reload. A file with one that is not
    /// is refused whole, so the file routes and the session routes never
    /// disagree about which tokens the drive accepts.
    #[tokio::test]
    async fn with_sessions_a_token_the_session_routes_refuse_is_not_loaded() {
        let (old, new) = ("o".repeat(32), "n".repeat(40));
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("drive.token");
        std::fs::write(&path, "a-short-secret\n").unwrap();
        // The same file is a valid token file without sessions.
        assert!(LoadedTokens::load(&path, false).is_ok());
        let refused = LoadedTokens::load(&path, true).err().expect("too short");
        assert!(refused.contains("line 1 is not a token remote_sessions accepts"));
        assert!(!refused.contains("secret"), "{refused}");

        let mut fixture = Fixture::with_sessions(&format!("{old}\n"), true);
        fixture.write(&format!("{new}\n{old}\n"));
        assert_eq!(fixture.watch.look(false).await, Outcome::Reloaded);
        assert_eq!(fixture.tokens.len(), 2);

        // Too short, and a character the session routes do not take.
        for bad in ["a-short-secret".to_owned(), format!("{new}!")] {
            fixture.write(&format!("{bad}\n{new}\n"));
            let Outcome::Refused(reason) = fixture.watch.look(false).await else {
                panic!("a token the session routes refuse was loaded");
            };
            assert!(reason.contains("line 1 is not a token remote_sessions accepts"));
            assert!(!reason.contains("secret") && !reason.contains(&new));
            assert_eq!(fixture.tokens.len(), 2);
        }

        fixture.write(&format!("{new}\n"));
        assert_eq!(fixture.watch.look(false).await, Outcome::Reloaded);
        assert_eq!(fixture.tokens.len(), 1);
    }

    #[tokio::test]
    async fn a_changed_file_replaces_the_tokens_and_an_unchanged_one_does_nothing() {
        let mut fixture = Fixture::new("old\n");
        assert_eq!(fixture.watch.look(false).await, Outcome::Unseen);
        // Asked for by SIGHUP, the same file still gets an answer.
        assert_eq!(fixture.watch.look(true).await, Outcome::Same);

        fixture.write("new\nold\n");
        assert_eq!(fixture.watch.look(false).await, Outcome::Reloaded);
        assert_eq!(fixture.tokens.len(), 2);
        assert_eq!(fixture.watch.look(false).await, Outcome::Unseen);

        // Rewritten differently but listing the same tokens.
        fixture.write("new\n\nold\n");
        assert_eq!(fixture.watch.look(false).await, Outcome::Same);

        fixture.write("new\n");
        assert_eq!(fixture.watch.look(true).await, Outcome::Reloaded);
        assert_eq!(fixture.tokens.len(), 1);
    }

    #[tokio::test]
    async fn a_bad_file_is_refused_once_and_the_tokens_stay() {
        let mut fixture = Fixture::new("new\nold\n");

        fixture.write("new\nold\nthird\n");
        let Outcome::Refused(reason) = fixture.watch.look(false).await else {
            panic!("a file with three tokens was accepted");
        };
        assert!(reason.contains("more than 2"), "{reason}");
        assert_eq!(fixture.tokens.len(), 2);
        // Not again at the next poll, but again when asked.
        assert_eq!(fixture.watch.look(false).await, Outcome::Unseen);
        assert!(matches!(
            fixture.watch.look(true).await,
            Outcome::Refused(_)
        ));

        std::fs::remove_file(&fixture.watch.path).unwrap();
        let Outcome::Refused(reason) = fixture.watch.look(false).await else {
            panic!("a missing file was accepted");
        };
        assert!(reason.contains("cannot read"), "{reason}");
        assert_eq!(fixture.watch.look(false).await, Outcome::Unseen);
        assert_eq!(fixture.tokens.len(), 2);

        // The file is back as it was: accepted, and nothing to change.
        fixture.write("new\nold\n");
        assert_eq!(fixture.watch.look(false).await, Outcome::Same);
    }
}
