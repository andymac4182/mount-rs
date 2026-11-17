//! The suite in `suites/launch_contract.rs` against the listener's own routes.

#[path = "support/serving.rs"]
mod serving;
#[path = "suites/launch_contract.rs"]
mod suite;

const BEHIND_HANDLER: bool = false;
