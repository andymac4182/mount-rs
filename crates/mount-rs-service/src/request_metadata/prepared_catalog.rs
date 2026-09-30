//! Candidate selection belongs to one exact immutable catalog authority.
use crate::catalog::{CatalogError, CatalogSnapshot, GrantDefinition};
use std::{
    collections::BTreeMap,
    sync::{Arc, LockResult, Mutex, RwLock, TryLockError, TryLockResult},
};

type GrantScopes = BTreeMap<String, BTreeMap<String, Vec<String>>>;

/// Preparation selects only scope; claims, Drive membership and permissions
/// remain request-time decisions. The public mutable/serialized snapshot has no
/// derived index that could survive an edit to one of its maps.
pub(crate) struct PreparedCatalog {
    source: Arc<CatalogSnapshot>,
    scopes: Option<GrantScopes>,
}

impl PreparedCatalog {
    fn new(source: Arc<CatalogSnapshot>) -> Self {
        let mut scopes = GrantScopes::new();
        // Source iteration gives every scope the original ascending grant-ID order.
        // Each grant is indexed once, irrespective of its number of Drives.
        for (id, grant) in &source.grants {
            scopes
                .entry(grant.partition_id.clone())
                .or_default()
                .entry(grant.policy_id.clone())
                .or_default()
                .push(id.clone());
        }
        Self {
            source,
            scopes: Some(scopes),
        }
    }

    fn full_scan(source: Arc<CatalogSnapshot>) -> Self {
        Self {
            source,
            scopes: None,
        }
    }

    pub(crate) fn snapshot(&self) -> &CatalogSnapshot {
        &self.source
    }

    pub(crate) fn candidates(&self, partition: &str, policy: &str) -> GrantCandidates<'_> {
        let Some(scopes) = self.scopes.as_ref() else {
            return GrantCandidates::Full(self.source.grants.iter());
        };
        let ids = scopes
            .get(partition)
            .and_then(|policies| policies.get(policy))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        GrantCandidates::Scoped {
            source: &self.source.grants,
            ids: ids.iter(),
        }
    }
}

pub(crate) enum GrantCandidates<'a> {
    Scoped {
        source: &'a BTreeMap<String, GrantDefinition>,
        ids: std::slice::Iter<'a, String>,
    },
    Full(std::collections::btree_map::Iter<'a, String, GrantDefinition>),
}
impl<'a> Iterator for GrantCandidates<'a> {
    type Item = (&'a str, &'a GrantDefinition);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Full(grants) => grants.next().map(|(id, grant)| (id.as_str(), grant)),
            Self::Scoped { source, ids } => {
                let id = ids.next()?;
                // IDs were copied from this retained immutable source, never a revision peer.
                let (id, grant) = source
                    .get_key_value(id)
                    .expect("prepared grant ID belongs to its immutable source");
                Some((id.as_str(), grant))
            }
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Full(grants) => grants.size_hint(),
            Self::Scoped { ids, .. } => ids.size_hint(),
        }
    }
}
impl ExactSizeIterator for GrantCandidates<'_> {}
impl std::iter::FusedIterator for GrantCandidates<'_> {}

pub(crate) struct PreparedCatalogRead {
    pub(crate) catalog: Arc<PreparedCatalog>,
    pub(crate) waited: bool,
}

// Synchronous cache contention is also an authorization observation boundary.
// Report it so dispatch can repeat expiration/policy/current-authority checks.
fn observed_lock<G>(
    attempted: TryLockResult<G>,
    blocking: impl FnOnce() -> LockResult<G>,
    waited: &mut bool,
) -> Result<G, CatalogError> {
    match attempted {
        Ok(guard) => Ok(guard),
        Err(TryLockError::Poisoned(_)) => {
            Err(CatalogError::Invalid("grant candidate index unavailable"))
        }
        Err(TryLockError::WouldBlock) => {
            *waited = true;
            #[cfg(test)]
            fire_preparation_hook(PreparationTestPoint::ContendedLock);
            blocking().map_err(|_| CatalogError::Invalid("grant candidate index unavailable"))
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparationTestPoint {
    BeforeBuild,
    ContendedLock,
}
#[cfg(test)]
type PreparationTestHook = Box<dyn FnMut(PreparationTestPoint)>;
#[cfg(test)]
thread_local! {
    static PREPARATION_HOOK: std::cell::RefCell<Option<PreparationTestHook>> =
        const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(crate) fn set_preparation_hook(hook: Box<dyn FnMut(PreparationTestPoint)>) {
    PREPARATION_HOOK.with(|current| *current.borrow_mut() = Some(hook));
}
#[cfg(test)]
fn fire_preparation_hook(point: PreparationTestPoint) {
    PREPARATION_HOOK.with(|current| {
        if let Some(hook) = current.borrow_mut().as_mut() {
            hook(point);
        }
    });
}

/// One published entry and one serialized cold preparation. Readers retain their
/// own authority/index pair after a catalog replacement; they never borrow a cache
/// guard across authorization, awaits, or audit writer acquisition.
#[derive(Default)]
pub(crate) struct PreparedCatalogCache {
    current: RwLock<Option<Arc<PreparedCatalog>>>,
    // Prevent a catalog change from causing every concurrent client to build O(G).
    // Build outside the published-entry RwLock so old-source hits can still proceed.
    build: Mutex<()>,
}
impl PreparedCatalogCache {
    fn cached(
        &self,
        source: &Arc<CatalogSnapshot>,
        waited: &mut bool,
    ) -> Result<Option<Arc<PreparedCatalog>>, CatalogError> {
        let current = observed_lock(self.current.try_read(), || self.current.read(), waited)?;
        Ok(current
            .as_ref()
            .filter(|prepared| Arc::ptr_eq(&prepared.source, source))
            .map(Arc::clone))
    }

    pub(crate) fn prepare(
        &self,
        source: Arc<CatalogSnapshot>,
    ) -> Result<PreparedCatalogRead, CatalogError> {
        // CatalogStore's default shared load returns a new, uniquely owned Arc
        // each time. Preserve its full-map behavior without a shared cache gate:
        // a contention retry otherwise loads another fresh Arc and can starve.
        // This observation selects traversal only; both paths retain exact authority.
        if Arc::strong_count(&source) == 1 {
            return Ok(PreparedCatalogRead {
                catalog: Arc::new(PreparedCatalog::full_scan(source)),
                waited: false,
            });
        }
        let mut waited = false;
        if let Some(catalog) = self.cached(&source, &mut waited)? {
            return Ok(PreparedCatalogRead { catalog, waited });
        }
        let _build = observed_lock(self.build.try_lock(), || self.build.lock(), &mut waited)?;
        if let Some(catalog) = self.cached(&source, &mut waited)? {
            return Ok(PreparedCatalogRead { catalog, waited });
        }
        #[cfg(test)]
        fire_preparation_hook(PreparationTestPoint::BeforeBuild);
        let prepared = Arc::new(PreparedCatalog::new(source));
        let mut current = observed_lock(
            self.current.try_write(),
            || self.current.write(),
            &mut waited,
        )?;
        let prior = current.replace(Arc::clone(&prepared));
        drop(current);
        // Deallocate an obsolete index after releasing the published-entry lock.
        drop(prior);
        Ok(PreparedCatalogRead {
            catalog: prepared,
            waited,
        })
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
#[path = "prepared_catalog/paired_benchmark.rs"]
mod paired_benchmark;
