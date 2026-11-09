//! Reproduce a final-frame handoff race in the dependency's two-channel body.
use bytes::Bytes;
use http_body_util::{BodyExt, channel::Channel};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let iterations = 200_000;
    let mut missing = 0;
    for _ in 0..iterations {
        let (mut sender, body) = Channel::<Bytes, std::convert::Infallible>::new(2);
        let producer =
            tokio::spawn(async move { sender.send_data(Bytes::from_static(b"complete")).await });
        let bytes = body.collect().await.unwrap().to_bytes();
        producer.await.unwrap().unwrap();
        if bytes.as_ref() != b"complete" {
            missing += 1;
        }
    }
    println!(
        "{}",
        serde_json::json!({"iterations":iterations,"missing_final_frames":missing,
        "scope":"dependency Channel producer/consumer stress diagnostic; no filesystem or network"})
    );
}
