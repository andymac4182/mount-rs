//! Immutable-byte cache. The backing store remains the sole durability authority.
mod distributed;
mod local;
mod peer;
mod store;
pub use distributed::*;
pub use local::*;
use mount_rs_core::{
    Result,
    error::{ErrorCode, FsError},
    storage::{BlockId, ConcurrentBackingId},
};
pub use peer::*;
use sha2::{Digest, Sha256};
pub use store::*;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopeIdentity {
    pub cluster: String,
    pub partition: String,
    pub drive: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheScope {
    pub identity: ScopeIdentity,
    pub backing: ConcurrentBackingId,
}
impl CacheScope {
    pub fn digest(&self) -> String {
        digest(&[
            self.identity.cluster.as_bytes(),
            self.identity.partition.as_bytes(),
            self.identity.drive.as_bytes(),
            &self.backing.as_bytes(),
        ])
    }
}
pub(crate) fn digest(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_be_bytes());
        h.update(p);
    }
    format!("{:x}", h.finalize())
}
pub(crate) fn error() -> FsError {
    FsError::new(ErrorCode::Eio).with_syscall("blob cache")
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegrityPolicy {
    Opaque,
    Sha256Prefixed,
    Sha256Colon,
    ObjectStoreSha256OrOpaque,
    PgliteMd5,
}
impl IntegrityPolicy {
    pub fn verify(self, id: &BlockId, bytes: &[u8]) -> Result<()> {
        let expected = match self {
            Self::Opaque => return Ok(()),
            Self::Sha256Prefixed => id.0.strip_prefix('b'),
            Self::Sha256Colon => id.0.strip_prefix("sha256:"),
            Self::ObjectStoreSha256OrOpaque => {
                let hex = id.0.strip_prefix('b').ok_or_else(error)?;
                if hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Ok(());
                }
                Some(hex)
            }
            Self::PgliteMd5 => {
                use md5::Md5;
                let mut hasher = Md5::new();
                const HEX: &[u8; 16] = b"0123456789abcdef";
                // PgLite hashes lowercase ASCII hex; stream it without a temporary string.
                for chunk in bytes.chunks(32) {
                    let mut encoded = [0u8; 64];
                    for (i, b) in chunk.iter().enumerate() {
                        encoded[2 * i] = HEX[(b >> 4) as usize];
                        encoded[2 * i + 1] = HEX[(b & 15) as usize];
                    }
                    hasher.update(&encoded[..chunk.len() * 2]);
                }
                return verify_hex(id.0.as_bytes(), &hasher.finalize());
            }
        }
        .ok_or_else(error)?;
        verify_hex(expected.as_bytes(), &Sha256::digest(bytes))
    }
}
fn verify_hex(expected: &[u8], digest: &[u8]) -> Result<()> {
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    if expected.len() != digest.len() * 2 {
        return Err(error());
    }
    for (pair, byte) in expected.chunks_exact(2).zip(digest) {
        if nibble(pair[0])
            .zip(nibble(pair[1]))
            .map(|(hi, lo)| hi * 16 + lo)
            != Some(*byte)
        {
            return Err(error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn integrity_is_provider_specific() {
        let bytes = b"hello";
        let sha = format!("{:x}", Sha256::digest(bytes));
        assert!(
            IntegrityPolicy::Sha256Prefixed
                .verify(&BlockId(format!("b{sha}")), bytes)
                .is_ok()
        );
        assert!(
            IntegrityPolicy::Sha256Colon
                .verify(&BlockId(format!("sha256:{sha}")), bytes)
                .is_ok()
        );
        assert!(
            IntegrityPolicy::Sha256Prefixed
                .verify(&BlockId(format!("b{sha}")), b"wrong")
                .is_err()
        );
        assert!(
            IntegrityPolicy::Opaque
                .verify(&BlockId("bwrong".into()), bytes)
                .is_ok()
        );
        assert!(
            IntegrityPolicy::ObjectStoreSha256OrOpaque
                .verify(&BlockId("b0123456789abcdef0123456789abcdef".into()), bytes)
                .is_ok()
        );
        use md5::Md5;
        let md5 = format!("{:x}", Md5::digest(b"68656c6c6f"));
        assert!(
            IntegrityPolicy::PgliteMd5
                .verify(&BlockId(md5), bytes)
                .is_ok()
        );
    }
    #[test]
    fn integrity_rejects_malformed_legacy_ids_and_accepts_valid_fdb_hex() {
        assert!(
            IntegrityPolicy::ObjectStoreSha256OrOpaque
                .verify(&BlockId("invalid".into()), b"bytes")
                .is_err()
        );
        let id = BlockId(format!("sha256:{:X}", Sha256::digest(b"bytes")));
        assert!(IntegrityPolicy::Sha256Colon.verify(&id, b"bytes").is_ok());
    }
}
