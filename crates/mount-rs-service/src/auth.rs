//! OIDC signature verification and exact catalog grant evaluation.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{
    ECDSA_P256_SHA256_FIXED, RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents, UnparsedPublicKey,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::time::Duration;
use url::Url;

use crate::catalog::{CatalogSnapshot, Permission};

const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_JWKS_BYTES: usize = 256 * 1024;
const MAX_TOKEN_LIFETIME_SECONDS: i64 = 12 * 60 * 60;
const CLOCK_SKEW_SECONDS: i64 = 60;

#[must_use]
pub fn is_safe_public_https_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some_and(|port| port != 443)
    {
        return false;
    }
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if matches!(host.as_str(), "localhost" | "metadata.google.internal")
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
    {
        return false;
    }
    let ip_host = host.trim_start_matches('[').trim_end_matches(']');
    ip_host.parse::<IpAddr>().map_or(true, is_public_ip)
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let octets = address.octets();
            !(address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_broadcast()
                || address.is_documentation()
                || address.is_multicast()
                || address.is_unspecified()
                || octets[0] == 0
                || octets[0] >= 240
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 198 && (18..=19).contains(&octets[1])))
        }
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(mapped));
            }
            !(address.is_loopback()
                || address.is_unspecified()
                || address.is_unique_local()
                || address.is_unicast_link_local()
                || address.is_multicast()
                || address.segments()[0] == 0x2001 && address.segments()[1] == 0x0db8)
        }
    }
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    jwks_uri: String,
}

pub fn validated_jwks_uri(issuer: &str, document: &str) -> Result<String, AuthError> {
    if document.len() > MAX_JWKS_BYTES {
        return Err(AuthError("OIDC discovery document too large"));
    }
    let document: DiscoveryDocument =
        serde_json::from_str(document).map_err(|_| AuthError("invalid OIDC discovery"))?;
    if document.issuer != issuer || !is_safe_public_https_url(&document.jwks_uri) {
        return Err(AuthError("untrusted OIDC discovery"));
    }
    Ok(document.jwks_uri)
}

async fn fetch_public_https(url: &str) -> Result<String, AuthError> {
    if !is_safe_public_https_url(url) {
        return Err(AuthError("untrusted OIDC metadata URL"));
    }
    let parsed = Url::parse(url).map_err(|_| AuthError("invalid OIDC metadata URL"))?;
    let host = parsed
        .host_str()
        .ok_or(AuthError("invalid OIDC metadata host"))?;
    let dns_host = host.trim_start_matches('[').trim_end_matches(']');
    let addresses: Vec<SocketAddr> = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((dns_host, parsed.port_or_known_default().unwrap_or(443))),
    )
    .await
    .map_err(|_| AuthError("OIDC metadata DNS timeout"))?
    .map_err(|_| AuthError("OIDC metadata DNS unavailable"))?
    .collect();
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(address.ip())) {
        return Err(AuthError("untrusted OIDC metadata address"));
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .resolve_to_addrs(dns_host, &addresses)
        .build()
        .map_err(|_| AuthError("OIDC metadata client unavailable"))?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| AuthError("OIDC metadata unavailable"))?;
    if !response.status().is_success() {
        return Err(AuthError("OIDC metadata unavailable"));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_JWKS_BYTES as u64)
    {
        return Err(AuthError("OIDC metadata too large"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AuthError("OIDC metadata unavailable"))?
    {
        if bytes.len() + chunk.len() > MAX_JWKS_BYTES {
            return Err(AuthError("OIDC metadata too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| AuthError("OIDC metadata is not UTF-8"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthError(&'static str);

impl std::fmt::Display for AuthError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str(self.0)
    }
}

impl std::error::Error for AuthError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kty", deny_unknown_fields)]
pub enum Jwk {
    #[serde(rename = "RSA")]
    Rsa { kid: String, n: String, e: String },
    #[serde(rename = "EC")]
    EcP256 { kid: String, x: String, y: String },
}

impl Jwk {
    fn kid(&self) -> &str {
        match self {
            Self::Rsa { kid, .. } | Self::EcP256 { kid, .. } => kid,
        }
    }

    fn algorithm(&self) -> &'static str {
        match self {
            Self::Rsa { .. } => "RS256",
            Self::EcP256 { .. } => "ES256",
        }
    }

    fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), AuthError> {
        match self {
            Self::Rsa { n, e, .. } => {
                let modulus = decode_b64(n)?;
                let exponent = decode_b64(e)?;
                let public_key = RsaPublicKeyComponents {
                    n: &modulus,
                    e: &exponent,
                };
                public_key
                    .verify(&RSA_PKCS1_2048_8192_SHA256, message, signature)
                    .map_err(|_| AuthError("invalid JWT signature"))
            }
            Self::EcP256 { x, y, .. } => {
                let x = decode_b64(x)?;
                let y = decode_b64(y)?;
                if x.len() != 32 || y.len() != 32 || signature.len() != 64 {
                    return Err(AuthError("invalid JWT signature"));
                }
                let mut key = Vec::with_capacity(65);
                key.push(4);
                key.extend_from_slice(&x);
                key.extend_from_slice(&y);
                UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, key)
                    .verify(message, signature)
                    .map_err(|_| AuthError("invalid JWT signature"))
            }
        }
    }
}

fn decode_b64(value: &str) -> Result<Vec<u8>, AuthError> {
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AuthError("malformed JWT encoding"))
}

#[derive(Debug, Clone)]
pub struct VerifiedPrincipal {
    pub issuer: String,
    pub subject: String,
    pub expires_at: i64,
    pub claims: Value,
}

#[derive(Debug, Clone)]
pub struct OidcVerifier {
    issuer: String,
    audiences: Vec<String>,
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct JoseHeader {
    alg: String,
    kid: String,
    #[serde(default)]
    crit: Vec<String>,
    #[serde(default)]
    b64: Option<bool>,
}

#[derive(Deserialize)]
struct JwksDocument {
    keys: Vec<RawJwk>,
}

#[derive(Deserialize)]
struct RawJwk {
    kid: Option<String>,
    kty: String,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    r#use: Option<String>,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
}

impl OidcVerifier {
    pub async fn from_discovery(issuer: &str, audiences: &[String]) -> Result<Self, AuthError> {
        if !is_safe_public_https_url(issuer) {
            return Err(AuthError("invalid OIDC issuer"));
        }
        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let discovery = fetch_public_https(&discovery_url).await?;
        let jwks_url = validated_jwks_uri(issuer, &discovery)?;
        let jwks = fetch_public_https(&jwks_url).await?;
        Self::from_jwks_json(issuer, audiences, &jwks)
    }

    pub fn from_jwks_json(
        issuer: &str,
        audiences: &[String],
        document: &str,
    ) -> Result<Self, AuthError> {
        if document.len() > MAX_JWKS_BYTES {
            return Err(AuthError("JWKS too large"));
        }
        let raw: JwksDocument =
            serde_json::from_str(document).map_err(|_| AuthError("invalid JWKS"))?;
        if raw.keys.len() > 100 {
            return Err(AuthError("too many JWKS keys"));
        }
        let keys = raw
            .keys
            .into_iter()
            .filter_map(|key| {
                if key.r#use.as_deref().is_some_and(|usage| usage != "sig") {
                    return None;
                }
                let kid = key.kid.filter(|value| !value.is_empty())?;
                match key.kty.as_str() {
                    "RSA" if key.alg.as_deref().is_none_or(|alg| alg == "RS256") => {
                        Some(Jwk::Rsa {
                            kid,
                            n: key.n?,
                            e: key.e?,
                        })
                    }
                    "EC" if key.crv.as_deref() == Some("P-256")
                        && key.alg.as_deref().is_none_or(|alg| alg == "ES256") =>
                    {
                        Some(Jwk::EcP256 {
                            kid,
                            x: key.x?,
                            y: key.y?,
                        })
                    }
                    _ => None,
                }
            })
            .collect();
        Self::new(issuer, audiences, keys)
    }

    pub fn new(issuer: &str, audiences: &[String], keys: Vec<Jwk>) -> Result<Self, AuthError> {
        if !is_safe_public_https_url(issuer)
            || audiences.is_empty()
            || audiences.iter().any(|aud| aud.is_empty() || aud == "*")
            || keys.is_empty()
        {
            return Err(AuthError("invalid OIDC policy"));
        }
        let unique: std::collections::BTreeSet<_> = keys.iter().map(Jwk::kid).collect();
        if keys.len() > 100
            || unique.len() != keys.len()
            || keys.iter().any(|key| key.kid().is_empty())
        {
            return Err(AuthError("invalid OIDC key set"));
        }
        Ok(Self {
            issuer: issuer.to_owned(),
            audiences: audiences.to_vec(),
            keys,
        })
    }

    #[must_use]
    pub fn keys(&self) -> &[Jwk] {
        &self.keys
    }

    pub fn verify(&self, bearer: &str, now: i64) -> Result<VerifiedPrincipal, AuthError> {
        if bearer.len() > MAX_TOKEN_BYTES {
            return Err(AuthError("JWT too large"));
        }
        let mut segments = bearer.split('.');
        let header = segments.next().ok_or(AuthError("malformed JWT"))?;
        let body = segments.next().ok_or(AuthError("malformed JWT"))?;
        let signature = segments.next().ok_or(AuthError("malformed JWT"))?;
        if segments.next().is_some() || header.is_empty() || body.is_empty() || signature.is_empty()
        {
            return Err(AuthError("malformed JWT"));
        }
        let header: JoseHeader = serde_json::from_slice(&decode_b64(header)?)
            .map_err(|_| AuthError("malformed JWT header"))?;
        if !header.crit.is_empty() || header.b64 == Some(false) {
            return Err(AuthError("unsupported JWT critical header"));
        }
        let key = self
            .keys
            .iter()
            .find(|key| key.kid() == header.kid && key.algorithm() == header.alg)
            .ok_or(AuthError("unknown JWT key or algorithm"))?;
        let signature = decode_b64(signature)?;
        let signing_input = bearer
            .rsplit_once('.')
            .map(|(input, _)| input)
            .ok_or(AuthError("malformed JWT"))?;
        key.verify(signing_input.as_bytes(), &signature)?;
        let claims: Value = serde_json::from_slice(&decode_b64(body)?)
            .map_err(|_| AuthError("malformed JWT claims"))?;
        let issuer = string_claim(&claims, "iss")?;
        let subject = string_claim(&claims, "sub")?;
        let exp = integer_claim(&claims, "exp")?;
        let iat = integer_claim(&claims, "iat")?;
        let nbf = claims.get("nbf").map_or(Ok(None), |value| {
            value
                .as_i64()
                .map(Some)
                .ok_or(AuthError("invalid JWT time"))
        })?;
        if issuer != self.issuer
            || subject.is_empty()
            || !audience_matches(claims.get("aud"), &self.audiences)
            || exp < now - CLOCK_SKEW_SECONDS
            || exp <= iat
            || iat > now + CLOCK_SKEW_SECONDS
            || nbf.is_some_and(|value| value > now + CLOCK_SKEW_SECONDS)
            || exp.saturating_sub(iat) > MAX_TOKEN_LIFETIME_SECONDS
        {
            return Err(AuthError("invalid JWT claims"));
        }
        Ok(VerifiedPrincipal {
            issuer: issuer.to_owned(),
            subject: subject.to_owned(),
            expires_at: exp,
            claims,
        })
    }
}

fn string_claim<'a>(claims: &'a Value, name: &str) -> Result<&'a str, AuthError> {
    claims
        .get(name)
        .and_then(Value::as_str)
        .ok_or(AuthError("invalid JWT claim"))
}

fn integer_claim(claims: &Value, name: &str) -> Result<i64, AuthError> {
    claims
        .get(name)
        .and_then(Value::as_i64)
        .ok_or(AuthError("invalid JWT time"))
}

fn audience_matches(value: Option<&Value>, configured: &[String]) -> bool {
    match value {
        Some(Value::String(single)) => configured.contains(single),
        Some(Value::Array(values)) => values.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|single| configured.iter().any(|aud| aud == single))
        }),
        _ => false,
    }
}

#[must_use]
pub fn authorize_drive(
    catalog: &CatalogSnapshot,
    policy_id: &str,
    claims: &Value,
    partition_id: &str,
    drive_id: &str,
) -> Option<Permission> {
    if !catalog
        .partitions
        .get(partition_id)?
        .drives
        .contains_key(drive_id)
    {
        return None;
    }
    catalog
        .grants
        .values()
        .filter(|grant| {
            grant_matches(
                &grant.partition_id,
                partition_id,
                &grant.policy_id,
                policy_id,
                grant.claim_conditions.iter().map(|(path, expected)| {
                    (
                        crate::request_metadata::claim_pointer(claims, path)
                            .and_then(Value::as_str),
                        expected.as_str(),
                    )
                }),
            )
        })
        .filter_map(|grant| grant.drives.get(drive_id).copied())
        .max_by_key(|permission| matches!(permission, Permission::Write))
}

// Inputs are claim strings resolved by the production JSON-pointer lookup above.
// Missing and non-string claims both resolve to None and cannot satisfy a grant.
fn grant_matches<'a>(
    grant_partition: &str,
    partition: &str,
    grant_policy: &str,
    policy: &str,
    conditions: impl Iterator<Item = (Option<&'a str>, &'a str)>,
) -> bool {
    if grant_partition != partition || grant_policy != policy {
        return false;
    }
    let mut any = false;
    for (actual, expected) in conditions {
        any = true;
        if actual != Some(expected) {
            return false;
        }
    }
    any
}

#[cfg(kani)]
mod proofs {
    use super::grant_matches;

    #[kani::proof]
    #[kani::unwind(5)]
    fn remote_grant_requires_exact_scope_and_all_claims() {
        // Two one-byte identities; zero, one, or two resolved conditions.
        let partition: bool = kani::any();
        let policy: bool = kani::any();
        let count: usize = kani::any();
        kani::assume(count <= 2);
        let present: [bool; 2] = kani::any();
        let equal: [bool; 2] = kani::any();
        let actual = [
            present[0].then_some(if equal[0] { "a" } else { "b" }),
            present[1].then_some(if equal[1] { "a" } else { "b" }),
        ];
        let conditions = [(actual[0], "a"), (actual[1], "a")];
        let accepted = grant_matches(
            "a",
            if partition { "a" } else { "b" },
            "a",
            if policy { "a" } else { "b" },
            conditions[..count].iter().copied(),
        );
        let expected = partition
            && policy
            && count != 0
            && present[0]
            && equal[0]
            && (count == 1 || (present[1] && equal[1]));
        assert_eq!(accepted, expected);
        kani::cover!(accepted && count == 2);
        kani::cover!(!accepted && !partition && policy);
        kani::cover!(!accepted && partition && !policy);
        kani::cover!(!accepted && count == 0);
        kani::cover!(!accepted && count == 2 && present[0] && equal[0] && !present[1]);
        kani::cover!(!accepted && count == 2 && present[0] && equal[0] && present[1] && !equal[1]);
    }
}

/// Production authenticator. Only catalog-pinned policies are contacted.
pub struct CatalogAuthenticator {
    catalog: std::sync::Arc<dyn crate::catalog::CatalogStore>,
    cache: tokio::sync::Mutex<std::collections::BTreeMap<String, CachedPolicy>>,
    source: std::sync::Arc<dyn OidcKeySource>,
}

struct CachedPolicy {
    configuration: Value,
    verifier: Option<OidcVerifier>,
    fetched: tokio::time::Instant,
    attempted: tokio::time::Instant,
}

#[async_trait::async_trait]
pub trait OidcKeySource: Send + Sync {
    async fn fetch(&self, issuer: &str, audiences: &[String]) -> Result<OidcVerifier, AuthError>;
}
struct DiscoveryKeySource;
#[async_trait::async_trait]
impl OidcKeySource for DiscoveryKeySource {
    async fn fetch(&self, issuer: &str, audiences: &[String]) -> Result<OidcVerifier, AuthError> {
        OidcVerifier::from_discovery(issuer, audiences).await
    }
}
impl CatalogAuthenticator {
    pub fn new(catalog: std::sync::Arc<dyn crate::catalog::CatalogStore>) -> Self {
        Self::with_key_source(catalog, std::sync::Arc::new(DiscoveryKeySource))
    }
    pub fn with_key_source(
        catalog: std::sync::Arc<dyn crate::catalog::CatalogStore>,
        source: std::sync::Arc<dyn OidcKeySource>,
    ) -> Self {
        Self {
            catalog,
            source,
            cache: tokio::sync::Mutex::new(std::collections::BTreeMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl crate::server::Authenticator for CatalogAuthenticator {
    async fn authenticate(
        &self,
        token: &str,
        partition_id: &str,
    ) -> Result<crate::dispatch::SessionIdentity, ()> {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(());
        }
        let payload = token.split('.').nth(1).ok_or(())?;
        let unverified: Value =
            serde_json::from_slice(&decode_b64(payload).map_err(|_| ())?).map_err(|_| ())?;
        let header: JoseHeader = serde_json::from_slice(
            &decode_b64(token.split('.').next().ok_or(())?).map_err(|_| ())?,
        )
        .map_err(|_| ())?;
        let claimed_issuer = unverified.get("iss").and_then(Value::as_str).ok_or(())?;
        let catalog = self.catalog.load_current().await.map_err(|_| ())?;
        let partition = catalog.partitions.get(partition_id).ok_or(())?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| ())?
            .as_secs() as i64;
        let mut cache = self.cache.lock().await;
        cache.retain(|id, _| catalog.issuer_policies.contains_key(id));
        let mut matched = None;
        for (id, configuration) in &catalog.issuer_policies {
            if !catalog
                .grants
                .values()
                .any(|grant| grant.partition_id == partition_id && grant.policy_id == *id)
            {
                continue;
            }
            let policy: crate::catalog::IssuerPolicyDefinition =
                serde_json::from_value(configuration.clone()).map_err(|_| ())?;
            if policy.issuer != claimed_issuer || !policy.algorithms.contains(&header.alg) {
                continue;
            }
            let fresh = cache.get(id).is_some_and(|entry| {
                entry.configuration == *configuration
                    && (if entry.verifier.is_some() {
                        entry.fetched.elapsed().as_secs() < 600
                    } else {
                        entry.attempted.elapsed().as_secs() < 60
                    })
            });
            if !fresh {
                let verifier = self
                    .source
                    .fetch(&policy.issuer, &policy.audiences)
                    .await
                    .ok();
                cache.insert(
                    id.clone(),
                    CachedPolicy {
                        configuration: configuration.clone(),
                        verifier,
                        fetched: tokio::time::Instant::now(),
                        attempted: tokio::time::Instant::now(),
                    },
                );
            }
            let entry = cache.get_mut(id).ok_or(())?;
            let Some(verifier) = &entry.verifier else {
                continue;
            };
            let mut principal = verifier.verify(token, now);
            // At most one discovery/JWKS refresh per minute, including unknown keys.
            if principal.is_err() && entry.attempted.elapsed().as_secs() >= 60 {
                entry.attempted = tokio::time::Instant::now();
                if let Ok(verifier) = self.source.fetch(&policy.issuer, &policy.audiences).await {
                    principal = verifier.verify(token, now);
                    entry.verifier = Some(verifier);
                    entry.fetched = tokio::time::Instant::now();
                }
            }
            let Ok(principal) = principal else {
                continue;
            };
            if principal.expires_at <= now
                || !partition.drives.keys().any(|drive| {
                    authorize_drive(&catalog, id, &principal.claims, partition_id, drive).is_some()
                })
            {
                continue;
            }
            if matched.is_some() {
                return Err(());
            }
            matched = Some(crate::dispatch::SessionIdentity {
                partition_id: partition_id.into(),
                policy_id: id.clone(),
                issuer: principal.issuer,
                subject: principal.subject,
                signing_algorithm: header.alg.clone(),
                claims: principal.claims,
                expires_at: principal.expires_at,
            });
        }
        matched.ok_or(())
    }
}
