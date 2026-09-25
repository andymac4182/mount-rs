use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use mount_rs_service::auth::{
    Jwk, OidcVerifier, authorize_drive, is_safe_public_https_url, validated_jwks_uri,
};
use mount_rs_service::catalog::{
    CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
};
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
};
use serde_json::json;

fn signed_token(claims: serde_json::Value) -> (String, Jwk) {
    signed_token_with_header(claims, json!({"alg":"ES256","kid":"test-key"}))
}

fn signed_token_with_header(claims: serde_json::Value, header: serde_json::Value) -> (String, Jwk) {
    let random = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &random).unwrap();
    let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &random)
        .unwrap();
    let point = key.public_key().as_ref();
    let jwk = Jwk::EcP256 {
        kid: "test-key".into(),
        x: URL_SAFE_NO_PAD.encode(&point[1..33]),
        y: URL_SAFE_NO_PAD.encode(&point[33..65]),
    };
    let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let signing_input = format!("{header}.{body}");
    let signature = key.sign(&random, signing_input.as_bytes()).unwrap();
    (
        format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        ),
        jwk,
    )
}

#[test]
fn oidc_verifier_accepts_signed_claims_and_rejects_wrong_audience() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (token, key) = signed_token(json!({
        "iss": "https://issuer.example.com", "aud": "mount-rs",
        "sub": "workload-1", "repository_id": "repo-1", "iat": now,
        "exp": now + 300
    }));
    let verifier = OidcVerifier::new(
        "https://issuer.example.com",
        &["mount-rs".into()],
        vec![key],
    )
    .unwrap();
    let identity = verifier.verify(&token, now).unwrap();
    assert_eq!(identity.subject, "workload-1");
    assert_eq!(identity.claims["repository_id"], "repo-1");
    let wrong_audience = OidcVerifier::new(
        "https://issuer.example.com",
        &["some-other-service".into()],
        verifier.keys().to_vec(),
    )
    .unwrap();
    assert!(wrong_audience.verify(&token, now).is_err());
    assert!(verifier.verify(&token, now + 400).is_err());
    let mut tampered = token.clone().into_bytes();
    let last = tampered.len() - 2;
    tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
    assert!(
        verifier
            .verify(std::str::from_utf8(&tampered).unwrap(), now)
            .is_err()
    );
    let (wrong_issuer, key) = signed_token(json!({
        "iss": "https://attacker.example.com", "aud": "mount-rs",
        "sub": "workload-1", "repository_id": "repo-1", "iat": now,
        "exp": now + 300
    }));
    let verifier = OidcVerifier::new(
        "https://issuer.example.com",
        &["mount-rs".into()],
        vec![key],
    )
    .unwrap();
    assert!(verifier.verify(&wrong_issuer, now).is_err());
}

#[test]
fn grants_are_partition_and_drive_specific() {
    let mut catalog = CatalogSnapshot::empty();
    for partition in ["red", "blue"] {
        catalog.partitions.insert(
            partition.into(),
            PartitionDefinition {
                drives: BTreeMap::from([(
                    "data".into(),
                    DriveDefinition {
                        driver: json!({"kind":"memory"}),
                    },
                )]),
            },
        );
    }
    catalog.issuer_policies.insert(
        "oidc".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
    );
    catalog.grants.insert(
        "red-reader".into(),
        GrantDefinition {
            partition_id: "red".into(),
            policy_id: "oidc".into(),
            drives: BTreeMap::from([("data".into(), Permission::Read)]),
            claim_conditions: BTreeMap::from([("/repository_id".into(), "repo-1".into())]),
        },
    );
    let claims = json!({"repository_id":"repo-1"});
    assert_eq!(
        authorize_drive(&catalog, "oidc", &claims, "red", "data"),
        Some(Permission::Read)
    );
    assert_eq!(
        authorize_drive(&catalog, "oidc", &claims, "blue", "data"),
        None
    );
    assert_eq!(
        authorize_drive(
            &catalog,
            "oidc",
            &json!({"repository_id":"repo-2"}),
            "red",
            "data"
        ),
        None
    );
}

#[test]
fn oidc_metadata_urls_cannot_target_private_networks() {
    assert!(is_safe_public_https_url("https://issuer.example.com/keys"));
    for url in [
        "http://issuer.example.com/keys",
        "https://127.0.0.1/keys",
        "https://10.0.0.1/keys",
        "https://[::1]/keys",
        "https://metadata.google.internal/keys",
        "https://issuer.example.com@127.0.0.1/keys",
        "https://issuer.example.com/keys#fragment",
    ] {
        assert!(!is_safe_public_https_url(url), "accepted {url}");
    }
}

#[test]
fn standard_jwks_accepts_only_supported_signing_keys() {
    let (_, key) = signed_token(json!({}));
    let Jwk::EcP256 { x, y, .. } = key else {
        panic!("expected EC key")
    };
    let jwks = json!({"keys": [
        {"kty":"EC", "kid":"valid", "crv":"P-256", "x":x, "y":y,
         "alg":"ES256", "use":"sig"},
        {"kty":"oct", "kid":"secret", "k":"c2VjcmV0"},
        {"kty":"EC", "kid":"wrong-curve", "crv":"P-384", "x":x, "y":y}
    ]});
    let verifier = OidcVerifier::from_jwks_json(
        "https://issuer.example.com",
        &["mount-rs".into()],
        &jwks.to_string(),
    )
    .unwrap();
    assert_eq!(verifier.keys().len(), 1);
}

#[test]
fn discovery_must_match_pinned_issuer_and_public_jwks_url() {
    let issuer = "https://issuer.example.com/tenant";
    assert_eq!(
        validated_jwks_uri(issuer, r#"{"issuer":"https://issuer.example.com/tenant","jwks_uri":"https://keys.example.com/jwks"}"#).unwrap(),
        "https://keys.example.com/jwks"
    );
    assert!(
        validated_jwks_uri(
            issuer,
            r#"{"issuer":"https://other.example.com","jwks_uri":"https://keys.example.com/jwks"}"#
        )
        .is_err()
    );
    assert!(
        validated_jwks_uri(
            issuer,
            r#"{"issuer":"https://issuer.example.com/tenant","jwks_uri":"https://127.0.0.1/jwks"}"#
        )
        .is_err()
    );
}

struct TestKeySource {
    key: Jwk,
}
#[async_trait::async_trait]
impl mount_rs_service::auth::OidcKeySource for TestKeySource {
    async fn fetch(
        &self,
        issuer: &str,
        audiences: &[String],
    ) -> Result<OidcVerifier, mount_rs_service::auth::AuthError> {
        OidcVerifier::new(issuer, audiences, vec![self.key.clone()])
    }
}
#[tokio::test]
async fn catalog_authentication_verifies_signature_and_partition_grants() {
    use mount_rs_service::{
        auth::CatalogAuthenticator, catalog::SqliteCatalog, server::Authenticator,
    };
    use std::sync::Arc;
    let directory = tempfile::tempdir().unwrap();
    let catalog = Arc::new(
        SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap(),
    );
    let mut snapshot = CatalogSnapshot::empty();
    for partition in ["red", "blue"] {
        snapshot.partitions.insert(
            partition.into(),
            PartitionDefinition {
                drives: BTreeMap::from([(
                    "data".into(),
                    DriveDefinition {
                        driver: json!({"kind":"memory"}),
                    },
                )]),
            },
        );
    }
    snapshot.issuer_policies.insert(
        "oidc".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
    );
    snapshot.grants.insert(
        "writer".into(),
        GrantDefinition {
            partition_id: "red".into(),
            policy_id: "oidc".into(),
            drives: BTreeMap::from([("data".into(), Permission::Write)]),
            claim_conditions: BTreeMap::from([("/repository_id".into(), "repo-1".into())]),
        },
    );
    catalog.compare_and_swap(0, snapshot).await.unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (token, key) = signed_token(
        json!({"iss":"https://issuer.example.com","aud":"mount-rs","sub":"workload","repository_id":"repo-1","iat":now,"exp":now+300}),
    );
    let auth =
        CatalogAuthenticator::with_key_source(catalog.clone(), Arc::new(TestKeySource { key }));
    assert_eq!(
        auth.authenticate(&token, "red").await.unwrap().partition_id,
        "red"
    );
    assert!(auth.authenticate(&token, "blue").await.is_err());
    let mut overlapping = catalog.load_current().await.unwrap();
    overlapping.issuer_policies.insert(
        "duplicate".into(),
        overlapping.issuer_policies["oidc"].clone(),
    );
    let mut grant = overlapping.grants["writer"].clone();
    grant.policy_id = "duplicate".into();
    overlapping.grants.insert("duplicate".into(), grant);
    catalog.compare_and_swap(1, overlapping).await.unwrap();
    assert!(auth.authenticate(&token, "red").await.is_err());
    let mut tampered = token.clone().into_bytes();
    let index = tampered.len() - 2;
    tampered[index] = if tampered[index] == b'A' { b'B' } else { b'A' };
    assert!(
        auth.authenticate(std::str::from_utf8(&tampered).unwrap(), "red")
            .await
            .is_err()
    );
    let mut snapshot = catalog.load_current().await.unwrap();
    snapshot.grants.clear();
    catalog.compare_and_swap(2, snapshot).await.unwrap();
    // A previously cached signing key must never preserve a revoked grant.
    assert!(auth.authenticate(&token, "red").await.is_err());
}

#[test]
fn rs256_public_fixture_verifies_without_private_key() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/rs256-public.json")).unwrap();
    let verifier = OidcVerifier::new(
        "https://issuer.example.com",
        &["mount-rs".into()],
        vec![Jwk::Rsa {
            kid: fixture["kid"].as_str().unwrap().into(),
            n: fixture["n"].as_str().unwrap().into(),
            e: fixture["e"].as_str().unwrap().into(),
        }],
    )
    .unwrap();
    assert_eq!(
        verifier
            .verify(fixture["token"].as_str().unwrap(), 1_700_000_001)
            .unwrap()
            .subject,
        "test-workload"
    );
    assert!(
        verifier
            .verify(fixture["token"].as_str().unwrap(), 1_700_001_000)
            .is_err()
    );
}

struct RotatingKeys {
    key: std::sync::Mutex<Jwk>,
    fetches: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl mount_rs_service::auth::OidcKeySource for RotatingKeys {
    async fn fetch(
        &self,
        issuer: &str,
        audiences: &[String],
    ) -> Result<OidcVerifier, mount_rs_service::auth::AuthError> {
        self.fetches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        OidcVerifier::new(issuer, audiences, vec![self.key.lock().unwrap().clone()])
    }
}
#[tokio::test(start_paused = true)]
async fn unknown_signing_key_refresh_is_throttled_then_rotation_is_accepted() {
    use mount_rs_service::{
        auth::CatalogAuthenticator, catalog::SqliteCatalog, server::Authenticator,
    };
    use std::sync::{Arc, atomic::Ordering};
    let directory = tempfile::tempdir().unwrap();
    let catalog = Arc::new(
        SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap(),
    );
    let mut snapshot = CatalogSnapshot::empty();
    snapshot.partitions.insert(
        "red".into(),
        PartitionDefinition {
            drives: BTreeMap::from([(
                "data".into(),
                DriveDefinition {
                    driver: json!({"kind":"memory"}),
                },
            )]),
        },
    );
    snapshot.issuer_policies.insert(
        "oidc".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
    );
    snapshot.grants.insert(
        "grant".into(),
        GrantDefinition {
            partition_id: "red".into(),
            policy_id: "oidc".into(),
            drives: BTreeMap::from([("data".into(), Permission::Read)]),
            claim_conditions: BTreeMap::from([("/sub".into(), "workload".into())]),
        },
    );
    catalog.compare_and_swap(0, snapshot).await.unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = json!({"iss":"https://issuer.example.com","aud":"mount-rs","sub":"workload","iat":now,"exp":now+300});
    let (old_token, old_key) = signed_token(claims.clone());
    let (new_token, new_key) = signed_token(claims);
    let source = Arc::new(RotatingKeys {
        key: std::sync::Mutex::new(old_key),
        fetches: std::sync::atomic::AtomicUsize::new(0),
    });
    let auth = CatalogAuthenticator::with_key_source(catalog, source.clone());
    assert!(auth.authenticate(&old_token, "red").await.is_ok());
    *source.key.lock().unwrap() = new_key;
    for _ in 0..3 {
        assert!(auth.authenticate(&new_token, "red").await.is_err());
    }
    assert_eq!(source.fetches.load(Ordering::SeqCst), 1);
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    assert!(auth.authenticate(&new_token, "red").await.is_ok());
    assert_eq!(source.fetches.load(Ordering::SeqCst), 2);
    assert!(auth.authenticate(&old_token, "red").await.is_err());
}

#[test]
fn signed_jwt_rejects_invalid_time_identity_and_critical_headers() {
    let now = 1_700_000_000_i64;
    let baseline = json!({"iss":"https://issuer.example.com","aud":"mount-rs","sub":"workload","iat":now,"exp":now+300});
    let cases = [
        ("future iat", "iat", json!(now + 61)),
        ("future nbf", "nbf", json!(now + 61)),
        ("expired", "exp", json!(now - 61)),
        ("excess lifetime", "exp", json!(now + 43201)),
        ("empty subject", "sub", json!("")),
        ("numeric subject", "sub", json!(12)),
        ("wrong audience array", "aud", json!(["other"])),
        ("invalid expiry", "exp", json!("tomorrow")),
    ];
    for (name, field, value) in cases {
        let mut claims = baseline.clone();
        claims[field] = value;
        let (token, key) = signed_token(claims);
        let verifier = OidcVerifier::new(
            "https://issuer.example.com",
            &["mount-rs".into()],
            vec![key],
        )
        .unwrap();
        assert!(verifier.verify(&token, now).is_err(), "accepted {name}");
    }
    for header in [
        json!({"alg":"none","kid":"test-key"}),
        json!({"alg":"HS256","kid":"test-key"}),
        json!({"alg":"ES256","kid":"test-key","crit":["custom"],"custom":true}),
        json!({"alg":"ES256","kid":"test-key","b64":false}),
    ] {
        let (token, key) = signed_token_with_header(baseline.clone(), header.clone());
        let verifier = OidcVerifier::new(
            "https://issuer.example.com",
            &["mount-rs".into()],
            vec![key],
        )
        .unwrap();
        assert!(
            verifier.verify(&token, now).is_err(),
            "accepted header {header}"
        );
    }
    let mut claims = baseline;
    claims["aud"] = json!(["other", "mount-rs"]);
    let (token, key) = signed_token(claims);
    assert!(
        OidcVerifier::new(
            "https://issuer.example.com",
            &["mount-rs".into()],
            vec![key]
        )
        .unwrap()
        .verify(&token, now)
        .is_ok()
    );
}

#[test]
fn jwt_malformed_inputs_and_duplicate_key_ids_fail_closed() {
    let (_, key) = signed_token(json!({}));
    assert!(
        OidcVerifier::new(
            "https://issuer.example.com",
            &["mount-rs".into()],
            vec![key.clone(), key.clone()]
        )
        .is_err()
    );
    let verifier = OidcVerifier::new(
        "https://issuer.example.com",
        &["mount-rs".into()],
        vec![key],
    )
    .unwrap();
    for token in ["", "a", "a.b", "a.b.c.d", "!.!.!", "e30.e30.AA"] {
        assert!(verifier.verify(token, 1_700_000_000).is_err());
    }
    assert!(
        verifier
            .verify(&"a".repeat(16 * 1024 + 1), 1_700_000_000)
            .is_err()
    );
}

#[test]
fn grant_conditions_are_conjunctive_typed_and_write_requires_a_matching_grant() {
    let mut catalog = CatalogSnapshot::empty();
    catalog.partitions.insert(
        "p".into(),
        PartitionDefinition {
            drives: BTreeMap::from([("d".into(), DriveDefinition { driver: json!({}) })]),
        },
    );
    let reader = GrantDefinition {
        partition_id: "p".into(),
        policy_id: "issuer".into(),
        drives: BTreeMap::from([("d".into(), Permission::Read)]),
        claim_conditions: BTreeMap::from([("/sub".into(), "s".into())]),
    };
    let mut writer = reader.clone();
    writer.drives.insert("d".into(), Permission::Write);
    writer
        .claim_conditions
        .insert("/nested/role".into(), "writer".into());
    catalog.grants.insert("read".into(), reader);
    catalog.grants.insert("write".into(), writer);
    for role in [
        json!(null),
        json!(false),
        json!(1),
        json!(["writer"]),
        json!("reader"),
    ] {
        assert_eq!(
            authorize_drive(
                &catalog,
                "issuer",
                &json!({"sub":"s","nested":{"role":role}}),
                "p",
                "d"
            ),
            Some(Permission::Read)
        );
    }
    let claims = json!({"sub":"s","nested":{"role":"writer"}});
    assert_eq!(
        authorize_drive(&catalog, "issuer", &claims, "p", "d"),
        Some(Permission::Write)
    );
    assert_eq!(authorize_drive(&catalog, "other", &claims, "p", "d"), None);
    assert_eq!(
        authorize_drive(&catalog, "issuer", &claims, "p", "missing"),
        None
    );
    assert_eq!(
        authorize_drive(
            &catalog,
            "issuer",
            &json!({"nested":{"role":"writer"}}),
            "p",
            "d"
        ),
        None
    );
    for grant in catalog.grants.values_mut() {
        grant.claim_conditions.clear();
    }
    assert_eq!(authorize_drive(&catalog, "issuer", &claims, "p", "d"), None);
}
