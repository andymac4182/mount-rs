//! Owner-side credential mapping for session routes. A client identity comes
//! only from configured credentials, never from forwarded identity headers.
use http::{HeaderMap, header::AUTHORIZATION};
use mount_rs_core::{ErrorCode, FsError, Result};
use std::{collections::BTreeSet, fmt};
use subtle::ConstantTimeEq;

/// Static credential snapshot for one volume. Serve only through authenticated
/// encrypted transport; this type does not terminate TLS or rotate credentials.
/// Configure high-entropy bearer tokens; minimum length is not an entropy test.
pub struct SessionCredentials {
    entries: Vec<(String, String)>,
}
impl fmt::Debug for SessionCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionCredentials")
            .field("count", &self.entries.len())
            .field("credentials", &"<redacted>")
            .finish()
    }
}
impl SessionCredentials {
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
                || !(32..=256).contains(&token.len())
                || !token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b))
            {
                return Err(invalid());
            }
        }
        Ok(Self { entries })
    }
    /// Reject missing/duplicate/malformed authorization. Caller-added identity
    /// headers have no effect. No successful match short-circuits the scan.
    pub fn authenticate<'a>(&'a self, headers: &HeaderMap) -> Result<&'a str> {
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
        let mut principal = None;
        for (identity, expected) in &self.entries {
            if bool::from(expected.as_bytes().ct_eq(token.as_bytes())) {
                principal = Some(identity.as_str());
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
