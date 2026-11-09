//! Deterministically send the final frame between Channel's two queue checks.
#[path = "support/handoff_waker.rs"]
mod handoff_waker;
use bytes::Bytes;
use http_body_util::channel::Channel;
use hyper::body::{Body, Frame};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
fn main() {
    let (mut sender, mut body) = Channel::<Bytes, std::convert::Infallible>::new(2);
    let waker = handoff_waker::on_clone(2, move || {
        sender
            .try_send(Frame::data(Bytes::from_static(b"complete")))
            .unwrap();
        drop(sender);
    });
    let mut cx = Context::from_waker(&waker);
    let premature_eof = matches!(Pin::new(&mut body).poll_frame(&mut cx), Poll::Ready(None));
    let queued_frame_after_eof = matches!(Pin::new(&mut body).poll_frame(&mut cx),
        Poll::Ready(Some(Ok(frame))) if frame.data_ref().unwrap().as_ref() == b"complete");
    println!(
        "{}",
        serde_json::json!({
            "premature_eof":premature_eof,"queued_frame_after_eof":queued_frame_after_eof,
            "scope":"final frame sent during registration on the separate error channel"
        })
    );
    assert!(
        premature_eof && queued_frame_after_eof,
        "dependency race did not reproduce"
    );
}
