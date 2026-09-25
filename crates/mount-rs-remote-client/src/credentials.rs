//! Fresh, bounded credentials for remote Drive requests.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use base64::Engine;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_INPUT_BYTES: u64 = MAX_TOKEN_BYTES as u64 + 2;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// A credential source that is evaluated for each request.
#[derive(Clone)]
pub enum CredentialSource {
    File(PathBuf),
    Command(Vec<OsString>),
}

impl fmt::Debug for CredentialSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(_) => f.write_str("CredentialSource::File([redacted])"),
            Self::Command(_) => f.write_str("CredentialSource::Command([redacted])"),
        }
    }
}

#[derive(Clone)]
pub struct SecretToken(String);

impl SecretToken {
    pub fn expose(&self) -> &str {
        &self.0
    }

    fn parse(input: &[u8]) -> Result<Self, CredentialError> {
        let text = std::str::from_utf8(input).map_err(|_| CredentialError::InvalidToken)?;
        let token = text.trim();
        if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
            return Err(if token.len() > MAX_TOKEN_BYTES {
                CredentialError::TooLarge
            } else {
                CredentialError::InvalidToken
            });
        }
        let mut segments = token.split('.');
        for _ in 0..3 {
            let segment = segments.next().ok_or(CredentialError::InvalidToken)?;
            if segment.is_empty()
                || base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(segment)
                    .is_err()
            {
                return Err(CredentialError::InvalidToken);
            }
        }
        if segments.next().is_some() {
            return Err(CredentialError::InvalidToken);
        }
        Ok(Self(token.to_owned()))
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretToken([redacted])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialError {
    Io,
    EmptyCommand,
    CommandFailed,
    InvalidToken,
    TooLarge,
    Timeout,
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io => f.write_str("credential I/O failed"),
            Self::EmptyCommand => f.write_str("credential command is empty"),
            Self::CommandFailed => f.write_str("credential command failed"),
            Self::InvalidToken => f.write_str("credential is not a valid JWT"),
            Self::TooLarge => f.write_str("credential exceeds the size limit"),
            Self::Timeout => f.write_str("credential command timed out"),
        }
    }
}

impl std::error::Error for CredentialError {}

impl CredentialSource {
    pub async fn token(&self) -> Result<SecretToken, CredentialError> {
        self.token_with_timeout(COMMAND_TIMEOUT).await
    }

    async fn token_with_timeout(&self, timeout: Duration) -> Result<SecretToken, CredentialError> {
        match self {
            Self::File(path) => {
                let file = tokio::fs::File::open(path)
                    .await
                    .map_err(|_| CredentialError::Io)?;
                let mut contents = Vec::new();
                file.take(MAX_INPUT_BYTES)
                    .read_to_end(&mut contents)
                    .await
                    .map_err(|_| CredentialError::Io)?;
                if contents.len() as u64 == MAX_INPUT_BYTES {
                    return Err(CredentialError::TooLarge);
                }
                SecretToken::parse(&contents)
            }
            Self::Command(argv) => {
                let (program, args) = argv.split_first().ok_or(CredentialError::EmptyCommand)?;
                let mut child = Command::new(program)
                    .args(args)
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .stdout(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|_| CredentialError::Io)?;
                let stdout = child.stdout.take().ok_or(CredentialError::Io)?;
                let result = tokio::time::timeout(timeout, async {
                    let mut contents = Vec::new();
                    stdout
                        .take(MAX_INPUT_BYTES)
                        .read_to_end(&mut contents)
                        .await
                        .map_err(|_| CredentialError::Io)?;
                    if contents.len() as u64 == MAX_INPUT_BYTES {
                        return Err(CredentialError::TooLarge);
                    }
                    let status = child.wait().await.map_err(|_| CredentialError::Io)?;
                    if !status.success() {
                        return Err(CredentialError::CommandFailed);
                    }
                    SecretToken::parse(&contents)
                })
                .await;
                match result {
                    Ok(Ok(token)) => Ok(token),
                    Ok(Err(error)) => {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        Err(error)
                    }
                    Err(_) => {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        Err(CredentialError::Timeout)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    const JWT: &str = "e30.e30.e30";

    #[tokio::test]
    async fn file_is_read_again_after_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        tokio::fs::write(&path, format!(" \n{JWT}\n"))
            .await
            .unwrap();
        let source = CredentialSource::File(path.clone());
        assert_eq!(source.token().await.unwrap().expose(), JWT);

        let replacement = dir.path().join("replacement");
        tokio::fs::write(&replacement, "YWJj.ZGVm.Z2hp\n")
            .await
            .unwrap();
        tokio::fs::rename(replacement, path).await.unwrap();
        assert_eq!(source.token().await.unwrap().expose(), "YWJj.ZGVm.Z2hp");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_captures_stdout_without_shell() {
        let source =
            CredentialSource::Command(vec![OsString::from("/bin/echo"), OsString::from(JWT)]);
        assert_eq!(source.token().await.unwrap().expose(), JWT);
    }

    #[tokio::test]
    async fn rejects_invalid_and_oversized_file_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        let source = CredentialSource::File(path.clone());
        for value in [
            "abc.def",
            "abc..def",
            "abc.de f.ghi",
            "abc=.def.ghi",
            "abc.def.ghi.extra",
        ] {
            tokio::fs::write(&path, value).await.unwrap();
            assert!(source.token().await.is_err(), "accepted invalid token");
        }
        tokio::fs::write(&path, "a".repeat(16 * 1024 + 1))
            .await
            .unwrap();
        assert!(source.token().await.is_err());
    }

    #[tokio::test]
    async fn accepts_maximum_length_token_with_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        let token = format!("e30.{}.e30", "A".repeat(MAX_TOKEN_BYTES - 8));
        assert_eq!(token.len(), MAX_TOKEN_BYTES);
        tokio::fs::write(&path, format!("{token}\n")).await.unwrap();
        let source = CredentialSource::File(path);
        assert_eq!(source.token().await.unwrap().expose(), token);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_timeout_is_bounded() {
        let source =
            CredentialSource::Command(vec![OsString::from("/bin/sleep"), OsString::from("2")]);
        let result = source
            .token_with_timeout(std::time::Duration::from_millis(20))
            .await;
        assert!(matches!(result, Err(CredentialError::Timeout)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_output_is_bounded() {
        let source = CredentialSource::Command(vec![OsString::from("/usr/bin/yes")]);
        assert!(matches!(
            source.token().await,
            Err(CredentialError::TooLarge)
        ));
    }

    #[test]
    fn debug_does_not_expose_secrets_or_arguments() {
        let token = SecretToken::parse(JWT.as_bytes()).unwrap();
        assert!(!format!("{token:?}").contains(JWT));
        let source = CredentialSource::Command(vec![
            OsString::from("/bin/echo"),
            OsString::from("sensitive-argument"),
        ]);
        let display = format!("{source:?}");
        assert!(!display.contains("sensitive-argument"));
        assert!(!display.contains("/bin/echo"));
    }
}
