//! Immutable advisory route snapshots. Readers do not take a map mutex.
use crate::VolumeRoute;
use arc_swap::ArcSwap;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;

pub(crate) struct CachedRoute {
    born: Instant,
    last_use_ns: AtomicU64,
    pub(crate) route: VolumeRoute,
    pub(crate) header: Option<http::HeaderValue>,
    expires: ArcSwap<Instant>,
}
impl CachedRoute {
    pub(crate) fn can_refresh(&self, now: Instant, idle: Duration) -> bool {
        now < **self.expires.load()
            && now.saturating_duration_since(
                self.born + Duration::from_nanos(self.last_use_ns.load(Ordering::Relaxed)),
            ) < idle
    }
    pub(crate) fn confirm(&self, started: Instant, ttl: Duration) -> bool {
        let now = Instant::now();
        if now >= **self.expires.load() || now >= started + ttl {
            return false;
        }
        self.expires.rcu(|old| Arc::new((**old).max(started + ttl)));
        true
    }
}

type Routes = BTreeMap<String, Arc<CachedRoute>>;
#[derive(Default)]
pub(crate) struct RouteCache {
    routes: ArcSwap<Routes>,
}
impl RouteCache {
    pub(crate) fn get(&self, volume: &str, now: Instant) -> Option<Arc<CachedRoute>> {
        self.routes
            .load()
            .get(volume)
            .filter(|entry| now < **entry.expires.load())
            .cloned()
    }
    pub(crate) fn get_active(&self, volume: &str, now: Instant) -> Option<Arc<CachedRoute>> {
        let snapshot = self.routes.load();
        let entry = snapshot.get(volume)?;
        if now >= **entry.expires.load() {
            return None;
        }
        entry.last_use_ns.fetch_max(
            now.saturating_duration_since(entry.born).as_nanos() as u64,
            Ordering::Relaxed,
        );
        Some(entry.clone())
    }
    pub(crate) fn insert(
        &self,
        route: &VolumeRoute,
        expires: Instant,
        maximum: usize,
    ) -> Arc<CachedRoute> {
        let entry = Arc::new(CachedRoute {
            born: Instant::now(),
            last_use_ns: AtomicU64::new(0),
            route: route.clone(),
            header: serde_json::to_string(route)
                .ok()
                .and_then(|value| value.parse().ok()),
            expires: ArcSwap::from_pointee(expires),
        });
        // RCU retries the entire update against the latest snapshot after CAS
        // contention. No external side effects belong in this closure.
        self.routes.rcu(|previous| {
            let mut routes = (**previous).clone();
            routes.retain(|_, entry| **entry.expires.load() > Instant::now());
            if routes
                .get(&route.volume)
                .is_some_and(|entry| entry.route.fence > route.fence)
            {
                return routes;
            }
            if routes.len() >= maximum && !routes.contains_key(&route.volume) {
                routes.pop_first();
            }
            routes.insert(route.volume.clone(), entry.clone());
            routes
        });
        entry
    }
    pub(crate) fn discard_entry(&self, route: &VolumeRoute, expected: &Arc<CachedRoute>) {
        self.routes.rcu(|previous| {
            let mut routes = (**previous).clone();
            if routes
                .get(&route.volume)
                .is_some_and(|e| Arc::ptr_eq(e, expected))
            {
                routes.remove(&route.volume);
            }
            routes
        });
    }
    pub(crate) fn invalidate(&self, route: &VolumeRoute) {
        self.routes.rcu(|previous| {
            let mut routes = (**previous).clone();
            if routes
                .get(&route.volume)
                .is_some_and(|entry| entry.route == *route)
            {
                routes.remove(&route.volume);
            }
            routes
        });
    }

    /// Renew only a still-present exact route after a trusted peer response.
    /// Never recreate an invalidated/evicted record or replace a new owner.
    pub(crate) fn renew(
        &self,
        route: &VolumeRoute,
        started: Instant,
        now: Instant,
        ttl: std::time::Duration,
    ) {
        let expires = started + ttl;
        if expires <= now {
            return;
        }
        let refresh_before = now + ttl / 2;
        let routes = self.routes.load();
        let Some(entry) = routes.get(&route.volume) else {
            return;
        };
        if entry.route != *route || **entry.expires.load() > refresh_before {
            return;
        }
        // Route identity is immutable. Renew only this entry, without copying
        // the volume map. Concurrent invalidation/replacement can detach it;
        // updating a detached entry cannot resurrect it or affect its successor.
        entry.expires.rcu(|previous| {
            if **previous <= refresh_before && **previous < expires {
                Arc::new(expires)
            } else {
                previous.clone()
            }
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn route(volume: String, fence: u64) -> VolumeRoute {
        VolumeRoute {
            volume,
            fence,
            node: "node".into(),
            boot: "boot".into(),
            registration: "registration".into(),
            address: "127.0.0.1:1234".parse().unwrap(),
            tls_server_name: "localhost".into(),
        }
    }
    #[test]
    fn concurrent_publications_preserve_other_volumes_and_newer_fences() {
        let cache = RouteCache::default();
        let expiry = Instant::now() + Duration::from_secs(60);
        std::thread::scope(|scope| {
            for i in 0..32 {
                let cache = &cache;
                scope.spawn(move || {
                    cache.insert(&route(format!("v{i}"), 1), expiry, 64);
                    cache.insert(&route("shared".into(), i + 1), expiry, 64);
                });
            }
        });
        for i in 0..32 {
            assert!(cache.get(&format!("v{i}"), Instant::now()).is_some());
        }
        assert_eq!(cache.get("shared", Instant::now()).unwrap().route.fence, 32);
        cache.invalidate(&route("shared".into(), 1));
        assert_eq!(cache.get("shared", Instant::now()).unwrap().route.fence, 32);
        cache.invalidate(&route("shared".into(), 32));
        assert!(cache.get("shared", Instant::now()).is_none());
    }
    #[test]
    fn response_renewal_cannot_resurrect_or_replace_routes() {
        let cache = RouteCache::default();
        let now = Instant::now();
        let old = route("volume".into(), 1);
        cache.insert(&old, now + Duration::from_secs(1), 2);
        let snapshot = cache.routes.load_full();
        cache.renew(
            &old,
            now,
            now + Duration::from_secs(1),
            Duration::from_secs(3),
        );
        assert!(Arc::ptr_eq(&snapshot, &cache.routes.load_full()));
        assert!(cache.get("volume", now + Duration::from_secs(2)).is_some());
        cache.invalidate(&old);
        cache.renew(
            &old,
            now,
            now + Duration::from_secs(1),
            Duration::from_secs(3),
        );
        assert!(cache.get("volume", now).is_none());
        let new = route("volume".into(), 2);
        cache.insert(&new, now + Duration::from_secs(1), 2);
        cache.renew(
            &old,
            now,
            now + Duration::from_secs(1),
            Duration::from_secs(3),
        );
        assert_eq!(cache.get("volume", now).unwrap().route.fence, 2);
        assert!(cache.get("volume", now + Duration::from_secs(2)).is_none());
    }

    #[test]
    fn delayed_response_cannot_extend_route_past_request_deadline() {
        let cache = RouteCache::default();
        let start = Instant::now();
        let ttl = Duration::from_secs(4);
        let old = route("volume".into(), 1);
        for age in [ttl, ttl + Duration::from_secs(1)] {
            cache.insert(&old, start + ttl / 2, 2);
            cache.renew(&old, start, start + age, ttl);
            assert!(cache.get("volume", start + ttl / 2).is_none());
        }
        cache.renew(&old, start, start + ttl / 2, ttl);
        assert!(
            cache
                .get("volume", start + ttl - Duration::from_millis(1))
                .is_some()
        );
        assert!(cache.get("volume", start + ttl).is_none());
    }

    #[test]
    fn background_confirmation_is_bounded_and_cannot_modify_a_replacement() {
        let cache = RouteCache::default();
        let now = Instant::now();
        let ttl = Duration::from_secs(1);
        let route = route("active".into(), 1);
        let detached = cache.insert(&route, now + ttl, 1);
        let replacement = cache.insert(&route, now + ttl / 2, 1);
        assert!(detached.confirm(now, ttl));
        cache.discard_entry(&route, &detached);
        assert!(cache.get("active", now + ttl / 2).is_none());
        assert!(cache.get_active("active", now).is_some());
        assert!(replacement.confirm(now, ttl));
        assert!(cache.get("active", now + ttl).is_none());
        assert!(!replacement.can_refresh(now + ttl / 2, ttl / 3));
        let expired = cache.insert(&route, now, 1);
        assert!(!expired.confirm(Instant::now(), ttl));
    }

    #[test]
    fn expiry_and_capacity_remain_bounded() {
        let cache = RouteCache::default();
        let now = Instant::now();
        cache.insert(&route("expired".into(), 1), now, 2);
        assert!(cache.get("expired", now).is_none());
        for i in 0..10 {
            cache.insert(&route(format!("v{i}"), 1), now + Duration::from_secs(60), 2);
        }
        assert_eq!(cache.routes.load().len(), 2);
    }
}
