//! Production target qualification; separate from the historical saturation control.
use mount_rs_service::catalog::{
    CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
};
use serde_json::json;
use std::collections::BTreeMap;

pub(crate) fn target_catalog(clients: usize) -> CatalogSnapshot {
    let mut snapshot = CatalogSnapshot::empty();
    snapshot.issuer_policies.insert(
        "load-policy".into(),
        json!({"issuer":"https://load.example.com","audiences":["mount-rs"]}),
    );
    for client in 0..clients {
        let partition = format!("partition-{}", client / 2);
        let drive = format!("sandbox-{client}");
        snapshot
            .partitions
            .entry(partition.clone())
            .or_insert_with(|| PartitionDefinition {
                drives: BTreeMap::new(),
            })
            .drives
            .insert(
                drive.clone(),
                DriveDefinition {
                    driver: json!({"kind":"tidb-test","sandbox":client}),
                },
            );
        snapshot.grants.insert(
            drive.clone(),
            GrantDefinition {
                partition_id: partition,
                policy_id: "load-policy".into(),
                drives: BTreeMap::from([(drive, Permission::Write)]),
                claim_conditions: BTreeMap::from([("/sandbox_id".into(), client.to_string())]),
            },
        );
    }
    snapshot
}

#[derive(Clone, Copy)]
pub(crate) enum FileProfile {
    Mixed,
}
impl FileProfile {
    pub(crate) fn size(self, file: usize) -> usize {
        assert!(file < 1_000, "file outside bounded profile");
        match file {
            0..990 => 4_096,
            990..999 => 131_072,
            _ => 1_048_576,
        }
    }
}
pub(crate) struct GenerationLedger {
    profile: FileProfile,
    pub(crate) generations: Vec<u64>,
}
impl GenerationLedger {
    pub(crate) fn new(profile: FileProfile, files: usize) -> Self {
        Self {
            profile,
            generations: vec![0; (0..files).map(|file| profile.size(file) / 4096).sum()],
        }
    }
    pub(crate) fn index(&self, file: usize, block: usize) -> usize {
        assert!(
            block < self.profile.size(file) / 4_096,
            "block outside file"
        );
        let start = match file {
            0..990 => file,
            990..999 => 990 + (file - 990) * 32,
            _ => 1_278,
        };
        start + block
    }
    pub(crate) fn generation(&self, file: usize, block: usize) -> u64 {
        self.generations[self.index(file, block)]
    }
    pub(crate) fn commit(&mut self, file: usize, block: usize, generation: u64) {
        let index = self.index(file, block);
        self.generations[index] = generation;
    }
    pub(crate) fn bytes(&self) -> usize {
        self.generations.len() * std::mem::size_of::<u64>()
    }
}
pub(crate) fn oracle_block(
    partition: u64,
    drive: u64,
    file: u64,
    block: u64,
    generation: u64,
) -> [u8; 4096] {
    let mut data = [0; 4096];
    for (field, value) in [partition, drive, file, block, generation]
        .iter()
        .enumerate()
    {
        data[field * 8..(field + 1) * 8].copy_from_slice(&value.to_le_bytes());
    }
    let digest = ring::digest::digest(&ring::digest::SHA256, &data[..40]);
    let mut state = u64::from_le_bytes(digest.as_ref()[..8].try_into().unwrap());
    for chunk in data[40..].chunks_mut(8) {
        // SplitMix64 expands a tuple-specific seed; the distinct 40-byte tuple
        // header additionally guarantees block identity without relying on hash collisions.
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^= value >> 31;
        chunk.copy_from_slice(&value.to_le_bytes()[..chunk.len()]);
    }
    data
}
pub(crate) fn verify_block(
    data: &[u8],
    partition: u64,
    drive: u64,
    file: u64,
    block: u64,
    generation: u64,
) -> bool {
    data == oracle_block(partition, drive, file, block, generation)
}

pub(crate) struct SignedTokens {
    key: ring::signature::EcdsaKeyPair,
    pub(crate) jwk: mount_rs_service::auth::Jwk,
    issued_at: u64,
}
impl SignedTokens {
    pub(crate) fn new() -> Self {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let point = key.public_key().as_ref();
        let jwk = mount_rs_service::auth::Jwk::EcP256 {
            kid: "qualification".into(),
            x: URL_SAFE_NO_PAD.encode(&point[1..33]),
            y: URL_SAFE_NO_PAD.encode(&point[33..65]),
        };
        Self {
            key,
            jwk,
            issued_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }
    pub(crate) fn token(&self, client: usize, lifetime: u64) -> String {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"qualification"}"#);
        let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"iss":"https://load.example.com","aud":"mount-rs","sub":format!("client-{client}"),"sandbox_id":client.to_string(),"iat":self.issued_at,"exp":self.issued_at+lifetime})).unwrap());
        let input = format!("{header}.{claims}");
        let signature = self
            .key
            .sign(&ring::rand::SystemRandom::new(), input.as_bytes())
            .unwrap();
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
    }
}

pub(crate) async fn expect_authentication_denial(
    endpoint: &quinn::Endpoint,
    address: std::net::SocketAddr,
    partition: &str,
    token: &str,
) -> Result<(), String> {
    use mount_rs_remote_protocol::{Message, PROTOCOL_VERSION, read_frame, write_frame};
    let connection = endpoint
        .connect(address, "localhost")
        .map_err(|_| "negative probe TLS setup failed")?
        .await
        .map_err(|_| "negative probe TLS handshake failed")?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|_| "negative probe stream open failed")?;
    write_frame(
        &mut send,
        &Message::ClientHello {
            version: PROTOCOL_VERSION,
            partition_id: partition.into(),
            bearer: token.into(),
        },
    )
    .await
    .map_err(|_| "negative probe hello write failed")?;
    send.finish().map_err(|_| "negative probe FIN failed")?;
    let response = tokio::time::timeout(std::time::Duration::from_secs(10), read_frame(&mut recv))
        .await
        .map_err(|_| "negative probe hello timed out")?;
    if response.is_ok() {
        connection.close(0u32.into(), b"unexpected negative probe hello");
        return Err("cross-Partition hello was not rejected".into());
    }
    let closed = tokio::time::timeout(std::time::Duration::from_secs(1), connection.closed())
        .await
        .map_err(|_| "negative probe missing authentication close")?;
    match closed {
        quinn::ConnectionError::ApplicationClosed(close)
            if close.error_code == quinn::VarInt::from_u32(1)
                && close.reason.as_ref() == b"authentication failed" =>
        {
            Ok(())
        }
        _ => Err("negative probe failed without explicit authentication denial".into()),
    }
}
