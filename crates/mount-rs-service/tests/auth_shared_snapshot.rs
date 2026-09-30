use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use mount_rs_service::{
    auth::{AuthError, CatalogAuthenticator, Jwk, OidcKeySource, OidcVerifier},
    catalog::{
        CatalogError, CatalogSnapshot, CatalogStore, DriveDefinition, GrantDefinition,
        PartitionDefinition, Permission, SqliteCatalog,
    },
    server::Authenticator,
};
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
};
use rusqlite::{Connection, params};
use serde_json::json;

struct CountedCatalog {
    inner: SqliteCatalog,
    owned_loads: AtomicUsize,
    shared_loads: AtomicUsize,
}

#[async_trait]
impl CatalogStore for CountedCatalog {
    async fn load_current(&self) -> Result<CatalogSnapshot, CatalogError> {
        self.owned_loads.fetch_add(1, Ordering::Relaxed);
        self.inner.load_current().await
    }

    async fn load_shared_current(&self) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        self.shared_loads.fetch_add(1, Ordering::Relaxed);
        self.inner.load_shared_current().await
    }

    async fn compare_and_swap(
        &self,
        expected_revision: u64,
        next: CatalogSnapshot,
    ) -> Result<u64, CatalogError> {
        self.inner.compare_and_swap(expected_revision, next).await
    }
}

impl CountedCatalog {
    async fn open(path: &std::path::Path, snapshot: CatalogSnapshot) -> Arc<Self> {
        let inner = SqliteCatalog::open(path).await.unwrap();
        inner.compare_and_swap(0, snapshot).await.unwrap();
        Arc::new(Self {
            inner,
            owned_loads: AtomicUsize::new(0),
            shared_loads: AtomicUsize::new(0),
        })
    }

    fn assert_loads(&self, authentications: usize) {
        assert_eq!(
            self.owned_loads.load(Ordering::Relaxed),
            0,
            "authentication must not request a deep-cloned owned catalog"
        );
        assert_eq!(
            self.shared_loads.load(Ordering::Relaxed),
            authentications,
            "each authentication must still load a current shared snapshot"
        );
    }
}

struct Keys {
    key: Jwk,
    fetches: AtomicUsize,
}

#[async_trait]
impl OidcKeySource for Keys {
    async fn fetch(&self, issuer: &str, audiences: &[String]) -> Result<OidcVerifier, AuthError> {
        self.fetches.fetch_add(1, Ordering::Relaxed);
        OidcVerifier::new(issuer, audiences, vec![self.key.clone()])
    }
}

struct Signer {
    key: EcdsaKeyPair,
    source: Arc<Keys>,
}

impl Signer {
    fn new() -> Self {
        let random = SystemRandom::new();
        let pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &random).unwrap();
        let key =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &random)
                .unwrap();
        let point = key.public_key().as_ref();
        let source = Arc::new(Keys {
            key: Jwk::EcP256 {
                kid: "shared-catalog-test".into(),
                x: URL_SAFE_NO_PAD.encode(&point[1..33]),
                y: URL_SAFE_NO_PAD.encode(&point[33..65]),
            },
            fetches: AtomicUsize::new(0),
        });
        Self { key, source }
    }

    fn token(&self, expires_at: i64) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"shared-catalog-test"}"#);
        let body = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "iss":"https://issuer.example.com", "aud":"mount-rs", "sub":"workload",
                "iat":now()-300, "exp":expires_at,
            }))
            .unwrap(),
        );
        let input = format!("{header}.{body}");
        let signature = self
            .key
            .sign(&SystemRandom::new(), input.as_bytes())
            .unwrap();
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn authority(partitions: usize) -> CatalogSnapshot {
    let mut snapshot = CatalogSnapshot::empty();
    snapshot.issuer_policies.insert(
        "oidc".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
    );
    for index in 0..partitions {
        let partition_id = format!("partition-{index:04}");
        let mut drives = BTreeMap::new();
        for drive_id in ["left", "right"] {
            drives.insert(
                drive_id.into(),
                DriveDefinition {
                    driver: json!({"kind":"memory"}),
                },
            );
            snapshot.grants.insert(
                format!("grant-{index:04}-{drive_id}"),
                GrantDefinition {
                    partition_id: partition_id.clone(),
                    policy_id: "oidc".into(),
                    drives: BTreeMap::from([(drive_id.into(), Permission::Write)]),
                    claim_conditions: BTreeMap::from([("/sub".into(), "workload".into())]),
                },
            );
        }
        snapshot
            .partitions
            .insert(partition_id, PartitionDefinition { drives });
    }
    snapshot
}

fn replace_document_at_same_revision(path: &std::path::Path, mut snapshot: CatalogSnapshot) {
    snapshot.revision = 1;
    snapshot.validate().unwrap();
    let outsider = Connection::open(path).unwrap();
    assert_eq!(
        outsider
            .execute(
                "UPDATE service_catalog SET document=?1 WHERE singleton=1 AND revision=1",
                params![serde_json::to_vec(&snapshot).unwrap()],
            )
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn authentication_shares_current_ten_thousand_drive_catalog_without_owned_loads() {
    let directory = tempfile::tempdir().unwrap();
    let snapshot = authority(5_000);
    assert_eq!(snapshot.partitions.len(), 5_000);
    assert_eq!(
        snapshot
            .partitions
            .values()
            .map(|partition| partition.drives.len())
            .sum::<usize>(),
        10_000
    );
    assert_eq!(snapshot.grants.len(), 10_000);
    let catalog = CountedCatalog::open(&directory.path().join("catalog.sqlite"), snapshot).await;
    let signer = Signer::new();
    let auth = CatalogAuthenticator::with_key_source(catalog.clone(), signer.source.clone());
    let token = signer.token(now() + 600);
    for (count, partition) in [(1, "partition-0000"), (2, "partition-4999")] {
        let identity = auth.authenticate(&token, partition).await.unwrap();
        assert_eq!(identity.partition_id, partition);
        assert_eq!(identity.policy_id, "oidc");
        assert_eq!(identity.subject, "workload");
        catalog.assert_loads(count);
    }
    assert_eq!(signer.source.fetches.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn shared_authentication_observes_grant_revocation_at_unchanged_revision() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite");
    let catalog = CountedCatalog::open(&path, authority(1)).await;
    let signer = Signer::new();
    let auth = CatalogAuthenticator::with_key_source(catalog.clone(), signer.source.clone());
    let token = signer.token(now() + 600);
    assert!(auth.authenticate(&token, "partition-0000").await.is_ok());
    let mut revoked = authority(1);
    revoked.grants.clear();
    replace_document_at_same_revision(&path, revoked);
    assert!(auth.authenticate(&token, "partition-0000").await.is_err());
    catalog.assert_loads(2);
    assert_eq!(signer.source.fetches.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn shared_authentication_observes_policy_change_at_unchanged_revision() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite");
    let catalog = CountedCatalog::open(&path, authority(1)).await;
    let signer = Signer::new();
    let auth = CatalogAuthenticator::with_key_source(catalog.clone(), signer.source.clone());
    let token = signer.token(now() + 600);
    assert!(auth.authenticate(&token, "partition-0000").await.is_ok());
    let mut revoked = authority(1);
    revoked.issuer_policies.insert(
        "oidc".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["different-audience"]}),
    );
    replace_document_at_same_revision(&path, revoked);
    assert!(auth.authenticate(&token, "partition-0000").await.is_err());
    catalog.assert_loads(2);
    assert_eq!(signer.source.fetches.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn shared_authentication_rejects_currently_expired_token_within_verifier_skew() {
    let directory = tempfile::tempdir().unwrap();
    let catalog =
        CountedCatalog::open(&directory.path().join("catalog.sqlite"), authority(1)).await;
    let signer = Signer::new();
    let auth = CatalogAuthenticator::with_key_source(catalog.clone(), signer.source.clone());
    let expires_at = now() - 10;
    let token = signer.token(expires_at);
    let verifier = OidcVerifier::new(
        "https://issuer.example.com",
        &["mount-rs".into()],
        vec![signer.source.key.clone()],
    )
    .unwrap();
    assert_eq!(
        verifier.verify(&token, expires_at + 10).unwrap().expires_at,
        expires_at
    );
    assert!(auth.authenticate(&token, "partition-0000").await.is_err());
    catalog.assert_loads(1);
    assert_eq!(signer.source.fetches.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn shared_authentication_fails_closed_on_unreadable_current_authority_with_warm_keys() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite");
    let catalog = CountedCatalog::open(&path, authority(1)).await;
    let signer = Signer::new();
    let auth = CatalogAuthenticator::with_key_source(catalog.clone(), signer.source.clone());
    let token = signer.token(now() + 600);
    assert!(auth.authenticate(&token, "partition-0000").await.is_ok());
    let outsider = Connection::open(path).unwrap();
    assert_eq!(
        outsider
            .execute(
                "UPDATE service_catalog SET document=?1 WHERE singleton=1 AND revision=1",
                params![b"{".as_slice()],
            )
            .unwrap(),
        1
    );
    assert!(auth.authenticate(&token, "partition-0000").await.is_err());
    catalog.assert_loads(2);
    assert_eq!(signer.source.fetches.load(Ordering::Relaxed), 1);
}
