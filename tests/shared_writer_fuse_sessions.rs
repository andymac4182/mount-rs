//! Two independent FUSE protocol sessions sharing one authoritative chunked owner.
//! No kernel mount or remote network transport is implied by this test.
use mount_rs_chunked::{AtimePolicy, CachedBlockStore, ChunkedFs, ChunkedOptions};
use mount_rs_core::{FsDriver, storage::BlockStore};
use mount_rs_fuse::{
    RequestHeader,
    session::{FuseSession, FuseSessionOptions},
};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use std::sync::Arc;

struct Client(FuseSession);
impl Client {
    async fn new(driver: Arc<dyn FsDriver>) -> Self {
        let mut client = Self(FuseSession::with_options(
            driver,
            FuseSessionOptions::default().without_kernel_cache(),
        ));
        let init: Vec<u8> = [7u32, 41, 65536, u32::MAX, u32::MAX]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        client.request(26, 0, &init).await;
        client
    }
    async fn request(&mut self, opcode: u32, nodeid: u64, body: &[u8]) -> Vec<u8> {
        let mut frame = RequestHeader {
            len: (40 + body.len()) as u32,
            opcode,
            unique: 42,
            nodeid,
            uid: 0,
            gid: 0,
            pid: 0,
            total_extlen: 0,
        }
        .encode()
        .to_vec();
        frame.extend(body);
        let reply = self.0.handle(&frame).await.unwrap().unwrap();
        assert_eq!(
            i32::from_le_bytes(reply[4..8].try_into().unwrap()),
            0,
            "opcode {opcode}"
        );
        reply[16..].to_vec()
    }
    async fn open(&mut self, name: &[u8]) -> (u64, u64) {
        let entry = self.request(1, 1, name).await;
        let node = number(&entry, 0);
        let mut input = [0; 8];
        input[..4].copy_from_slice(&2u32.to_le_bytes());
        let opened = self.request(14, node, &input).await;
        (node, number(&opened, 0))
    }
    async fn write(&mut self, handle: (u64, u64), offset: u64, bytes: &[u8]) {
        let mut body = io_body(handle.1, offset, bytes.len());
        body.extend(bytes);
        let result = self.request(16, handle.0, &body).await;
        assert_eq!(
            u32::from_le_bytes(result[..4].try_into().unwrap()) as usize,
            bytes.len()
        );
    }
    async fn read(&mut self, handle: (u64, u64)) -> Vec<u8> {
        self.request(15, handle.0, &io_body(handle.1, 0, 32)).await
    }
    async fn close(&mut self, handle: (u64, u64)) {
        let mut body = vec![0; 24];
        body[..8].copy_from_slice(&handle.1.to_le_bytes());
        self.request(18, handle.0, &body).await;
    }
}
fn number(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
fn io_body(handle: u64, offset: u64, length: usize) -> Vec<u8> {
    let mut body = vec![0; 40];
    body[..8].copy_from_slice(&handle.to_le_bytes());
    body[8..16].copy_from_slice(&offset.to_le_bytes());
    body[16..20].copy_from_slice(&(length as u32).to_le_bytes());
    body
}
async fn exercise<B: BlockStore + 'static>(blocks: B) {
    let fs = Arc::new(
        ChunkedFs::open(
            MemoryMetadataStore::new(),
            CachedBlockStore::new(blocks, 4096, 128),
            ChunkedOptions::fixed("shared-wire-owner", 4)
                .unwrap()
                .with_atime_policy(AtimePolicy::NoAtime),
        )
        .await
        .unwrap(),
    );
    let initial = fs.open("/file", "w", 0o600).await.unwrap();
    initial.write(b"00000000", Some(0)).await.unwrap();
    initial.close().await.unwrap();
    let mut a = Client::new(fs.clone()).await;
    let mut b = Client::new(fs.clone()).await;
    let ah = a.open(b"file\0").await;
    let bh = b.open(b"file\0").await;
    // Independent sessions issue disjoint writes concurrently to the same inode.
    tokio::join!(a.write(ah, 0, b"AAAA"), b.write(bh, 4, b"BBBB"));
    assert_eq!(a.read(ah).await, b"AAAABBBB");
    assert_eq!(b.read(bh).await, b"AAAABBBB");
    let mut rename = 1u64.to_le_bytes().to_vec();
    rename.extend(b"file\0renamed\0");
    b.request(12, 1, &rename).await;
    assert_eq!(a.read(ah).await, b"AAAABBBB");
    b.request(10, 1, b"renamed\0").await;
    a.write(ah, 0, b"CCCC").await;
    assert_eq!(b.read(bh).await, b"CCCCBBBB");
    let replacement = fs.open("/file", "w", 0o600).await.unwrap();
    replacement.write(b"new", Some(0)).await.unwrap();
    replacement.close().await.unwrap();
    let replacement_handle = a.open(b"file\0").await;
    assert_ne!(replacement_handle.0, ah.0);
    assert_eq!(a.read(replacement_handle).await, b"new");
    assert_eq!(a.read(ah).await, b"CCCCBBBB");
    a.close(replacement_handle).await;
    a.close(ah).await;
    b.close(bh).await;
    fs.shutdown().await.unwrap();
}
#[tokio::test]
async fn shared_writer_sessions_with_memory_contents() {
    exercise(MemoryBlockStore::new()).await;
}
#[cfg(unix)]
#[tokio::test]
async fn shared_writer_sessions_with_durable_local_contents() {
    let directory = tempfile::tempdir().unwrap();
    exercise(mount_rs_host::LocalBlockStore::open(directory.path()).unwrap()).await;
}
