use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use mount_rs_9p::{
    P9_GETATTR_BASIC, P9_NOTAG, P9_RGETATTR, P9_RLERROR, P9_RVERSION, P9_TGETATTR, P9_TVERSION,
    P9AttachOptions, P9Server, P9ServerOptions, Tgetattr, Tversion, decode_message, encode_message,
    read_rlerror, write_tgetattr, write_tversion,
};
use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle, FsDriver, Result as FsResult, Stats,
};
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

fn version_request(tag: u16) -> Vec<u8> {
    encode_message(P9_TVERSION, tag, 32, |writer| {
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
async fn same_tag_tversion_does_not_orphan_outstanding_request() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let server = Arc::new(P9Server::new(
        BlockingStatDriver {
            inner: MemoryFs::empty(),
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        P9ServerOptions {
            max_in_flight: 2,
            ..P9ServerOptions::default()
        },
    ));
    let (server_stream, mut client_stream) = tokio::io::duplex(64 * 1024);
    let connection = server
        .attach(server_stream, P9AttachOptions::default())
        .unwrap();

    client_stream
        .write_all(&version_request(P9_NOTAG))
        .await
        .unwrap();
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
        .expect("old tag-7 request reaches blocked driver");
    assert_eq!(connection.session.inflight(), 1);

    client_stream.write_all(&version_request(7)).await.unwrap();
    let version_reply = timeout(Duration::from_secs(2), read_frame(&mut client_stream))
        .await
        .expect("same-tag Tversion must answer");
    let (header, mut body) = decode_message(&version_reply).unwrap();
    assert_eq!(header.tag, 7);

    let result = match header.type_ {
        P9_RVERSION => {
            let inflight = connection.session.inflight();
            release.notify_waiters();
            Ok(inflight)
        }
        P9_RLERROR => {
            let code = read_rlerror(&mut body).unwrap().ecode;
            release.notify_waiters();
            let old_reply = timeout(Duration::from_secs(2), read_frame(&mut client_stream)).await;
            let inflight = connection.session.inflight();
            Err((code, old_reply, inflight))
        }
        other => panic!("unexpected reply to same-tag Tversion: {other}"),
    };

    timeout(Duration::from_secs(2), connection.close())
        .await
        .expect("close connection after assertion")
        .unwrap();
    server.close().await.unwrap();

    match result {
        Ok(inflight) => assert_eq!(inflight, 0, "Rversion must drain the old tag"),
        Err((code, old_reply, inflight)) => {
            assert_eq!(code, ErrorCode::Eproto.errno().unsigned_abs());
            let old_reply = old_reply.expect("EPROTO must leave the old request able to finish");
            let old_header = decode_message(&old_reply).unwrap().0;
            assert_eq!((old_header.type_, old_header.tag), (P9_RGETATTR, 7));
            assert_eq!(inflight, 0, "EPROTO must not leave an orphaned tag");
        }
    }
}
