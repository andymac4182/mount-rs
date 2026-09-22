use mount_rs_core::Loopback;
use mount_rs_memfs::MemoryFs;

pub async fn parity_filesystem() -> Loopback {
    use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
    match std::env::args().nth(1).as_deref().unwrap_or("memory") {
        "memory" => Loopback::new(MemoryFs::empty()),
        "chunked-memory" => Loopback::new(
            ChunkedFs::open(
                mount_rs_memory::MemoryMetadataStore::new(),
                mount_rs_memory::MemoryBlockStore::new(),
                ChunkedOptions::fixed("parity", 7).unwrap(),
            )
            .await
            .unwrap(),
        ),
        "chunked-sqlite" => Loopback::new(
            ChunkedFs::open(
                mount_rs_sqlite::SqliteMetadataStore::in_memory().unwrap(),
                mount_rs_sqlite::SqliteBlockStore::in_memory().unwrap(),
                ChunkedOptions::fixed("parity", 7).unwrap(),
            )
            .await
            .unwrap(),
        ),
        other => panic!("unsupported parity backend: {other}"),
    }
}
