//! The suite in `suites/launch_contract.rs` against a server whose listener
//! hands each request to a handler, as it does with `remote_sessions` or
//! `forwarding` configured. See `support/serving.rs`.

#[path = "support/serving.rs"]
mod serving;
#[path = "suites/launch_contract.rs"]
mod suite;

const BEHIND_HANDLER: bool = true;
