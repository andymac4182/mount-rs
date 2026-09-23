//! Bounded service qualification for atomic conditional object creation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use mount_rs_core::storage::ConcurrentBackingId;
use mount_rs_core::{ErrorCode, FsError, Result};
use object_store::ObjectStore;
use object_store::path::Path as ObjectPath;
use object_store::{PutMode, PutOptions, PutPayload, PutResult};
use tokio::sync::Barrier;

use crate::{ObjectStoreBlockStore, validate_prefix, verify_configured_backing_id};

/// An isolated marker owned by a single qualification attempt. External
/// callers cannot direct its cleanup at a production block authority.
#[derive(Debug)]
pub struct PrivateQualificationPrefix(ObjectPath);

impl PrivateQualificationPrefix {
    pub fn marker_path(&self) -> ObjectPath {
        ObjectPath::from(format!("{}/_mount-rs-backing-id-v2", self.0))
    }
}

/// Generate an isolated marker beneath the selected block prefix, so scoped
/// service credentials can test their own write domain.
pub fn generate_private_qualification_prefix(selected: &str) -> Result<PrivateQualificationPrefix> {
    let selected = validate_prefix(selected)?;
    Ok(PrivateQualificationPrefix(ObjectPath::from(format!(
        "{selected}/_mount-rs-qualification-v2/{}/blocks",
        uuid::Uuid::new_v4()
    ))))
}

/// Test two independently constructed signed probe clients on a private,
/// disposable prefix before a provider may bind user metadata to the service.
/// The provider must supply clients built from its validated configuration.
pub async fn prove_two_configured_clients(
    first_data: Arc<dyn ObjectStore>,
    second_data: Arc<dyn ObjectStore>,
    first_probe: Arc<dyn ObjectStore>,
    second_probe: Arc<dyn ObjectStore>,
    prefix: &PrivateQualificationPrefix,
) -> Result<()> {
    let marker = prefix.marker_path();
    let first_blocks = ObjectStoreBlockStore::new(first_data.clone(), prefix.0.as_ref(), true)?;
    let second_blocks = ObjectStoreBlockStore::new(second_data.clone(), prefix.0.as_ref(), true)?;
    let attempted_create = AtomicBool::new(false);
    let creates_in_flight = AtomicUsize::new(0);
    let creates_overlapped = AtomicBool::new(false);
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        // This private, random prefix must start empty. Both clients must send
        // distinct conditional Creates and one must explicitly lose; a service
        // that silently overwrites on Create must never issue a qualification.
        for store in [&first_probe, &second_probe, &first_data, &second_data] {
            require_initial_marker_missing(store.as_ref(), &marker).await?;
        }
        let first = ConcurrentBackingId::from_bytes(*uuid::Uuid::new_v4().as_bytes())?;
        let second = ConcurrentBackingId::from_bytes(*uuid::Uuid::new_v4().as_bytes())?;
        let candidate_payload = |id: ConcurrentBackingId| {
            let mut bytes = Vec::with_capacity(20);
            bytes.extend_from_slice(b"MRC2");
            bytes.extend_from_slice(&id.as_bytes());
            PutPayload::from(bytes)
        };
        let create_options = PutOptions {
            mode: PutMode::Create,
            ..PutOptions::default()
        };
        let second_options = create_options.clone();
        let first_retry_options = create_options.clone();
        let second_retry_options = create_options.clone();
        let start = Barrier::new(2);
        attempted_create.store(true, Ordering::SeqCst);
        // A sequential success followed by a conflict cannot test a service's
        // behavior under simultaneous Creates. Require both request futures
        // to be active together before examining the winner and loser.
        let (mut first_result, mut second_result) = futures_util::future::join(
            async {
                start.wait().await;
                if creates_in_flight.fetch_add(1, Ordering::SeqCst) > 0 {
                    creates_overlapped.store(true, Ordering::SeqCst);
                }
                let result = first_probe
                    .put_opts(&marker, candidate_payload(first), create_options)
                    .await;
                creates_in_flight.fetch_sub(1, Ordering::SeqCst);
                result
            },
            async {
                start.wait().await;
                if creates_in_flight.fetch_add(1, Ordering::SeqCst) > 0 {
                    creates_overlapped.store(true, Ordering::SeqCst);
                }
                let result = second_probe
                    .put_opts(&marker, candidate_payload(second), second_options)
                    .await;
                creates_in_flight.fetch_sub(1, Ordering::SeqCst);
                result
            },
        )
        .await;
        if !creates_overlapped.load(Ordering::SeqCst) {
            return Err(unqualified_object_store());
        }
        if first_result.is_ok() && second_result.as_ref().is_err_and(retryable_cleanup_error) {
            second_result = retry_losing_conditional_create(|| {
                second_probe.put_opts(
                    &marker,
                    candidate_payload(second),
                    second_retry_options.clone(),
                )
            })
            .await?;
        } else if second_result.is_ok() && first_result.as_ref().is_err_and(retryable_cleanup_error)
        {
            first_result = retry_losing_conditional_create(|| {
                first_probe.put_opts(
                    &marker,
                    candidate_payload(first),
                    first_retry_options.clone(),
                )
            })
            .await?;
        }
        let winner = qualification_race_winner(first_result, second_result, first, second)?;
        let bytes = first_data
            .get(&marker)
            .await
            .map_err(|_| FsError::backend("object-store qualification marker read failed"))?
            .bytes()
            .await
            .map_err(|_| FsError::backend("object-store qualification marker read failed"))?;
        if bytes.len() != 20
            || &bytes[..4] != b"MRC2"
            || &bytes[4..] != winner.as_bytes().as_slice()
        {
            return Err(FsError::new(ErrorCode::Estale)
                .with_message("object-store qualification marker read-back changed"));
        }
        verify_configured_backing_id(first_probe.as_ref(), &first_blocks, winner).await?;
        verify_configured_backing_id(second_probe.as_ref(), &second_blocks, winner).await?;
        Ok(())
    })
    .await
    .map_err(|_| FsError::backend("object-store qualification race timed out"))?;
    if !attempted_create.load(Ordering::SeqCst) {
        return result;
    }
    delete_owned_marker_with_retry(|| first_probe.delete(&marker)).await?;
    for store in [&first_probe, &second_probe, &first_data, &second_data] {
        verify_owned_marker_absent(store.as_ref(), &marker).await?;
    }
    result
}

async fn require_initial_marker_missing(
    store: &dyn ObjectStore,
    marker: &ObjectPath,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut attempts = 0_u32;
        loop {
            match store.get(marker).await {
                Err(object_store::Error::NotFound { .. }) => return Ok(()),
                Ok(_) => return Err(unqualified_object_store()),
                Err(error) if retryable_cleanup_error(&error) => {
                    let scale = 1_u64 << attempts.min(4);
                    tokio::time::sleep(Duration::from_millis((100 * scale).min(1_700))).await;
                    attempts = attempts.saturating_add(1);
                }
                Err(_) => {
                    return Err(FsError::backend(
                        "object-store qualification initial marker read failed",
                    ));
                }
            }
        }
    })
    .await
    .map_err(|_| FsError::backend("object-store initial marker read timed out"))?
}

fn qualification_race_winner(
    first: object_store::Result<PutResult>,
    second: object_store::Result<PutResult>,
    first_id: ConcurrentBackingId,
    second_id: ConcurrentBackingId,
) -> Result<ConcurrentBackingId> {
    let conflict = |error: &object_store::Error| {
        matches!(
            error,
            object_store::Error::Precondition { .. } | object_store::Error::AlreadyExists { .. }
        )
    };
    match (first, second) {
        (Ok(_), Err(error)) if conflict(&error) => Ok(first_id),
        (Err(error), Ok(_)) if conflict(&error) => Ok(second_id),
        (Err(first_error), Err(second_error))
            if conflict(&first_error) && retryable_cleanup_error(&second_error) =>
        {
            Ok(second_id)
        }
        (Err(first_error), Err(second_error))
            if conflict(&second_error) && retryable_cleanup_error(&first_error) =>
        {
            Ok(first_id)
        }
        _ => Err(unqualified_object_store()),
    }
}

async fn retry_losing_conditional_create<F, Fut>(
    mut create: F,
) -> Result<object_store::Result<PutResult>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = object_store::Result<PutResult>>,
{
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut attempts = 0_u32;
        loop {
            match create().await {
                Err(error) if retryable_cleanup_error(&error) => {
                    let scale = 1_u64 << attempts.min(4);
                    tokio::time::sleep(Duration::from_millis((100 * scale).min(1_700))).await;
                    attempts = attempts.saturating_add(1);
                }
                result => return result,
            }
        }
    })
    .await
    .map_err(|_| FsError::backend("object-store losing conditional Create timed out"))
}

async fn delete_owned_marker_with_retry<F, Fut>(mut delete: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = object_store::Result<()>>,
{
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut attempts = 0_u32;
        loop {
            match delete().await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => return Ok(()),
                Err(error) if retryable_cleanup_error(&error) => {
                    let scale = 1_u64 << attempts.min(4);
                    tokio::time::sleep(Duration::from_millis((100 * scale).min(1_700))).await;
                    attempts = attempts.saturating_add(1);
                }
                Err(_) => {
                    return Err(FsError::backend(
                        "object-store qualification marker cleanup failed",
                    ));
                }
            }
        }
    })
    .await
    .map_err(|_| FsError::backend("object-store qualification marker cleanup timed out"))?
}

async fn verify_owned_marker_absent(store: &dyn ObjectStore, marker: &ObjectPath) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut attempts = 0_u32;
        loop {
            match store.get(marker).await {
                Err(object_store::Error::NotFound { .. }) => return Ok(()),
                Ok(_) => {}
                Err(error) if retryable_cleanup_error(&error) => {}
                Err(_) => {
                    return Err(FsError::backend(
                        "object-store qualification marker cleanup read failed",
                    ));
                }
            }
            let scale = 1_u64 << attempts.min(4);
            tokio::time::sleep(Duration::from_millis((100 * scale).min(1_700))).await;
            attempts = attempts.saturating_add(1);
        }
    })
    .await
    .map_err(|_| FsError::backend("object-store qualification cleanup read timed out"))?
}

fn retryable_cleanup_error(error: &object_store::Error) -> bool {
    let message = error.to_string();
    message.contains("SlowDown")
        || message.contains(" 429 ")
        || message.contains(" 503 ")
        || matches!(
            error,
            object_store::Error::Generic { .. } | object_store::Error::JoinError { .. }
        )
}

fn unqualified_object_store() -> FsError {
    FsError::new(ErrorCode::Enotsup)
        .with_syscall("qualify conditional object creation")
        .with_message("the selected service did not prove atomic conditional Create")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn cleanup_retries_throttle_and_lost_delete_acknowledgement() {
        for first_error in ["HTTP 429 Too Many Requests", "lost delete reply"] {
            let attempts = AtomicUsize::new(0);
            delete_owned_marker_with_retry(|| {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    if attempt == 0 {
                        Err(object_store::Error::Generic {
                            store: "qualification-test",
                            source: Box::new(std::io::Error::other(first_error)),
                        })
                    } else {
                        Ok(())
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(attempts.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn losing_create_retries_throttle_until_conditional_conflict() {
        let attempts = AtomicUsize::new(0);
        let result = retry_losing_conditional_create(|| {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt == 0 {
                    Err(object_store::Error::Generic {
                        store: "qualification-test",
                        source: Box::new(std::io::Error::other("HTTP 429 Too Many Requests")),
                    })
                } else {
                    Err(object_store::Error::Precondition {
                        path: "private-marker".to_owned(),
                        source: Box::new(std::io::Error::other("conditional conflict")),
                    })
                }
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            result,
            Err(object_store::Error::Precondition { .. })
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn race_requires_one_accepted_create_and_one_observed_conflict() {
        let accepted = PutResult {
            e_tag: None,
            version: None,
        };
        let conflict = object_store::Error::Precondition {
            path: "private-marker".to_owned(),
            source: Box::new(std::io::Error::other("conditional conflict")),
        };
        let first = ConcurrentBackingId::from_bytes([1; 16]).unwrap();
        let second = ConcurrentBackingId::from_bytes([2; 16]).unwrap();
        assert_eq!(
            qualification_race_winner(Ok(accepted.clone()), Err(conflict), first, second).unwrap(),
            first
        );
        assert!(
            qualification_race_winner(Ok(accepted.clone()), Ok(accepted), first, second).is_err()
        );
    }
}
