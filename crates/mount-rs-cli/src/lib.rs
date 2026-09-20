//! Command-line entry point and scaffolding for mount-rs.
//!
//! The parser, help/version rendering, probe rendering, and watcher are kept
//! importable so they can be tested without creating a native mount. The
//! `run` function is the only path that creates a mount, and it does so only
//! for the explicit mount command (the default command is also a mount, just
//! like the upstream `mountx` CLI).

pub mod color;
pub mod parser;
pub mod runtime;
mod stale;
pub mod watch;

pub use parser::{CliOptions, Command, DriverChoice, ParseError, TransportChoice, parse_args};
pub use runtime::{CliError, run};
