//! Ownership hooks for fallible provider and filesystem construction.
//!
//! These hooks run only during construction. They introduce no filesystem or
//! storage method forwarding, global resource registry, or executor dependency.

use std::sync::Arc;

use async_trait::async_trait;

use crate::Result;

/// The actual owner of a partially constructed resource or storage authority.
///
/// A successful close must acknowledge release of this owner's resources. An
/// unknown acquisition, ambiguous checkout or failed authority must return an
/// error and keep the resources required for reconciliation alive. Cleanup
/// callers must stop before closing dependent providers when authority cleanup
/// is unproven. An old token snapshot must not substitute for the current owner.
#[async_trait]
pub trait ConstructionResource: Send + Sync {
    async fn close(&self) -> Result<()>;
}

/// An application-retained journal, established before polling construction.
///
/// Constructors transfer a reference to their actual owner synchronously before
/// a later await can fail or be canceled. An authority owner must record its
/// acquisition intent before awaiting acquisition, then track the exact latest
/// acknowledged authority or the actual filesystem that assumes it.
///
/// `retain` must take ownership without panicking or rejecting the resource. A
/// constructor must not assume that returning an error, dropping its future or
/// dropping a caller's waiter proves cleanup. The journal's lifetime and owned
/// asynchronous cleanup remain the application's responsibility.
pub trait ConstructionObserver: Send + Sync {
    fn retain(&self, resource: Arc<dyn ConstructionResource>);
}
