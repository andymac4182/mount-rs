//! Ownership controls for a single SDK construction attempt.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use mount_rs_core::{ErrorCode, FsError, Result};
use tokio::sync::Notify;

use crate::ConstructionJournal;

const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum Outcome {
    Success,
    Error,
    Panic,
}

struct Probe {
    name: &'static str,
    calls: AtomicUsize,
    started: Notify,
    release: Notify,
    gated: bool,
    outcome: Outcome,
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl Probe {
    fn new(name: &'static str, outcome: Outcome) -> Arc<Self> {
        Arc::new(Self {
            name,
            calls: AtomicUsize::new(0),
            started: Notify::new(),
            release: Notify::new(),
            gated: false,
            outcome,
            order: Arc::default(),
        })
    }
}

#[async_trait]
impl ConstructionResource for Probe {
    async fn close(&self) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.order.lock().unwrap().push(self.name);
        self.started.notify_one();
        if self.gated {
            self.release.notified().await;
        }
        match self.outcome {
            Outcome::Success => Ok(()),
            Outcome::Error => Err(FsError::new(ErrorCode::Eacces)),
            Outcome::Panic => panic!("controlled construction cleanup panic"),
        }
    }
}

async fn closed(journal: &ConstructionJournal) -> Result<()> {
    tokio::time::timeout(DEADLINE, journal.close())
        .await
        .expect("owned construction cleanup did not finish")
}

#[tokio::test]
async fn cancelled_cleanup_waiter_does_not_cancel_or_duplicate_owned_cleanup() {
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let mut probe = Probe::new("provider", Outcome::Success);
    Arc::get_mut(&mut probe).unwrap().gated = true;
    journal.retain(probe.clone());
    attempt.fail();

    let waiter = {
        let journal = journal.clone();
        tokio::spawn(async move { journal.close().await })
    };
    tokio::time::timeout(DEADLINE, probe.started.notified())
        .await
        .unwrap();
    assert!(journal.snapshot().closing);
    assert_eq!(journal.snapshot().retained_resources, 1);
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    probe.release.notify_one();
    closed(&journal).await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert!(journal.snapshot().cleanup_complete);
    assert_eq!(journal.snapshot().retained_resources, 0);
    closed(&journal).await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn acknowledged_cleanup_releases_the_actual_registered_owner() {
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let resource = Probe::new("provider", Outcome::Success);
    let weak = Arc::downgrade(&resource);
    journal.retain(resource);
    assert!(weak.upgrade().is_some());
    attempt.fail();
    closed(&journal).await.unwrap();
    assert!(weak.upgrade().is_none());
    assert!(journal.snapshot().cleanup_complete);
    assert!(!journal.snapshot().uncertain);
}

#[tokio::test]
async fn failed_authority_cleanup_retains_it_and_does_not_close_dependent_provider() {
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let provider = Probe::new("provider", Outcome::Success);
    let mut authority = Probe::new("authority", Outcome::Error);
    Arc::get_mut(&mut authority).unwrap().order = provider.order.clone();
    let provider_weak = Arc::downgrade(&provider);
    let authority_weak = Arc::downgrade(&authority);
    journal.retain(provider.clone());
    journal.retain(authority.clone());
    attempt.fail();
    assert_eq!(closed(&journal).await.unwrap_err().code, ErrorCode::Eacces);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(authority.calls.load(Ordering::SeqCst), 1);
    assert_eq!(*provider.order.lock().unwrap(), vec!["authority"]);
    drop(provider);
    drop(authority);
    assert!(provider_weak.upgrade().is_some());
    assert!(authority_weak.upgrade().is_some());
    let snapshot = journal.snapshot();
    assert!(snapshot.uncertain);
    assert!(!snapshot.cleanup_complete);
    assert_eq!(snapshot.retained_resources, 2);
    assert_eq!(closed(&journal).await.unwrap_err().code, ErrorCode::Eacces);
    assert_eq!(
        authority_weak
            .upgrade()
            .unwrap()
            .calls
            .load(Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn panicking_cleanup_has_a_terminal_error_and_retains_all_owners() {
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let provider = Probe::new("provider", Outcome::Success);
    let authority = Probe::new("authority", Outcome::Panic);
    journal.retain(provider.clone());
    journal.retain(authority.clone());
    attempt.fail();
    assert_eq!(closed(&journal).await.unwrap_err().code, ErrorCode::Eio);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(authority.calls.load(Ordering::SeqCst), 1);
    assert!(journal.snapshot().uncertain);
    assert_eq!(journal.snapshot().retained_resources, 2);
    assert_eq!(closed(&journal).await.unwrap_err().code, ErrorCode::Eio);
    assert_eq!(authority.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn open_attempt_cannot_be_closed_and_an_abandoned_attempt_stays_uncertain() {
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let provider = Probe::new("provider", Outcome::Success);
    journal.retain(provider.clone());
    assert_eq!(closed(&journal).await.unwrap_err().code, ErrorCode::Ebusy);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(journal.snapshot().opening);
    drop(attempt);
    assert_eq!(closed(&journal).await.unwrap_err().code, ErrorCode::Eio);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(journal.snapshot().uncertain);
    assert_eq!(journal.snapshot().retained_resources, 1);
    assert!(!journal.snapshot().cleanup_complete);
}

#[tokio::test]
async fn successful_handoff_releases_only_the_journals_reference_and_cannot_be_reused() {
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let transferred_owner = Probe::new("provider", Outcome::Success);
    let weak = Arc::downgrade(&transferred_owner);
    journal.retain(transferred_owner.clone());
    attempt.handoff().unwrap();
    assert!(weak.upgrade().is_some());
    assert_eq!(journal.snapshot().retained_resources, 0);
    assert!(journal.snapshot().handed_off);
    assert!(!journal.snapshot().cleanup_complete);
    assert!(journal.begin().is_err());
    assert_eq!(closed(&journal).await.unwrap_err().code, ErrorCode::Estale);
    assert_eq!(transferred_owner.calls.load(Ordering::SeqCst), 0);
    drop(transferred_owner);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn late_registration_is_retained_and_cannot_be_mistaken_for_complete_cleanup() {
    let journal = ConstructionJournal::new();
    journal.begin().unwrap().fail();
    closed(&journal).await.unwrap();
    assert!(journal.snapshot().cleanup_complete);
    let provider = Probe::new("late-provider", Outcome::Success);
    let weak = Arc::downgrade(&provider);
    journal.retain(provider);
    assert!(weak.upgrade().is_some());
    assert_eq!(closed(&journal).await.unwrap_err().code, ErrorCode::Eio);
    assert!(journal.snapshot().uncertain);
    assert!(!journal.snapshot().cleanup_complete);
    assert_eq!(journal.snapshot().retained_resources, 1);
}

#[tokio::test]
async fn late_registration_during_authority_close_stops_dependent_provider_cleanup() {
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let provider = Probe::new("provider", Outcome::Success);
    let mut authority = Probe::new("authority", Outcome::Success);
    Arc::get_mut(&mut authority).unwrap().gated = true;
    journal.retain(provider.clone());
    journal.retain(authority.clone());
    attempt.fail();
    let waiter = {
        let journal = journal.clone();
        tokio::spawn(async move { journal.close().await })
    };
    tokio::time::timeout(DEADLINE, authority.started.notified())
        .await
        .unwrap();
    journal.retain(Probe::new("late-resource", Outcome::Success));
    authority.release.notify_one();
    assert_eq!(
        tokio::time::timeout(DEADLINE, waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code,
        ErrorCode::Eio
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(authority.calls.load(Ordering::SeqCst), 1);
    assert_eq!(journal.snapshot().retained_resources, 3);
    assert!(journal.snapshot().uncertain);
}

#[tokio::test]
async fn resource_retained_by_completion_waker_invalidates_the_waiters_success() {
    use std::task::{Context, Poll, Wake, Waker};

    struct LateOwnerWaker {
        journal: ConstructionJournal,
        wakes: AtomicUsize,
    }
    impl Wake for LateOwnerWaker {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            if self.wakes.fetch_add(1, Ordering::SeqCst) == 0 {
                self.journal
                    .retain(Probe::new("completion-late-owner", Outcome::Success));
            }
        }
    }

    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let mut provider = Probe::new("provider", Outcome::Success);
    Arc::get_mut(&mut provider).unwrap().gated = true;
    journal.retain(provider.clone());
    attempt.fail();
    let wake = Arc::new(LateOwnerWaker {
        journal: journal.clone(),
        wakes: AtomicUsize::new(0),
    });
    let waker = Waker::from(wake.clone());
    let mut future = Box::pin(journal.close());
    assert!(matches!(
        future.as_mut().poll(&mut Context::from_waker(&waker)),
        Poll::Pending
    ));
    tokio::time::timeout(DEADLINE, provider.started.notified())
        .await
        .unwrap();
    provider.release.notify_one();
    tokio::time::timeout(DEADLINE, async {
        while wake.wakes.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        future.as_mut().poll(&mut Context::from_waker(&waker)),
        Poll::Ready(Err(error)) if error.code == ErrorCode::Eio
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert!(journal.snapshot().uncertain);
    assert!(!journal.snapshot().cleanup_complete);
    assert_eq!(journal.snapshot().retained_resources, 1);
}

#[tokio::test]
async fn final_resource_drop_registering_a_late_owner_cannot_publish_cleanup_success() {
    struct RegisterOnDrop(ConstructionJournal);
    #[async_trait]
    impl ConstructionResource for RegisterOnDrop {
        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }
    impl Drop for RegisterOnDrop {
        fn drop(&mut self) {
            self.0
                .retain(Probe::new("drop-late-owner", Outcome::Success));
        }
    }
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    journal.retain(Arc::new(RegisterOnDrop(journal.clone())));
    attempt.fail();
    assert_eq!(closed(&journal).await.unwrap_err().code, ErrorCode::Eio);
    assert!(journal.snapshot().uncertain);
    assert!(!journal.snapshot().cleanup_complete);
    assert_eq!(journal.snapshot().retained_resources, 1);
}

#[tokio::test]
async fn concurrent_cleanup_waiters_share_exactly_one_owned_pass() {
    let journal = ConstructionJournal::new();
    let attempt = journal.begin().unwrap();
    let mut provider = Probe::new("provider", Outcome::Success);
    Arc::get_mut(&mut provider).unwrap().gated = true;
    journal.retain(provider.clone());
    attempt.fail();
    let waiters: Vec<_> = (0..16)
        .map(|_| {
            let journal = journal.clone();
            tokio::spawn(async move { journal.close().await })
        })
        .collect();
    tokio::time::timeout(DEADLINE, provider.started.notified())
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    provider.release.notify_one();
    for waiter in waiters {
        tokio::time::timeout(DEADLINE, waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert!(journal.snapshot().cleanup_complete);
}
