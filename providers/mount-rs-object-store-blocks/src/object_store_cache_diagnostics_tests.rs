use super::*;
use mount_rs_core::diagnostics::object_store::Observer;

#[test]
fn actual_cache_owner_releases_payload_without_observer_retaining_it() {
    let observer = Observer::isolated();
    let cache = ObjectStoreBlockCache::new_with_observer(&observer);
    let cache = Arc::new(cache);
    let weak = Arc::downgrade(&cache);
    let clone = Arc::clone(&cache);
    cache.insert("one", b"abc", None);
    cache.insert("two", b"12345", None);
    assert_eq!(
        (
            observer.snapshot().unwrap().cache.live,
            observer.snapshot().unwrap().cache.resident_entries,
            observer.snapshot().unwrap().cache.payload_bytes
        ),
        (1, 2, 8)
    );
    drop(cache);
    assert_eq!(observer.snapshot().unwrap().cache.live, 1);
    drop(clone);
    assert!(weak.upgrade().is_none());
    let final_state = observer.snapshot().unwrap().cache;
    assert_eq!(
        (
            final_state.live,
            final_state.resident_entries,
            final_state.payload_bytes,
            final_state.released
        ),
        (0, 0, 0, 1)
    );
}

#[test]
fn cache_replacement_removal_and_lru_keep_actual_residency() {
    let observer = Observer::isolated();
    let cache = ObjectStoreBlockCache::new_with_observer(&observer);
    cache.insert("one", b"abc", None);
    cache.insert("one", b"12", None);
    cache.insert("two", b"4567", None);
    cache.get("one").unwrap();
    cache.remove("missing", None);
    let populated = observer.snapshot().unwrap().cache;
    assert_eq!(
        (populated.resident_entries, populated.payload_bytes),
        (2, 6)
    );
    cache.remove("one", None);
    let removed = observer.snapshot().unwrap().cache;
    assert_eq!((removed.resident_entries, removed.payload_bytes), (1, 4));
    for index in 0..MAX_CACHE_ENTRIES {
        cache.insert(&format!("entry-{index}"), b"x", None);
    }
    assert!(cache.get("two").is_none());
    let capacity = observer.snapshot().unwrap().cache;
    assert_eq!(
        (capacity.resident_entries, capacity.payload_bytes),
        (MAX_CACHE_ENTRIES as u64, MAX_CACHE_ENTRIES as u64)
    );
}

#[test]
fn byte_cap_and_oversize_rejection_preserve_existing_policy() {
    let observer = Observer::isolated();
    let cache = ObjectStoreBlockCache::new_with_observer(&observer);
    let full = vec![3; MAX_CACHE_BYTES];
    cache.insert("full", &full, None);
    assert_eq!(
        observer.snapshot().unwrap().cache.payload_bytes,
        MAX_CACHE_BYTES as u64
    );
    cache.insert("small", b"xy", None);
    assert!(cache.get("full").is_none());
    let before = observer.snapshot().unwrap().cache;
    cache.insert("oversize", &vec![4; MAX_CACHE_BYTES + 1], None);
    assert!(cache.get("oversize").is_none());
    assert_eq!(observer.snapshot().unwrap().cache, before);
    assert_eq!(cache.get("small").as_deref(), Some(b"xy".as_slice()));
}

#[test]
fn poisoned_cache_is_unknown_once_and_keeps_existing_bypass() {
    let observer = Observer::isolated();
    let cache = Arc::new(ObjectStoreBlockCache::new_with_observer(&observer));
    cache.insert("known", b"abc", None);
    cache.insert("partial", &[9], None);
    let poisoned = Arc::clone(&cache);
    assert!(
        std::thread::spawn(move || {
            let _state = poisoned.state.lock().unwrap();
            panic!("poison actual state after partial mutation");
        })
        .join()
        .is_err()
    );
    assert!(cache.get("known").is_none());
    cache.insert("bypassed", b"no", None);
    cache.remove("known", None);
    let incomplete = observer.snapshot().unwrap().cache;
    assert_eq!(
        (
            incomplete.live,
            incomplete.unknown_live,
            incomplete.resident_entries,
            incomplete.payload_bytes
        ),
        (1, 1, 0, 0)
    );
    drop(cache);
    let released = observer.snapshot().unwrap().cache;
    assert_eq!(
        (released.live, released.unknown_live, released.released),
        (0, 0, 1)
    );
}

#[test]
fn disabled_cache_observation_keeps_cache_behavior() {
    let observer = Observer::disabled();
    let cache = ObjectStoreBlockCache::new_with_observer(&observer);
    cache.insert("one", b"abc", None);
    assert_eq!(cache.get("one").as_deref(), Some(b"abc".as_slice()));
    cache.remove("one", None);
    assert!(cache.get("one").is_none());
    assert!(observer.snapshot().is_none());
}

#[test]
fn actual_store_clone_shares_cache_until_final_facade_drop() {
    let observer = Observer::isolated();
    let mut original = ObjectStoreBlockStore::new(
        Arc::new(object_store::memory::InMemory::new()),
        "observed-clone/blocks",
        false,
    )
    .unwrap();
    // Test-only injection into the real facade; no production observer policy.
    original.cache = Arc::new(ObjectStoreBlockCache::new_with_observer(&observer));
    original.cache.insert("one", b"abc", None);
    let weak = Arc::downgrade(&original.cache);
    let clone = original.clone();
    assert!(Arc::ptr_eq(&original.cache, &clone.cache));
    drop(original);
    assert!(weak.upgrade().is_some());
    assert_eq!(
        (
            observer.snapshot().unwrap().cache.live,
            observer.snapshot().unwrap().cache.payload_bytes
        ),
        (1, 3)
    );
    drop(clone);
    assert!(weak.upgrade().is_none());
    let final_state = observer.snapshot().unwrap().cache;
    assert_eq!(
        (
            final_state.created,
            final_state.released,
            final_state.live,
            final_state.payload_bytes
        ),
        (1, 1, 0, 0)
    );
}

#[tokio::test]
#[ignore = "requires isolated process MOUNT_RS_PROFILE_IO=1 and this exact selector"]
async fn actual_qualification_temporary_adapters_release_both_observed_caches() {
    assert_eq!(std::env::var("MOUNT_RS_PROFILE_IO").as_deref(), Ok("1"));
    assert!(mount_rs_core::diagnostics::storage::enabled());
    let observer = Observer::enabled();
    let before = observer.snapshot().unwrap().cache;
    let backing: Arc<dyn object_store::ObjectStore> =
        Arc::new(object_store::memory::InMemory::new());
    let prefix = generate_private_qualification_prefix("observed-qualification/blocks").unwrap();
    let result = prove_two_configured_clients(
        backing.clone(),
        backing.clone(),
        backing.clone(),
        backing.clone(),
        &prefix,
    )
    .await;
    // InMemory may finish Creates without overlapping; that valid qualification
    // refusal still runs the actual two-adapter construction and cleanup path.
    assert!(
        result.is_ok()
            || result
                .as_ref()
                .is_err_and(|error| error.is(ErrorCode::Enotsup)),
        "unrelated qualification failure: {result:?}"
    );
    let after = observer.snapshot().unwrap().cache;
    assert_eq!(
        (
            after.created - before.created,
            after.released - before.released
        ),
        (2, 2)
    );
    assert_eq!(after.live, before.live);
    assert_eq!(
        (
            after.resident_entries,
            after.payload_bytes,
            after.unknown_live
        ),
        (
            before.resident_entries,
            before.payload_bytes,
            before.unknown_live
        )
    );
}

const RAW_BUDGET_TEST_ENTRY_CHARGE: usize = 128;

#[derive(Debug, Default)]
struct ActualRawCacheResidency {
    entries: usize,
    payload_bytes: usize,
    charged_bytes: usize,
}

// Read the real retained entries rather than a budget counter or observer. Each
// argument must be a distinct cache owner; facade clones would count twice.
fn actual_raw_cache_residency(stores: &[&ObjectStoreBlockStore]) -> ActualRawCacheResidency {
    let mut actual = ActualRawCacheResidency::default();
    for (index, store) in stores.iter().enumerate() {
        for previous in &stores[..index] {
            assert!(!Arc::ptr_eq(&previous.cache, &store.cache));
        }
        let state = store.cache.state.lock().unwrap();
        for payload in state.entries.values() {
            actual.entries += 1;
            actual.payload_bytes += payload.bytes.len();
            actual.charged_bytes += payload.bytes.len().max(1) + RAW_BUDGET_TEST_ENTRY_CHARGE;
        }
    }
    actual
}

fn raw_budget_store(
    backing: Arc<dyn ObjectStore>,
    prefix: &str,
    budget: &RawBlockCacheBudget,
) -> ObjectStoreBlockStore {
    ObjectStoreBlockStore::new_with_cache_budget(backing, prefix, false, budget.clone()).unwrap()
}

async fn assert_raw_budget_backing_bytes(
    backing: &dyn ObjectStore,
    store: &ObjectStoreBlockStore,
    id: &BlockId,
    expected: &[u8],
) {
    let remote = backing
        .get(&store.object_path(id).unwrap())
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(remote.as_ref(), expected);
}

#[tokio::test]
async fn shared_raw_cache_budget_bounds_bytes_across_independent_drive_caches() {
    let backing = Arc::new(object_store::memory::InMemory::new());
    let budget = RawBlockCacheBudget::new(RAW_BUDGET_TEST_ENTRY_CHARGE + 4, 16);
    let first = raw_budget_store(backing.clone(), "budget-byte/drive-a", &budget);
    let second = raw_budget_store(backing.clone(), "budget-byte/drive-b", &budget);

    let first_id = first.put(b"abcd").await.unwrap();
    let initial = actual_raw_cache_residency(&[&first]);
    assert_eq!((initial.entries, initial.payload_bytes), (1, 4));
    assert_eq!(initial.charged_bytes, budget.max_charged_bytes());
    assert_eq!(
        first.cache.get(&first_id.0).as_deref(),
        Some(b"abcd".as_slice())
    );

    // A full cache cannot turn an accepted backing write or read into an error.
    let second_id = second.put(b"wxyz").await.unwrap();
    first.flush().await.unwrap();
    second.flush().await.unwrap();
    assert_eq!(first.get(&first_id).await.unwrap(), b"abcd");
    assert_eq!(second.get(&second_id).await.unwrap(), b"wxyz");
    assert_eq!(first.get_for_migration(&first_id).await.unwrap(), b"abcd");
    assert_eq!(second.get_for_migration(&second_id).await.unwrap(), b"wxyz");
    assert_raw_budget_backing_bytes(backing.as_ref(), &first, &first_id, b"abcd").await;
    assert_raw_budget_backing_bytes(backing.as_ref(), &second, &second_id, b"wxyz").await;

    let actual = actual_raw_cache_residency(&[&first, &second]);
    assert!(
        actual.charged_bytes <= budget.max_charged_bytes(),
        "independent drive caches over-admitted shared charged-byte capacity: {actual:?}"
    );
}

#[tokio::test]
async fn shared_raw_cache_budget_charges_empty_payloads_across_drives() {
    let backing = Arc::new(object_store::memory::InMemory::new());
    let budget = RawBlockCacheBudget::new(RAW_BUDGET_TEST_ENTRY_CHARGE + 1, 16);
    let first = raw_budget_store(backing.clone(), "budget-empty/drive-a", &budget);
    let second = raw_budget_store(backing.clone(), "budget-empty/drive-b", &budget);
    let first_id = first.put(b"").await.unwrap();
    let initial = actual_raw_cache_residency(&[&first]);
    assert_eq!((initial.entries, initial.payload_bytes), (1, 0));
    assert_eq!(initial.charged_bytes, budget.max_charged_bytes());

    let second_id = second.put(b"").await.unwrap();
    assert_eq!(first_id, second_id);
    assert!(first.get(&first_id).await.unwrap().is_empty());
    assert!(second.get(&second_id).await.unwrap().is_empty());
    assert_raw_budget_backing_bytes(backing.as_ref(), &first, &first_id, b"").await;
    assert_raw_budget_backing_bytes(backing.as_ref(), &second, &second_id, b"").await;

    let actual = actual_raw_cache_residency(&[&first, &second]);
    assert_eq!(actual.payload_bytes, 0);
    assert!(
        actual.charged_bytes <= budget.max_charged_bytes(),
        "empty payloads bypassed positive shared byte/bookkeeping charges: {actual:?}"
    );
}

#[tokio::test]
async fn shared_raw_cache_budget_bounds_entries_independently_of_bytes() {
    let backing = Arc::new(object_store::memory::InMemory::new());
    let budget = RawBlockCacheBudget::new(64 * 1024, 1);
    let first = raw_budget_store(backing.clone(), "budget-entry/drive-a", &budget);
    let second = raw_budget_store(backing.clone(), "budget-entry/drive-b", &budget);
    let first_id = first.put(b"a").await.unwrap();
    assert_eq!(actual_raw_cache_residency(&[&first]).entries, 1);
    let second_id = second.put(b"b").await.unwrap();
    assert_eq!(first.get(&first_id).await.unwrap(), b"a");
    assert_eq!(second.get(&second_id).await.unwrap(), b"b");
    assert_raw_budget_backing_bytes(backing.as_ref(), &first, &first_id, b"a").await;
    assert_raw_budget_backing_bytes(backing.as_ref(), &second, &second_id, b"b").await;

    let actual = actual_raw_cache_residency(&[&first, &second]);
    assert!(actual.charged_bytes < budget.max_charged_bytes());
    assert!(
        actual.entries <= budget.max_entries(),
        "shared entry capacity was exceeded even though byte capacity remained: {actual:?}"
    );
}

#[tokio::test]
async fn shared_raw_cache_budget_disabled_admission_preserves_backing_outcomes() {
    let backing = Arc::new(object_store::memory::InMemory::new());
    let budget = RawBlockCacheBudget::new(0, 0);
    let store = raw_budget_store(backing.clone(), "budget-disabled/blocks", &budget);
    let body = b"accepted without raw cache credit";
    let id = store.put(body).await.unwrap();
    store.flush().await.unwrap();
    assert_eq!(store.get(&id).await.unwrap(), body);
    assert_eq!(store.get_for_migration(&id).await.unwrap(), body);
    assert_raw_budget_backing_bytes(backing.as_ref(), &store, &id, body).await;

    // Admission policy cannot weaken conditional-create collision checks.
    let conflicting = raw_budget_store(backing.clone(), "budget-disabled/conflict", &budget);
    let different = b"existing object at the same content-addressed path";
    backing
        .put(
            &conflicting.object_path(&id).unwrap(),
            PutPayload::from(different.to_vec()),
        )
        .await
        .unwrap();
    assert!(conflicting.put(body).await.unwrap_err().is(ErrorCode::Eio));
    assert_raw_budget_backing_bytes(backing.as_ref(), &conflicting, &id, different).await;
    assert!(conflicting.get(&id).await.unwrap_err().is(ErrorCode::Eio));
    assert_eq!(actual_raw_cache_residency(&[&conflicting]).entries, 0);

    let actual = actual_raw_cache_residency(&[&store]);
    assert_eq!(
        actual.entries, 0,
        "a disabled shared budget still retained raw cache payloads: {actual:?}"
    );
}

#[tokio::test]
async fn shared_raw_cache_budget_keeps_same_id_isolated_by_prefix_and_backing() {
    let backing = Arc::new(object_store::memory::InMemory::new());
    let other_backing = Arc::new(object_store::memory::InMemory::new());
    let budget = RawBlockCacheBudget::new(RAW_BUDGET_TEST_ENTRY_CHARGE + 5, 1);
    let first = raw_budget_store(backing.clone(), "budget-scope/drive-a", &budget);
    let sibling = raw_budget_store(backing.clone(), "budget-scope/drive-b", &budget);
    let other = raw_budget_store(other_backing.clone(), "budget-scope/drive-a", &budget);
    let missing = raw_budget_store(backing.clone(), "budget-scope/missing", &budget);
    let id = BlockId("b0123456789abcdef0123456789abcdef".to_owned());

    // Legacy IDs may legitimately name different bytes in different authorities.
    for (remote, store, body) in [
        (backing.as_ref(), &first, b"alpha".as_slice()),
        (backing.as_ref(), &sibling, b"bravo".as_slice()),
        (other_backing.as_ref(), &other, b"other".as_slice()),
    ] {
        remote
            .put(
                &store.object_path(&id).unwrap(),
                PutPayload::from(body.to_vec()),
            )
            .await
            .unwrap();
        assert_eq!(store.get(&id).await.unwrap(), body);
    }
    // Reads after competition must still select each store's own authority.
    assert_eq!(first.get(&id).await.unwrap(), b"alpha");
    assert_eq!(sibling.get(&id).await.unwrap(), b"bravo");
    assert_eq!(other.get(&id).await.unwrap(), b"other");
    assert!(missing.get(&id).await.unwrap_err().is(ErrorCode::Enoent));
    assert_eq!(actual_raw_cache_residency(&[&missing]).entries, 0);
    assert_raw_budget_backing_bytes(backing.as_ref(), &first, &id, b"alpha").await;
    assert_raw_budget_backing_bytes(backing.as_ref(), &sibling, &id, b"bravo").await;
    assert_raw_budget_backing_bytes(other_backing.as_ref(), &other, &id, b"other").await;

    let actual = actual_raw_cache_residency(&[&first, &sibling, &other]);
    assert!(
        actual.charged_bytes <= budget.max_charged_bytes()
            && actual.entries <= budget.max_entries(),
        "isolated prefix/backing caches over-admitted their common capacity: {actual:?}"
    );
}

#[test]
fn shared_raw_cache_budget_exact_fit_overflow_and_refusal_do_not_leak_credit() {
    let budget = RawBlockCacheBudget::new(RAW_BUDGET_TEST_ENTRY_CHARGE + 2, 3);
    let cache =
        ObjectStoreBlockCache::new_with_observer_and_budget(&Observer::disabled(), budget.clone());
    cache.insert("exact", b"ab", None);
    assert_eq!(cache.get("exact").as_deref(), Some(b"ab".as_slice()));
    let exact = budget.snapshot().unwrap();
    assert_eq!(
        (exact.charged_bytes, exact.payload_bytes, exact.entries),
        (130, 2, 1)
    );
    assert!(matches!(
        budget.try_reserve(usize::MAX),
        RawBlockCacheReservationAttempt::AtCapacity { .. }
    ));
    assert!(matches!(
        budget.try_reserve(0),
        RawBlockCacheReservationAttempt::AtCapacity { .. }
    ));
    let refused = budget.snapshot().unwrap();
    assert_eq!(
        (
            refused.charged_bytes,
            refused.payload_bytes,
            refused.entries
        ),
        (130, 2, 1),
        "byte refusal must not consume entry or payload credit"
    );
    assert_eq!(refused.rejected_reservations, 2);
    assert!(!budget.is_unavailable());
    drop(cache);
    let empty = budget.snapshot().unwrap();
    assert_eq!(
        (empty.charged_bytes, empty.payload_bytes, empty.entries),
        (0, 0, 0)
    );

    let reservation = match budget.try_reserve(0) {
        RawBlockCacheReservationAttempt::Reserved(reservation) => reservation,
        _ => panic!("empty payload must fit after actual owner release"),
    };
    let reserved = budget.snapshot().unwrap();
    assert_eq!(
        (
            reserved.charged_bytes,
            reserved.payload_bytes,
            reserved.entries
        ),
        (129, 0, 1)
    );
    drop(reservation);
    assert_eq!(budget.snapshot().unwrap().charged_bytes, 0);
    assert_eq!(budget.snapshot().unwrap().entries, 0);
}

#[test]
fn shared_raw_cache_budget_either_zero_limit_disables_copy_and_admission() {
    for budget in [
        RawBlockCacheBudget::new(0, 8),
        RawBlockCacheBudget::new(1024, 0),
    ] {
        let local = LocalState::default();
        let observer = Observer::disabled();
        let cache = ObjectStoreBlockCache::new_with_observer_and_budget(&observer, budget.clone());
        cache.insert("empty", b"", Some(&local));
        cache.insert("nonempty", b"data", Some(&local));
        assert!(cache.get("empty").is_none());
        assert!(cache.get("nonempty").is_none());
        assert!(observer.snapshot().is_none());
        assert_eq!(local.snapshot().entries[Local::CacheCopy as usize].calls, 0);
        let disabled = budget.snapshot().unwrap();
        assert_eq!(
            (
                disabled.charged_bytes,
                disabled.payload_bytes,
                disabled.entries
            ),
            (0, 0, 0)
        );
        assert_eq!(disabled.admission_skips, 2);
    }
}

#[test]
fn shared_raw_cache_budget_rejected_admission_does_not_copy_payload() {
    let budget = RawBlockCacheBudget::new(RAW_BUDGET_TEST_ENTRY_CHARGE + 1, 8);
    let observer = Observer::disabled();
    let accepted_work = LocalState::default();
    let refused_work = LocalState::default();
    let first = ObjectStoreBlockCache::new_with_observer_and_budget(&observer, budget.clone());
    let second = ObjectStoreBlockCache::new_with_observer_and_budget(&observer, budget.clone());
    first.insert("accepted", b"a", Some(&accepted_work));
    second.insert("refused", b"b", Some(&refused_work));
    let accepted = &accepted_work.snapshot().entries[Local::CacheCopy as usize];
    assert_eq!((accepted.calls, accepted.output_bytes), (1, 1));
    let refused = &refused_work.snapshot().entries[Local::CacheCopy as usize];
    assert_eq!(
        (refused.calls, refused.input_bytes, refused.output_bytes),
        (0, 0, 0)
    );
    assert_eq!(first.get("accepted").as_deref(), Some(b"a".as_slice()));
    assert!(second.get("refused").is_none());
    let state = budget.snapshot().unwrap();
    assert_eq!((state.charged_bytes, state.entries), (129, 1));
    assert_eq!((state.rejected_reservations, state.admission_skips), (1, 1));
}

#[test]
fn shared_raw_cache_budget_busy_bank_bypasses_without_blocking_or_copying() {
    let budget = RawBlockCacheBudget::new(1024, 8);
    let cache = Arc::new(ObjectStoreBlockCache::new_with_observer_and_budget(
        &Observer::disabled(),
        budget.clone(),
    ));
    cache.insert("existing", b"a", None);
    let guard = budget.inner.state.lock().unwrap();
    let competing = Arc::clone(&cache);
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let work = LocalState::default();
        competing.insert("busy", b"new", Some(&work));
        sender
            .send(work.snapshot().entries[Local::CacheCopy as usize].calls)
            .unwrap();
    });
    let result = receiver.recv_timeout(Duration::from_secs(2));
    // Release before joining or asserting so a regression cannot strand a thread.
    drop(guard);
    worker.join().unwrap();
    assert_eq!(
        result.unwrap(),
        0,
        "admission must return while the bank remains locked"
    );
    assert_eq!(cache.get("existing").as_deref(), Some(b"a".as_slice()));
    assert!(cache.get("busy").is_none());
    let state = budget.snapshot().unwrap();
    assert_eq!((state.charged_bytes, state.entries), (129, 1));
    assert_eq!((state.contention_rejections, state.admission_skips), (1, 1));
    assert!(!budget.is_unavailable());
}

#[test]
fn shared_raw_cache_budget_concurrent_prefix_admissions_keep_bounded_high_water() {
    const OWNERS: usize = 16;
    const PAYLOAD_BYTES: usize = 4096;
    const ADMITTED_ENTRIES: usize = 4;
    let budget = RawBlockCacheBudget::new(
        ADMITTED_ENTRIES * (PAYLOAD_BYTES + RAW_BUDGET_TEST_ENTRY_CHARGE),
        ADMITTED_ENTRIES,
    );
    let backing = Arc::new(object_store::memory::InMemory::new());
    let stores: Vec<_> = (0..OWNERS)
        .map(|index| {
            Arc::new(raw_budget_store(
                backing.clone(),
                &format!("budget-race/{index}"),
                &budget,
            ))
        })
        .collect();
    let payload = Arc::new(vec![7; PAYLOAD_BYTES]);
    // A real admitted entry prevents an all-bypass run from masquerading as a
    // working cache; the other fifteen independent owners race for capacity.
    stores[0].cache.insert("seed", payload.as_slice(), None);
    let barrier = Arc::new(std::sync::Barrier::new(OWNERS - 1));
    let workers: Vec<_> = stores[1..]
        .iter()
        .map(|store| {
            let store = Arc::clone(store);
            let payload = Arc::clone(&payload);
            let barrier = Arc::clone(&barrier);
            let budget = budget.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.cache.insert("racing", payload.as_slice(), None);
                let snapshot = budget.snapshot().unwrap();
                assert!(snapshot.charged_bytes <= budget.max_charged_bytes());
                assert!(snapshot.entries <= budget.max_entries());
                assert!(snapshot.high_water_charged_bytes <= budget.max_charged_bytes());
                assert!(snapshot.high_water_entries <= budget.max_entries());
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let owners: Vec<_> = stores.iter().map(Arc::as_ref).collect();
    let actual = actual_raw_cache_residency(&owners);
    assert!(actual.entries >= 1);
    assert!(actual.entries <= ADMITTED_ENTRIES);
    assert!(actual.charged_bytes <= budget.max_charged_bytes());
    let settled = budget.snapshot().unwrap();
    assert_eq!(
        (
            settled.charged_bytes,
            settled.payload_bytes,
            settled.entries
        ),
        (actual.charged_bytes, actual.payload_bytes, actual.entries)
    );
    assert!(settled.high_water_charged_bytes >= settled.charged_bytes);
    assert!(settled.high_water_entries >= settled.entries);
    assert!(settled.admission_skips > 0);
    drop(owners);
    drop(stores);
    let released = budget.snapshot().unwrap();
    assert_eq!(
        (
            released.charged_bytes,
            released.payload_bytes,
            released.entries
        ),
        (0, 0, 0)
    );
}

#[tokio::test]
async fn shared_raw_cache_budget_clone_retains_credit_until_final_owner_drop() {
    let backing = Arc::new(object_store::memory::InMemory::new());
    let budget = RawBlockCacheBudget::new(RAW_BUDGET_TEST_ENTRY_CHARGE + 1, 8);
    let original = raw_budget_store(backing.clone(), "budget-generation/blocks", &budget);
    let first_id = original.put(b"a").await.unwrap();
    let weak = Arc::downgrade(&original.cache);
    let retained = original.clone();
    drop(original);
    assert!(weak.upgrade().is_some());
    assert_eq!(budget.snapshot().unwrap().entries, 1);

    let next = raw_budget_store(backing.clone(), "budget-generation/blocks", &budget);
    let next_id = next.put(b"b").await.unwrap();
    assert_eq!(next.get(&next_id).await.unwrap(), b"b");
    assert!(next.cache.get(&next_id.0).is_none());
    assert_eq!(retained.get(&first_id).await.unwrap(), b"a");
    assert_raw_budget_backing_bytes(backing.as_ref(), &next, &next_id, b"b").await;
    assert_eq!(budget.snapshot().unwrap().entries, 1);
    drop(retained);
    assert!(weak.upgrade().is_none());
    assert_eq!(budget.snapshot().unwrap().entries, 0);
    assert_eq!(next.get(&next_id).await.unwrap(), b"b");
    assert!(next.cache.get(&next_id.0).is_some());
    assert_eq!(budget.snapshot().unwrap().entries, 1);
    drop(next);
    assert_eq!(budget.snapshot().unwrap().charged_bytes, 0);
}

#[test]
fn shared_raw_cache_budget_replacement_eviction_and_removal_release_once() {
    let budget = RawBlockCacheBudget::new(2 * (RAW_BUDGET_TEST_ENTRY_CHARGE + 2), 8);
    let cache =
        ObjectStoreBlockCache::new_with_observer_and_budget(&Observer::disabled(), budget.clone());
    cache.insert("one", b"ab", None);
    cache.insert("two", b"cd", None);
    assert_eq!(
        (
            budget.snapshot().unwrap().charged_bytes,
            budget.snapshot().unwrap().entries
        ),
        (260, 2)
    );
    cache.insert("one", b"x", None);
    assert_eq!(cache.get("one").as_deref(), Some(b"x".as_slice()));
    assert_eq!(
        (
            budget.snapshot().unwrap().charged_bytes,
            budget.snapshot().unwrap().entries
        ),
        (259, 2)
    );
    cache.insert("three", b"ef", None);
    assert!(cache.get("two").is_none());
    assert_eq!(cache.get("one").as_deref(), Some(b"x".as_slice()));
    assert_eq!(cache.get("three").as_deref(), Some(b"ef".as_slice()));
    assert_eq!(
        (
            budget.snapshot().unwrap().charged_bytes,
            budget.snapshot().unwrap().entries
        ),
        (259, 2)
    );
    cache.remove("one", None);
    cache.remove("one", None);
    assert_eq!(
        (
            budget.snapshot().unwrap().charged_bytes,
            budget.snapshot().unwrap().entries
        ),
        (130, 1)
    );
    drop(cache);
    assert_eq!(
        (
            budget.snapshot().unwrap().charged_bytes,
            budget.snapshot().unwrap().entries
        ),
        (0, 0)
    );
    assert!(!budget.is_unavailable());

    let tiny = RawBlockCacheBudget::new(RAW_BUDGET_TEST_ENTRY_CHARGE + 1, 8);
    let preserved =
        ObjectStoreBlockCache::new_with_observer_and_budget(&Observer::disabled(), tiny.clone());
    preserved.insert("one", b"a", None);
    preserved.insert("one", b"too large", None);
    assert_eq!(preserved.get("one").as_deref(), Some(b"a".as_slice()));
    assert_eq!(
        (
            tiny.snapshot().unwrap().charged_bytes,
            tiny.snapshot().unwrap().entries
        ),
        (129, 1)
    );
}

#[tokio::test]
async fn shared_raw_cache_budget_poisoned_prefix_retains_credit_until_destruction() {
    let backing = Arc::new(object_store::memory::InMemory::new());
    let budget = RawBlockCacheBudget::new(RAW_BUDGET_TEST_ENTRY_CHARGE + 1, 8);
    let first = raw_budget_store(backing.clone(), "budget-poison/blocks", &budget);
    let id = first.put(b"a").await.unwrap();
    let weak = Arc::downgrade(&first.cache);
    let poisoned = Arc::clone(&first.cache);
    assert!(
        std::thread::spawn(move || {
            let _state = poisoned.state.lock().unwrap();
            panic!("poison a cache containing a genuinely credited entry");
        })
        .join()
        .is_err()
    );
    assert!(first.cache.get(&id.0).is_none());
    assert_eq!(first.get(&id).await.unwrap(), b"a");
    assert_eq!(
        (
            budget.snapshot().unwrap().charged_bytes,
            budget.snapshot().unwrap().entries
        ),
        (129, 1)
    );
    let next = raw_budget_store(backing, "budget-poison/blocks", &budget);
    assert_eq!(next.get(&id).await.unwrap(), b"a");
    assert!(next.cache.get(&id.0).is_none());
    assert_eq!(budget.snapshot().unwrap().entries, 1);
    drop(first);
    assert!(weak.upgrade().is_none());
    assert_eq!(budget.snapshot().unwrap().charged_bytes, 0);
    assert_eq!(next.get(&id).await.unwrap(), b"a");
    assert!(next.cache.get(&id.0).is_some());
    drop(next);
    assert_eq!(budget.snapshot().unwrap().charged_bytes, 0);
    assert!(!budget.is_unavailable());
}

#[test]
fn shared_raw_cache_budget_bank_poison_and_underflow_fail_closed() {
    let poisoned_budget = RawBlockCacheBudget::new(1024, 8);
    let cache = ObjectStoreBlockCache::new_with_observer_and_budget(
        &Observer::disabled(),
        poisoned_budget.clone(),
    );
    cache.insert("retained", b"a", None);
    let poisoned = poisoned_budget.clone();
    assert!(
        std::thread::spawn(move || {
            let _bank = poisoned.inner.state.lock().unwrap();
            panic!("poison the common reservation bank");
        })
        .join()
        .is_err()
    );
    assert!(poisoned_budget.snapshot().is_none());
    assert!(poisoned_budget.is_unavailable());
    cache.insert("refused", b"b", None);
    assert!(cache.get("refused").is_none());
    assert_eq!(cache.get("retained").as_deref(), Some(b"a".as_slice()));
    assert_eq!(poisoned_budget.admission_skips(), 1);
    drop(cache);
    assert!(poisoned_budget.snapshot().is_none());
    assert!(matches!(
        poisoned_budget.try_reserve(1),
        RawBlockCacheReservationAttempt::Unavailable
    ));

    let underflow = RawBlockCacheBudget::new(1024, 8);
    let cache = ObjectStoreBlockCache::new_with_observer_and_budget(
        &Observer::disabled(),
        underflow.clone(),
    );
    cache.insert("retained", b"a", None);
    // Fault injection into the release boundary must never create reusable
    // capacity or saturate an invalid subtraction into a fresh zero balance.
    underflow.release(130, 1);
    assert!(underflow.is_unavailable());
    assert!(underflow.snapshot().is_none());
    assert!(matches!(
        underflow.try_reserve(1),
        RawBlockCacheReservationAttempt::Unavailable
    ));
    assert_eq!(cache.get("retained").as_deref(), Some(b"a".as_slice()));
    drop(cache);
    assert!(underflow.snapshot().is_none());
}

#[test]
fn shared_raw_cache_budget_unwound_reservation_returns_all_credit() {
    let budget = RawBlockCacheBudget::new(RAW_BUDGET_TEST_ENTRY_CHARGE + 1, 1);
    let unwind = budget.clone();
    assert!(
        std::panic::catch_unwind(move || {
            let _reservation = match unwind.try_reserve(1) {
                RawBlockCacheReservationAttempt::Reserved(reservation) => reservation,
                _ => panic!("fresh capacity must admit the unwind control"),
            };
            assert_eq!(unwind.snapshot().unwrap().entries, 1);
            panic!("cancel insertion after reservation and before payload copy");
        })
        .is_err()
    );
    let released = budget.snapshot().unwrap();
    assert_eq!(
        (
            released.charged_bytes,
            released.payload_bytes,
            released.entries
        ),
        (0, 0, 0)
    );
    assert!(!budget.is_unavailable());
    let cache =
        ObjectStoreBlockCache::new_with_observer_and_budget(&Observer::disabled(), budget.clone());
    cache.insert("after-unwind", b"a", None);
    assert_eq!(cache.get("after-unwind").as_deref(), Some(b"a".as_slice()));
    assert_eq!(budget.snapshot().unwrap().entries, 1);
}

#[test]
fn shared_raw_cache_budget_full_width_charges_cannot_wrap_into_capacity() {
    let budget = RawBlockCacheBudget::new(usize::MAX, usize::MAX);
    for overflow in [usize::MAX, usize::MAX - RAW_BUDGET_TEST_ENTRY_CHARGE + 1] {
        assert!(matches!(
            budget.try_reserve(overflow),
            RawBlockCacheReservationAttempt::AtCapacity { .. }
        ));
        let empty = budget.snapshot().unwrap();
        assert_eq!(
            (empty.charged_bytes, empty.payload_bytes, empty.entries),
            (0, 0, 0)
        );
    }
    // Reserve arithmetic only: no enormous payload allocation or copy occurs.
    let payload = usize::MAX - RAW_BUDGET_TEST_ENTRY_CHARGE;
    let exact = match budget.try_reserve(payload) {
        RawBlockCacheReservationAttempt::Reserved(reservation) => reservation,
        _ => panic!("the largest representable non-overflowing charge must fit"),
    };
    let full = budget.snapshot().unwrap();
    assert_eq!(
        (full.charged_bytes, full.payload_bytes, full.entries),
        (usize::MAX, payload, 1)
    );
    assert!(matches!(
        budget.try_reserve(0),
        RawBlockCacheReservationAttempt::AtCapacity { .. }
    ));
    assert_eq!(budget.snapshot().unwrap().entries, 1);
    drop(exact);
    assert_eq!(budget.snapshot().unwrap().charged_bytes, 0);
    assert_eq!(budget.snapshot().unwrap().entries, 0);
    assert!(!budget.is_unavailable());
}

#[test]
fn shared_raw_cache_budget_failed_construction_never_acquires_entry_credit() {
    let budget = RawBlockCacheBudget::new(RAW_BUDGET_TEST_ENTRY_CHARGE + 1, 1);
    let backing = Arc::new(object_store::memory::InMemory::new());
    assert!(
        ObjectStoreBlockStore::new_with_cache_budget(
            backing.clone(),
            "drive/../other",
            false,
            budget.clone(),
        )
        .is_err()
    );
    let empty = budget.snapshot().unwrap();
    assert_eq!(
        (empty.charged_bytes, empty.payload_bytes, empty.entries),
        (0, 0, 0)
    );
    let valid = raw_budget_store(backing, "budget-construction/blocks", &budget);
    valid.cache.insert("control", b"a", None);
    assert_eq!(budget.snapshot().unwrap().entries, 1);
    drop(valid);
    assert_eq!(budget.snapshot().unwrap().entries, 0);
}

#[test]
fn shared_raw_cache_budget_concurrent_replacement_and_release_keep_bounded_high_water() {
    const OWNERS: usize = 8;
    let budget = RawBlockCacheBudget::new(4 * (RAW_BUDGET_TEST_ENTRY_CHARGE + 16), 4);
    let backing = Arc::new(object_store::memory::InMemory::new());
    let stores: Vec<_> = (0..OWNERS)
        .map(|index| {
            Arc::new(raw_budget_store(
                backing.clone(),
                &format!("budget-release-race/{index}"),
                &budget,
            ))
        })
        .collect();
    stores[0].cache.insert("seed", b"a", None);
    assert_eq!(budget.snapshot().unwrap().entries, 1);
    let barrier = Arc::new(std::sync::Barrier::new(OWNERS));
    let workers: Vec<_> = stores
        .iter()
        .enumerate()
        .map(|(index, store)| {
            let store = Arc::clone(store);
            let barrier = Arc::clone(&barrier);
            let budget = budget.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.cache.remove("seed", None);
                for round in 0..64 {
                    let value = (index + round) as u8;
                    store.cache.insert("active", &[value; 16], None);
                    store.cache.insert("active", &[value], None);
                    let replacing = budget.snapshot().unwrap();
                    assert!(replacing.charged_bytes <= budget.max_charged_bytes());
                    assert!(replacing.entries <= budget.max_entries());
                    assert!(replacing.high_water_charged_bytes <= budget.max_charged_bytes());
                    assert!(replacing.high_water_entries <= budget.max_entries());
                    store.cache.remove("active", None);
                    store.cache.insert("empty", b"", None);
                    store.cache.remove("empty", None);
                    let releasing = budget.snapshot().unwrap();
                    assert!(releasing.charged_bytes <= budget.max_charged_bytes());
                    assert!(releasing.entries <= budget.max_entries());
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let owners: Vec<_> = stores.iter().map(Arc::as_ref).collect();
    let actual = actual_raw_cache_residency(&owners);
    assert_eq!(
        (actual.charged_bytes, actual.payload_bytes, actual.entries),
        (0, 0, 0)
    );
    let settled = budget.snapshot().unwrap();
    assert_eq!(
        (
            settled.charged_bytes,
            settled.payload_bytes,
            settled.entries
        ),
        (0, 0, 0)
    );
    assert!(settled.high_water_charged_bytes > RAW_BUDGET_TEST_ENTRY_CHARGE);
    assert!(settled.high_water_charged_bytes <= budget.max_charged_bytes());
    assert!(settled.high_water_entries <= budget.max_entries());
    assert!(!budget.is_unavailable());
}
