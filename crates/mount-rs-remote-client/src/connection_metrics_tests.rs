//! Isolated real QUIC stream-credit and transaction-lifetime qualification.
use super::*;
use mount_rs_core::diagnostics::storage;
use mount_rs_remote_protocol::{
    OperationName,
    binary::{Incoming, IoResult},
};
use std::{
    future::{Future, poll_fn},
    task::Poll,
};
use tokio::time::timeout;

const LABEL: &str = "client.quic.open_bi";

async fn pair() -> (QuicTransport, quinn::Endpoint, quinn::Connection) {
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert = certificate.cert.der().clone();
    let key =
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into();
    let mut config = quinn::ServerConfig::with_single_cert(vec![cert.clone()], key).unwrap();
    let mut limits = quinn::TransportConfig::default();
    limits.max_concurrent_bidi_streams(0u32.into());
    config.transport_config(Arc::new(limits));
    let server = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let mut client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    client.set_default_client_config(
        quinn::ClientConfig::with_root_certificates(Arc::new(roots)).unwrap(),
    );
    let connecting = client
        .connect(server.local_addr().unwrap(), "localhost")
        .unwrap();
    let (connection, accepted) = tokio::join!(connecting, async {
        server.accept().await.unwrap().await.unwrap()
    });
    (
        QuicTransport {
            _endpoint: client,
            connection: connection.unwrap(),
            version: PROTOCOL_VERSION,
        },
        server,
        accepted,
    )
}

fn message() -> Message {
    Message::Request {
        request_id: 7,
        drive_id: "drive".into(),
        operation: Operation {
            name: OperationName::Stat,
            body: serde_json::json!({"path":"/"}),
        },
    }
}

async fn pending(future: &mut (impl Future + Unpin)) {
    poll_fn(|cx| {
        assert!(
            std::pin::Pin::new(&mut *future).poll(cx).is_pending(),
            "zero stream credit must block acquisition"
        );
        Poll::Ready(())
    })
    .await;
}

fn delta(before: &storage::Snapshot) -> storage::Snapshot {
    storage::snapshot().delta(before).unwrap()
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "isolated process: MOUNT_RS_PROFILE_IO=1, storage/request traces=0"]
async fn quic_stream_acquisition_outcomes_preserve_transactions() {
    assert_eq!(std::env::var("MOUNT_RS_PROFILE_IO").as_deref(), Ok("1"));
    assert_eq!(std::env::var("MOUNT_RS_TRACE_STORAGE").as_deref(), Ok("0"));
    assert!(storage::enabled());
    timeout(Duration::from_secs(15), async {
        let mut phases = Vec::new();
        let mut gauges = Vec::new();
        let (transport, server, accepted) = pair().await;
        let request = IoRequest { drive_id: "drive", handle: 11, position: Some(23) };
        let payload: Vec<u8> = (0..65543).map(|n| (n * 71 % 256) as u8).collect();
        let mut buffer = vec![0u8; payload.len()];
        let before = storage::snapshot();
        let mut control = transport.exchange(message());
        let mut read = transport.read(1, &request, &mut buffer);
        let mut write = transport.write(2, &request, &payload);
        println!("MOUNT_RS_CLIENT_FUTURE_OBJECT exchange_bytes={} read_bytes={} write_bytes={} allocation_count_unmeasured=true", std::mem::size_of_val(control.as_ref().get_ref()), std::mem::size_of_val(read.as_ref().get_ref()), std::mem::size_of_val(write.as_ref().get_ref()));
        pending(&mut control).await;
        pending(&mut read).await;
        pending(&mut write).await;
        gauges.push(("before_acquisition", storage::snapshot(), 3));
        drop(control);
        drop(read);
        drop(write);
        assert!(transport.connection.close_reason().is_none());
        assert!(accepted.close_reason().is_none());
        assert!(buffer.iter().all(|byte| *byte == 0));
        phases.push(("cancel_before_acquisition", delta(&before), (0, 0, 3)));

        accepted.set_max_concurrent_bi_streams(3u32.into());
        let before = storage::snapshot();
        let response = Message::Response { request_id: 7, result: Ok(serde_json::json!({"complete":true})) };
        let (result, ()) = tokio::join!(transport.exchange(message()), async {
            let (mut send, mut recv) = accepted.accept_bi().await.unwrap();
            assert_eq!(read_frame(&mut recv).await.unwrap(), message());
            assert!(recv.read_to_end(0).await.unwrap().is_empty());
            write_frame(&mut send, &response).await.unwrap();
            send.finish().unwrap();
        });
        assert_eq!(result.unwrap(), response);
        let (result, ()) = tokio::join!(transport.read(1, &request, &mut buffer), async {
            let (mut send, mut recv) = accepted.accept_bi().await.unwrap();
            let header = binary::Header::read(&mut recv).await.unwrap();
            match binary::read_body(&mut recv, header).await.unwrap() {
                Incoming::Read { request_id, request: actual, length } => {
                    assert_eq!((request_id, actual.drive_id.as_ref(), actual.handle, actual.position, length), (1, "drive", 11, Some(23), payload.len()));
                }
                _ => panic!("expected binary read"),
            }
            assert!(recv.read_to_end(0).await.unwrap().is_empty());
            binary::write_result(&mut send, 1, Ok(IoResult::Read(payload.clone()))).await.unwrap();
            send.finish().unwrap();
        });
        assert_eq!(result.unwrap(), payload.len());
        assert_eq!(buffer, payload);
        let (result, ()) = tokio::join!(transport.write(2, &request, &payload), async {
            let (mut send, mut recv) = accepted.accept_bi().await.unwrap();
            let header = binary::Header::read(&mut recv).await.unwrap();
            match binary::read_body(&mut recv, header).await.unwrap() {
                Incoming::Write { request_id, request: actual, data } => {
                    assert_eq!((request_id, actual.drive_id.as_ref(), actual.handle, actual.position), (2, "drive", 11, Some(23)));
                    assert_eq!(data, payload);
                }
                _ => panic!("expected binary write"),
            }
            assert!(recv.read_to_end(0).await.unwrap().is_empty());
            binary::write_result(&mut send, 2, Ok(IoResult::Write(payload.len()))).await.unwrap();
            send.finish().unwrap();
        });
        assert_eq!(result.unwrap(), payload.len());
        assert!(transport.connection.close_reason().is_none());
        phases.push(("successful_reuse", delta(&before), (3, 0, 0)));

        let before = storage::snapshot();
        let (result, ()) = tokio::join!(transport.read(3, &request, &mut buffer), async {
            let (mut send, mut recv) = accepted.accept_bi().await.unwrap();
            let header = binary::Header::read(&mut recv).await.unwrap();
            assert!(matches!(binary::read_body(&mut recv, header).await.unwrap(), Incoming::Read { request_id: 3, .. }));
            assert!(recv.read_to_end(0).await.unwrap().is_empty());
            send.write_all(&[0, 0xff, 1]).await.unwrap();
            send.finish().unwrap();
        });
        assert!(matches!(result, Err(ClientError::Protocol)));
        assert!(transport.connection.close_reason().is_some());
        phases.push(("protocol_error_after_acquisition", delta(&before), (1, 0, 0)));
        let before = storage::snapshot();
        assert!(matches!(transport.exchange(message()).await, Err(ClientError::Transport)));
        assert!(matches!(transport.read(4, &request, &mut buffer).await, Err(ClientError::Transport)));
        assert!(matches!(transport.write(5, &request, &payload).await, Err(ClientError::Transport)));
        phases.push(("closed_connection_acquisition_error", delta(&before), (0, 3, 0)));

        let (other, other_server, other_accepted) = pair().await;
        other_accepted.set_max_concurrent_bi_streams(1u32.into());
        let before = storage::snapshot();
        let mut call = other.exchange(message());
        let (send, recv) = tokio::select! {
            result = &mut call => panic!("response must remain held: {result:?}"),
            streams = async {
                let (send, mut recv) = other_accepted.accept_bi().await.unwrap();
                assert_eq!(read_frame(&mut recv).await.unwrap(), message());
                assert!(recv.read_to_end(0).await.unwrap().is_empty());
                (send, recv)
            } => streams,
        };
        gauges.push(("after_acquisition", storage::snapshot(), 0));
        drop(call);
        assert!(other.connection.close_reason().is_some());
        drop(send);
        drop(recv);
        phases.push(("cancel_after_acquisition", delta(&before), (1, 0, 0)));

        transport.close();
        other.close();
        server.close(0u32.into(), b"fixture shutdown");
        other_server.close(0u32.into(), b"fixture shutdown");
        timeout(Duration::from_secs(5), async {
            tokio::join!(transport._endpoint.wait_idle(), other._endpoint.wait_idle(), server.wait_idle(), other_server.wait_idle());
        }).await.expect("bounded endpoint connection drain");
        drop(accepted);
        drop(other_accepted);
        drop(transport);
        drop(other);
        drop(server);
        drop(other_server);
        println!("MOUNT_RS_CLIENT_STAGE behavior_oracles=complete full_binary_bytes=true pre_acquisition_reuse=true post_acquisition_close=true endpoints_drained=true");
        for (phase, snapshot, _) in &phases {
            if let Some(row) = snapshot.entries.iter().find(|row| row.name == LABEL) {
                println!("MOUNT_RS_CLIENT_STAGE phase={phase} available=true calls={} success={} error={} cancelled={} in_flight={} bytes={} histogram_total={}", row.calls, row.success, row.error, row.cancelled, row.in_flight, row.bytes, row.latency_log2_us.iter().sum::<u64>());
            } else {
                println!("MOUNT_RS_CLIENT_STAGE phase={phase} available=false");
            }
        }
        for (phase, snapshot, expected) in phases {
            let row = snapshot.entries.iter().find(|row| row.name == LABEL).expect("missing client stream metric after complete behavior oracles");
            assert_eq!((row.success, row.error, row.cancelled), expected, "{phase}");
            assert_eq!(row.calls, expected.0 + expected.1 + expected.2, "{phase}");
            assert_eq!(row.latency_log2_us.iter().sum::<u64>(), row.calls, "{phase}");
            assert_eq!((row.in_flight, snapshot.in_flight, row.bytes, row.returned_rows, row.returned_row_observations), (0, 0, 0, 0, 0), "{phase}");
        }
        for (phase, snapshot, expected) in gauges {
            let row = snapshot.entries.iter().find(|row| row.name == LABEL).unwrap();
            assert_eq!((row.in_flight, snapshot.in_flight), (expected, expected), "{phase}");
        }
    }).await.expect("bounded client stream acquisition qualification");
}
