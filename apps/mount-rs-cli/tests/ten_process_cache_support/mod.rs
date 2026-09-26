pub mod contracts;

#[cfg(all(
    feature = "local-oidc-fixture",
    debug_assertions,
    any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))
))]
pub mod config;
#[cfg(all(
    feature = "local-oidc-fixture",
    debug_assertions,
    any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))
))]
pub mod process;
#[cfg(all(
    feature = "local-oidc-fixture",
    debug_assertions,
    any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))
))]
pub mod scenario;
