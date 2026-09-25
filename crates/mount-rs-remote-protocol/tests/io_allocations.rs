use mount_rs_remote_protocol::binary::{self, Header, IoRequest};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use tokio::io::AsyncWrite;
thread_local! { static COUNT: Cell<Option<usize>> = const { Cell::new(None) }; }
struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        COUNT.with(|c| {
            if let Some(n) = c.get() {
                c.set(Some(n + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        COUNT.with(|c| {
            if let Some(n) = c.get() {
                c.set(Some(n + 1));
            }
        });
        unsafe { System.realloc(p, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;
struct Sink;
impl AsyncWrite for Sink {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        b: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Ok(b.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
fn ready<F: Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("in-memory codec must be ready"),
    }
}
fn count<T>(f: impl FnOnce() -> T) -> (T, usize) {
    COUNT.with(|c| c.set(Some(0)));
    let result = f();
    let n = COUNT.with(|c| c.replace(None).unwrap());
    (result, n)
}
#[test]
fn io_metadata_encode_and_read_body_allocate_nothing() {
    let request = IoRequest {
        drive_id: "data",
        handle: 17,
        position: Some(0),
    };
    let mut encoded = Vec::new();
    ready(binary::write_request(&mut encoded, 7, &request, 1024, None)).unwrap();
    let mut input = encoded.as_slice();
    let header = ready(Header::read(&mut input)).unwrap();
    let (result, encode) =
        count(|| ready(binary::write_request(&mut Sink, 7, &request, 1024, None)));
    result.unwrap();
    let (result, decode) = count(|| ready(binary::read_body(&mut input, header)));
    result.unwrap();
    assert_eq!(
        (encode, decode),
        (0, 0),
        "metadata encode/decode allocations"
    );
}

#[test]
fn write_metadata_helper_allocates_nothing_and_leaves_raw_payload_unread() {
    let request = IoRequest {
        drive_id: "drive",
        handle: u64::MAX,
        position: None,
    };
    let payload = [23; 4096];
    let mut bytes = Vec::new();
    ready(binary::write_request(
        &mut bytes,
        1,
        &request,
        0,
        Some(&payload),
    ))
    .unwrap();
    let mut reader = bytes.as_slice();
    let h = ready(Header::read(&mut reader)).unwrap();
    let (result, allocations) = count(|| ready(binary::read_io_metadata(&mut reader, h)));
    let metadata = result.unwrap();
    assert_eq!(allocations, 0);
    assert_eq!(metadata.drive_id.as_ref(), "drive");
    assert_eq!(reader, payload);
    let mut reader = &bytes[binary::HEADER_BYTES..];
    let (result, allocations) = count(|| ready(binary::read_body(&mut reader, h)));
    result.unwrap();
    assert_eq!(allocations, 1, "only the owned raw payload allocates");
}
