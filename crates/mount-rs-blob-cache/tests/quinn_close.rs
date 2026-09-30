//! First-packet CLOSE must use the same natural draining lifecycle as later packets.

#[cfg(test)]
mod tests {
    use bytes::{Bytes, BytesMut};
    use quinn_proto::{
        ClientConfig, Connection, ConnectionError, ConnectionHandle, DatagramEvent, Endpoint,
        EndpointConfig, Event, ServerConfig, TransportConfig, TransportErrorCode, VarInt,
    };
    use std::{
        net::SocketAddr,
        sync::Arc,
        time::{Duration, Instant},
    };

    const INITIAL_RTT: Duration = Duration::from_millis(333);
    const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
    const CLOSE_GRACE: Duration = Duration::from_secs(3);

    fn configs() -> (ServerConfig, ClientConfig) {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let certificate = cert.der().clone();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(signing_key.serialize_der().into());
        let mut server_config =
            ServerConfig::with_single_cert(vec![certificate.clone()], key).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(certificate).unwrap();
        let mut client_config = ClientConfig::with_root_certificates(Arc::new(roots)).unwrap();
        client_config.version(1);
        let mut transport = TransportConfig::default();
        transport.initial_rtt(INITIAL_RTT);
        transport.max_idle_timeout(Some(IDLE_TIMEOUT.try_into().unwrap()));
        let transport = Arc::new(transport);
        server_config.transport_config(transport.clone());
        client_config.transport_config(transport);
        (server_config, client_config)
    }

    fn endpoints() -> (Endpoint, Endpoint, ClientConfig) {
        let (server_config, client_config) = configs();
        let mut endpoint_config = EndpointConfig::default();
        endpoint_config.grease_quic_bit(false);
        let endpoint_config = Arc::new(endpoint_config);
        (
            Endpoint::new(endpoint_config.clone(), None, false, None),
            Endpoint::new(endpoint_config, Some(Arc::new(server_config)), false, None),
            client_config,
        )
    }

    fn initial_dcid(packet: &[u8]) -> &[u8] {
        assert!(packet.len() >= 1200, "client Initial must be padded");
        assert_eq!(packet[0] & 0xf0, 0xc0, "QUIC v1 Initial long header");
        assert_eq!(&packet[1..5], &1u32.to_be_bytes());
        let len = usize::from(packet[5]);
        assert!((8..=20).contains(&len));
        assert!(packet.len() >= 6 + len);
        &packet[6..6 + len]
    }

    #[derive(Default)]
    struct Observation {
        natural_drains: usize,
        connection_lost: usize,
        connected: usize,
        other_events: usize,
    }

    // Follow public poll order, forwarding actual endpoint events. Outbound server
    // datagrams are deliberately discarded, so the client never confirms a handshake.
    fn drive_without_delivery(
        endpoint: &mut Endpoint,
        handle: ConnectionHandle,
        connection: &mut Connection,
        now: Instant,
        observation: &mut Observation,
    ) -> Option<Instant> {
        let mut transmit_buffer = Vec::with_capacity(2048);
        let mut transmit_polls = 0;
        while connection
            .poll_transmit(now, 1, &mut transmit_buffer)
            .is_some()
        {
            transmit_polls += 1;
            assert!(transmit_polls <= 64, "bounded protocol-only transmit loop");
            transmit_buffer.clear();
        }
        let next_timeout = connection.poll_timeout();
        let mut endpoint_polls = 0;
        while let Some(event) = connection.poll_endpoint_events() {
            endpoint_polls += 1;
            assert!(endpoint_polls <= 64, "bounded endpoint event loop");
            observation.natural_drains += usize::from(event.is_drained());
            if let Some(feedback) = endpoint.handle_event(handle, event) {
                connection.handle_event(feedback);
            }
        }
        let mut application_polls = 0;
        while let Some(event) = connection.poll() {
            application_polls += 1;
            assert!(application_polls <= 64, "bounded application event loop");
            match event {
                Event::ConnectionLost { reason } => {
                    observation.connection_lost += 1;
                    assert!(
                        matches!(reason, ConnectionError::ConnectionClosed(ref close)
                        if close.error_code == TransportErrorCode::APPLICATION_ERROR)
                    );
                }
                Event::Connected => observation.connected += 1,
                _ => observation.other_events += 1,
            }
        }
        next_timeout
    }

    fn run_case(deliver_original_initial: bool) {
        let (mut client_endpoint, mut server_endpoint, client_config) = endpoints();
        // Logical addresses only: this test binds no UDP socket.
        let client_addr: SocketAddr = "127.0.0.1:40001".parse().unwrap();
        let server_addr: SocketAddr = "127.0.0.1:40002".parse().unwrap();
        let start = Instant::now();
        let (_, mut outgoing) = client_endpoint
            .connect(start, client_config, server_addr, "localhost")
            .unwrap();
        let mut packet = Vec::with_capacity(2048);
        let first_tx = outgoing
            .poll_transmit(start, 1, &mut packet)
            .expect("ClientHello Initial");
        assert_eq!(first_tx.destination, server_addr);
        let original_initial = packet[..first_tx.size].to_vec();
        let old_dcid = initial_dcid(&original_initial).to_vec();
        let mut response = Vec::with_capacity(2048);
        let mut observation = Observation::default();
        let mut accepted_before_close = if deliver_original_initial {
            let incoming = match server_endpoint.handle(
                start,
                client_addr,
                None,
                first_tx.ecn,
                BytesMut::from(original_initial.as_slice()),
                &mut response,
            ) {
                Some(DatagramEvent::NewConnection(incoming)) => incoming,
                _ => panic!("ordinary control must accept the original Initial first"),
            };
            response.clear();
            let (handle, mut connection) = server_endpoint
                .accept(incoming, start, &mut response, None)
                .unwrap();
            assert!(connection.is_handshaking());
            assert!(!connection.is_closed());
            assert!(connection.stats().frame_rx.crypto > 0);
            let _ = drive_without_delivery(
                &mut server_endpoint,
                handle,
                &mut connection,
                start,
                &mut observation,
            );
            assert_eq!(observation.connection_lost, 0);
            Some((handle, connection))
        } else {
            // Discard ClientHello, exactly as the owned UDP blackhole does.
            None
        };
        let closed_at = start + Duration::from_millis(1);
        outgoing.close(closed_at, VarInt::from_u32(0), Bytes::new());
        packet.clear();
        let close_tx = outgoing
            .poll_transmit(closed_at, 1, &mut packet)
            .expect("Initial CLOSE");
        assert_eq!(close_tx.destination, server_addr);
        assert_eq!(initial_dcid(&packet[..close_tx.size]), old_dcid.as_slice());
        response.clear();
        let event = server_endpoint.handle(
            closed_at,
            client_addr,
            None,
            close_tx.ecn,
            BytesMut::from(&packet[..close_tx.size]),
            &mut response,
        );
        let (server_handle, mut accepted) = if deliver_original_initial {
            let (handle, mut connection) = accepted_before_close.take().unwrap();
            match event {
                Some(DatagramEvent::ConnectionEvent(event_handle, event)) => {
                    assert_eq!(event_handle, handle);
                    connection.handle_event(event);
                }
                _ => panic!("ordinary control must route CLOSE to the existing connection"),
            }
            (handle, connection)
        } else {
            let incoming = match event {
                Some(DatagramEvent::NewConnection(incoming)) => incoming,
                _ => panic!("fresh endpoint must receive CLOSE as its first Initial"),
            };
            response.clear();
            let accepted = server_endpoint
                .accept(incoming, closed_at, &mut response, None)
                .unwrap();
            assert_eq!(accepted.1.stats().frame_rx.crypto, 0);
            accepted
        };
        assert!(accepted.is_closed());
        assert!(!accepted.is_handshaking());
        assert!(!accepted.is_drained());
        assert_eq!(accepted.stats().path.rtt, INITIAL_RTT);
        assert_eq!(accepted.stats().frame_rx.connection_close, 1);
        assert_eq!(server_endpoint.open_connections(), 1);
        // Reproduce the later endpoint-shutdown close as well. It must not strand
        // an already-Draining connection or postpone the natural grace period.
        accepted.close(closed_at, VarInt::from_u32(0), Bytes::new());
        let first_deadline = drive_without_delivery(
            &mut server_endpoint,
            server_handle,
            &mut accepted,
            closed_at,
            &mut observation,
        );
        assert_eq!(observation.connection_lost, 1);
        assert_eq!(observation.connected, 0);
        assert_eq!(observation.natural_drains, 0);
        // Close-first has Initial keys: 333ms RTT -> 999ms PTO -> 2.997s grace.
        // The ordinary control may already have Data keys, adding peer ACK delay
        // to PTO. Use that control's actual natural close deadline, bounded well
        // before idle, instead of assuming both spaces share the same grace.
        let after_close_grace = if deliver_original_initial {
            let deadline = first_deadline.expect("ordinary close must arm its natural timer");
            assert!(deadline > closed_at);
            assert!(deadline < closed_at + Duration::from_secs(4));
            deadline
        } else {
            closed_at + CLOSE_GRACE
        };
        accepted.handle_timeout(after_close_grace);
        let _ = drive_without_delivery(
            &mut server_endpoint,
            server_handle,
            &mut accepted,
            after_close_grace,
            &mut observation,
        );
        eprintln!(
            "quinn_first_close original_delivered={} first_timeout_us={:?} drained={} natural_events={} endpoint_open={} loss_events={} connected={}",
            deliver_original_initial,
            first_deadline.map(|t| t.saturating_duration_since(closed_at).as_micros()),
            accepted.is_drained(),
            observation.natural_drains,
            server_endpoint.open_connections(),
            observation.connection_lost,
            observation.connected,
        );
        assert!(
            accepted.is_drained(),
            "natural 3-PTO drain must precede the 30s idle timeout"
        );
        assert_eq!(
            observation.natural_drains, 1,
            "only the connection may produce its drained event"
        );
        assert_eq!(
            server_endpoint.open_connections(),
            0,
            "endpoint must remove its connection record"
        );
        assert_eq!(observation.connection_lost, 1);
        assert_eq!(observation.connected, 0);
    }

    #[test]
    fn first_close_initial_drains_without_waiting_for_idle_timeout() {
        run_case(false);
    }

    #[test]
    fn ordinary_initial_then_close_drains_without_waiting_for_idle_timeout() {
        run_case(true);
    }

    #[tokio::test]
    #[ignore = "isolated owned process: real UDP and bounded endpoint shutdown"]
    async fn first_close_initial_releases_real_endpoint_within_close_grace() {
        let (server_config, client_config) = configs();
        let endpoint =
            quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let server_addr = endpoint.local_addr().unwrap();
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut endpoint_config = EndpointConfig::default();
        endpoint_config.grease_quic_bit(false);
        let mut client_endpoint = Endpoint::new(Arc::new(endpoint_config), None, false, None);
        let start = Instant::now();
        let (_, mut outgoing) = client_endpoint
            .connect(start, client_config, server_addr, "localhost")
            .unwrap();
        let mut packet = Vec::with_capacity(2048);
        let hello = outgoing
            .poll_transmit(start, 1, &mut packet)
            .expect("ClientHello Initial");
        let original_dcid = initial_dcid(&packet[..hello.size]).to_vec();
        // The original Initial is consumed locally, never sent. Only actual
        // Quinn-generated encrypted CLOSE bytes reach the fresh real server.
        let closed_at = start + Duration::from_millis(1);
        outgoing.close(closed_at, VarInt::from_u32(0), Bytes::new());
        packet.clear();
        let close = outgoing
            .poll_transmit(closed_at, 1, &mut packet)
            .expect("Initial CLOSE");
        assert_eq!(initial_dcid(&packet[..close.size]), original_dcid);
        assert_eq!(close.destination, server_addr);
        assert_eq!(
            socket
                .send_to(&packet[..close.size], server_addr)
                .await
                .unwrap(),
            close.size
        );
        let incoming = tokio::time::timeout(Duration::from_secs(3), endpoint.accept())
            .await
            .expect("bounded first CLOSE incoming")
            .expect("fresh server accepts first Initial");
        let error = tokio::time::timeout(Duration::from_secs(3), incoming)
            .await
            .expect("bounded first CLOSE loss")
            .expect_err("CLOSE must not authenticate a connection");
        assert!(matches!(error, ConnectionError::ConnectionClosed(ref close)
            if close.error_code == TransportErrorCode::APPLICATION_ERROR));
        assert_eq!(
            endpoint.open_connections(),
            1,
            "accepted closed record must be observed before drain"
        );
        endpoint.close(VarInt::from_u32(0), b"fixture shutdown");
        tokio::time::timeout(Duration::from_secs(4), endpoint.wait_idle())
            .await
            .expect("first CLOSE must drain within natural grace, before 30s idle");
        assert_eq!(endpoint.open_connections(), 0);
        println!(
            "quinn_first_close_runtime first_packet_close=true loss_application_error=true endpoint_open=0 natural_idle_observed=true"
        );
        drop(socket);
        drop(endpoint);
    }
}
