//! The suite in `suites/openapi_conformance.rs` against a server whose
//! listener hands each request to a handler, as it does with
//! `remote_sessions` or `forwarding` configured. See `support/serving.rs`.

#[path = "support/blackbox.rs"]
mod blackbox;
#[path = "support/openapi.rs"]
mod openapi;
#[path = "support/serving.rs"]
mod serving;
#[path = "suites/openapi_conformance.rs"]
mod suite;

const BEHIND_HANDLER: bool = true;
