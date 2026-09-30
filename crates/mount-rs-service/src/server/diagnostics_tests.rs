use super::*;

#[test]
fn auth_stage_scope_helpers_allocate_no_heap_with_preconstructed_observer() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let observer = ServerDiagnostics::new(1, false);
    for enabled in [false, true] {
        let observer = enabled.then_some(&observer);
        let allocations = crate::dispatch::allocation_tests::count(|| {
            let mut future = std::pin::pin!(auth_scope(observer, async {
                for stage in [
                    AuthStage::Decode,
                    AuthStage::Catalog,
                    AuthStage::CacheWait,
                    AuthStage::PolicySelect,
                    AuthStage::KeyFetch,
                    AuthStage::JwtVerify,
                    AuthStage::GrantAuthorize,
                ] {
                    let mut span = auth_span(stage);
                    let observed = enabled && cfg!(feature = "io-profiling");
                    assert_eq!(span.recorder.is_some(), observed);
                    assert_eq!(span.started.is_some(), observed);
                    span.finish(Outcome::Success);
                }
            }));
            assert!(matches!(
                future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop())),
                Poll::Ready(())
            ));
        });
        assert_eq!(allocations, (0, 0), "helper/scope instrumentation only");
    }
    let snapshot = observer.snapshot();
    for entry in &snapshot.entries[14..21] {
        assert_eq!(entry.calls, u64::from(cfg!(feature = "io-profiling")));
    }
}

#[test]
fn websocket_inventory_and_application_snapshot_are_transport_specific() {
    assert_eq!(
        NAMES,
        [
            "admission.connection",
            "handshake.application",
            "handshake.tls",
            "auth.authenticate",
            "request.application",
            "request.read_incoming",
            "admission.ingress",
            "admission.egress",
            "dispatch.control",
            "dispatch.read",
            "dispatch.write",
            "response.encode",
            "response.submit",
            "session.cleanup",
            "auth.decode",
            "auth.catalog",
            "auth.cache.wait",
            "auth.policy.select",
            "auth.key.fetch",
            "auth.jwt.verify",
            "auth.grant.authorize",
            "handshake.websocket_upgrade",
        ]
    );
    assert_eq!(Operation::AuthDecode as usize, 14);
    assert_eq!(Operation::AuthGrantAuthorize as usize, 20);
    assert_eq!(Operation::WebSocketUpgrade as usize, 21);
    assert_eq!(NAMES[21], "handshake.websocket_upgrade");
    let observer = WebSocketDiagnostics::new(false);
    let snapshot = observer.snapshot();
    assert_eq!(snapshot.schema, "mount-rs.service-websocket.v1");
    assert!(snapshot.complete && snapshot.application_quiescent);
    assert_eq!(snapshot.entries.len(), 22);
    assert_eq!(snapshot.known_unavailable.len(), 6);
    let value = serde_json::to_value(snapshot).unwrap();
    for field in [
        "transport",
        "registry_snapshot_elapsed_ns",
        "udp_bytes",
        "frame_counts",
    ] {
        assert!(value.get(field).is_none());
    }
    assert!(!serde_json::to_string(&value).unwrap().contains("quic"));
}

#[test]
fn websocket_span_and_auth_scope_helpers_add_no_heap_allocations() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let observer = WebSocketDiagnostics::new(false);
    for enabled in [false, true] {
        let inner = enabled.then(|| observer.application_observer());
        let allocations = crate::dispatch::allocation_tests::count(|| {
            let mut upgrade = Span::new(inner, Operation::WebSocketUpgrade);
            assert_eq!(upgrade.recorder.is_some(), enabled);
            assert_eq!(upgrade.started.is_some(), enabled);
            upgrade.finish(Outcome::Success);
            let mut future = std::pin::pin!(auth_scope(inner, async {
                auth_span(AuthStage::Decode).finish(Outcome::Success);
            }));
            assert!(matches!(
                future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop())),
                Poll::Ready(())
            ));
        });
        assert_eq!(allocations, (0, 0), "preconstructed observer helpers only");
    }
    let snapshot = observer.snapshot();
    assert_eq!(snapshot.entries[21].success, 1);
    assert_eq!(
        snapshot.entries[14].success,
        u64::from(cfg!(feature = "io-profiling"))
    );
    assert!(snapshot.application_quiescent);
}

#[cfg(feature = "io-profiling")]
#[tokio::test]
async fn websocket_authentication_helper_scopes_catalog_auth_for_hello_and_renewal() {
    use auth_stage_fixture::*;
    for deadline in [Some(std::time::Duration::from_secs(30)), None] {
        let (auth, keys, token) = fixture(false);
        let observer = WebSocketDiagnostics::new(false);
        assert!(
            crate::websocket::authenticate(
                &auth,
                &token,
                "secret-partition",
                Some(observer.application_observer()),
                deadline,
            )
            .await
            .is_err()
        );
        let snapshot = observer.snapshot();
        for name in [
            "auth.decode",
            "auth.catalog",
            "auth.cache.wait",
            "auth.policy.select",
        ] {
            assert_eq!(
                snapshot
                    .entries
                    .iter()
                    .find(|entry| entry.name == name)
                    .unwrap()
                    .success,
                1
            );
        }
        assert_eq!(keys.calls.load(Ordering::SeqCst), 1);
        for name in ["auth.authenticate", "auth.key.fetch"] {
            assert_eq!(
                snapshot
                    .entries
                    .iter()
                    .find(|entry| entry.name == name)
                    .unwrap()
                    .error,
                1
            );
        }
        assert!(snapshot.complete && snapshot.application_quiescent);
        let json = serde_json::to_string(&snapshot).unwrap();
        for secret in [
            "secret-partition",
            "secret-issuer",
            "unverified-signature",
            "secret-key-source-error",
        ] {
            assert!(!json.contains(secret));
        }
    }
}

#[cfg(feature = "io-profiling")]
#[tokio::test]
async fn websocket_signed_catalog_hello_and_renewal_record_verification_and_grant_stages() {
    use auth_stage_fixture::*;
    let (auth, keys, token, dispatcher) = signed_fixture();
    let observer = WebSocketDiagnostics::new(false);
    let hello = crate::websocket::authenticate(
        &auth,
        &token,
        "secret-partition",
        Some(observer.application_observer()),
        Some(std::time::Duration::from_secs(30)),
    )
    .await
    .unwrap_or_else(|_| panic!("signed hello authentication denied"));
    let first = observer.snapshot();
    for name in [
        "auth.authenticate",
        "auth.jwt.verify",
        "auth.grant.authorize",
    ] {
        assert_eq!(
            first
                .entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap()
                .success,
            1
        );
    }
    let renewed = crate::websocket::authenticate(
        &auth,
        &token,
        "secret-partition",
        Some(observer.application_observer()),
        None,
    )
    .await
    .unwrap_or_else(|_| panic!("signed renewal authentication denied"));
    assert_eq!(hello.partition_id, renewed.partition_id);
    assert_eq!(hello.policy_id, renewed.policy_id);
    assert_eq!(hello.issuer, renewed.issuer);
    assert_eq!(hello.subject, renewed.subject);
    assert_eq!(hello.signing_algorithm, renewed.signing_algorithm);
    assert_eq!(hello.claims, renewed.claims);
    assert_eq!(hello.expires_at, renewed.expires_at);
    assert!(dispatcher.renewal_matches(&hello, &renewed).await);
    let after = observer.snapshot();
    for name in [
        "auth.authenticate",
        "auth.decode",
        "auth.catalog",
        "auth.cache.wait",
        "auth.policy.select",
        "auth.jwt.verify",
        "auth.grant.authorize",
    ] {
        let entry = after
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap();
        assert_eq!(
            (
                entry.calls,
                entry.success,
                entry.error,
                entry.timeout,
                entry.cancelled
            ),
            (2, 2, 0, 0, 0),
            "{name}"
        );
    }
    assert_eq!(
        keys.calls.load(Ordering::SeqCst),
        1,
        "renewal uses the cached signing key"
    );
    assert_eq!(after.entries[Operation::AuthKeyFetch as usize].success, 1);
    settled(&observer.application_observer().snapshot());
    let serialized = serde_json::to_string(&after).unwrap();
    assert!(!serialized.contains(&token));
    assert!(!serialized.contains("secret-signed-kid"));
    assert!(auth_span(AuthStage::JwtVerify).recorder.is_none());
}

#[cfg(feature = "io-profiling")]
#[tokio::test(start_paused = true)]
async fn websocket_hello_auth_timeout_cancels_catalog_stage_without_changing_deadline() {
    use auth_stage_fixture::*;
    let (auth, keys, token) = fixture(true);
    let observer = WebSocketDiagnostics::new(false);
    {
        let mut future = std::pin::pin!(crate::websocket::authenticate(
            &auth,
            &token,
            "secret-partition",
            Some(observer.application_observer()),
            Some(std::time::Duration::from_secs(30)),
        ));
        pending(future.as_mut());
        let live = observer.snapshot();
        assert_eq!(keys.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            live.entries[Operation::Authentication as usize].in_flight,
            1
        );
        assert_eq!(live.entries[Operation::AuthKeyFetch as usize].in_flight, 1);
        tokio::time::advance(std::time::Duration::from_secs(31)).await;
        assert!(future.await.is_err());
    }
    let after = observer.snapshot();
    assert_eq!(after.entries[Operation::Authentication as usize].timeout, 1);
    assert_eq!(after.entries[Operation::AuthKeyFetch as usize].cancelled, 1);
    assert!(after.complete && after.application_quiescent);
    assert!(auth_span(AuthStage::Decode).recorder.is_none());
}

#[cfg(feature = "io-profiling")]
mod auth_stage_fixture {
    use super::*;
    use crate::{
        auth::{AuthError, CatalogAuthenticator, Jwk, OidcKeySource, OidcVerifier},
        catalog::{
            CatalogError, CatalogSnapshot, CatalogStore, DriveDefinition, GrantDefinition,
            PartitionDefinition, Permission,
        },
        server::Authenticator,
    };
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::{
        collections::BTreeMap,
        future::Future,
        task::{Context, Poll, Waker},
    };

    struct CurrentCatalog(Arc<CatalogSnapshot>);
    #[async_trait::async_trait]
    impl CatalogStore for CurrentCatalog {
        async fn load_current(&self) -> Result<CatalogSnapshot, CatalogError> {
            panic!("authentication must load the shared current catalog")
        }
        async fn load_shared_current(&self) -> Result<Arc<CatalogSnapshot>, CatalogError> {
            Ok(self.0.clone())
        }
        async fn compare_and_swap(&self, _: u64, _: CatalogSnapshot) -> Result<u64, CatalogError> {
            Err(CatalogError::Conflict)
        }
    }
    pub(super) struct Keys {
        hold: bool,
        pub(super) calls: AtomicU64,
    }
    #[async_trait::async_trait]
    impl OidcKeySource for Keys {
        async fn fetch(&self, _: &str, _: &[String]) -> Result<OidcVerifier, AuthError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.hold {
                std::future::pending().await
            } else {
                OidcVerifier::from_jwks_json(
                    "https://secret-issuer.example",
                    &["secret-audience".into()],
                    "secret-key-source-error",
                )
            }
        }
    }
    fn authority() -> CatalogSnapshot {
        let mut snapshot = CatalogSnapshot::empty();
        snapshot.partitions.insert(
            "secret-partition".into(),
            PartitionDefinition {
                drives: BTreeMap::from([(
                    "secret-drive".into(),
                    DriveDefinition {
                        driver: serde_json::json!({"kind":"memory"}),
                    },
                )]),
            },
        );
        snapshot.issuer_policies.insert("secret-policy".into(), serde_json::json!({"issuer":"https://secret-issuer.example","audiences":["secret-audience"],"algorithms":["ES256"]}));
        snapshot.grants.insert(
            "secret-grant".into(),
            GrantDefinition {
                partition_id: "secret-partition".into(),
                policy_id: "secret-policy".into(),
                drives: BTreeMap::from([("secret-drive".into(), Permission::Read)]),
                claim_conditions: BTreeMap::from([("/sub".into(), "secret-subject".into())]),
            },
        );
        snapshot
    }
    pub(super) fn fixture(hold: bool) -> (CatalogAuthenticator, Arc<Keys>, String) {
        let keys = Arc::new(Keys {
            hold,
            calls: AtomicU64::new(0),
        });
        let auth = CatalogAuthenticator::with_key_source(
            Arc::new(CurrentCatalog(Arc::new(authority()))),
            keys.clone(),
        );
        // This exercises decode/selection before a failed or pending key fetch.
        // The signature is deliberately not claimed to be verified.
        let token = format!(
            "{}.{}.unverified-signature",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"secret-kid"}"#),
            URL_SAFE_NO_PAD
                .encode(br#"{"iss":"https://secret-issuer.example","sub":"secret-subject"}"#)
        );
        (auth, keys, token)
    }
    pub(super) struct SignedKeys {
        key: Jwk,
        pub(super) calls: AtomicU64,
    }
    #[async_trait::async_trait]
    impl OidcKeySource for SignedKeys {
        async fn fetch(
            &self,
            issuer: &str,
            audiences: &[String],
        ) -> Result<OidcVerifier, AuthError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            OidcVerifier::new(issuer, audiences, vec![self.key.clone()])
        }
    }
    pub(super) fn signed_fixture() -> (
        CatalogAuthenticator,
        Arc<SignedKeys>,
        String,
        crate::dispatch::DriveDispatcher,
    ) {
        use ring::{
            rand::SystemRandom,
            signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
        };
        let random = SystemRandom::new();
        let pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &random).unwrap();
        let key =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &random)
                .unwrap();
        let point = key.public_key().as_ref();
        let keys = Arc::new(SignedKeys {
            key: Jwk::EcP256 {
                kid: "secret-signed-kid".into(),
                x: URL_SAFE_NO_PAD.encode(&point[1..33]),
                y: URL_SAFE_NO_PAD.encode(&point[33..65]),
            },
            calls: AtomicU64::new(0),
        });
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"secret-signed-kid"}"#);
        let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
            "iss":"https://secret-issuer.example", "aud":"secret-audience", "sub":"secret-subject",
            "iat":now, "exp":now + 300,
        })).unwrap());
        let input = format!("{header}.{claims}");
        let signature = key.sign(&random, input.as_bytes()).unwrap();
        let token = format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()));
        let catalog = Arc::new(CurrentCatalog(Arc::new(authority())));
        let auth = CatalogAuthenticator::with_key_source(catalog.clone(), keys.clone());
        (
            auth,
            keys,
            token,
            crate::dispatch::DriveDispatcher::new(catalog),
        )
    }
    pub(super) fn row(snapshot: &ServerSnapshot, name: &str) -> ServiceEntry {
        snapshot
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap()
            .clone()
    }
    pub(super) fn settled(snapshot: &ServerSnapshot) {
        assert!(snapshot.complete && snapshot.application_quiescent);
        for entry in &snapshot.entries {
            assert_eq!(entry.in_flight, 0);
            assert_eq!(
                entry.calls,
                entry.success + entry.error + entry.timeout + entry.cancelled
            );
            assert_eq!(entry.calls, entry.latency_log2_us.iter().sum::<u64>());
        }
        let serialized = serde_json::to_string(snapshot).unwrap();
        for secret in [
            "secret-partition",
            "secret-drive",
            "secret-policy",
            "secret-issuer",
            "secret-audience",
            "secret-subject",
            "secret-kid",
            "secret-key-source-error",
            "unverified-signature",
            "secret-malformed-token",
        ] {
            assert!(!serialized.contains(secret), "secret in stage snapshot");
        }
    }
    pub(super) fn pending<F: Future>(future: std::pin::Pin<&mut F>) {
        assert!(matches!(
            future.poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
    }
    pub(super) async fn authenticate(
        auth: &CatalogAuthenticator,
        observer: &ServerDiagnostics,
        token: &str,
    ) -> Result<crate::dispatch::SessionIdentity, ()> {
        auth_scope(Some(observer), auth.authenticate(token, "secret-partition")).await
    }
}

#[cfg(feature = "io-profiling")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_stage_context_restores_across_nested_and_interleaved_polls() {
    fn record() {
        auth_span(AuthStage::Decode).finish(Outcome::Success);
    }
    let first = ServerDiagnostics::new(1, false);
    let second = ServerDiagnostics::new(1, false);
    tokio::join!(
        auth_scope(Some(&first), async {
            record();
            tokio::task::yield_now().await;
            record();
        }),
        auth_scope(Some(&second), async {
            record();
            tokio::task::yield_now().await;
            record();
        })
    );
    auth_scope(Some(&first), async {
        auth_scope(Some(&second), async {
            tokio::task::yield_now().await;
            record();
        })
        .await;
        record();
        tokio::spawn(async {
            assert!(auth_span(AuthStage::Decode).recorder.is_none());
        })
        .await
        .unwrap();
    })
    .await;
    assert!(auth_span(AuthStage::Decode).recorder.is_none());
    for observer in [&first, &second] {
        let snapshot = observer.snapshot();
        assert_eq!(auth_stage_fixture::row(&snapshot, "auth.decode").success, 3);
        auth_stage_fixture::settled(&snapshot);
    }
}

#[cfg(feature = "io-profiling")]
#[tokio::test]
async fn auth_stage_failed_fetch_records_error_before_negative_cache_reuse() {
    use auth_stage_fixture::*;
    let (auth, keys, token) = fixture(false);
    let observer = ServerDiagnostics::new(1, false);
    assert!(
        authenticate(&auth, &observer, "secret-malformed-token")
            .await
            .is_err()
    );
    for _ in 0..2 {
        assert!(authenticate(&auth, &observer, &token).await.is_err());
    }
    let snapshot = observer.snapshot();
    assert_eq!(
        (
            row(&snapshot, "auth.decode").calls,
            row(&snapshot, "auth.decode").error
        ),
        (3, 1)
    );
    assert_eq!(keys.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        (
            row(&snapshot, "auth.key.fetch").calls,
            row(&snapshot, "auth.key.fetch").error
        ),
        (1, 1)
    );
    for name in [
        "auth.decode",
        "auth.catalog",
        "auth.cache.wait",
        "auth.policy.select",
    ] {
        assert_eq!(row(&snapshot, name).success, 2);
    }
    for name in ["auth.jwt.verify", "auth.grant.authorize"] {
        assert_eq!(row(&snapshot, name).calls, 0);
    }
    settled(&snapshot);
}

#[cfg(feature = "io-profiling")]
#[tokio::test]
async fn auth_stage_pending_fetch_and_mutex_wait_retire_on_cancellation() {
    use crate::server::Authenticator;
    use auth_stage_fixture::*;
    let (auth, keys, token) = fixture(true);
    let first = ServerDiagnostics::new(1, false);
    let second = ServerDiagnostics::new(1, false);
    {
        let mut fetching = std::pin::pin!(auth_scope(
            Some(&first),
            auth.authenticate(&token, "secret-partition")
        ));
        pending(fetching.as_mut());
        let mut waiting = std::pin::pin!(auth_scope(
            Some(&second),
            auth.authenticate(&token, "secret-partition")
        ));
        pending(waiting.as_mut());
        assert_eq!(keys.calls.load(Ordering::SeqCst), 1);
        assert_eq!(row(&first.snapshot(), "auth.key.fetch").in_flight, 1);
        assert_eq!(row(&second.snapshot(), "auth.cache.wait").in_flight, 1);
        assert_eq!(row(&second.snapshot(), "auth.key.fetch").calls, 0);
    }
    assert_eq!(row(&first.snapshot(), "auth.key.fetch").cancelled, 1);
    assert_eq!(row(&second.snapshot(), "auth.cache.wait").cancelled, 1);
    for observer in [&first, &second] {
        settled(&observer.snapshot());
    }
    assert!(auth_span(AuthStage::Decode).recorder.is_none());
}

#[cfg(feature = "io-profiling")]
#[tokio::test(start_paused = true)]
async fn auth_stage_outer_timeout_is_distinct_from_inner_cancellation() {
    use auth_stage_fixture::*;
    let (auth, _, token) = fixture(true);
    let observer = ServerDiagnostics::new(1, false);
    let mut outer = Span::new(Some(&observer), Operation::Authentication);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        authenticate(&auth, &observer, &token),
    )
    .await;
    assert!(result.is_err());
    outer.finish(Outcome::Timeout);
    let snapshot = observer.snapshot();
    assert_eq!(
        (
            row(&snapshot, "auth.authenticate").timeout,
            row(&snapshot, "auth.authenticate").cancelled
        ),
        (1, 0)
    );
    assert_eq!(
        (
            row(&snapshot, "auth.key.fetch").timeout,
            row(&snapshot, "auth.key.fetch").cancelled
        ),
        (0, 1)
    );
    settled(&snapshot);
}

#[test]
fn entering_mutation_between_end_envelope_reads_cannot_claim_quiescence() {
    let observer = ServerDiagnostics::new(1, false);
    let pending = std::cell::RefCell::new(None);
    let snapshot = observer.snapshot_with_end_hook(|| {
        let mutation = Mutation::new(&observer.inner);
        let metric = &observer.inner.metrics[Operation::Request as usize];
        observer.inner.add(&metric.calls, 1);
        observer.inner.add(&metric.in_flight, 1);
        *pending.borrow_mut() = Some(mutation);
    });
    let live_requests = observer.inner.metrics[Operation::Request as usize]
        .in_flight
        .load(Ordering::SeqCst);
    drop(pending.into_inner());
    assert_eq!(live_requests, 1);
    assert!(
        !snapshot.complete,
        "concurrent mutation was omitted from capture envelope"
    );
    assert!(!snapshot.application_quiescent);
}

#[test]
fn spans_hold_activity_until_terminal_and_drop_records_cancellation() {
    let observer = ServerDiagnostics::new(2, false);
    let baseline = observer.snapshot();
    assert!(baseline.application_quiescent);
    let mut handshake = Span::new(Some(&observer), Operation::Handshake);
    let request = Span::new(Some(&observer), Operation::Request);
    let held = observer.snapshot();
    assert_eq!(held.active_handshakes_after, 1);
    assert_eq!(held.active_requests_after, 1);
    assert!(!held.application_quiescent);
    handshake.finish(Outcome::Error);
    drop(request);
    let after = observer.snapshot();
    assert!(after.complete && after.application_quiescent);
    assert_eq!(after.active_handshakes_after, 0);
    assert_eq!(after.active_requests_after, 0);
    let request = &after.entries[Operation::Request as usize];
    assert_eq!(
        (request.calls, request.cancelled, request.in_flight),
        (1, 1, 0)
    );
    assert_eq!(request.latency_log2_us.iter().sum::<u64>(), 1);
    assert_eq!(after.entries[Operation::Handshake as usize].error, 1);
    assert!(after.activity_sequence_after > baseline.activity_sequence_after);
}

#[test]
fn saturation_and_mutating_capture_cannot_claim_complete_quiescence() {
    let observer = ServerDiagnostics::new(1, false);
    let mutation = Mutation::new(&observer.inner);
    let during = observer.snapshot();
    assert!(!during.complete && !during.application_quiescent);
    drop(mutation);
    observer.inner.metrics[Operation::Request as usize]
        .calls
        .store(u64::MAX, Ordering::Relaxed);
    let mut span = Span::new(Some(&observer), Operation::Request);
    span.finish(Outcome::Success);
    let after = observer.snapshot();
    assert!(after.counter_saturated);
    assert!(!after.complete && !after.application_quiescent);
    assert_eq!(after.entries[Operation::Request as usize].calls, u64::MAX);
}

#[test]
fn slow_records_are_bounded_and_have_only_fixed_labels() {
    let observer = ServerDiagnostics::new(1, false);
    let mut output = Vec::new();
    for _ in 0..30 {
        write_slow_record(
            &mut output,
            &observer.inner,
            Operation::DispatchRead,
            Outcome::Timeout,
            200_000_000,
        )
        .unwrap();
    }
    let output = String::from_utf8(output).unwrap();
    assert_eq!(output.lines().count(), 16);
    assert!(output.lines().all(|line| line
        == "MOUNT_RS_SERVICE_SLOW operation=dispatch.read outcome=timeout elapsed_us=200000"));
    let websocket = WebSocketDiagnostics::new(false);
    let mut output = Vec::new();
    write_slow_record(
        &mut output,
        &websocket.application.inner,
        Operation::WebSocketUpgrade,
        Outcome::Error,
        200_000_000,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        "MOUNT_RS_SERVICE_SLOW operation=handshake.websocket_upgrade outcome=error elapsed_us=200000\n"
    );
}

#[test]
fn transport_fold_saturates_instead_of_wrapping() {
    let mut target = TransportCounters {
        udp_tx_bytes: u64::MAX - 2,
        ..Default::default()
    };
    let add = TransportCounters {
        udp_tx_bytes: 3,
        frame_rx: FrameCounts {
            stream: 7,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(target.add(add));
    assert_eq!(target.udp_tx_bytes, u64::MAX);
    assert_eq!(target.frame_rx.stream, 7);
}
