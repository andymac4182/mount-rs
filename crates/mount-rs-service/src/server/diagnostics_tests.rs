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
    for entry in &snapshot.entries[14..] {
        assert_eq!(entry.calls, u64::from(cfg!(feature = "io-profiling")));
    }
}

#[cfg(feature = "io-profiling")]
mod auth_stage_fixture {
    use super::*;
    use crate::{
        auth::{AuthError, CatalogAuthenticator, OidcKeySource, OidcVerifier},
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
    pub(super) fn fixture(hold: bool) -> (CatalogAuthenticator, Arc<Keys>, String) {
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
        let keys = Arc::new(Keys {
            hold,
            calls: AtomicU64::new(0),
        });
        let auth = CatalogAuthenticator::with_key_source(
            Arc::new(CurrentCatalog(Arc::new(snapshot))),
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
