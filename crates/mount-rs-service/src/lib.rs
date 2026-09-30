//! Server-owned Drive catalog and remote mount service.

pub mod auth;
pub mod catalog;
pub mod dispatch;
#[cfg(feature = "sdk-runtime")]
pub mod filesystem_runtime;
#[doc(hidden)]
pub mod object_store_diagnostics;
pub mod runtime_diagnostics;
pub mod runtime_pool;
pub mod server;
#[doc(hidden)]
pub mod service_diagnostics_frames;
pub mod startup;

mod transfer;

mod request_metadata;

pub mod websocket;
