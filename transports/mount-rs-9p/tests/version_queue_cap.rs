use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use mount_rs_9p::{
    P9_GETATTR_BASIC, P9_NOTAG, P9_RVERSION, P9_TCLUNK, P9_TGETATTR, P9_TVERSION, P9AttachOptions,
    P9Server, P9ServerOptions, Tgetattr, Tversion, decode_message, encode_message, write_tgetattr,
    write_tversion,
};
use mount_rs_core::{Capabilities, DirEntry, FileHandle, FsDriver, Result as FsResult, Stats};
use mount_rs_memfs::MemoryFs;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;
use tokio::time::timeout;

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

fn blocked_getattr_request() -> Vec<u8> {
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

fn minimal_clunk_request() -> Vec<u8> {
    let mut frame = Vec::with_capacity(11);
    frame.extend_from_slice(&11_u32.to_le_bytes());
    frame.push(P9_TCLUNK);
    frame.extend_from_slice(&8_u16.to_le_bytes());
    frame.extend_from_slice(&2_u32.to_le_bytes());
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
async fn full_pending_frame_queue_cannot_hide_a_renegotiation() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let server = Arc::new(P9Server::new(
        BlockingStatDriver {
            inner: MemoryFs::empty(),
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        P9ServerOptions {
            max_in_flight: 1,
            ..P9ServerOptions::default()
        },
    ));
    let (server_stream, mut client_stream) = tokio::io::duplex(64 * 1024);
    let connection = server
        .attach(server_stream, P9AttachOptions::default())
        .unwrap();

    client_stream.write_all(&version_request()).await.unwrap();
    let initial_version = timeout(Duration::from_secs(2), read_frame(&mut client_stream))
        .await
        .expect("initial Rversion");
    assert_eq!(
        decode_message(&initial_version).unwrap().0.type_,
        P9_RVERSION
    );
    connection.session.fid_create(1, "/").unwrap();

    client_stream
        .write_all(&blocked_getattr_request())
        .await
        .unwrap();
    timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("old request reaches blocked driver");
    assert_eq!(connection.session.inflight(), 1);

    // max_in_flight=1 sets pending_frame_limit to 9,362. These valid requests
    // fill the queue behind the blocked call; the trailing Tversion must still
    // be found and abort the old request without releasing the driver gate.
    let mut burst = minimal_clunk_request().repeat(9_362);
    burst.extend_from_slice(&version_request());
    timeout(Duration::from_secs(5), client_stream.write_all(&burst))
        .await
        .expect("send queue-filling burst")
        .unwrap();

    let renegotiation = timeout(Duration::from_secs(2), read_frame(&mut client_stream)).await;
    release.notify_waiters();
    timeout(Duration::from_secs(2), connection.close())
        .await
        .expect("close connection after assertion")
        .unwrap();
    server.close().await.unwrap();

    let renegotiation = renegotiation.expect("Rversion must arrive while old request is blocked");
    let header = decode_message(&renegotiation).unwrap().0;
    assert_eq!((header.type_, header.tag), (P9_RVERSION, P9_NOTAG));
}
