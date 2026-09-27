//! Source-only draft until the plan's explicit build and native leases are granted.
mod ten_process_cache_support;

#[cfg(all(
    feature = "local-oidc-fixture",
    debug_assertions,
    any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))
))]
#[test]
#[ignore = "ten owned CLI processes; requires the plan's explicit native lease"]
fn ten_process_sqlite_cache_qualification() {
    ten_process_cache_support::process::supervise();
}

#[cfg(all(
    feature = "local-oidc-fixture",
    debug_assertions,
    any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))
))]
#[test]
#[ignore = "private worker entry; the supervisor owns its process group"]
fn native_worker() {
    ten_process_cache_support::scenario::worker();
}
