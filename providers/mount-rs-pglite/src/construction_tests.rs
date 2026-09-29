//! Actual constructor ownership controls against a test-owned PostgreSQL wire
//! peer. The peer models startup and the first DDL response only; these tests
//! do not qualify PGlite SQL execution, persistence, or later schema migrations.

use super::*;
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use std::sync::Mutex as StdMutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Default)]
struct RecordingObserver {
    resources: StdMutex<Vec<Arc<dyn ConstructionResource>>>,
}

impl ConstructionObserver for RecordingObserver {
    fn retain(&self, resource: Arc<dyn ConstructionResource>) {
        self.resources.lock().unwrap().push(resource);
    }
}

impl RecordingObserver {
    fn count(&self) -> usize {
        self.resources.lock().unwrap().len()
    }

    fn resource(&self) -> Arc<dyn ConstructionResource> {
        assert_eq!(self.count(), 1);
        Arc::clone(&self.resources.lock().unwrap()[0])
    }
}

#[derive(Clone, Copy)]
enum Provider {
    Metadata,
    Blocks,
}

impl Provider {
    fn table(self) -> &'static str {
        match self {
            Self::Metadata => "mount_rs_metadata",
            Self::Blocks => "mount_rs_blocks",
        }
    }

    async fn connect(
        self,
        connection: &str,
        options: PgliteStorageOptions,
        observer: &dyn ConstructionObserver,
    ) -> Result<()> {
        match self {
            Self::Metadata => PgliteMetadataStore::connect_with_options_and_observer(
                connection,
                options,
                Some(observer),
            )
            .await
            .map(|_| ()),
            Self::Blocks => PgliteBlockStore::connect_with_options_and_observer(
                connection,
                options,
                Some(observer),
            )
            .await
            .map(|_| ()),
        }
    }
}

enum FirstDdl {
    Error,
    HoldThenSuccess,
    Success,
}

struct WirePeer {
    connection: String,
    query_seen: oneshot::Receiver<()>,
    release: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl WirePeer {
    async fn start(provider: Provider, response: FirstDdl) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (query_seen_tx, query_seen) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let startup_length = stream.read_u32().await.unwrap() as usize;
            assert!((8..=4096).contains(&startup_length));
            let mut startup = vec![0; startup_length - 4];
            stream.read_exact(&mut startup).await.unwrap();
            assert_eq!(&startup[..4], &196608_u32.to_be_bytes());
            frame(&mut stream, b'R', &0_u32.to_be_bytes()).await;
            frame(&mut stream, b'S', b"server_version\x0016.0\0").await;
            frame(&mut stream, b'S', b"client_encoding\0UTF8\0").await;
            frame(&mut stream, b'K', &[0, 0, 0, 1, 0, 0, 0, 2]).await;
            frame(&mut stream, b'Z', b"I").await;

            let (tag, query) = read_frame(&mut stream).await;
            assert_eq!(tag, b'Q');
            let expected = format!("CREATE TABLE IF NOT EXISTS {}", provider.table());
            assert!(query.starts_with(expected.as_bytes()));
            query_seen_tx.send(()).unwrap();
            match response {
                FirstDdl::Error => {
                    frame(
                        &mut stream,
                        b'E',
                        b"SERROR\0CXX000\0Mcontrolled constructor DDL failure\0\0",
                    )
                    .await;
                }
                FirstDdl::HoldThenSuccess => {
                    release_rx.await.unwrap();
                    frame(&mut stream, b'C', b"CREATE TABLE\0").await;
                }
                FirstDdl::Success => {
                    frame(&mut stream, b'C', b"CREATE TABLE\0").await;
                }
            }
            frame(&mut stream, b'Z', b"I").await;

            let (tag, body) = read_frame(&mut stream).await;
            assert_eq!(tag, b'X', "retained close must terminate the wire client");
            assert!(body.is_empty());
            let mut trailing = [0];
            assert_eq!(stream.read(&mut trailing).await.unwrap(), 0);
        });
        Self {
            connection: format!(
                "postgresql://postgres:postgres@127.0.0.1:{port}/postgres?sslmode=disable"
            ),
            query_seen,
            release: Some(release_tx),
            task: Some(task),
        }
    }

    async fn wait_until_first_ddl(&mut self) {
        tokio::time::timeout(DEADLINE, &mut self.query_seen)
            .await
            .expect("constructor did not reach first DDL")
            .unwrap();
    }

    fn release(&mut self) {
        self.release.take().unwrap().send(()).unwrap();
    }

    async fn finish(mut self) {
        let mut task = self.task.take().unwrap();
        match tokio::time::timeout(DEADLINE, &mut task).await {
            Ok(result) => result.expect("controlled wire peer panicked"),
            Err(_) => {
                task.abort();
                let _ = task.await;
                panic!("controlled wire peer did not terminate after resource close");
            }
        }
    }
}

impl Drop for WirePeer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn frame(stream: &mut TcpStream, tag: u8, body: &[u8]) {
    stream.write_u8(tag).await.unwrap();
    stream.write_u32((body.len() + 4) as u32).await.unwrap();
    stream.write_all(body).await.unwrap();
}

async fn read_frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let tag = stream.read_u8().await.unwrap();
    let length = stream.read_u32().await.unwrap() as usize;
    assert!((4..=1024 * 1024).contains(&length));
    let mut body = vec![0; length - 4];
    stream.read_exact(&mut body).await.unwrap();
    (tag, body)
}

async fn close_retained(observer: &RecordingObserver) {
    tokio::time::timeout(DEADLINE, observer.resource().close())
        .await
        .expect("retained database close did not complete")
        .unwrap();
}

async fn finish_constructor(mut constructor: JoinHandle<Result<()>>) -> Result<()> {
    match tokio::time::timeout(DEADLINE, &mut constructor).await {
        Ok(result) => result.expect("constructor task panicked"),
        Err(_) => {
            constructor.abort();
            let _ = constructor.await;
            panic!("constructor did not report controlled DDL failure");
        }
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn invalid_pglite_options_retain_no_constructor_resource() {
    runtime().block_on(async {
        for provider in [Provider::Metadata, Provider::Blocks] {
            let observer = RecordingObserver::default();
            let result = provider
                .connect(
                    "postgresql://postgres:postgres@127.0.0.1:1/postgres?sslmode=disable",
                    PgliteStorageOptions::new(""),
                    &observer,
                )
                .await;
            assert!(result.unwrap_err().is(ErrorCode::Einval));
            assert_eq!(observer.count(), 0);
        }
    });
}

#[test]
fn pglite_constructor_schema_failure_keeps_wire_task_owned_until_close() {
    runtime().block_on(async {
        for provider in [Provider::Metadata, Provider::Blocks] {
            let mut peer = WirePeer::start(provider, FirstDdl::Error).await;
            let observer = Arc::new(RecordingObserver::default());
            let constructor = tokio::spawn({
                let connection = peer.connection.clone();
                let observer = Arc::clone(&observer);
                async move {
                    provider
                        .connect(
                            &connection,
                            PgliteStorageOptions::new("constructor-error"),
                            observer.as_ref(),
                        )
                        .await
                }
            });
            peer.wait_until_first_ddl().await;
            assert_eq!(observer.count(), 1, "retain must precede the DDL await");
            let result = finish_constructor(constructor).await;
            assert!(result.is_err());
            assert_eq!(observer.count(), 1);
            close_retained(&observer).await;
            peer.finish().await;
        }
    });
}

#[test]
fn canceled_pglite_constructor_keeps_wire_task_owned_until_close() {
    runtime().block_on(async {
        for provider in [Provider::Metadata, Provider::Blocks] {
            let mut peer = WirePeer::start(provider, FirstDdl::HoldThenSuccess).await;
            let observer = Arc::new(RecordingObserver::default());
            let constructor = tokio::spawn({
                let connection = peer.connection.clone();
                let observer = Arc::clone(&observer);
                async move {
                    provider
                        .connect(
                            &connection,
                            PgliteStorageOptions::new("constructor-cancel"),
                            observer.as_ref(),
                        )
                        .await
                }
            });
            peer.wait_until_first_ddl().await;
            assert_eq!(observer.count(), 1, "retain must precede the DDL await");
            constructor.abort();
            assert!(constructor.await.unwrap_err().is_cancelled());
            assert_eq!(observer.count(), 1);
            // The wire peer remains responsive for explicit close. These tests
            // make no bounded-close claim for an unresponsive database server.
            peer.release();
            close_retained(&observer).await;
            peer.finish().await;
        }
    });
}

#[test]
fn successful_pglite_block_constructor_shares_idempotent_retained_close() {
    runtime().block_on(async {
        let mut peer = WirePeer::start(Provider::Blocks, FirstDdl::Success).await;
        let observer = RecordingObserver::default();
        let store = PgliteBlockStore::connect_with_options_and_observer(
            &peer.connection,
            PgliteStorageOptions::new("constructor-success"),
            Some(&observer),
        )
        .await
        .unwrap();
        peer.wait_until_first_ddl().await;
        assert_eq!(observer.count(), 1);
        tokio::time::timeout(DEADLINE, store.close())
            .await
            .expect("returned store close did not complete")
            .unwrap();
        close_retained(&observer).await;
        close_retained(&observer).await;
        peer.finish().await;
    });
}
