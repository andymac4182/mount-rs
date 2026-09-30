//! Public CLI JSON, owned certificates, and signed workload tokens.
use super::contracts::*;
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ring::{
    digest,
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
};
use serde_json::{Value, json};
use std::{
    fs,
    net::{SocketAddr, UdpSocket},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const ISSUER: &str = "https://issuer.example.com";
pub const AUDIENCE: &str = "mount-rs";
pub const CLUSTER: &str = "ten-process-private-fixture";

#[derive(Clone, Copy)]
pub struct CacheSettings<'a> {
    pub mode: &'a str,
    pub ram_bytes: usize,
    pub disk_bytes: usize,
    pub peers: &'a [usize],
    pub directory: &'a Path,
}

pub struct Fixture {
    pub root: PathBuf,
    pub peer_addresses: Vec<SocketAddr>,
    pub roots: rustls::RootCertStore,
    pub pins: Vec<String>,
    pub tokens: Vec<PathBuf>,
    pub bad_signature: PathBuf,
    pub document: Value,
    reservations: Vec<UdpSocket>,
}

pub fn sha256(bytes: &[u8]) -> String {
    digest::digest(&digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn pem(label: &str, der: &[u8]) -> String {
    let encoded = STANDARD.encode(der);
    let mut value = format!("-----BEGIN {label}-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        value.push_str(std::str::from_utf8(chunk).expect("base64 ASCII"));
        value.push('\n');
    }
    value.push_str(&format!("-----END {label}-----\n"));
    value
}
pub fn write(path: &Path, bytes: impl AsRef<[u8]>) -> Result<()> {
    fs::write(path, bytes).map_err(|e| e.to_string())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())
}
impl Fixture {
    pub fn credential_inventory(root: &Path) -> Vec<PathBuf> {
        let mut paths = (0..NODES)
            .map(|n| root.join(format!("token-{n}.jwt")))
            .collect::<Vec<_>>();
        paths.push(root.join("invalid-signature.jwt"));
        paths.extend((0..NODES).map(|n| root.join(format!("key-{n}.pem"))));
        paths
    }
    pub fn new(root: &Path) -> Result<Self> {
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|e| e.to_string())?;
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .map_err(|e| e.to_string())?;
        let bad_pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|e| e.to_string())?;
        let bad =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, bad_pkcs8.as_ref(), &rng)
                .map_err(|e| e.to_string())?;
        let point = key.public_key().as_ref();
        write(&root.join("fixture.jwks.json"), json!({"keys":[{
            "kty":"EC","kid":"fixture","crv":"P-256","alg":"ES256","use":"sig",
            "x":URL_SAFE_NO_PAD.encode(&point[1..33]), "y":URL_SAFE_NO_PAD.encode(&point[33..65])
        }]}).to_string())?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs();
        let sign = |key: &EcdsaKeyPair, n: usize| -> Result<String> {
            let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"fixture"}"#);
            let body = URL_SAFE_NO_PAD.encode(
                json!({"iss":ISSUER,"aud":AUDIENCE,
                "sub":format!("sandbox-{n}"),"repository_id":format!("repo-{n}"),
                "iat":now,"exp":now+3600})
                .to_string(),
            );
            let input = format!("{header}.{body}");
            let signature = key
                .sign(&rng, input.as_bytes())
                .map_err(|e| e.to_string())?;
            Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature)))
        };
        let mut tokens = Vec::new();
        for n in 0..NODES {
            let path = root.join(format!("token-{n}.jwt"));
            write(&path, sign(&key, n)?)?;
            tokens.push(path);
        }
        let bad_signature = root.join("invalid-signature.jwt");
        write(&bad_signature, sign(&bad, 0)?)?;
        let mut params =
            rcgen::CertificateParams::new(vec!["cache-ca".into()]).map_err(|e| e.to_string())?;
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = rcgen::CertifiedIssuer::self_signed(
            params,
            rcgen::KeyPair::generate().map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        write(&root.join("ca.pem"), pem("CERTIFICATE", ca.der().as_ref()))?;
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.der().clone()).map_err(|e| e.to_string())?;
        let mut pins = Vec::new();
        for n in 0..NODES {
            let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
            let mut params = rcgen::CertificateParams::new(vec!["localhost".into()])
                .map_err(|e| e.to_string())?;
            params.extended_key_usages = vec![
                rcgen::ExtendedKeyUsagePurpose::ServerAuth,
                rcgen::ExtendedKeyUsagePurpose::ClientAuth,
            ];
            let cert = params.signed_by(&key, &ca).map_err(|e| e.to_string())?;
            write(
                &root.join(format!("cert-{n}.pem")),
                pem("CERTIFICATE", cert.der().as_ref()),
            )?;
            write(
                &root.join(format!("key-{n}.pem")),
                pem("PRIVATE KEY", &key.serialize_der()),
            )?;
            pins.push(sha256(cert.der().as_ref()));
        }
        let mut reservations = Vec::new();
        let mut peer_addresses = Vec::new();
        for _ in 0..NODES {
            let socket = UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
            peer_addresses.push(socket.local_addr().map_err(|e| e.to_string())?);
            reservations.push(socket);
        }
        let mut partitions = serde_json::Map::new();
        let mut grants = serde_json::Map::new();
        for p in 0..PARTITIONS {
            let mut drives = serde_json::Map::new();
            for n in p * 2..p * 2 + 2 {
                drives.insert(drive(n), json!({"driver":{"kind":"splitstore","storage":{
                    "metadata":{"kind":"sqlite","path":root.join(format!("metadata-{n}.sqlite"))},
                    "blocks":{"kind":"sqlite","path":root.join(format!("blocks-{n}.sqlite"))},
                    "chunk_size_bytes":BLOCK_BYTES,"concurrent_writes":true,
                    "inode_updates":true,"compact_inode_updates":true
                }}}));
                grants.insert(format!("workload-{n}"), json!({"partition_id":partition(n),"policy_id":"oidc",
                    "drives":{(drive(n)):"write"},"claim_conditions":{"/repository_id":format!("repo-{n}")}}));
            }
            partitions.insert(format!("partition-{p}"), json!({"drives":drives}));
        }
        let document = json!({"revision":0,"partitions":partitions,
            "issuer_policies":{"oidc":{"issuer":ISSUER,"audiences":[AUDIENCE],"algorithms":["ES256"]}},
            "grants":grants});
        Ok(Self {
            root: root.into(),
            peer_addresses,
            roots,
            pins,
            tokens,
            bad_signature,
            document,
            reservations,
        })
    }
    pub fn release_reservations(&mut self) {
        self.reservations.clear();
    }
    pub fn apply_config(&self, revision: u64) -> Result<PathBuf> {
        let document = self.root.join(format!("catalog-{revision}.json"));
        write(&document, self.document.to_string())?;
        let config = self.root.join(format!("apply-{revision}.json"));
        write(
            &config,
            json!({"version":1,"catalog":self.root.join("service.sqlite"),
            "document":document,"expected_revision":revision})
            .to_string(),
        )?;
        Ok(config)
    }
    pub fn service_config(
        &self,
        n: usize,
        generation: u64,
        settings: CacheSettings<'_>,
    ) -> Result<PathBuf> {
        let CacheSettings {
            mode,
            ram_bytes: ram,
            disk_bytes: disk,
            peers,
            directory: cache_path,
        } = settings;
        let peers = peers.iter().map(|other| json!({"id":format!("node-{other}"),
            "address":self.peer_addresses[*other],"server_name":"localhost",
            "certificate_sha256":self.pins[*other],
            "partitions":(0..PARTITIONS).map(|p| format!("partition-{p}")).collect::<Vec<_>>()
        })).collect::<Vec<_>>();
        let path = self.root.join(format!("service-{n}-{generation}.json"));
        write(&path, json!({"version":1,"catalog":self.root.join("service.sqlite"),
            "listen":"127.0.0.1:0","certificate":self.root.join(format!("cert-{n}.pem")),
            "private_key":self.root.join(format!("key-{n}.pem")),
            "local_oidc_fixture":{"issuer":ISSUER,"audiences":[AUDIENCE],"jwks":self.root.join("fixture.jwks.json")},
            "cache":{"cluster":CLUSTER,"node_id":format!("node-{n}"),"disk_path":cache_path,
                "ram_bytes":ram,"disk_bytes":disk,"max_entries":4096,
                "peer_listen":self.peer_addresses[n],"ca_certificate":self.root.join("ca.pem"),
                "certificate":self.root.join(format!("cert-{n}.pem")),"private_key":self.root.join(format!("key-{n}.pem")),
                "discovery":mode,"peers":peers,"max_blob_bytes":64*1024,
                "peer_query_limit":3,"deadline_ms":500,"maintenance_capacity":64,
                "placement_concurrency":4,"hedge_delay_ms":25,"max_inflight":128,
                "peer_transfer_bytes":8*1024*1024}
        }).to_string())?;
        Ok(path)
    }
}
