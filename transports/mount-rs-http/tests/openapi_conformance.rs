//! The suite in `suites/openapi_conformance.rs` against the listener's own
//! routes.

#[path = "support/blackbox.rs"]
mod blackbox;
#[path = "support/openapi.rs"]
mod openapi;
#[path = "support/serving.rs"]
mod serving;
#[path = "suites/openapi_conformance.rs"]
mod suite;

const BEHIND_HANDLER: bool = false;
