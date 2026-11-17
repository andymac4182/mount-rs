#![cfg(feature = "http3")]
//! The peer protocol version rule, between a real owner and a real
//! forwarder over HTTP/3: version 1 with and without the marker is served,
//! and a version either side cannot speak is refused without running or
//! sending the request.
use bytes::Bytes;
use http::{Request, Response};
use http_body_util::BodyExt;
use mount_rs_cluster::transport::*;
use mount_rs_cluster::*;
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio_util::sync::CancellationToken;

struct Identity {
    cert: Certificate,
    key: KeyPair,
}
fn peer(ca: &Identity, name: &str) -> Identity {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["localhost".into()]).unwrap();
    params.subject_alt_names.push(SanType::URI(
        format!("urn:agent-tunnel:peer:{name}").try_into().unwrap(),
    ));
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    Identity {
        cert: params.signed_by(&key, &ca.cert, &ca.key).unwrap(),
        key,
    }
}

fn public_request(method: &str, volume: &str, data: &[u8]) -> Request<mount_rs_http::HttpBody> {
    Request::builder()
        .method(method)
        .uri(format!("/v1/drives/{volume}/fs/data"))
        .header("authorization", "Bearer secret-token")
        .header("content-length", data.len())
        .body(
            http_body_util::Full::new(Bytes::copy_from_slice(data))
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap()
}

/// An observer and the refusals it was told of.
fn observer() -> (PeerProtocolObserver, UnboundedReceiver<PeerProtocolRefusal>) {
    let (send, receive) = unbounded_channel();
    (
        Arc::new(move |refusal: &PeerProtocolRefusal| {
            let _ = send.send(refusal.clone());
        }),
        receive,
    )
}

#[tokio::test]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn a_peer_protocol_version_neither_side_can_speak_is_refused_and_never_run() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let namespace = format!(
            "protocol-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let filesystem = Arc::new(
            mount_rs_sdk::Filesystem::split(mount_rs_sdk::SplitOptions::memory("owner", 4096))
                .await
                .unwrap(),
        );
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca = Identity {
            cert: params.self_signed(&key).unwrap(),
            key,
        };
        let owner = peer(&ca, "owner");
        let ingress = peer(&ca, "ingress");
        let owner_pin = spki_sha256_from_der(owner.cert.der()).unwrap();
        let ingress_pin = spki_sha256_from_der(ingress.cert.der()).unwrap();
        let endpoint = quinn::Endpoint::server(
            load_peer_server_config_from_pem(
                owner.cert.pem().as_bytes(),
                owner.key.serialize_pem().as_bytes(),
                ca.cert.pem().as_bytes(),
            )
            .unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        )
        .unwrap();
        let address = endpoint.local_addr().unwrap();
        let route = VolumeRoute {
            volume: "volume".into(),
            node: "owner".into(),
            boot: "boot".into(),
            registration: "registration".into(),
            fence: filesystem.checked_owner_fence().await.unwrap(),
            address,
            tls_server_name: "localhost".into(),
            protocols: PeerProtocolRange::SUPPORTED,
        };
        let redis = redis::Client::open(std::env::var("MOUNT_RS_REDIS_TEST_URL").unwrap()).unwrap();
        let registry = RedisRouteRegistry::new(
            redis.get_multiplexed_async_connection().await.unwrap(),
            namespace,
            Duration::from_secs(2),
        )
        .unwrap();
        let registration = OwnedVolumeRegistration::new(
            filesystem.clone(),
            registry.clone(),
            route.clone(),
            Duration::from_secs(20),
        )
        .await
        .unwrap();
        registration.advertise().await.unwrap();
        let handler = OwnerHttpHandler::new(
            filesystem.clone(),
            route.clone(),
            "secret-token".into(),
            mount_rs_http::HttpServerOptions::default(),
        )
        .await
        .unwrap();
        let (owner_observer, mut owner_refusals) = observer();
        let router = OwnerHttpRouter::new(vec![handler])
            .unwrap()
            .with_peer_protocol_observer(owner_observer);

        // The same listener also stands in for an owner of a later release:
        // for every other volume it records the version it was sent and
        // answers as an owner that does not speak it.
        let (sent_versions, mut received_versions) = unbounded_channel::<Option<String>>();
        let peer_handler =
            move |identity: TlsIdentity, request: Request<()>, mut stream: PeerServerStream| {
                let router = router.clone();
                let sent_versions = sent_versions.clone();
                async move {
                    if request.uri().path().starts_with("/v1/drives/volume/") {
                        return router.handle(identity, request, stream).await;
                    }
                    let version = request
                        .headers()
                        .get(PEER_PROTOCOL_HEADER)
                        .map(|value| value.to_str().unwrap().to_owned());
                    let _ = sent_versions.send(version);
                    stream
                        .send_response(
                            Response::builder()
                                .status(426)
                                .header("x-mount-dispatch", "not-dispatched")
                                .body(())
                                .unwrap(),
                        )
                        .await?;
                    stream.finish().await
                }
            };
        let server = PeerServer::new(
            endpoint,
            ApprovedPeerPins::new([ingress_pin]).unwrap(),
            PeerTransportLimits::default(),
            |_: &TlsIdentity, _: &Request<()>| true,
            peer_handler,
        )
        .unwrap();
        let cancel = CancellationToken::new();
        let server_task = tokio::spawn(server.serve(cancel.clone()));

        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(
            load_peer_client_config_from_pem(
                ingress.cert.pem().as_bytes(),
                ingress.key.serialize_pem().as_bytes(),
                ca.cert.pem().as_bytes(),
            )
            .unwrap(),
        );
        let client = Arc::new(
            PeerClient::new(
                endpoint,
                ApprovedPeerPins::new([owner_pin]).unwrap(),
                PeerTransportLimits::default(),
            )
            .unwrap(),
        );
        let peers = Arc::new(
            PeerDirectory::new(vec![TrustedPeer {
                node: "owner".into(),
                boot: "boot".into(),
                address,
                tls_server_name: "localhost".into(),
                pin: owner_pin,
            }])
            .unwrap(),
        );
        let (ingress_observer, mut ingress_refusals) = observer();
        let forwarder = IngressForwarder::new(
            registry.clone(),
            peers,
            client.clone(),
            ForwardOptions {
                operation_timeout: Duration::from_secs(5),
                ..Default::default()
            },
        )
        .unwrap()
        .with_peer_protocol_observer(ingress_observer);

        // This build's forwarder and owner agree, and nothing is reported.
        let data = b"written in version 1";
        let response = forwarder
            .forward(public_request("PUT", "volume", data))
            .await
            .unwrap();
        assert!(response.status().is_success());
        response.into_body().collect().await.unwrap();

        // A forwarded request as the owner's peer sends it, in `version`
        // (`None`: a release from before the marker, which names none).
        let destination = PeerDestination::new(address, "localhost");
        let raw = |method: &'static str, versions: &[&str], body: &'static [u8]| {
            let mut request = Request::builder()
                .method(method)
                .uri("https://localhost/v1/drives/volume/fs/data")
                .header("authorization", "Bearer secret-token")
                .header(ROUTE_HEADER, serde_json::to_string(&route).unwrap())
                .header(HOP_HEADER, "1");
            for version in versions {
                request = request.header(PEER_PROTOCOL_HEADER, *version);
            }
            let request = request.body(()).unwrap();
            let (client, destination) = (client.clone(), destination.clone());
            async move {
                let mut stream = client.open(destination, request).await.unwrap();
                if !body.is_empty() {
                    stream.send_chunk(Bytes::from_static(body)).await.unwrap();
                }
                stream.finish().await.unwrap();
                let response = stream.recv_response().await.unwrap();
                let mut received = Vec::new();
                while let Some(chunk) = stream.recv_chunk().await.unwrap() {
                    received.extend_from_slice(chunk.as_bytes());
                }
                (response, received)
            }
        };
        for accepted in [&[][..], &["1"]] {
            let (response, body) = raw("GET", accepted, b"").await;
            assert_eq!(response.status(), 200, "version {accepted:?}");
            assert_eq!(body, data);
        }
        assert!(owner_refusals.try_recv().is_err());

        // A version the owner does not speak, or cannot read, is refused
        // before the request is run: the write does not happen.
        let next = (PEER_PROTOCOL_VERSION + 1).to_string();
        let next: &'static str = next.leak();
        for (refused, reported) in [
            (&[next][..], next),
            (&["0"], "0"),
            (&["latest"], "latest"),
            (&["1", "1"], "1 (repeated)"),
        ] {
            let (response, body) = raw("PUT", refused, b"must not be written").await;
            assert_eq!(response.status(), 426, "version {refused:?}");
            assert_eq!(response.headers()["x-mount-dispatch"], "not-dispatched");
            assert_eq!(
                response.headers()[PEER_PROTOCOL_HEADER],
                PeerProtocolRange::SUPPORTED.to_string().as_str()
            );
            assert!(body.is_empty());
            assert_eq!(
                owner_refusals.try_recv().unwrap(),
                PeerProtocolRefusal::Request {
                    peer: "ingress".into(),
                    offered: reported.into()
                }
            );
        }
        let (response, body) = raw("GET", &[], b"").await;
        assert_eq!(response.status(), 200);
        assert_eq!(body, data, "a refused request was run");

        // What a public client sees when the owner refuses the forwarder's
        // version, and what the forwarder reports. The stand-in owner is
        // published under a version 1 record, so the request is sent.
        let published = |volume: &str, protocols: PeerProtocolRange| VolumeRoute {
            volume: volume.into(),
            registration: format!("registration-{volume}"),
            fence: 1,
            protocols,
            ..route.clone()
        };
        let refuser = published("refuser", PeerProtocolRange::V1);
        assert!(
            registry
                .advertise(&refuser, Duration::from_secs(20))
                .await
                .unwrap()
        );
        let unavailable = |response: Response<mount_rs_http::HttpBody>| {
            assert_eq!(response.status(), 503);
            assert_eq!(response.headers()["x-mount-dispatch"], "not-dispatched");
            assert_eq!(response.headers()["retry-after"], "1");
            assert!(!response.headers().contains_key(PEER_PROTOCOL_HEADER));
        };
        let serve = |volume: &'static str| {
            mount_rs_http::HttpRequestHandler::handle(
                &forwarder,
                public_request("PUT", volume, b"data"),
            )
        };
        unavailable(serve("refuser").await);
        assert_eq!(
            received_versions.try_recv().unwrap().as_deref(),
            Some(PEER_PROTOCOL_VERSION.to_string().as_str()),
            "the forwarder did not name its version"
        );
        assert_eq!(
            ingress_refusals.try_recv().unwrap(),
            PeerProtocolRefusal::Refused {
                volume: "refuser".into(),
                owner: "owner".into()
            }
        );

        // An owner whose record accepts only later versions is sent nothing.
        let later = PeerProtocolRange {
            min: PEER_PROTOCOL_VERSION + 1,
            max: PEER_PROTOCOL_VERSION + 2,
        };
        assert!(
            registry
                .advertise(&published("future", later), Duration::from_secs(20))
                .await
                .unwrap()
        );
        assert_eq!(
            forwarder
                .forward(public_request("PUT", "future", b"data"))
                .await
                .err(),
            Some(ForwardError::IncompatiblePeer)
        );
        unavailable(serve("future").await);
        assert!(received_versions.try_recv().is_err(), "a request was sent");
        for _ in 0..2 {
            assert_eq!(
                ingress_refusals.try_recv().unwrap(),
                PeerProtocolRefusal::Route {
                    volume: "future".into(),
                    owner: "owner".into(),
                    accepts: later
                }
            );
        }
        assert_eq!(forwarder.diagnostics().peer_protocol_refused, 3);

        // An owner of a later release that still accepts this version is
        // sent this version: the forwarder speaks the highest both do.
        let both = PeerProtocolRange {
            min: MIN_PEER_PROTOCOL_VERSION,
            max: PEER_PROTOCOL_VERSION + 1,
        };
        assert!(
            registry
                .advertise(&published("both", both), Duration::from_secs(20))
                .await
                .unwrap()
        );
        unavailable(serve("both").await);
        assert_eq!(
            received_versions.try_recv().unwrap().as_deref(),
            Some(PEER_PROTOCOL_VERSION.to_string().as_str())
        );

        // The first drive was served throughout.
        let response = forwarder
            .forward(public_request("GET", "volume", &[]))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            &data[..]
        );
        assert!(owner_refusals.try_recv().is_err());

        for volume in ["refuser", "future", "both"] {
            let protocols = match volume {
                "future" => later,
                "both" => both,
                _ => PeerProtocolRange::V1,
            };
            registry
                .remove(&published(volume, protocols))
                .await
                .unwrap();
        }
        registration.withdraw().await.unwrap();
        client.shutdown().await.unwrap();
        cancel.cancel();
        server_task.await.unwrap().unwrap();
        filesystem.shutdown().await.unwrap();
    })
    .await
    .expect("peer protocol test timed out");
}
