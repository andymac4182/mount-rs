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
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"test-key"}"#);
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
    let mut tampered = token.into_bytes();
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
