use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use mount_rs_9p::{
    P9_NOFID, P9_NOTAG, P9_TATTACH, P9_TVERSION, P9AttachOptions, P9Server, P9ServerOptions,
    Tattach, Tversion, encode_message, write_tattach, write_tversion,
};
use mount_rs_memfs::MemoryFs;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::time::timeout;

struct FailFirstWriteStream {
    inner: tokio::io::DuplexStream,
    failed: Arc<AtomicBool>,
}

impl AsyncRead for FailFirstWriteStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for FailFirstWriteStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if !self.failed.swap(true, Ordering::AcqRel) {
            return Poll::Ready(Err(io::Error::other("deliberate Rversion write failure")));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
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

fn attach_request() -> Vec<u8> {
    encode_message(P9_TATTACH, 1, 64, |writer| {
        write_tattach(
            writer,
            &Tattach {
                fid: 1,
                afid: P9_NOFID,
                uname: "write-failure-test".to_owned(),
                aname: String::new(),
                n_uname: u32::MAX,
            },
        )
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_rversion_write_never_dispatches_coalesced_attach() {
    let server = Arc::new(P9Server::new(MemoryFs::empty(), P9ServerOptions::default()));
    let (server_stream, mut client_stream) = tokio::io::duplex(64 * 1024);
    let failed = Arc::new(AtomicBool::new(false));
    let connection = server
        .attach(
            FailFirstWriteStream {
                inner: server_stream,
                failed: Arc::clone(&failed),
            },
            P9AttachOptions::default(),
        )
        .unwrap();

    let burst = [&version_request()[..], &attach_request()[..]].concat();
    client_stream.write_all(&burst).await.unwrap();
    timeout(Duration::from_secs(2), connection.wait_closed())
        .await
        .expect("failed Rversion write closes the connection");

    assert!(
        failed.load(Ordering::Acquire),
        "Rversion write was attempted"
    );
    let messages = connection.session.stats().messages;
    assert_eq!(messages.get("Tversion"), Some(&1));
    assert_eq!(
        messages.get("Tattach"),
        None,
        "deferred attach was dispatched"
    );

    server.close().await.unwrap();
}
