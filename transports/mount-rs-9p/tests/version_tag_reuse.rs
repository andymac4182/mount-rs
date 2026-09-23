use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mount_rs_9p::{
    P9_GETATTR_BASIC, P9_NOTAG, P9_RLERROR, P9_RVERSION, P9_TGETATTR, P9_TVERSION, P9Session,
    Tgetattr, Tversion, decode_message, encode_message, write_tgetattr, write_tversion,
};
use mount_rs_core::{Capabilities, DirEntry, FileHandle, FsDriver, Result as FsResult, Stats};
use mount_rs_memfs::MemoryFs;
use tokio::sync::Notify;
use tokio::time::timeout;

struct TwoBlockedStats {
    inner: MemoryFs,
    calls: AtomicUsize,
    old_entered: Arc<Notify>,
    old_release: Arc<Notify>,
    new_entered: Arc<Notify>,
    new_release: Arc<Notify>,
}

impl FsDriver for TwoBlockedStats {
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
            match self.calls.fetch_add(1, Ordering::AcqRel) {
                0 => {
                    let release = self.old_release.notified();
                    tokio::pin!(release);
                    release.as_mut().enable();
                    self.old_entered.notify_one();
                    release.await;
                }
                1 => {
                    let release = self.new_release.notified();
                    tokio::pin!(release);
                    release.as_mut().enable();
                    self.new_entered.notify_one();
                    release.await;
                }
                _ => {}
            }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_request_cleanup_does_not_remove_reused_tag_after_version_reset() {
    let old_entered = Arc::new(Notify::new());
    let old_release = Arc::new(Notify::new());
    let new_entered = Arc::new(Notify::new());
    let new_release = Arc::new(Notify::new());
    let session = P9Session::new(TwoBlockedStats {
        inner: MemoryFs::empty(),
        calls: AtomicUsize::new(0),
        old_entered: Arc::clone(&old_entered),
        old_release: Arc::clone(&old_release),
        new_entered: Arc::clone(&new_entered),
        new_release: Arc::clone(&new_release),
    });
    let version = version_request();
    let request = getattr_request();

    let first_version = session.handle_call(&version).await.unwrap();
    assert_eq!(decode_message(&first_version).unwrap().0.type_, P9_RVERSION);
    session.fid_create(1, "/").unwrap();

    let old_session = session.clone();
    let old_request = request.clone();
    let old = tokio::spawn(async move { old_session.handle_call(&old_request).await });
    timeout(Duration::from_secs(2), old_entered.notified())
        .await
        .expect("old tag-7 call reaches blocked driver");
    assert_eq!(session.inflight(), 1);

    let second_version = session.handle_call(&version).await.unwrap();
    assert_eq!(
        decode_message(&second_version).unwrap().0.type_,
        P9_RVERSION
    );
    session.fid_create(1, "/").unwrap();

    let new_session = session.clone();
    let new_request = request.clone();
    let new = tokio::spawn(async move { new_session.handle_call(&new_request).await });
    timeout(Duration::from_secs(2), new_entered.notified())
        .await
        .expect("new tag-7 call reaches blocked driver");
    assert_eq!(session.inflight(), 1);

    old_release.notify_waiters();
    timeout(Duration::from_secs(2), old)
        .await
        .expect("old request resumes")
        .expect("old request task joins")
        .expect("old request gets a reply");

    assert_eq!(
        session.inflight(),
        1,
        "old request cleanup removed the newer tag-7 Pending"
    );
    let duplicate = session.handle_call(&request).await.unwrap();
    assert_eq!(
        decode_message(&duplicate).unwrap().0.type_,
        P9_RLERROR,
        "third tag-7 call must be rejected while the new call is active"
    );

    new_release.notify_waiters();
    timeout(Duration::from_secs(2), new)
        .await
        .expect("new request resumes")
        .expect("new request task joins")
        .expect("new request gets a reply");
    assert_eq!(session.inflight(), 0);
}
