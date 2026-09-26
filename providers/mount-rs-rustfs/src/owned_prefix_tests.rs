use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use mount_rs_core::ErrorCode;
use object_store::list::{PaginatedListOptions, PaginatedListResult, PaginatedListStore};
use object_store::path::Path;
use object_store::{ListResult, ObjectMeta};
use tokio::sync::Notify;

use super::{RustFsConfig, observe_owned_prefix_absence_with};

const PREFIX: &str = "owned-layout-pair/run-20260927_1.0_abc/blocks";

#[derive(Debug, PartialEq, Eq)]
struct Request {
    prefix: Option<String>,
    max_keys: Option<usize>,
    delimiter: Option<String>,
    offset: Option<String>,
    page_token: Option<String>,
    extensions_empty: bool,
}

enum Response {
    Empty,
    Objects(Vec<&'static str>),
    CommonPrefix,
    Continuation,
    Error,
    Pending,
}

struct FakeListing {
    response: Response,
    requests: Mutex<Vec<Request>>,
    entered: Notify,
    pending: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}

impl FakeListing {
    fn new(response: Response) -> Self {
        Self {
            response,
            requests: Mutex::new(Vec::new()),
            entered: Notify::new(),
            pending: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicUsize::new(0)),
        }
    }
}

struct AwaitedListing {
    pending: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}

impl Drop for AwaitedListing {
    fn drop(&mut self) {
        self.pending.fetch_sub(1, Ordering::SeqCst);
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

fn object(location: &str) -> ObjectMeta {
    ObjectMeta {
        location: Path::parse(location).unwrap(),
        last_modified: UNIX_EPOCH.into(),
        size: 0,
        e_tag: None,
        version: None,
    }
}

#[async_trait]
impl PaginatedListStore for FakeListing {
    async fn list_paginated(
        &self,
        prefix: Option<&str>,
        options: PaginatedListOptions,
    ) -> object_store::Result<PaginatedListResult> {
        self.requests.lock().unwrap().push(Request {
            prefix: prefix.map(str::to_owned),
            max_keys: options.max_keys,
            delimiter: options.delimiter.as_deref().map(str::to_owned),
            offset: options.offset,
            page_token: options.page_token,
            extensions_empty: options.extensions.is_empty(),
        });
        let mut page = PaginatedListResult {
            result: ListResult {
                common_prefixes: Vec::new(),
                objects: Vec::new(),
            },
            page_token: None,
        };
        match &self.response {
            Response::Empty => {}
            Response::Objects(locations) => {
                page.result.objects = locations.iter().map(|location| object(location)).collect();
            }
            Response::CommonPrefix => {
                page.result
                    .common_prefixes
                    .push(Path::parse(format!("{PREFIX}/nested")).unwrap());
            }
            Response::Continuation => page.page_token = Some("private-continuation-token".into()),
            Response::Error => {
                return Err(object_store::Error::NotSupported {
                    source: std::io::Error::other("private endpoint/token/error").into(),
                });
            }
            Response::Pending => {
                self.pending.fetch_add(1, Ordering::SeqCst);
                let _awaited = AwaitedListing {
                    pending: self.pending.clone(),
                    dropped: self.dropped.clone(),
                };
                self.entered.notify_one();
                return std::future::pending().await;
            }
        }
        Ok(page)
    }
}

#[tokio::test]
async fn empty_observation_uses_one_exact_bounded_signed_list_seam() {
    let store = FakeListing::new(Response::Empty);
    assert!(
        observe_owned_prefix_absence_with(&store, PREFIX, Duration::from_secs(1))
            .await
            .unwrap()
    );
    assert_eq!(
        *store.requests.lock().unwrap(),
        vec![Request {
            prefix: Some(format!("{PREFIX}/")),
            max_keys: Some(1),
            delimiter: None,
            offset: None,
            page_token: None,
            extensions_empty: true,
        }]
    );
}

#[tokio::test]
async fn any_owned_child_including_reserved_and_nested_markers_is_present() {
    for child in [
        "owned-layout-pair/run-20260927_1.0_abc/blocks/b00000000000000000000000000000000",
        "owned-layout-pair/run-20260927_1.0_abc/blocks/_mount-rs-backing-id-v2",
        "owned-layout-pair/run-20260927_1.0_abc/blocks/_mount-rs-concurrent-probe-v1",
        "owned-layout-pair/run-20260927_1.0_abc/blocks/_mount-rs-qualification-v2/uuid/blocks/_mount-rs-backing-id-v2",
        "owned-layout-pair/run-20260927_1.0_abc/blocks/unknown/nested-child",
    ] {
        let store = FakeListing::new(Response::Objects(vec![child]));
        assert!(
            !observe_owned_prefix_absence_with(&store, PREFIX, Duration::from_secs(1))
                .await
                .unwrap()
        );
        assert_eq!(store.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn sibling_locations_and_incomplete_page_shapes_fail_closed() {
    for response in [
        Response::Objects(vec![
            "owned-layout-pair/run-20260927_1.0_abc/blocks-other/b00000000000000000000000000000000",
        ]),
        Response::Objects(vec!["other-run/blocks/b00000000000000000000000000000000"]),
        Response::Objects(vec!["owned-layout-pair/run-20260927_1.0_abc/blocks"]),
        Response::Objects(vec![
            "owned-layout-pair/run-20260927_1.0_abc/blocks/a",
            "owned-layout-pair/run-20260927_1.0_abc/blocks/b",
        ]),
        Response::CommonPrefix,
        Response::Continuation,
        Response::Error,
    ] {
        let store = FakeListing::new(response);
        let error = observe_owned_prefix_absence_with(&store, PREFIX, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(error.is(ErrorCode::Eio));
        assert!(!error.to_string().contains("private"));
        assert_eq!(store.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn unsafe_or_oversized_prefixes_are_rejected_before_dispatch() {
    let oversized = "a".repeat(513);
    for prefix in [
        "",
        "/",
        "/owned/run/blocks",
        "owned/run/blocks/",
        "owned//blocks",
        ".",
        "..",
        "owned/./blocks",
        "owned/../blocks",
        "owned\\run/blocks",
        "owned%2Frun/blocks",
        "owned/run:1/blocks",
        "owned/run?key/blocks",
        "owned/run#key/blocks",
        "owned run/blocks",
        "owned/é/blocks",
        "owned/\0/blocks",
        "owned/\n/blocks",
        oversized.as_str(),
    ] {
        let store = FakeListing::new(Response::Empty);
        let error = observe_owned_prefix_absence_with(&store, prefix, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(error.is(ErrorCode::Einval));
        assert!(store.requests.lock().unwrap().is_empty());
    }
    let store = FakeListing::new(Response::Empty);
    assert!(
        observe_owned_prefix_absence_with(&store, &"a".repeat(512), Duration::from_secs(1))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn configured_public_method_checks_scope_before_building_a_client() {
    let config = RustFsConfig {
        endpoint: "invalid-private-endpoint".into(),
        bucket: "invalid-private-bucket".into(),
        access_key_id: "private-key".into(),
        secret_access_key: "private-secret".into(),
        region: "private-region".into(),
    };
    let error = config
        .observe_owned_prefix_absence("../private-scope")
        .await
        .unwrap_err();
    assert!(error.is(ErrorCode::Einval));
    assert!(!error.to_string().contains("private"));
}

#[tokio::test]
async fn deadline_drops_a_held_listing_once_without_retry() {
    let store = FakeListing::new(Response::Pending);
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        observe_owned_prefix_absence_with(&store, PREFIX, Duration::from_millis(10)),
    )
    .await
    .unwrap();
    let error = result.unwrap_err();
    assert!(error.is(ErrorCode::Eio));
    assert!(error.to_string().contains("timed out"));
    assert_eq!(store.requests.lock().unwrap().len(), 1);
    assert_eq!(store.pending.load(Ordering::SeqCst), 0);
    assert_eq!(store.dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn caller_cancellation_drops_the_exact_held_listing() {
    let store = Arc::new(FakeListing::new(Response::Pending));
    let retained = store.clone();
    let task = tokio::spawn(async move {
        observe_owned_prefix_absence_with(retained.as_ref(), PREFIX, Duration::from_secs(30)).await
    });
    tokio::time::timeout(Duration::from_secs(1), store.entered.notified())
        .await
        .unwrap();
    assert_eq!(store.pending.load(Ordering::SeqCst), 1);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(store.pending.load(Ordering::SeqCst), 0);
    assert_eq!(store.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(store.requests.lock().unwrap().len(), 1);
}
