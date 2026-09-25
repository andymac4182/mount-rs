//! Allocator regression for request audit metadata. Counts only this thread.
use super::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
thread_local! {static COUNTS: Cell<(bool,usize,usize)> = const {Cell::new((false,0,0))};}
struct Counter;
unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let _ = COUNTS.try_with(|c| {
            let (on, n, b) = c.get();
            if on {
                c.set((true, n + 1, b + l.size()));
            }
        });
        unsafe { System.alloc(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let _ = COUNTS.try_with(|c| {
            let (on, a, b) = c.get();
            if on {
                c.set((true, a + 1, b + n));
            }
        });
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static ALLOCATOR: Counter = Counter;
pub(crate) fn count(work: impl FnOnce()) -> (usize, usize) {
    COUNTS.with(|c| c.set((true, 0, 0)));
    work();
    COUNTS.with(|c| {
        let (_, n, b) = c.get();
        c.set((false, 0, 0));
        (n, b)
    })
}
#[test]
fn audit_metadata_has_zero_heap_allocations() {
    use crate::catalog::{CatalogSnapshot, GrantDefinition, Permission};
    let identity = SessionIdentity {
        partition_id: "p\nwith\"escapes".into(),
        policy_id: "policy".into(),
        issuer: "https://issuer.example".into(),
        subject: "subject".into(),
        signing_algorithm: "RS256".into(),
        claims: serde_json::json!({"sub":"subject"}),
        expires_at: i64::MAX,
    };
    let mut catalog = CatalogSnapshot::empty();
    for name in ["first", "second"] {
        catalog.grants.insert(
            name.into(),
            GrantDefinition {
                partition_id: identity.partition_id.clone(),
                policy_id: "policy".into(),
                drives: std::collections::BTreeMap::from([("d".into(), Permission::Write)]),
                claim_conditions: std::collections::BTreeMap::from([(
                    "/sub".into(),
                    "subject".into(),
                )]),
            },
        );
    }
    let event = serde_json::json!({"event":"remote_access","partition_id":identity.partition_id,"drive_id":"d","grant_ids":["first","second"],"operation":"handle_write","request_id":42,"outcome":"ok"});
    let record = AuditRecord {
        event: "remote_access",
        partition_id: &identity.partition_id,
        drive_id: "d",
        grant_ids: MatchingGrants {
            catalog: &catalog,
            identity: &identity,
            drive_id: "d",
        },
        operation: OperationName::HandleWrite,
        request_id: 42,
        outcome: "ok",
    };
    let mut output = Vec::with_capacity(4096);
    let allocated = count(|| write_audit(&mut output, &record).unwrap());
    assert_eq!(allocated, (0, 0), "audit metadata allocates");
    assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), event);
}
#[test]
fn borrowed_policy_matching_allocates_nothing_and_rejects_bad_shapes() {
    let identity = SessionIdentity {
        partition_id: "p".into(),
        policy_id: "policy".into(),
        issuer: "https://issuer.example".into(),
        subject: "subject".into(),
        signing_algorithm: "RS256".into(),
        claims: serde_json::json!({"aud":["other","mount"]}),
        expires_at: i64::MAX,
    };
    let policy = serde_json::json!({"issuer":"https://issuer.example","audiences":["mount"]});
    assert_eq!(
        count(|| assert!(policy_matches(&policy, &identity))),
        (0, 0)
    );
    for invalid in [
        serde_json::json!({"issuer":"https://issuer.example","audiences":["mount"],"algorithms":null}),
        serde_json::json!({"issuer":"https://issuer.example","audiences":[42,"mount"]}),
        serde_json::json!({"issuer":"https://issuer.example","audiences":["mount"],"unexpected":true}),
    ] {
        assert!(!policy_matches(&invalid, &identity));
    }
}

#[test]
fn claim_pointer_matches_reference_and_allocates_nothing() {
    use crate::request_metadata::claim_pointer;
    let claims = serde_json::json!({"":true,"plain":"value","a/b":{"~key":"escaped","~1":"nested","~2":"literal","café":"utf8"},"array":["zero",{"deep":"one"}]});
    for pointer in [
        "",
        "/",
        "/plain",
        "/a~1b/~0key",
        "/a~1b/~01",
        "/a~1b/~2",
        "/a~1b/café",
        "/array/0",
        "/array/1/deep",
        "/array/01",
        "/array/+1",
        "/array/-1",
        "/array/99999999999999999999999",
        "/missing",
        "invalid",
    ] {
        let expected = claims.pointer(pointer);
        assert_eq!(
            count(|| assert_eq!(claim_pointer(&claims, pointer), expected)),
            (0, 0)
        );
    }
}
