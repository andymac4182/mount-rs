use super::*;
use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::storage::{
    BlockId, BlockStore, ConcurrentBackingId, ConcurrentModeState, LoadedMetadata, MetadataStore,
    Namespace, WriterLease,
};
use mount_rs_memory::MemoryBlockStore;

fn anonymous_credentials() -> RpcCredentials {
    RpcCredentials {
        flavor: AUTH_NONE,
        uid: None,
        gid: None,
        gids: Vec::new(),
    }
}

fn backing_id() -> ConcurrentBackingId {
    ConcurrentBackingId::from_bytes([0x71; 16]).expect("nonzero test backing")
}

// Every successful bound CAS advances revision, so its delta counts the
// actual ChunkedFs publications without relying on calls to an NFS mock.
#[derive(Clone)]
struct CasMetadata(Arc<Mutex<LoadedMetadata>>);

impl CasMetadata {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(LoadedMetadata {
            revision: 0,
            namespace: None,
        })))
    }

    fn revision(&self) -> u64 {
        self.0.lock().expect("test metadata lock").revision
    }
}

#[async_trait]
impl MetadataStore for CasMetadata {
    fn durable(&self) -> bool {
        false
    }

    async fn load(&self) -> FsResult<LoadedMetadata> {
        Ok(self.0.lock().expect("test metadata lock").clone())
    }

    async fn concurrent_mode_state(&self) -> FsResult<ConcurrentModeState> {
        Ok(ConcurrentModeState::Mrc2(backing_id()))
    }

    async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> FsResult<()> {
        if backing == backing_id() {
            Ok(())
        } else {
            Err(FsError::new(ErrorCode::Estale))
        }
    }

    async fn acquire_writer(&self, _: &str, _: Duration) -> FsResult<WriterLease> {
        Err(FsError::new(ErrorCode::Enotsup))
    }

    async fn renew_writer(&self, _: &WriterLease, _: Duration) -> FsResult<WriterLease> {
        Err(FsError::new(ErrorCode::Enotsup))
    }

    async fn release_writer(&self, _: &WriterLease) -> FsResult<()> {
        Err(FsError::new(ErrorCode::Enotsup))
    }

    async fn publish(&self, _: u64, _: &WriterLease, _: Namespace) -> FsResult<u64> {
        Err(FsError::new(ErrorCode::Enotsup))
    }

    async fn publish_bound_if_revision(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
        namespace: Namespace,
    ) -> FsResult<u64> {
        self.prepare_bound_concurrent_mode(backing).await?;
        namespace.validate()?;
        let mut state = self.0.lock().expect("test metadata lock");
        if state.revision != expected_revision {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        state.revision += 1;
        state.namespace = Some(namespace);
        Ok(state.revision)
    }

    async fn flush(&self) -> FsResult<()> {
        Ok(())
    }
}

struct SharedBlocks(MemoryBlockStore);

#[async_trait]
impl BlockStore for SharedBlocks {
    fn durable(&self) -> bool {
        false
    }

    async fn prepare_concurrent_backing(&self) -> FsResult<ConcurrentBackingId> {
        Ok(backing_id())
    }

    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> FsResult<()> {
        if expected == backing_id() {
            Ok(())
        } else {
            Err(FsError::new(ErrorCode::Estale))
        }
    }

    async fn put(&self, bytes: &[u8]) -> FsResult<BlockId> {
        self.0.put(bytes).await
    }

    async fn get(&self, id: &BlockId) -> FsResult<Vec<u8>> {
        self.0.get(id).await
    }

    async fn flush(&self) -> FsResult<()> {
        self.0.flush().await
    }

    async fn delete(&self, id: &BlockId) -> FsResult<()> {
        self.0.delete(id).await
    }
}

type CreatedFs = ChunkedFs<CasMetadata, SharedBlocks>;

async fn fixture() -> (Nfs3Session, CreatedFs, CasMetadata) {
    let metadata = CasMetadata::new();
    let filesystem = ChunkedFs::open(
        metadata.clone(),
        SharedBlocks(MemoryBlockStore::new()),
        ChunkedOptions::fixed("created-owner-test", 4096)
            .expect("test chunker")
            .with_concurrent_writes(true)
            .with_identity(501, 20, 0o027)
            .with_root_mode(0o777),
    )
    .await
    .expect("open concurrent filesystem");
    let session = Nfs3Session::new(
        filesystem.clone(),
        NfsSessionOptions {
            shared_concurrent_view: true,
            ..NfsSessionOptions::default()
        },
    );
    (session, filesystem, metadata)
}

async fn target(filesystem: &CreatedFs, path: &str) -> PathGuard {
    let stats = filesystem.lstat(path).await.expect("created inode stats");
    PathGuard {
        path: path.to_owned(),
        identity: PathIdentity::from_stats(&stats).expect("created inode identity"),
    }
}

#[tokio::test]
async fn matching_auth_sys_create_omits_one_ownership_cas() {
    let (session, filesystem, metadata) = fixture().await;
    let mount_call = crate::rpc::encode_call(
        1,
        MOUNT_PROGRAM,
        MOUNT_V3,
        MOUNTPROC3_MNT,
        None,
        None,
        &crate::xdr::encode_xdr(|writer| writer.string("/")),
    );
    let reply = session
        .handle_call(&mount_call, NfsRequestContext::default())
        .await
        .expect("MOUNT reply");
    let (_, mut body) = crate::rpc::decode_reply(&reply).expect("MOUNT body");
    let root = read_mount_res(&mut body).expect("MOUNT result").fh.unwrap();
    let auth = crate::rpc::auth_sys(501, 20, "matching-owner");
    for (index, mode, attributes, publications) in [
        (0, CREATE_EXCLUSIVE, None, 1),
        (
            1,
            CREATE_GUARDED,
            Some(Sattr3 {
                mode: Some(0o640),
                ..Sattr3::default()
            }),
            2,
        ),
    ] {
        let name = format!("same-owner-{index}");
        let arguments = crate::xdr::encode_xdr(|writer| {
            write_create_args(
                writer,
                &Create3args {
                    where_: DirOpArgs {
                        dir: root.clone(),
                        name: name.clone(),
                    },
                    mode,
                    attributes,
                    verf: (mode == CREATE_EXCLUSIVE).then(|| vec![index; 8]),
                },
            );
        });
        let call = crate::rpc::encode_call(
            u32::from(index) + 2,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_CREATE,
            Some(&auth),
            None,
            &arguments,
        );
        let before = metadata.revision();
        let reply = session
            .handle_call(&call, NfsRequestContext::default())
            .await
            .expect("CREATE reply");
        let (_, mut body) = crate::rpc::decode_reply(&reply).expect("CREATE body");
        assert_eq!(read_create_res(&mut body).unwrap().status, NFS3_OK);
        assert_eq!(
            metadata.revision() - before,
            publications,
            "CREATE must omit only its redundant ownership CAS"
        );
        let stats = filesystem.lstat(&format!("/{name}")).await.unwrap();
        assert_eq!(
            (stats.uid, stats.gid, stats.mode & 0o7777),
            (501, 20, 0o640)
        );
    }
    assert!(session.destroy().await);
    filesystem.shutdown().await.unwrap();
}

#[tokio::test]
async fn created_owner_corrects_only_differing_specified_credentials() {
    for (uid, gid) in [(502, 20), (501, 21), (502, 21)] {
        let (session, filesystem, metadata) = fixture().await;
        filesystem.write_file("/created", b"").await.unwrap();
        let target = target(&filesystem, "/created").await;
        let before = metadata.revision();
        session
            .claim_created_owner(
                &target,
                &RpcCredentials {
                    flavor: AUTH_SYS,
                    uid: Some(uid),
                    gid: Some(gid),
                    gids: Vec::new(),
                },
                None,
                false,
                0o666,
            )
            .await
            .unwrap();
        assert_eq!(metadata.revision() - before, 1);
        let stats = filesystem.lstat("/created").await.unwrap();
        assert_eq!((stats.uid, stats.gid), (uid, gid));
        assert_eq!(stats.mode & 0o7777, 0o640, "driver umask stays applied");
        session.destroy().await;
        filesystem.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn created_owner_preserves_unknown_credentials_and_explicit_client_setattr() {
    let (session, filesystem, metadata) = fixture().await;
    filesystem.write_file("/created", b"").await.unwrap();
    let target = target(&filesystem, "/created").await;
    for credentials in [
        anonymous_credentials(),
        RpcCredentials {
            flavor: AUTH_SYS,
            uid: Some(u32::MAX),
            gid: Some(u32::MAX),
            gids: Vec::new(),
        },
    ] {
        let before = metadata.revision();
        session
            .claim_created_owner(&target, &credentials, None, false, 0o666)
            .await
            .unwrap();
        assert_eq!(metadata.revision(), before);
    }
    let before = metadata.revision();
    let old_ctime = filesystem.lstat("/created").await.unwrap().ctime_ms;
    session
        .apply_created_sattr(
            &target,
            &Sattr3 {
                mode: Some(0o640),
                uid: Some(501),
                gid: Some(20),
                ..Sattr3::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(metadata.revision() - before, 1);
    let stats = filesystem.lstat("/created").await.unwrap();
    assert_eq!(
        (stats.uid, stats.gid, stats.mode & 0o7777),
        (501, 20, 0o640)
    );
    assert!(
        stats.ctime_ms > old_ctime,
        "explicit setattr still changes ctime"
    );
    session.destroy().await;
    filesystem.shutdown().await.unwrap();
}

#[tokio::test]
async fn created_owner_keeps_parent_setgid_group_and_directory_mode_inheritance() {
    let (session, filesystem, metadata) = fixture().await;
    filesystem
        .mkdir("/created", MkdirOptions::default())
        .await
        .unwrap();
    let target = target(&filesystem, "/created").await;
    let mut parent = filesystem.lstat("/").await.unwrap();
    parent.mode |= S_ISGID;
    parent.gid = 41;
    let before = metadata.revision();
    session
        .claim_created_owner(
            &target,
            &anonymous_credentials(),
            Some(&parent),
            true,
            0o777,
        )
        .await
        .unwrap();
    assert_eq!(metadata.revision() - before, 1);
    let stats = filesystem.lstat("/created").await.unwrap();
    assert_eq!(
        (stats.uid, stats.gid, stats.mode & 0o7777),
        (501, 41, 0o2750)
    );
    let before = metadata.revision();
    session
        .claim_created_owner(
            &target,
            &RpcCredentials {
                flavor: AUTH_SYS,
                uid: Some(501),
                gid: Some(20),
                gids: Vec::new(),
            },
            Some(&parent),
            true,
            0o777,
        )
        .await
        .unwrap();
    assert_eq!(
        metadata.revision(),
        before,
        "inherited owner and mode already match"
    );
    session.destroy().await;
    filesystem.shutdown().await.unwrap();
}

#[tokio::test]
async fn created_owner_keeps_setgid_executable_membership_rules() {
    for (uid, groups, expected_mode) in [
        (501, Vec::new(), 0o750),
        (501, vec![41], 0o2750),
        (0, Vec::new(), 0o2750),
    ] {
        let (session, filesystem, metadata) = fixture().await;
        filesystem.write_file("/created", b"").await.unwrap();
        filesystem.chmod("/created", 0o2750).await.unwrap();
        let target = target(&filesystem, "/created").await;
        let mut parent = filesystem.lstat("/").await.unwrap();
        parent.mode |= S_ISGID;
        parent.gid = 41;
        let before = metadata.revision();
        session
            .claim_created_owner(
                &target,
                &RpcCredentials {
                    flavor: AUTH_SYS,
                    uid: Some(uid),
                    gid: Some(20),
                    gids: groups,
                },
                Some(&parent),
                false,
                0o2750,
            )
            .await
            .unwrap();
        assert_eq!(metadata.revision() - before, 1);
        let stats = filesystem.lstat("/created").await.unwrap();
        assert_eq!(
            (stats.uid, stats.gid, stats.mode & 0o7777),
            (uid, 41, expected_mode)
        );
        session.destroy().await;
        filesystem.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn created_owner_still_rejects_stale_identity_when_credentials_match() {
    let (session, filesystem, metadata) = fixture().await;
    filesystem.write_file("/created", b"").await.unwrap();
    let stale_target = target(&filesystem, "/created").await;
    filesystem.unlink("/created").await.unwrap();
    filesystem.write_file("/created", b"").await.unwrap();
    let before = metadata.revision();
    let error = session
        .claim_created_owner(
            &stale_target,
            &RpcCredentials {
                flavor: AUTH_SYS,
                uid: Some(501),
                gid: Some(20),
                gids: Vec::new(),
            },
            None,
            false,
            0o666,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Estale);
    assert_eq!(metadata.revision(), before);
    session.destroy().await;
    filesystem.shutdown().await.unwrap();
}
