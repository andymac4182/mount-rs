//! Server-owned Drive catalog and remote mount service.

pub mod auth;
pub mod catalog;
pub mod dispatch;
pub mod runtime_diagnostics;
pub mod runtime_pool;
pub mod server;
pub mod startup;

mod transfer;

mod request_metadata;

pub mod websocket;
