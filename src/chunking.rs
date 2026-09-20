//! Versioned chunking contracts, independent of metadata and byte providers.
//!
//! Configuration belongs to each persisted file layout. A new default must
//! never silently reinterpret the chunks of an existing file.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;

use crate::{ErrorCode, FsError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkerConfig {
    pub algorithm: String,
    pub version: u32,
    pub parameters: BTreeMap<String, u64>,
}

/// Produces nonempty, ordered, borrowed chunks without copying the file data.
/// Concatenating them must reproduce the input exactly. An empty input yields
/// no chunks. Algorithms must be deterministic for their persisted config.
pub trait Chunker: Send + Sync {
    fn config(&self) -> ChunkerConfig;
    fn chunks<'a>(&self, bytes: &'a [u8]) -> Box<dyn Iterator<Item = &'a [u8]> + Send + 'a>;
}

#[derive(Debug, Clone, Copy)]
pub struct FixedSizeChunker {
    size: NonZeroUsize,
}

impl FixedSizeChunker {
    pub fn new(size: usize) -> Result<Self> {
        let size = NonZeroUsize::new(size).ok_or_else(|| {
            FsError::new(ErrorCode::Einval).with_message("chunk size must be positive")
        })?;
        Ok(Self { size })
    }

    pub fn chunk_size(&self) -> usize {
        self.size.get()
    }

    /// Locate the fixed chunk containing a file offset, including offsets
    /// beyond current EOF. This does not allocate or read bytes.
    pub fn locate(&self, offset: u64) -> (u64, usize) {
        let size = self.size.get() as u64;
        (offset / size, (offset % size) as usize)
    }
}

impl Chunker for FixedSizeChunker {
    fn config(&self) -> ChunkerConfig {
        ChunkerConfig {
            algorithm: "fixed-size".to_owned(),
            version: 1,
            parameters: BTreeMap::from([("chunk_size".to_owned(), self.size.get() as u64)]),
        }
    }

    fn chunks<'a>(&self, bytes: &'a [u8]) -> Box<dyn Iterator<Item = &'a [u8]> + Send + 'a> {
        Box::new(bytes.chunks(self.size.get()))
    }
}

/// Restore a supported algorithm, rejecting unknown versions/parameters rather
/// than falling back to a different data layout. Additional algorithms can
/// implement `Chunker` without changes to provider-specific stores.
pub fn from_config(config: &ChunkerConfig) -> Result<Box<dyn Chunker>> {
    if config.algorithm != "fixed-size" || config.version != 1 {
        return Err(
            FsError::new(ErrorCode::Enotsup).with_message("unsupported chunker algorithm/version")
        );
    }
    if config.parameters.len() != 1 {
        return Err(FsError::new(ErrorCode::Einval).with_message("invalid fixed-size parameters"));
    }
    let size = config
        .parameters
        .get("chunk_size")
        .and_then(|size| usize::try_from(*size).ok())
        .ok_or_else(|| FsError::new(ErrorCode::Einval).with_message("invalid chunk_size"))?;
    Ok(Box::new(FixedSizeChunker::new(size)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_chunks_reassemble_without_copying_or_empty_tail() {
        for size in [1, 2, 3, 4096, 65536] {
            let chunker = FixedSizeChunker::new(size).unwrap();
            for length in [0, 1, size - 1, size, size + 1, 2 * size, 2 * size + 7] {
                let bytes: Vec<_> = (0..length).map(|n| (n % 251) as u8).collect();
                let chunks: Vec<_> = chunker.chunks(&bytes).collect();
                assert_eq!(chunks.concat(), bytes);
                assert_eq!(chunks.len(), length.div_ceil(size));
                let mut offset = 0;
                for chunk in chunks {
                    assert!(!chunk.is_empty() && chunk.len() <= size);
                    assert_eq!(chunk.as_ptr(), bytes[offset..].as_ptr());
                    offset += chunk.len();
                }
                let config: ChunkerConfig =
                    serde_json::from_slice(&serde_json::to_vec(&chunker.config()).unwrap())
                        .unwrap();
                assert_eq!(
                    from_config(&config)
                        .unwrap()
                        .chunks(&bytes)
                        .collect::<Vec<_>>(),
                    chunker.chunks(&bytes).collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn rejects_invalid_or_unknown_layouts() {
        assert!(FixedSizeChunker::new(0).is_err());
        let mut config = FixedSizeChunker::new(4096).unwrap().config();
        config.version = 2;
        assert!(from_config(&config).is_err());
        config.version = 1;
        config.parameters.insert("ignored-option".into(), 1);
        assert!(from_config(&config).is_err());
        config.parameters.remove("ignored-option");
        config.parameters.insert("chunk_size".into(), 0);
        assert!(from_config(&config).is_err());
    }

    #[test]
    fn offsets_preserve_full_u64_range() {
        let chunker = FixedSizeChunker::new(4096).unwrap();
        for offset in [0, 1, 4095, 4096, 4097, u64::MAX] {
            let (index, within) = chunker.locate(offset);
            assert_eq!(index * 4096 + within as u64, offset);
        }
    }
}
