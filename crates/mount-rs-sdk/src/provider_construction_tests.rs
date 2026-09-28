//! SDK composition controls using actual constructors at owned wire boundaries.
//! PostgreSQL supplies startup and one DDL response; MySQL supplies an initial
//! greeting error or no greeting. Neither fixture qualifies live SQL or storage.

use super::*;
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use mount_rs_core::{ErrorCode, FsError};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const BOUND: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum Backend {
    Pglite,
    Tidb,
}

#[derive(Clone, Copy)]
enum Role {
    Metadata,
    Blocks,
}

struct Peer {
    config: StoreConfig,
    reached: Option<oneshot::Receiver<()>>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<bool>>,
}

impl Peer {
    async fn start(backend: Backend, role: Role, reject: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (reached_tx, reached) = oneshot::channel();
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            drop(listener);
            match backend {
                Backend::Pglite => {
                    let length = stream.read_u32().await.unwrap() as usize;
                    assert!((8..=4096).contains(&length));
                    let mut startup = vec![0; length - 4];
                    stream.read_exact(&mut startup).await.unwrap();
                    assert_eq!(&startup[..4], &196608_u32.to_be_bytes());
                    pg_frame(&mut stream, b'R', &0_u32.to_be_bytes()).await;
                    pg_frame(&mut stream, b'S', b"server_version\x0016.0\0").await;
                    pg_frame(&mut stream, b'S', b"client_encoding\0UTF8\0").await;
                    pg_frame(&mut stream, b'K', &[0, 0, 0, 1, 0, 0, 0, 2]).await;
                    pg_frame(&mut stream, b'Z', b"I").await;
                    let (tag, query) = pg_read(&mut stream).await;
                    assert_eq!(tag, b'Q');
                    let table = match role {
                        Role::Metadata => "mount_rs_metadata",
                        Role::Blocks => "mount_rs_blocks",
                    };
                    assert!(
                        query.starts_with(format!("CREATE TABLE IF NOT EXISTS {table}").as_bytes())
                    );
                    reached_tx.send(()).unwrap();
                    if !reject {
                        // Explicit fixture stop closes the peer. It is not
                        // reported as an acknowledged client cleanup.
                        let _ = stopped.await;
                        return false;
                    }
                    pg_frame(
                        &mut stream,
                        b'E',
                        b"SERROR\0CXX000\0Mowned SDK DDL rejection\0\0",
                    )
                    .await;
                    pg_frame(&mut stream, b'Z', b"I").await;
                    let frame = tokio::select! {
                        frame = pg_read(&mut stream) => frame,
                        _ = &mut stopped => return false,
                    };
                    assert_eq!(frame, (b'X', Vec::new()));
                }
                Backend::Tidb => {
                    if reject {
                        let mut payload = vec![0xff, 0x15, 0x04, b'#'];
                        payload.extend_from_slice(b"28000owned SDK greeting rejection");
                        let length = payload.len();
                        stream
                            .write_all(&[
                                length as u8,
                                (length >> 8) as u8,
                                (length >> 16) as u8,
                                0,
                            ])
                            .await
                            .unwrap();
                        stream.write_all(&payload).await.unwrap();
                    }
                    reached_tx.send(()).unwrap();
                }
            }
            let mut byte = [0];
            tokio::select! {
                read = stream.read(&mut byte) => {
                    assert_eq!(read.unwrap(), 0, "no MySQL handshake or SQL was supplied");
                    true
                }
                _ = &mut stopped => false,
            }
        });
        let config = match backend {
            Backend::Pglite => StoreConfig::Pglite {
                connection: format!("postgresql://postgres@{address}/postgres?sslmode=disable"),
                volume_key: "sdk-construction".into(),
                durable: false,
            },
            Backend::Tidb => StoreConfig::Tidb {
                connection: format!("mysql://unused@{address}/unused"),
                volume_key: "sdk-construction".into(),
                durable: false,
            },
        };
        Self {
            config,
            reached: Some(reached),
            stop: Some(stop),
            task: Some(task),
        }
    }

    async fn reached(&mut self) {
        tokio::time::timeout(BOUND, self.reached.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }

    async fn finish(mut self, client_cleanup: bool) {
        let stop_delivered = if !client_cleanup {
            Some(self.stop.take().unwrap().send(()).is_ok())
        } else {
            None
        };
        let mut task = self.task.take().unwrap();
        let outcome = match tokio::time::timeout(BOUND, &mut task).await {
            Ok(joined) => joined.unwrap(),
            Err(_) => {
                task.abort();
                let _ = task.await;
                panic!("owned SDK wire fixture did not finish");
            }
        };
        if client_cleanup {
            assert!(outcome, "fixture stop does not acknowledge client cleanup");
        } else if stop_delivered == Some(false) {
            assert!(
                outcome,
                "undelivered stop requires actual peer EOF and join"
            );
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn pg_frame(stream: &mut TcpStream, tag: u8, body: &[u8]) {
    stream.write_u8(tag).await.unwrap();
    stream.write_u32((body.len() + 4) as u32).await.unwrap();
    stream.write_all(body).await.unwrap();
}

async fn pg_read(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let tag = stream.read_u8().await.unwrap();
    let length = stream.read_u32().await.unwrap() as usize;
    assert!((4..=1024 * 1024).contains(&length));
    let mut body = vec![0; length - 4];
    stream.read_exact(&mut body).await.unwrap();
    (tag, body)
}

fn pair(role: Role, provider: StoreConfig) -> (StoreConfig, StoreConfig) {
    match role {
        Role::Metadata => (provider, StoreConfig::Memory),
        Role::Blocks => (StoreConfig::Memory, provider),
    }
}

async fn rejected(backend: Backend, role: Role) {
    let mut peer = Peer::start(backend, role, true).await;
    let (metadata, blocks) = pair(role, peer.config.clone());
    let journal = crate::ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let result = tokio::time::timeout(
        BOUND,
        open_storage_in_context_with_observer(&metadata, &blocks, None, None, Some(&journal)),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    peer.reached().await;
    assert_eq!(
        journal.snapshot().retained_resources,
        1,
        "one canonical provider group"
    );
    attempt.fail();
    tokio::time::timeout(BOUND, journal.close())
        .await
        .unwrap()
        .unwrap();
    assert!(journal.snapshot().cleanup_complete);
    assert_eq!(journal.snapshot().retained_resources, 0);
    peer.finish(true).await;
}

async fn cancelled(backend: Backend, role: Role) {
    let mut peer = Peer::start(backend, role, false).await;
    let (metadata, blocks) = pair(role, peer.config.clone());
    let journal = crate::ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let task = tokio::spawn({
        let journal = journal.clone();
        async move {
            open_storage_in_context_with_observer(&metadata, &blocks, None, None, Some(&journal))
                .await
        }
    });
    peer.reached().await;
    assert_eq!(journal.snapshot().retained_resources, 1);
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    // Even an incorrectly acknowledged cancellation cannot make the provider
    // group claim cleanup: its abandoned registration guard is uncertain.
    attempt.fail();
    assert!(journal.close().await.unwrap_err().is(ErrorCode::Eio));
    assert!(journal.snapshot().uncertain);
    assert_eq!(journal.snapshot().retained_resources, 1);
    peer.finish(false).await;
}

#[tokio::test]
async fn observed_pglite_metadata_schema_error_closes_actual_owner() {
    rejected(Backend::Pglite, Role::Metadata).await;
}
#[tokio::test]
async fn observed_pglite_block_schema_error_closes_actual_owner() {
    rejected(Backend::Pglite, Role::Blocks).await;
}
#[tokio::test]
async fn observed_tidb_metadata_greeting_error_closes_actual_private_pool() {
    rejected(Backend::Tidb, Role::Metadata).await;
}
#[tokio::test]
async fn observed_tidb_block_greeting_error_closes_actual_private_pool() {
    rejected(Backend::Tidb, Role::Blocks).await;
}
#[tokio::test]
async fn cancelled_pglite_metadata_open_retains_uncertain_group() {
    cancelled(Backend::Pglite, Role::Metadata).await;
}
#[tokio::test]
async fn cancelled_pglite_block_open_retains_uncertain_group() {
    cancelled(Backend::Pglite, Role::Blocks).await;
}
#[tokio::test]
async fn cancelled_tidb_metadata_open_retains_uncertain_group() {
    cancelled(Backend::Tidb, Role::Metadata).await;
}

#[tokio::test]
async fn uncertain_wire_fixture_joins_early_eof_before_stop() {
    let mut peer = Peer::start(Backend::Tidb, Role::Metadata, false).await;
    let StoreConfig::Tidb { connection, .. } = &peer.config else {
        panic!("expected controlled TiDB wire fixture");
    };
    let address = connection
        .strip_prefix("mysql://unused@")
        .unwrap()
        .strip_suffix("/unused")
        .unwrap();
    let client = tokio::net::TcpStream::connect(address).await.unwrap();
    peer.reached().await;
    drop(client);
    tokio::time::timeout(BOUND, async {
        while !peer.task.as_ref().unwrap().is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Force EOF before stop delivery. The terminal task must still be joined;
    // this fixture observation makes no provider/journal cleanup claim.
    peer.finish(false).await;
}
#[tokio::test]
async fn cancelled_tidb_block_open_retains_uncertain_group() {
    cancelled(Backend::Tidb, Role::Blocks).await;
}

#[derive(Default)]
struct Observer(Mutex<Vec<Arc<dyn ConstructionResource>>>);
impl ConstructionObserver for Observer {
    fn retain(&self, resource: Arc<dyn ConstructionResource>) {
        self.0.lock().unwrap().push(resource);
    }
}

struct Probe {
    id: usize,
    events: Arc<Mutex<Vec<usize>>>,
    calls: AtomicUsize,
    fail: bool,
}
#[async_trait::async_trait]
impl ConstructionResource for Probe {
    async fn close(&self) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.events.lock().unwrap().push(self.id);
        tokio::task::yield_now().await;
        if self.fail {
            Err(FsError::new(ErrorCode::Eio))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn provider_group_preserves_metadata_first_and_closes_each_owner_once() {
    let observer = Observer::default();
    let construction = ProviderConstruction::new(&observer);
    assert_eq!(
        observer.0.lock().unwrap().len(),
        1,
        "group precedes child registration"
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let probes: Vec<_> = [0, 1]
        .into_iter()
        .map(|id| {
            Arc::new(Probe {
                id,
                events: events.clone(),
                calls: AtomicUsize::new(0),
                fail: false,
            })
        })
        .collect();
    for probe in &probes {
        construction.retain(probe.clone());
    }
    let group = construction.finish();
    group.close().await.unwrap();
    let group = observer.0.lock().unwrap()[0].clone();
    group.close().await.unwrap();
    assert_eq!(*events.lock().unwrap(), [0, 1]);
    for probe in probes {
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn provider_group_failed_owner_retains_later_dependencies_and_is_terminal() {
    let observer = Observer::default();
    let construction = ProviderConstruction::new(&observer);
    let events = Arc::new(Mutex::new(Vec::new()));
    let first = Arc::new(Probe {
        id: 0,
        events: events.clone(),
        calls: AtomicUsize::new(0),
        fail: true,
    });
    let later = Arc::new(Probe {
        id: 1,
        events: events.clone(),
        calls: AtomicUsize::new(0),
        fail: false,
    });
    construction.retain(first.clone());
    construction.retain(later.clone());
    let weak = Arc::downgrade(&later);
    drop(later);
    let group = construction.finish();
    assert!(group.close().await.is_err());
    assert!(group.close().await.is_err());
    assert_eq!(*events.lock().unwrap(), [0]);
    assert_eq!(first.calls.load(Ordering::SeqCst), 1);
    assert!(
        weak.upgrade().is_some(),
        "failed close keeps reconciliation dependencies"
    );
}

#[tokio::test]
async fn provider_group_late_registration_rejects_cleanup_and_retains_owner() {
    let observer = Observer::default();
    let construction = ProviderConstruction::new(&observer);
    let group = construction.finish();
    let owner = Arc::new(Probe {
        id: 0,
        events: Arc::new(Mutex::new(Vec::new())),
        calls: AtomicUsize::new(0),
        fail: false,
    });
    let weak = Arc::downgrade(&owner);
    group.retain(owner);
    assert!(group.close().await.is_err());
    assert!(weak.upgrade().is_some());
    assert!(weak.upgrade().unwrap().events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn provider_group_poison_rejects_cleanup_and_retains_owner() {
    let observer = Observer::default();
    let construction = ProviderConstruction::new(&observer);
    let owner = Arc::new(Probe {
        id: 0,
        events: Arc::new(Mutex::new(Vec::new())),
        calls: AtomicUsize::new(0),
        fail: false,
    });
    let weak = Arc::downgrade(&owner);
    construction.retain(owner);
    let group = construction.finish();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _state = group.state.lock().unwrap();
            panic!("owned group poison control");
        }))
        .is_err()
    );
    assert!(group.close().await.is_err());
    assert!(weak.upgrade().is_some());
    assert!(weak.upgrade().unwrap().events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn observed_context_tidb_error_does_not_close_context_owned_pool() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let connection = format!("mysql://unused@{}/unused", listener.local_addr().unwrap());
    let (second_checkout_tx, second_checkout) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut second_checkout_tx = Some(second_checkout_tx);
        for index in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut payload = vec![0xff, 0x15, 0x04, b'#'];
            payload.extend_from_slice(b"28000owned context reuse rejection");
            let length = payload.len();
            socket
                .write_all(&[length as u8, (length >> 8) as u8, (length >> 16) as u8, 0])
                .await
                .unwrap();
            socket.write_all(&payload).await.unwrap();
            if index == 1 {
                second_checkout_tx.take().unwrap().send(()).unwrap();
            }
            let mut byte = [0];
            assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
        }
    });
    let config = StoreConfig::Tidb {
        connection: connection.clone(),
        volume_key: "context-owned".into(),
        durable: false,
    };
    let context = StorageContext::new(1).unwrap();
    let journal = crate::ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    assert!(
        tokio::time::timeout(
            BOUND,
            context.inspect_compact_layout_with_construction_observer(
                &config,
                &StoreConfig::Memory,
                &journal,
            )
        )
        .await
        .unwrap()
        .is_err()
    );
    attempt.fail();
    journal.close().await.unwrap();
    context.require_open().unwrap();
    let reused = context.tidb(&connection).unwrap();
    assert_eq!(context.inner.lock().unwrap().tidb.len(), 1);
    // A disconnected pool cannot reach this independently accepted second
    // connection. No successful MySQL handshake or schema is claimed.
    assert!(
        tokio::time::timeout(
            BOUND,
            reused.blocks(TidbStorageOptions::new("after-attempt"))
        )
        .await
        .unwrap()
        .is_err()
    );
    let reached_again = tokio::time::timeout(BOUND, second_checkout).await;
    tokio::time::timeout(BOUND, context.close())
        .await
        .unwrap()
        .unwrap();
    if !matches!(reached_again, Ok(Ok(()))) {
        peer.abort();
        let _ = peer.await;
        panic!("journal cleanup disconnected the context-owned pool");
    }
    tokio::time::timeout(BOUND, peer).await.unwrap().unwrap();
}

#[tokio::test]
async fn observed_sync_blocks_stay_retained_after_decorator_error() {
    struct Capture(Mutex<Option<std::sync::Weak<dyn BlockStore>>>);
    impl crate::filesystem::BlockStoreDecorator for Capture {
        fn decorate(
            &self,
            _: &StoreConfig,
            store: Arc<dyn BlockStore>,
        ) -> Result<Arc<dyn BlockStore>> {
            *self.0.lock().unwrap() = Some(Arc::downgrade(&store));
            Err(FsError::new(ErrorCode::Eacces))
        }
    }
    let decorator = Capture(Mutex::new(None));
    let journal = crate::ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let result = open_storage_in_context_with_observer(
        &StoreConfig::Memory,
        &StoreConfig::Memory,
        Some(&decorator),
        None,
        Some(&journal),
    )
    .await;
    assert!(matches!(result, Err(error) if error.is(ErrorCode::Eacces)));
    let weak = decorator.0.lock().unwrap().clone().unwrap();
    assert!(
        weak.upgrade().is_some(),
        "original provider survives failed decoration"
    );
    attempt.fail();
    journal.close().await.unwrap();
    assert!(weak.upgrade().is_none());
}

async fn observed_inspection_cancelled_at_constructor(role: Role) {
    let mut peer = Peer::start(Backend::Pglite, role, false).await;
    let (metadata, blocks) = pair(role, peer.config.clone());
    let context = StorageContext::new(1).unwrap();
    let observer = Arc::new(Observer::default());
    let operation = tokio::spawn({
        let context = context.clone();
        let observer = observer.clone();
        async move {
            context
                .inspect_compact_layout_with_construction_observer(
                    &metadata,
                    &blocks,
                    observer.as_ref(),
                )
                .await
        }
    });
    peer.reached().await;
    let groups = observer.0.lock().unwrap().clone();
    assert_eq!(
        groups.len(),
        1,
        "provider group precedes the blocked constructor"
    );
    assert_eq!(groups[0].close().await.unwrap_err().code, ErrorCode::Ebusy);
    operation.abort();
    assert!(matches!(operation.await, Err(error) if error.is_cancelled()));
    assert_eq!(groups[0].close().await.unwrap_err().code, ErrorCode::Eio);
    assert_eq!(groups[0].close().await.unwrap_err().code, ErrorCode::Eio);
    assert_eq!(observer.0.lock().unwrap().len(), 1);
    // Stopping this controlled peer is containment, not client cleanup proof.
    peer.finish(false).await;
    assert!(context.inner.lock().unwrap().tidb.is_empty());
    context.close().await.unwrap();
}

#[tokio::test]
async fn observed_inspection_cancelled_metadata_constructor_retains_uncertainty() {
    observed_inspection_cancelled_at_constructor(Role::Metadata).await;
}

#[tokio::test]
async fn observed_inspection_cancelled_block_constructor_retains_uncertainty() {
    observed_inspection_cancelled_at_constructor(Role::Blocks).await;
}

async fn observed_inspection_constructor_error_closes_actual_owner(role: Role) {
    let mut peer = Peer::start(Backend::Pglite, role, true).await;
    let (metadata, blocks) = pair(role, peer.config.clone());
    let context = StorageContext::new(1).unwrap();
    let observer = Observer::default();
    let inspected = tokio::time::timeout(
        BOUND,
        context.inspect_compact_layout_with_construction_observer(&metadata, &blocks, &observer),
    )
    .await
    .unwrap();
    assert!(inspected.is_err());
    peer.reached().await;
    let groups = observer.0.lock().unwrap().clone();
    assert_eq!(groups.len(), 1);
    tokio::time::timeout(BOUND, groups[0].close())
        .await
        .unwrap()
        .unwrap();
    groups[0].close().await.unwrap();
    context.require_open().unwrap();
    peer.finish(true).await;
    context.close().await.unwrap();
}

#[tokio::test]
async fn observed_inspection_metadata_error_closes_actual_wire_owner() {
    observed_inspection_constructor_error_closes_actual_owner(Role::Metadata).await;
}

#[tokio::test]
async fn observed_inspection_block_error_closes_actual_wire_owner() {
    observed_inspection_constructor_error_closes_actual_owner(Role::Blocks).await;
}

#[tokio::test]
async fn observed_inspection_retained_operation_survives_waiter_timeout() {
    let mut peer = Peer::start(Backend::Pglite, Role::Metadata, false).await;
    let metadata = peer.config.clone();
    let context = StorageContext::new(1).unwrap();
    let observer = Arc::new(Observer::default());
    let mut operation = tokio::spawn({
        let context = context.clone();
        let observer = observer.clone();
        async move {
            context
                .inspect_compact_layout_with_construction_observer(
                    &metadata,
                    &StoreConfig::Memory,
                    observer.as_ref(),
                )
                .await
        }
    });
    peer.reached().await;
    let groups = observer.0.lock().unwrap().clone();
    assert_eq!(groups.len(), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut operation)
            .await
            .is_err()
    );
    assert!(!operation.is_finished());
    assert_eq!(groups[0].close().await.unwrap_err().code, ErrorCode::Ebusy);
    // End the held wire operation without cancelling its actual owner. The
    // constructor then returns an acknowledged provider error; this fixture
    // supplies no successful SQL, compact layout or backing-store durability.
    peer.finish(false).await;
    assert!(
        tokio::time::timeout(BOUND, &mut operation)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    tokio::time::timeout(BOUND, groups[0].close())
        .await
        .unwrap()
        .unwrap();
    groups[0].close().await.unwrap();
    context.require_open().unwrap();
    context.close().await.unwrap();
}
