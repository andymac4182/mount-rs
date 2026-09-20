use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use mount_rs_9p::{
    P9_NOFID, P9_NOTAG, P9_RATTACH, P9_RSETATTR, P9_RVERSION, P9_SETATTR_ATIME,
    P9_SETATTR_ATIME_SET, P9_SETATTR_MTIME, P9_SETATTR_MTIME_SET, P9_TATTACH, P9_TSETATTR,
    P9_TVERSION, P9Session, P9Time, Tattach, Tsetattr, Tversion, decode_message, encode_message,
    read_rversion, write_tattach, write_tsetattr, write_tversion,
};
use mount_rs_core::{FsDriver, MemoryFs};

type TimestampCall = (String, i128, i128, bool);

struct SpyDriver {
    inner: MemoryFs,
    calls: Mutex<Vec<TimestampCall>>,
}

impl FsDriver for SpyDriver {
    fn capabilities(&self) -> mount_rs_core::Capabilities {
        self.inner.capabilities()
    }

    fn has_utimens(&self) -> bool {
        true
    }

    fn utimens<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        atime_ns: i128,
        mtime_ns: i128,
        follow_symlinks: bool,
    ) -> Pin<Box<dyn Future<Output = mount_rs_core::Result<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let path = path.to_owned();
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((path, atime_ns, mtime_ns, follow_symlinks));
            Ok(())
        })
    }

    fn stat<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> Pin<
        Box<dyn Future<Output = mount_rs_core::Result<mount_rs_core::Stats>> + Send + 'async_trait>,
    >
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.stat(path).await })
    }

    fn readdir<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> Pin<
        Box<
            dyn Future<Output = mount_rs_core::Result<Vec<mount_rs_core::DirEntry>>>
                + Send
                + 'async_trait,
        >,
    >
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
    ) -> Pin<
        Box<
            dyn Future<Output = mount_rs_core::Result<Arc<dyn mount_rs_core::FileHandle>>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'a: 'async_trait,
        'b: 'async_trait,
        'c: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.open(path, flags, mode).await })
    }
}

fn frame<F>(type_: u8, tag: u16, write: F) -> Vec<u8>
where
    F: FnOnce(&mut mount_rs_9p::P9Writer) -> Result<(), mount_rs_9p::P9Error>,
{
    encode_message(type_, tag, 512, write).expect("test message encodes")
}

async fn call(session: &P9Session, request: Vec<u8>) -> Vec<u8> {
    session
        .handle_call(&request)
        .await
        .expect("complete request gets a response")
}

fn response_type(response: &[u8]) -> u8 {
    decode_message(response)
        .expect("response frame decodes")
        .0
        .type_
}

#[tokio::test]
async fn tsetattr_forwards_signed_i128_nanoseconds_without_millisecond_loss() {
    let driver = Arc::new(SpyDriver {
        inner: MemoryFs::empty(),
        calls: Mutex::new(Vec::new()),
    });
    let session = P9Session::from_arc(driver.clone());

    let version = call(
        &session,
        frame(P9_TVERSION, P9_NOTAG, |writer| {
            write_tversion(
                writer,
                &Tversion {
                    msize: 8192,
                    version: "9P2000.L".to_owned(),
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&version), P9_RVERSION);
    let (_, version) =
        mount_rs_9p::decode_message_as(&version, read_rversion).expect("Rversion decodes");
    assert_eq!(version.version, "9P2000.L");

    let attach = call(
        &session,
        frame(P9_TATTACH, 1, |writer| {
            write_tattach(
                writer,
                &Tattach {
                    fid: 1,
                    afid: P9_NOFID,
                    uname: "utimens-test".to_owned(),
                    aname: String::new(),
                    n_uname: u32::MAX,
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&attach), P9_RATTACH);

    // 9P2000.L carries an unsigned seconds field.  The values below exercise
    // the largest representable wire seconds and a non-millisecond nanosecond
    // part; the transport must widen them to signed i128 before calling the
    // core extension rather than narrowing through i64 or milliseconds.
    let setattr = call(
        &session,
        frame(P9_TSETATTR, 2, |writer| {
            write_tsetattr(
                writer,
                Tsetattr {
                    fid: 1,
                    valid: P9_SETATTR_ATIME
                        | P9_SETATTR_ATIME_SET
                        | P9_SETATTR_MTIME
                        | P9_SETATTR_MTIME_SET,
                    mode: 0,
                    uid: 0,
                    gid: 0,
                    size: 0,
                    atime: P9Time {
                        sec: u64::MAX,
                        nsec: 123_456_789,
                    },
                    mtime: P9Time {
                        sec: 42,
                        nsec: 987_654_321,
                    },
                },
            );
            Ok(())
        }),
    )
    .await;
    assert_eq!(response_type(&setattr), P9_RSETATTR);

    assert_eq!(
        *driver.calls.lock().unwrap(),
        vec![(
            "/".to_owned(),
            i128::from(u64::MAX) * 1_000_000_000 + 123_456_789,
            42_987_654_321,
            false,
        )]
    );
}
