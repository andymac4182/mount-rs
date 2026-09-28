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
    let poisoned = Arc::clone(&cache);
    assert!(
        std::thread::spawn(move || {
            let mut state = poisoned.state.lock().unwrap();
            state.entries.insert("partial".into(), vec![9]);
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
