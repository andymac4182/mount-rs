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
const SEND_LABEL: &str = "client.quic.request_send";
const RECEIVE_LABEL: &str = "client.quic.response_receive";

fn io_outcomes(phase: &str) -> ((u64, u64, u64), (u64, u64, u64)) {
    match phase {
        "cancel_before_acquisition" | "closed_connection_acquisition_error" => {
            ((0, 0, 0), (0, 0, 0))
        }
        "successful_reuse" => ((3, 0, 0), (3, 0, 0)),
        "protocol_error_after_acquisition" | "response_eof_error" => ((1, 0, 0), (0, 1, 0)),
        "remote_error_after_response" | "delayed_send_success" => ((1, 0, 0), (1, 0, 0)),
        "cancel_after_acquisition" => ((1, 0, 0), (0, 0, 1)),
        "cancel_during_send" => ((0, 0, 1), (0, 0, 0)),
        "send_validation_error" | "binary_send_stop" | "control_send_stop" => {
            ((0, 1, 0), (0, 0, 0))
        }
        _ => panic!("unknown fixed client phase"),
    }
}

async fn pair() -> (QuicTransport, quinn::Endpoint, quinn::Connection) {
    pair_with_receive_window(None).await
}

async fn pair_with_receive_window(
    window: Option<u32>,
) -> (QuicTransport, quinn::Endpoint, quinn::Connection) {
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert = certificate.cert.der().clone();
    let key =
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into();
    let mut config = quinn::ServerConfig::with_single_cert(vec![cert.clone()], key).unwrap();
    let mut limits = quinn::TransportConfig::default();
    limits.max_concurrent_bidi_streams(0u32.into());
    if let Some(window) = window {
        limits.stream_receive_window((128 * 1024_u32).into());
        limits.receive_window(window.into());
    }
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
        gauges.push(("before_acquisition", storage::snapshot(), (3, 3, 0, 0)));
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
        let (result, ()) = tokio::join!(other.read(9, &request, &mut buffer), async {
            let (mut send, mut recv) = other_accepted.accept_bi().await.unwrap();
            let header = binary::Header::read(&mut recv).await.unwrap();
            assert!(matches!(binary::read_body(&mut recv, header).await.unwrap(), Incoming::Read { request_id: 9, .. }));
            assert!(recv.read_to_end(0).await.unwrap().is_empty());
            binary::write_result(&mut send, 9, Err(mount_rs_remote_protocol::WireError { code: "EIO".into() })).await.unwrap();
            send.finish().unwrap();
        });
        assert!(matches!(result, Err(ClientError::Remote(code)) if code == "EIO"));
        assert_eq!(buffer, payload);
        assert!(other.connection.close_reason().is_none());
        phases.push(("remote_error_after_response", delta(&before), (1, 0, 0)));
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
        gauges.push(("after_acquisition", storage::snapshot(), (0, 1, 0, 1)));
        drop(call);
        assert!(other.connection.close_reason().is_some());
        drop(send);
        drop(recv);
        phases.push(("cancel_after_acquisition", delta(&before), (1, 0, 0)));

        // Sixteen bytes of connection credit cannot hold the 32-byte
        // header, while stream credit allows the complete payload after release.
        let (delayed, delayed_server, delayed_accepted) = pair_with_receive_window(Some(16)).await;
        delayed_accepted.set_max_concurrent_bi_streams(1u32.into());
        let before = storage::snapshot();
        let mut call = delayed.write(13, &request, &payload);
        let (mut send, mut recv) = tokio::select! {
            result = &mut call => panic!("constrained peer must keep send pending: {result:?}"),
            streams = delayed_accepted.accept_bi() => streams.unwrap(),
        };
        pending(&mut call).await;
        gauges.push(("before_send_release", storage::snapshot(), (0, 1, 1, 0)));
        delayed_accepted.set_receive_window((1024 * 1024_u32).into());
        let (result, ()) = tokio::join!(&mut call, async {
            let header = binary::Header::read(&mut recv).await.unwrap();
            match binary::read_body(&mut recv, header).await.unwrap() {
                Incoming::Write { request_id, request: actual, data } => {
                    assert_eq!((request_id, actual.drive_id.as_ref(), actual.handle, actual.position), (13, "drive", 11, Some(23)));
                    assert_eq!(data, payload);
                }
                _ => panic!("expected complete delayed write"),
            }
            assert!(recv.read_to_end(0).await.unwrap().is_empty());
            binary::write_result(&mut send, 13, Ok(IoResult::Write(payload.len()))).await.unwrap();
            send.finish().unwrap();
        });
        assert_eq!(result.unwrap(), payload.len());
        drop(call);
        assert!(delayed.connection.close_reason().is_none());
        drop(send);
        drop(recv);
        phases.push(("delayed_send_success", delta(&before), (1, 0, 0)));

        // The unread peer stream makes this actual send await pending.
        let (blocked, blocked_server, blocked_accepted) = pair_with_receive_window(Some(16)).await;
        blocked_accepted.set_max_concurrent_bi_streams(1u32.into());
        let before = storage::snapshot();
        let mut call = blocked.write(10, &request, &payload);
        let streams = tokio::select! {
            result = &mut call => panic!("constrained peer must keep send pending: {result:?}"),
            streams = blocked_accepted.accept_bi() => streams.unwrap(),
        };
        pending(&mut call).await;
        gauges.push(("during_send", storage::snapshot(), (0, 1, 1, 0)));
        drop(call);
        assert!(blocked.connection.close_reason().is_some());
        drop(streams);
        phases.push(("cancel_during_send", delta(&before), (1, 0, 0)));

        let (binary_stop, binary_stop_server, binary_stop_accepted) = pair_with_receive_window(Some(16)).await;
        binary_stop_accepted.set_max_concurrent_bi_streams(1u32.into());
        let before = storage::snapshot();
        let mut call = binary_stop.write(14, &request, &payload);
        let (send, mut recv) = tokio::select! {
            result = &mut call => panic!("binary send must remain held: {result:?}"),
            streams = binary_stop_accepted.accept_bi() => streams.unwrap(),
        };
        pending(&mut call).await;
        recv.stop(17u32.into()).unwrap();
        assert!(matches!(call.await, Err(ClientError::Protocol)));
        assert!(binary_stop.connection.close_reason().is_some());
        drop(send);
        drop(recv);
        phases.push(("binary_send_stop", delta(&before), (1, 0, 0)));

        let (control_stop, control_stop_server, control_stop_accepted) = pair_with_receive_window(Some(16)).await;
        control_stop_accepted.set_max_concurrent_bi_streams(1u32.into());
        let before = storage::snapshot();
        let mut call = control_stop.exchange(message());
        let (send, mut recv) = tokio::select! {
            result = &mut call => panic!("control send must remain held: {result:?}"),
            streams = control_stop_accepted.accept_bi() => streams.unwrap(),
        };
        pending(&mut call).await;
        recv.stop(17u32.into()).unwrap();
        assert!(matches!(call.await, Err(ClientError::Transport)));
        assert!(control_stop.connection.close_reason().is_some());
        drop(send);
        drop(recv);
        phases.push(("control_send_stop", delta(&before), (1, 0, 0)));

        // Existing typed-frame validation fails after acquiring a stream.
        let (invalid, invalid_server, invalid_accepted) = pair().await;
        invalid_accepted.set_max_concurrent_bi_streams(1u32.into());
        let oversized = vec![0u8; binary::MAX_IO_BYTES + 1];
        let before = storage::snapshot();
        assert!(matches!(invalid.write(11, &request, &oversized).await, Err(ClientError::Protocol)));
        assert!(invalid.connection.close_reason().is_some());
        phases.push(("send_validation_error", delta(&before), (1, 0, 0)));

        let (trailing, trailing_server, trailing_accepted) = pair().await;
        trailing_accepted.set_max_concurrent_bi_streams(1u32.into());
        let before = storage::snapshot();
        let (result, ()) = tokio::join!(trailing.write(12, &request, &payload), async {
            let (mut send, mut recv) = trailing_accepted.accept_bi().await.unwrap();
            let header = binary::Header::read(&mut recv).await.unwrap();
            match binary::read_body(&mut recv, header).await.unwrap() {
                Incoming::Write { request_id, data, .. } => {
                    assert_eq!(request_id, 12);
                    assert_eq!(data, payload);
                }
                _ => panic!("expected complete write before trailing response"),
            }
            assert!(recv.read_to_end(0).await.unwrap().is_empty());
            binary::write_result(&mut send, 12, Ok(IoResult::Write(payload.len()))).await.unwrap();
            send.write_all(&[0xff]).await.unwrap();
            send.finish().unwrap();
        });
        assert!(matches!(result, Err(ClientError::Protocol)));
        assert!(trailing.connection.close_reason().is_some());
        phases.push(("response_eof_error", delta(&before), (1, 0, 0)));

        transport.close();
        other.close();
        blocked.close();
        delayed.close();
        binary_stop.close();
        control_stop.close();
        delayed_server.close(0u32.into(), b"fixture shutdown");
        binary_stop_server.close(0u32.into(), b"fixture shutdown");
        control_stop_server.close(0u32.into(), b"fixture shutdown");
        invalid.close();
        trailing.close();
        blocked_server.close(0u32.into(), b"fixture shutdown");
        invalid_server.close(0u32.into(), b"fixture shutdown");
        trailing_server.close(0u32.into(), b"fixture shutdown");
        server.close(0u32.into(), b"fixture shutdown");
        other_server.close(0u32.into(), b"fixture shutdown");
        timeout(Duration::from_secs(5), async {
            tokio::join!(transport._endpoint.wait_idle(), other._endpoint.wait_idle(), server.wait_idle(), other_server.wait_idle(), blocked._endpoint.wait_idle(), blocked_server.wait_idle(), invalid._endpoint.wait_idle(), invalid_server.wait_idle(), trailing._endpoint.wait_idle(), trailing_server.wait_idle(), delayed._endpoint.wait_idle(), delayed_server.wait_idle(), binary_stop._endpoint.wait_idle(), binary_stop_server.wait_idle(), control_stop._endpoint.wait_idle(), control_stop_server.wait_idle());
        }).await.expect("bounded endpoint connection drain");
        drop(accepted);
        drop(other_accepted);
        drop(transport);
        drop(other);
        drop(server);
        drop(other_server);
        drop(blocked_accepted);
        drop(blocked);
        drop(blocked_server);
        drop(invalid_accepted);
        drop(invalid);
        drop(invalid_server);
        drop(trailing_accepted);
        drop(trailing);
        drop(trailing_server);
        drop(delayed_accepted);
        drop(delayed);
        drop(delayed_server);
        drop(binary_stop_accepted);
        drop(binary_stop);
        drop(binary_stop_server);
        drop(control_stop_accepted);
        drop(control_stop);
        drop(control_stop_server);
        println!("MOUNT_RS_CLIENT_STAGE behavior_oracles=complete full_binary_bytes=true pre_acquisition_reuse=true post_acquisition_close=true endpoints_drained=true");
        for (phase, snapshot, _) in &phases {
            if let Some(row) = snapshot.entries.iter().find(|row| row.name == LABEL) {
                println!("MOUNT_RS_CLIENT_STAGE phase={phase} available=true calls={} success={} error={} cancelled={} in_flight={} bytes={} histogram_total={}", row.calls, row.success, row.error, row.cancelled, row.in_flight, row.bytes, row.latency_log2_us.iter().sum::<u64>());
            } else {
                println!("MOUNT_RS_CLIENT_STAGE phase={phase} available=false");
            }
        }
        for (phase, snapshot, _) in &phases {
            for name in [SEND_LABEL, RECEIVE_LABEL] {
                if let Some(row) = snapshot.entries.iter().find(|row| row.name == name) {
                    println!("MOUNT_RS_CLIENT_IO_STAGE phase={phase} row={name} available=true calls={} success={} error={} cancelled={} in_flight={} bytes={} histogram_total={}", row.calls, row.success, row.error, row.cancelled, row.in_flight, row.bytes, row.latency_log2_us.iter().sum::<u64>());
                } else {
                    println!("MOUNT_RS_CLIENT_IO_STAGE phase={phase} row={name} available=false");
                }
            }
        }
        for (phase, snapshot, expected) in phases {
            let (send_expected, receive_expected) = io_outcomes(phase);
            for (name, expected) in [(SEND_LABEL, send_expected), (RECEIVE_LABEL, receive_expected)] {
                let row = snapshot.entries.iter().find(|row| row.name == name).expect("missing client I/O metric after complete behavior oracles");
                assert_eq!((row.success, row.error, row.cancelled), expected, "{phase} {name}");
                assert_eq!(row.calls, expected.0 + expected.1 + expected.2, "{phase} {name}");
                assert_eq!(row.latency_log2_us.iter().sum::<u64>(), row.calls, "{phase} {name}");
                assert_eq!((row.in_flight, snapshot.in_flight, row.bytes, row.returned_rows, row.returned_row_observations), (0, 0, 0, 0, 0), "{phase} {name}");
            }
            let row = snapshot.entries.iter().find(|row| row.name == LABEL).expect("missing client stream metric after complete behavior oracles");
            assert_eq!((row.success, row.error, row.cancelled), expected, "{phase}");
            assert_eq!(row.calls, expected.0 + expected.1 + expected.2, "{phase}");
            assert_eq!(row.latency_log2_us.iter().sum::<u64>(), row.calls, "{phase}");
            assert_eq!((row.in_flight, snapshot.in_flight, row.bytes, row.returned_rows, row.returned_row_observations), (0, 0, 0, 0, 0), "{phase}");
        }
        for (phase, snapshot, expected) in gauges {
            let row = snapshot.entries.iter().find(|row| row.name == LABEL).unwrap();
            let send = snapshot.entries.iter().find(|row| row.name == SEND_LABEL).unwrap();
            let receive = snapshot.entries.iter().find(|row| row.name == RECEIVE_LABEL).unwrap();
            assert_eq!((row.in_flight, snapshot.in_flight, send.in_flight, receive.in_flight), expected, "{phase}");
        }
    }).await.expect("bounded client stream acquisition qualification");
}
