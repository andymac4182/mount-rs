//! Owner-side credential mapping for session routes. A client identity comes
//! only from configured credentials, never from forwarded identity headers.
use http::{HeaderMap, header::AUTHORIZATION};
use mount_rs_core::{ErrorCode, FsError, Result};
use mount_rs_http::DriveTokens;
use std::{
    collections::BTreeSet,
    fmt,
    hash::{BuildHasher, RandomState},
};
use subtle::ConstantTimeEq;

/// The credentials of one volume's session routes. Serve only through
/// authenticated encrypted transport; this type does not terminate TLS.
/// Configure high-entropy bearer tokens; minimum length is not an entropy test.
///
/// Built with [`new`](Self::new) it is a fixed snapshot. Built with
/// [`for_drive`](Self::for_drive) it reads the drive's own token set at
/// every request, so a rotation of that set reaches the session routes at
/// the same moment as the drive's file routes.
pub struct SessionCredentials {
    source: Source,
}
enum Source {
    Fixed(Vec<(String, String)>),
    /// Each token of the drive is a client of its own, named by `label`
    /// and a keyed hash of the token.
    Drive {
        label: String,
        tokens: DriveTokens,
        /// Random per process, so a principal says nothing about its token
        /// and the token itself is not kept as a session's owner.
        key: RandomState,
    },
}
impl Clone for SessionCredentials {
    /// A failover candidate builds a fresh session registry each time it
    /// takes a volume, from the same configured credentials. A clone of
    /// drive credentials reads the same token set.
    fn clone(&self) -> Self {
        Self {
            source: match &self.source {
                Source::Fixed(entries) => Source::Fixed(entries.clone()),
                Source::Drive { label, tokens, key } => Source::Drive {
                    label: label.clone(),
                    tokens: tokens.clone(),
                    key: key.clone(),
                },
            },
        }
    }
}
impl fmt::Debug for SessionCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = match &self.source {
            Source::Fixed(entries) => entries.len(),
            Source::Drive { tokens, .. } => tokens.len(),
        };
        f.debug_struct("SessionCredentials")
            .field("count", &count)
            .field("credentials", &"<redacted>")
            .finish()
    }
}
impl SessionCredentials {
    /// Whether `token` may be a session credential: 32 to 256 characters
    /// from `A-Z a-z 0-9 - . _ ~ + / =`.
    pub fn valid_token(token: &str) -> bool {
        (32..=256).contains(&token.len())
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b))
    }
    /// Credentials that are whatever `tokens` holds when a request arrives.
    ///
    /// Each token is a principal of its own (`label` and a keyed hash of
    /// the token), the same for as long as the token stays in the set. So a
    /// session belongs to the token that made it, as with fixed
    /// credentials: another token of the drive does not reach it, and once
    /// its token has left the set nothing does, and it is left to idle
    /// expiry. Whoever replaces the set must keep every token
    /// [`valid_token`](Self::valid_token); one that is not never
    /// authenticates here.
    pub fn for_drive(label: impl Into<String>, tokens: DriveTokens) -> Result<Self> {
        let label = label.into();
        // Leaves room for the hash within a principal's 256 bytes.
        if label.is_empty() || label.len() > 128 {
            return Err(FsError::new(ErrorCode::Einval).with_syscall("session-auth"));
        }
        Ok(Self {
            source: Source::Drive {
                label,
                tokens,
                key: RandomState::new(),
            },
        })
    }
    /// Each principal and token must be unique within this volume's snapshot.
    /// Validation errors never contain either credential or principal values.
    pub fn new(entries: Vec<(String, String)>) -> Result<Self> {
        let invalid = || FsError::new(ErrorCode::Einval).with_syscall("session-auth");
        if entries.is_empty() || entries.len() > 4096 {
            return Err(invalid());
        }
        let mut principals = BTreeSet::new();
        let mut tokens = BTreeSet::new();
        for (principal, token) in &entries {
            if principal.is_empty()
                || principal.len() > 256
                || !principals.insert(principal)
                || !tokens.insert(token)
                || !Self::valid_token(token)
            {
                return Err(invalid());
            }
        }
        Ok(Self {
            source: Source::Fixed(entries),
        })
    }
    /// Reject missing/duplicate/malformed authorization. Caller-added identity
    /// headers have no effect. No successful match short-circuits the scan.
    ///
    /// Returns the principal: the configured one of fixed credentials, or
    /// the one of the presented token of drive credentials.
    pub fn authenticate(&self, headers: &HeaderMap) -> Result<String> {
        let denied = || FsError::new(ErrorCode::Eacces).with_syscall("session-auth");
        let mut values = headers.get_all(AUTHORIZATION).iter();
        let value = values.next().ok_or_else(denied)?;
        if values.next().is_some() {
            return Err(denied());
        }
        let value = value.to_str().map_err(|_| denied())?;
        let (scheme, token) = value.split_once(' ').ok_or_else(denied)?;
        if !scheme.eq_ignore_ascii_case("Bearer") || !(32..=256).contains(&token.len()) {
            return Err(denied());
        }
        let entries = match &self.source {
            Source::Fixed(entries) => entries,
            Source::Drive { label, tokens, key } => {
                // Compares every token of the set in full. The hash is
                // taken only of a token that matched.
                return if tokens.matches(token) {
                    Ok(format!("{label}-{:016x}", key.hash_one(token)))
                } else {
                    Err(denied())
                };
            }
        };
        let mut principal = None;
        for (identity, expected) in entries {
            if bool::from(expected.as_bytes().ct_eq(token.as_bytes())) {
                principal = Some(identity.clone());
            }
        }
        principal.ok_or_else(denied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_bind_identity_and_ignore_spoofed_identity_headers() {
        let secret = "a".repeat(32);
        let credentials = SessionCredentials::new(vec![
            ("alice".into(), secret.clone()),
            ("bob".into(), "b".repeat(32)),
        ])
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, format!("bEaReR {secret}").parse().unwrap());
        headers.insert("x-mount-client", "bob".parse().unwrap());
        assert_eq!(credentials.authenticate(&headers).unwrap(), "alice");
        assert!(!format!("{credentials:?}").contains(&secret));
        headers.append(AUTHORIZATION, format!("Bearer {secret}").parse().unwrap());
        assert_eq!(
            credentials.authenticate(&headers).unwrap_err().code,
            ErrorCode::Eacces
        );
    }
    #[test]
    fn drive_credentials_follow_the_drive_tokens_as_they_are_replaced() {
        let (old, new) = ("o".repeat(32), "n".repeat(32));
        let tokens = DriveTokens::new(vec![old.clone()]).unwrap();
        let credentials = SessionCredentials::for_drive("client", tokens.clone()).unwrap();
        // A failover candidate clones them each time it takes the volume.
        let cloned = credentials.clone();
        let bearer = |token: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
            headers
        };
        let principal = |credentials: &SessionCredentials, token: &str| {
            credentials.authenticate(&bearer(token)).ok()
        };
        let of_old = principal(&credentials, &old).expect("the old token");
        assert!(of_old.starts_with("client-"), "{of_old}");
        assert!(!of_old.contains(&old));
        assert_eq!(principal(&credentials, &new), None);

        // The overlap: each token is a client of its own, so neither
        // reaches the other's sessions, and the old one is who it was.
        tokens.replace(vec![new.clone(), old.clone()]).unwrap();
        let of_new = principal(&credentials, &new).expect("the new token");
        assert_ne!(of_new, of_old);
        for credentials in [&credentials, &cloned] {
            assert_eq!(principal(credentials, &old).as_ref(), Some(&of_old));
            assert_eq!(principal(credentials, &new).as_ref(), Some(&of_new));
        }

        // The old token is dropped: refused from the next request on.
        tokens.replace(vec![new.clone()]).unwrap();
        for credentials in [&credentials, &cloned] {
            assert_eq!(principal(credentials, &old), None);
            assert_eq!(principal(credentials, &new).as_ref(), Some(&of_new));
        }

        // A token the session routes do not allow never authenticates,
        // even if the drive was given it.
        tokens.replace(vec!["short".into()]).unwrap();
        assert_eq!(principal(&credentials, "short"), None);
        assert!(!format!("{credentials:?}").contains("short"));
        assert!(SessionCredentials::for_drive("", tokens.clone()).is_err());
        assert!(SessionCredentials::for_drive("l".repeat(129), tokens).is_err());
        assert!(SessionCredentials::valid_token(&old));
        assert!(!SessionCredentials::valid_token("short"));
        assert!(!SessionCredentials::valid_token(&"sp ace".repeat(8)));
    }
    #[test]
    fn malformed_or_unknown_credentials_never_authenticate() {
        let secret = "a".repeat(32);
        let credentials = SessionCredentials::new(vec![("alice".into(), secret.clone())]).unwrap();
        assert!(credentials.authenticate(&HeaderMap::new()).is_err());
        for value in [
            format!("Basic {secret}"),
            format!("Bearer  {secret}"),
            format!("Bearer {secret},Bearer {secret}"),
            format!("Bearer {}", "b".repeat(32)),
            "Bearer short".into(),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, value.parse().unwrap());
            let error = credentials.authenticate(&headers).unwrap_err();
            assert_eq!(error.code, ErrorCode::Eacces);
            assert!(!format!("{error:?}").contains(&secret));
        }
        assert!(
            SessionCredentials::new(vec![
                ("alice".into(), secret.clone()),
                ("bob".into(), secret)
            ])
            .is_err()
        );
    }
}
