//! FoundationDB-backed `mount-rs` metadata and immutable block stores.
//!
//! The provider is registered in the core workspace, but its native client is
//! still opt-in. Managed constructors boot one shared native client network.
//! Applications must drop every provider handle and stop/join that network at
//! their terminal process boundary with [`shutdown_client_network`].
//!
//! Without the `foundationdb` feature, or on a target the native client does
//! not support, the crate has no public items: only the lifecycle rules of
//! the client network (`network_gate`) are compiled, so they can be tested
//! without the native client library.

// The lifecycle has no caller when the provider is not compiled.
#![cfg_attr(
    not(all(
        feature = "foundationdb",
        any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
        )
    )),
    allow(dead_code)
)]

mod network_gate;

#[cfg(all(
    feature = "foundationdb",
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )
))]
mod provider;

#[cfg(all(
    feature = "foundationdb",
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )
))]
pub use provider::*;
