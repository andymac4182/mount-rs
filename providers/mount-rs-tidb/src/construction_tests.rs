//! Actual private constructors and mysql_async pools at a controlled greeting boundary.
//! The loopback endpoint supplies no TiDB session or schema behavior.

use super::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{error::Elapsed, timeout};

const BOUND: Duration = Duration::from_secs(5);
type CloseOutcome = std::result::Result<mount_rs_core::Result<()>, Elapsed>;

#[derive(Default)]
struct RecordingObserver {
    resources: Mutex<Vec<Arc<dyn ConstructionResource>>>,
}

impl ConstructionObserver for RecordingObserver {
    fn retain(&self, resource: Arc<dyn ConstructionResource>) {
        self.resources.lock().unwrap().push(resource);
    }
}

impl RecordingObserver {
    fn retained_count(&self) -> usize {
        self.resources.lock().unwrap().len()
    }

    async fn close_retained(&self) -> Vec<CloseOutcome> {
        let resources = self.resources.lock().unwrap().clone();
        let mut results = Vec::with_capacity(resources.len());
        for resource in resources {
            results.push(timeout(BOUND, resource.close()).await);
        }
        results
    }
}

#[derive(Clone, Copy)]
enum Constructor {
    Metadata,
    Blocks,
}

impl Constructor {
    async fn connect(
        self,
        url: &str,
        options: TidbStorageOptions,
        observer: &dyn ConstructionObserver,
    ) -> mount_rs_core::Result<()> {
        match self {
            Self::Metadata => {
                TidbMetadataStore::connect_with_options_and_observer(url, options, Some(observer))
                    .await
                    .map(drop)
            }
            Self::Blocks => {
                TidbBlockStore::connect_with_options_and_observer(url, options, Some(observer))
                    .await
                    .map(drop)
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Greeting {
    Reject,
    Hold,
}

#[derive(Debug)]
struct SocketOutcome {
    accepted: bool,
    eof: bool,
    client_bytes: usize,
}

struct GreetingFixture {
    url: String,
    accepted: Option<oneshot::Receiver<()>>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<io::Result<SocketOutcome>>,
}

impl GreetingFixture {
    async fn start(greeting: Greeting) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut socket, _) = tokio::select! {
                accepted = listener.accept() => accepted?,
                _ = &mut stop_rx => return Ok(SocketOutcome {
                    accepted: false,
                    eof: false,
                    client_bytes: 0,
                }),
            };
            // Own one exact connection; no listener remains available for retries.
            drop(listener);
            if matches!(greeting, Greeting::Reject) {
                // A MySQL ERR packet can precede the normal server greeting.
                let mut payload = vec![0xff, 0x15, 0x04, b'#'];
                payload.extend_from_slice(b"28000");
                payload.extend_from_slice(b"controlled constructor rejection");
                let length = payload.len();
                let header = [length as u8, (length >> 8) as u8, (length >> 16) as u8, 0];
                socket.write_all(&header).await?;
                socket.write_all(&payload).await?;
            }
            let _ = accepted_tx.send(());
            let mut client_bytes = 0;
            let mut byte = [0_u8; 1];
            loop {
                tokio::select! {
                    read = socket.read(&mut byte) => match read? {
                        0 => return Ok(SocketOutcome { accepted: true, eof: true, client_bytes }),
                        count => client_bytes += count,
                    },
                    _ = &mut stop_rx => return Ok(SocketOutcome {
                        accepted: true,
                        eof: false,
                        client_bytes,
                    }),
                }
            }
        });
        Self {
            url: format!("mysql://unused@{address}/unused"),
            accepted: Some(accepted_rx),
            stop: Some(stop_tx),
            task,
        }
    }

    async fn wait_accepted(&mut self) -> bool {
        matches!(
            timeout(BOUND, self.accepted.take().unwrap()).await,
            Ok(Ok(()))
        )
    }

    async fn finish(mut self) -> SocketOutcome {
        let joined = match timeout(BOUND, &mut self.task).await {
            Ok(joined) => joined,
            Err(_) => {
                // A failed EOF assertion still stops and joins the owned fixture.
                // This fallback is not counted as observed client socket closure.
                let _ = self.stop.take().unwrap().send(());
                match timeout(BOUND, &mut self.task).await {
                    Ok(joined) => joined,
                    Err(_) => {
                        self.task.abort();
                        let _ = self.task.await;
                        panic!("owned greeting fixture did not finish after its stop signal");
                    }
                }
            }
        };
        joined
            .expect("owned greeting fixture task failed")
            .expect("owned greeting fixture socket failed")
    }
}

fn assert_closed(results: Vec<CloseOutcome>) {
    assert_eq!(results.len(), 1, "exact private pool must remain retained");
    for result in results {
        result
            .expect("retained private pool close exceeded its deadline")
            .expect("retained private pool close failed");
    }
}

fn assert_socket_closed(outcome: SocketOutcome) {
    assert!(
        outcome.accepted,
        "actual constructor never reached loopback"
    );
    assert!(
        outcome.eof,
        "fixture stop is not proof of client socket EOF"
    );
    assert_eq!(outcome.client_bytes, 0, "no handshake or SQL was supplied");
}

async fn rejected_greeting_retains_pool(constructor: Constructor) {
    let fixture = GreetingFixture::start(Greeting::Reject).await;
    let observer = RecordingObserver::default();
    let result = timeout(
        BOUND,
        constructor.connect(&fixture.url, TidbStorageOptions::new("rejected"), &observer),
    )
    .await;
    let retained = observer.retained_count();
    let closed = observer.close_retained().await;
    let outcome = fixture.finish().await;
    assert!(matches!(result, Ok(Err(_))), "actual constructor must fail");
    assert_eq!(retained, 1, "pool ownership must survive constructor error");
    assert_closed(closed);
    assert_socket_closed(outcome);
}

async fn cancelled_greeting_retains_pool(constructor: Constructor) {
    let mut fixture = GreetingFixture::start(Greeting::Hold).await;
    let observer = Arc::new(RecordingObserver::default());
    let mut task = tokio::spawn({
        let url = fixture.url.clone();
        let observer = observer.clone();
        async move {
            constructor
                .connect(
                    &url,
                    TidbStorageOptions::new("cancelled"),
                    observer.as_ref(),
                )
                .await
        }
    });
    let accepted = fixture.wait_accepted().await;
    let retained_before_abort = observer.retained_count();
    task.abort();
    let (cancelled, cancellation_timed_out) = match timeout(BOUND, &mut task).await {
        Ok(joined) => (joined, false),
        Err(_) => {
            task.abort();
            (task.await, true)
        }
    };
    let retained_after_abort = observer.retained_count();
    let closed = observer.close_retained().await;
    let outcome = fixture.finish().await;
    assert!(accepted, "actual constructor must reach the held greeting");
    assert!(
        !cancellation_timed_out,
        "constructor cancellation exceeded its deadline"
    );
    assert!(matches!(cancelled, Err(error) if error.is_cancelled()));
    assert_eq!(
        retained_before_abort, 1,
        "retain must precede checkout await"
    );
    assert_eq!(
        retained_after_abort, 1,
        "pool must survive awaited cancellation"
    );
    assert_closed(closed);
    assert_socket_closed(outcome);
}

async fn invalid_options_retain_nothing(constructor: Constructor) {
    let observer = RecordingObserver::default();
    let options = TidbStorageOptions::new("invalid").with_max_block_bytes(0);
    let result = constructor
        .connect("mysql://unused@127.0.0.1:1/unused", options, &observer)
        .await;
    assert!(result.is_err());
    assert_eq!(observer.retained_count(), 0);
}

#[tokio::test]
async fn metadata_constructor_greeting_error_retains_closeable_pool() {
    rejected_greeting_retains_pool(Constructor::Metadata).await;
}

#[tokio::test]
async fn block_constructor_greeting_error_retains_closeable_pool() {
    rejected_greeting_retains_pool(Constructor::Blocks).await;
}

#[tokio::test]
async fn metadata_constructor_awaited_cancellation_retains_closeable_pool() {
    cancelled_greeting_retains_pool(Constructor::Metadata).await;
}

#[tokio::test]
async fn block_constructor_awaited_cancellation_retains_closeable_pool() {
    cancelled_greeting_retains_pool(Constructor::Blocks).await;
}

#[tokio::test]
async fn metadata_constructor_invalid_options_retain_nothing() {
    invalid_options_retain_nothing(Constructor::Metadata).await;
}

#[tokio::test]
async fn block_constructor_invalid_options_retain_nothing() {
    invalid_options_retain_nothing(Constructor::Blocks).await;
}
