use mount_rs_core::{FsDriver, Loopback, MemoryFs};
use mount_rs_fuse::{RequestHeader, session::FuseSession};
use mount_rs_r2::open_object_store;
use mount_rs_sqlite::open_sqlite_memory;
use object_store::memory::InMemory;
use std::sync::Arc;

struct StatOnlyDriver(MemoryFs);
#[async_trait::async_trait]
impl FsDriver for StatOnlyDriver {
    fn capabilities(&self) -> mount_rs_core::Capabilities {
        self.0.capabilities()
    }
    async fn stat(&self, path: &str) -> mount_rs_core::Result<mount_rs_core::Stats> {
        self.0.stat(path).await
    }
    async fn readdir(&self, path: &str) -> mount_rs_core::Result<Vec<mount_rs_core::DirEntry>> {
        self.0.readdir(path).await
    }
    async fn open(
        &self,
        path: &str,
        flags: &str,
        mode: u32,
    ) -> mount_rs_core::Result<Arc<dyn mount_rs_core::FileHandle>> {
        self.0.open(path, flags, mode).await
    }
}

#[tokio::test]
async fn fuse_lookup_falls_back_to_stat_when_lstat_is_unimplemented() {
    let fs = MemoryFs::empty();
    fs.open("/file", "w", 0o644)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let mut session = FuseSession::new(Arc::new(StatOnlyDriver(fs)));
    let init: Vec<u8> = [7u32, 41, 65536, 0]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    call(&mut session, 26, 0, &init).await;
    let entry = call(&mut session, 1, 1, b"file\0").await;
    assert_eq!(entry.len(), 128);
    let node = u64::from_le_bytes(entry[..8].try_into().unwrap());
    let attr = call(&mut session, 3, node, &[0; 16]).await;
    assert_eq!(attr.len(), 104);
}

async fn call(session: &mut FuseSession, opcode: u32, nodeid: u64, body: &[u8]) -> Vec<u8> {
    let mut frame = RequestHeader {
        len: (40 + body.len()) as u32,
        opcode,
        unique: 1,
        nodeid,
        uid: 0,
        gid: 0,
        pid: 0,
        total_extlen: 0,
    }
    .encode()
    .to_vec();
    frame.extend(body);
    let reply = session.handle(&frame).await.unwrap().unwrap();
    assert_eq!(
        i32::from_le_bytes(reply[4..8].try_into().unwrap()),
        0,
        "opcode {opcode}: {reply:?}"
    );
    reply[16..].to_vec()
}

async fn scenario(driver: Arc<dyn FsDriver>) -> Vec<u8> {
    let mut session = FuseSession::new(driver.clone());
    let init: Vec<u8> = [7u32, 41, 65536, 0]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    call(&mut session, 26, 0, &init).await;
    let mut create = vec![0; 16];
    create[..4].copy_from_slice(&0x42u32.to_le_bytes());
    create[4..8].copy_from_slice(&0o640u32.to_le_bytes());
    create.extend(b"file\0");
    let entry = call(&mut session, 35, 1, &create).await;
    let node = u64::from_le_bytes(entry[..8].try_into().unwrap());
    let handle = u64::from_le_bytes(entry[128..136].try_into().unwrap());
    let mut io = vec![0; 40];
    io[..8].copy_from_slice(&handle.to_le_bytes());
    io[16..20].copy_from_slice(&5u32.to_le_bytes());
    let mut write = io.clone();
    write.extend([0, 255, 1, 128, 2]);
    call(&mut session, 16, node, &write).await;
    let mut rename = 1u64.to_le_bytes().to_vec();
    rename.extend(b"file\0renamed\0");
    call(&mut session, 12, 1, &rename).await;
    let result = call(&mut session, 15, node, &io).await;
    assert_eq!(result, [0, 255, 1, 128, 2]);
    let mut sync = vec![0; 16];
    sync[..8].copy_from_slice(&handle.to_le_bytes());
    call(&mut session, 20, node, &sync).await;
    let mut release = vec![0; 24];
    release[..8].copy_from_slice(&handle.to_le_bytes());
    call(&mut session, 18, node, &release).await;
    session.destroy().await;
    let fs = Loopback::from_arc(driver);
    assert_eq!(fs.read_file("/renamed").await.unwrap(), result);
    assert_eq!(fs.stat("/renamed").await.unwrap().mode & 0o7777, 0o640);
    result
}

#[tokio::test]
async fn fuse_io_runs_over_memory_sqlite_and_object_store() {
    let memory = scenario(Arc::new(MemoryFs::empty())).await;
    assert_eq!(
        scenario(Arc::new(open_sqlite_memory().await.unwrap())).await,
        memory
    );
    let store = Arc::new(InMemory::new());
    assert_eq!(
        scenario(Arc::new(
            open_object_store(store.clone(), "fuse/state")
                .await
                .unwrap()
        ))
        .await,
        memory
    );
    let reopened = Loopback::new(open_object_store(store, "fuse/state").await.unwrap());
    assert_eq!(reopened.read_file("/renamed").await.unwrap(), memory);
}

#[tokio::test]
async fn fuse_sqlite_operations_survive_database_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fuse.db");
    let expected = scenario(Arc::new(mount_rs_sqlite::open_sqlite(&path).await.unwrap())).await;
    let reopened = Loopback::new(mount_rs_sqlite::open_sqlite(&path).await.unwrap());
    assert_eq!(reopened.read_file("/renamed").await.unwrap(), expected);
    assert_eq!(
        reopened.stat("/renamed").await.unwrap().mode & 0o7777,
        0o640
    );
}

#[tokio::test]
#[ignore = "requires the real PGlite socket server; run through scripts/test-pglite.sh"]
async fn fuse_pglite_operations_survive_connection_reopen() {
    let url = std::env::var("PGLITE_DATABASE_URL").expect("PGLITE_DATABASE_URL is required");
    let key = format!("fuse-test-{}", std::process::id());
    let expected = scenario(Arc::new(
        mount_rs_pglite::connect_pglite_with_key(&url, &key)
            .await
            .unwrap(),
    ))
    .await;
    let reopened = Loopback::new(
        mount_rs_pglite::connect_pglite_with_key(&url, &key)
            .await
            .unwrap(),
    );
    assert_eq!(reopened.read_file("/renamed").await.unwrap(), expected);
}
