use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use mount_rs_9p::{
    P9_GETATTR_BASIC, P9_NOTAG, P9_RGETATTR, P9_RLERROR, P9_RVERSION, P9_TGETATTR, P9_TVERSION,
    P9AttachOptions, P9Server, P9ServerOptions, Tgetattr, Tversion, decode_message, encode_message,
    write_tgetattr, write_tversion,
};
use mount_rs_core::{Capabilities, DirEntry, FileHandle, FsDriver, Result as FsResult, Stats};
use mount_rs_memfs::MemoryFs;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};

struct BlockingStatDriver {
    inner: MemoryFs,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl FsDriver for BlockingStatDriver {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn stat<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> Pin<Box<dyn Future<Output = FsResult<Stats>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let release = self.release.notified();
            tokio::pin!(release);
            release.as_mut().enable();
            self.entered.notify_one();
            release.await;
            self.inner.stat(path).await
        })
    }

    fn readdir<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> Pin<Box<dyn Future<Output = FsResult<Vec<DirEntry>>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.readdir(path).await })
    }

    fn open<'a, 'b, 'c, 'async_trait>(
        &'a self,
        path: &'b str,
        flags: &'c str,
        mode: u32,
    ) -> Pin<Box<dyn Future<Output = FsResult<Arc<dyn FileHandle>>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        'c: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.open(path, flags, mode).await })
    }
}

fn version_request() -> Vec<u8> {
    encode_message(P9_TVERSION, P9_NOTAG, 32, |writer| {
        write_tversion(
            writer,
            &Tversion {
                msize: 8192,
                version: "9P2000.L".to_owned(),
            },
        )
    })
    .unwrap()
}

fn getattr_request() -> Vec<u8> {
    encode_message(P9_TGETATTR, 7, 32, |writer| {
        write_tgetattr(
            writer,
            Tgetattr {
                fid: 1,
                request_mask: P9_GETATTR_BASIC,
            },
        );
        Ok(())
    })
    .unwrap()
}

fn malformed_version_request() -> Vec<u8> {
    let mut frame = Vec::with_capacity(7);
    frame.extend_from_slice(&7_u32.to_le_bytes());
    frame.push(P9_TVERSION);
    frame.extend_from_slice(&P9_NOTAG.to_le_bytes());
    frame
}

async fn read_frame<R: AsyncRead + Unpin>(stream: &mut R) -> Vec<u8> {
    let mut header = [0_u8; 7];
    stream.read_exact(&mut header).await.unwrap();
    let size = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
    assert!(size >= header.len());
    let mut frame = header.to_vec();
    frame.resize(size, 0);
    stream.read_exact(&mut frame[header.len()..]).await.unwrap();
    frame
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_version_does_not_abort_an_outstanding_valid_request() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let server = Arc::new(P9Server::new(
        BlockingStatDriver {
            inner: MemoryFs::empty(),
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        P9ServerOptions::default(),
    ));
    let (server_stream, mut client_stream) = tokio::io::duplex(64 * 1024);
    let connection = server
        .attach(server_stream, P9AttachOptions::default())
        .unwrap();

    client_stream.write_all(&version_request()).await.unwrap();
    let version = timeout(Duration::from_secs(2), read_frame(&mut client_stream))
        .await
        .expect("initial version reply");
    assert_eq!(decode_message(&version).unwrap().0.type_, P9_RVERSION);
    connection.session.fid_create(1, "/").unwrap();

    client_stream.write_all(&getattr_request()).await.unwrap();
    timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("valid request reaches blocked driver");
    assert_eq!(connection.session.inflight(), 1);

    client_stream
        .write_all(&malformed_version_request())
        .await
        .unwrap();
    timeout(Duration::from_secs(2), async {
        while connection
            .session
            .stats()
            .messages
            .get("Tversion")
            .copied()
            .unwrap_or(0)
            < 2
        {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("malformed version is handled before releasing old request");
    release.notify_waiters();

    let mut saw_version_error = false;
    let mut saw_getattr = false;
    for _ in 0..2 {
        let reply = timeout(Duration::from_secs(2), read_frame(&mut client_stream))
            .await
            .expect("malformed Tversion must leave old request alive");
        let header = decode_message(&reply).unwrap().0;
        match (header.type_, header.tag) {
            (P9_RLERROR, P9_NOTAG) => saw_version_error = true,
            (P9_RGETATTR, 7) => saw_getattr = true,
            other => panic!("unexpected 9P reply: {other:?}"),
        }
    }
    assert!(saw_version_error);
    assert!(saw_getattr);
    assert_eq!(connection.session.msize(), Some(8192));

    connection.close().await.unwrap();
    server.close().await.unwrap();
}
