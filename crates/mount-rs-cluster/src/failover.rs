//! The per-volume failover decisions of one candidate owner, with no I/O.
//!
//! A candidate is either a standby for a volume or its owner. This machine
//! decides when a standby should try to open the volume and when an owner
//! should let it go. It never decides who may write: opening a volume takes
//! the storage writer lease, and only that makes a node the owner. See
//! `docs/serve-http-failover.md`.
use crate::{RegistryError, placement};
use std::time::Duration;
use tokio::time::Instant;

/// Longest back-off after an open that failed for a reason other than the
/// lease being held, or after losing a lease.
const MAX_FAILURE_BACKOFF: Duration = Duration::from_secs(60);
/// A lease that is still held is retried at most this many poll intervals
/// apart.
const MAX_LEASE_RETRY_POLLS: u32 = 5;
/// Ownership that lasted this long clears the failure back-off.
const STABLE_OWNERSHIP: Duration = Duration::from_secs(60);
/// A hand-back that nobody took doubles the wait before the next, up to this.
const MAX_FAIL_BACK_AFTER: Duration = Duration::from_secs(3600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FailoverTimings {
    /// How often a standby reads the route and an owner checks its lease.
    pub poll: Duration,
    /// A standby ranked `k` among the live candidates waits `k` times this
    /// after the route disappears before it tries to open the volume.
    pub stagger: Duration,
    /// Hand a volume back once another live candidate has ranked first for
    /// this long. `None` keeps a volume until its owner stops.
    pub fail_back_after: Option<Duration>,
    /// After handing a volume back, do not try to take it for this long.
    pub fail_back_hold_off: Duration,
}
impl Default for FailoverTimings {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(1),
            stagger: Duration::from_secs(2),
            fail_back_after: None,
            fail_back_hold_off: Duration::from_secs(15),
        }
    }
}
impl FailoverTimings {
    pub fn validate(&self) -> Result<(), RegistryError> {
        let poll = Duration::from_millis(100)..=Duration::from_secs(10);
        let fail_back = Duration::from_secs(1)..=Duration::from_secs(86_400);
        if !poll.contains(&self.poll)
            || self.stagger > Duration::from_secs(60)
            || self
                .fail_back_after
                .is_some_and(|d| !fail_back.contains(&d))
            || self.fail_back_hold_off > Duration::from_secs(300)
        {
            return Err(RegistryError::InvalidInput);
        }
        Ok(())
    }
}

/// What a standby read from the route registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteView {
    /// The registry could not be read. This is not evidence of anything.
    Unknown,
    /// The registry was read and no other node's route is published. A
    /// leftover route naming this node, which this process did not publish,
    /// counts as absent.
    Absent,
    /// Another node's route is published.
    Published,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandbyAction {
    Wait,
    /// Try to open the volume now, then report the result with `opened`.
    Open,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenOutcome {
    /// The volume is open and this node holds its writer lease.
    Acquired,
    /// The lease is held by someone else (possibly a crashed owner).
    LeaseHeld,
    /// The open failed for any other reason.
    Failed,
}

/// What an owner knows about its own hold on the volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerView {
    /// The lease is held with the fence the route was published under.
    Held,
    /// The lease is gone or its fence changed, or a successor's route is
    /// published.
    Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseReason {
    LeaseLost,
    /// Handing the volume back to a better-ranked live candidate.
    FailBack,
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerAction {
    Keep,
    /// Stop serving the volume, withdraw its route and release the lease,
    /// then report with `released`.
    Release(ReleaseReason),
}

#[derive(Debug)]
pub struct FailoverMachine {
    volume: String,
    node: String,
    timings: FailoverTimings,
    /// When the route was first read as absent, with no read since that
    /// failed or found a route.
    absent_since: Option<Instant>,
    /// No open may start before this (lease retry, back-off, hold-off).
    not_before: Option<Instant>,
    lease_retry: Duration,
    /// Consecutive failed opens and lost leases.
    failures: u32,
    /// Not a candidate until this: the node recently failed to open or keep
    /// the volume.
    unviable_until: Option<Instant>,
    owner_since: Option<Instant>,
    /// As owner: since when another live candidate has ranked first.
    elsewhere_since: Option<Instant>,
    /// The current wait before a hand-back; doubles when one is not taken.
    fail_back_after: Option<Duration>,
    /// Handed the volume back and has not yet seen another node publish it.
    handed_back: bool,
}
impl FailoverMachine {
    pub fn new(
        volume: impl Into<String>,
        node: impl Into<String>,
        timings: FailoverTimings,
    ) -> Result<Self, RegistryError> {
        timings.validate()?;
        Ok(Self {
            volume: volume.into(),
            node: node.into(),
            timings,
            absent_since: None,
            not_before: None,
            lease_retry: timings.poll,
            failures: 0,
            unviable_until: None,
            owner_since: None,
            elsewhere_since: None,
            fail_back_after: timings.fail_back_after,
            handed_back: false,
        })
    }

    /// Whether this node should announce itself as a live candidate. False
    /// while it backs off after failing to open or keep the volume, so that
    /// the others stop ranking it.
    pub fn is_candidate(&self, now: Instant) -> bool {
        self.unviable_until.is_none_or(|until| now >= until)
    }

    fn rank(&self, live: &[String]) -> usize {
        placement::rank(&self.volume, &self.node, live.iter().map(String::as_str))
    }

    /// One standby poll. `live` is the trusted live candidates, or `None`
    /// when the candidate set could not be read.
    pub fn standby(
        &mut self,
        now: Instant,
        route: RouteView,
        live: Option<&[String]>,
    ) -> StandbyAction {
        match route {
            RouteView::Unknown => {
                self.absent_since = None;
                StandbyAction::Wait
            }
            RouteView::Published => {
                self.absent_since = None;
                self.lease_retry = self.timings.poll;
                if self.handed_back {
                    // The hand-back was taken: the next one need not wait longer.
                    self.handed_back = false;
                    self.fail_back_after = self.timings.fail_back_after;
                }
                StandbyAction::Wait
            }
            RouteView::Absent => {
                let since = *self.absent_since.get_or_insert(now);
                if self.not_before.is_some_and(|at| now < at) || !self.is_candidate(now) {
                    return StandbyAction::Wait;
                }
                // Without the candidate set the rank is unknown; wait for it.
                let Some(live) = live else {
                    return StandbyAction::Wait;
                };
                let rank = u32::try_from(self.rank(live)).unwrap_or(u32::MAX);
                if now.saturating_duration_since(since) >= self.timings.stagger.saturating_mul(rank)
                {
                    StandbyAction::Open
                } else {
                    StandbyAction::Wait
                }
            }
        }
    }

    fn failure_backoff(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        let doubled = Duration::from_secs(1)
            .saturating_mul(1u32.checked_shl(self.failures - 1).unwrap_or(u32::MAX));
        let until = now + doubled.min(MAX_FAILURE_BACKOFF);
        self.not_before = Some(until);
        self.unviable_until = Some(until);
    }

    /// The result of the open that `standby` asked for.
    pub fn opened(&mut self, now: Instant, outcome: OpenOutcome) {
        match outcome {
            OpenOutcome::Acquired => {
                self.owner_since = Some(now);
                self.absent_since = None;
                self.not_before = None;
                self.elsewhere_since = None;
                self.lease_retry = self.timings.poll;
                if self.handed_back {
                    // Nobody took the volume that was handed back.
                    self.handed_back = false;
                    self.fail_back_after = self
                        .fail_back_after
                        .map(|after| after.saturating_mul(2).min(MAX_FAIL_BACK_AFTER));
                }
            }
            OpenOutcome::LeaseHeld => {
                self.not_before = Some(now + self.lease_retry);
                self.lease_retry = self
                    .lease_retry
                    .saturating_mul(2)
                    .min(self.timings.poll.saturating_mul(MAX_LEASE_RETRY_POLLS));
            }
            OpenOutcome::Failed => self.failure_backoff(now),
        }
    }

    /// One owner poll. `live` is as for `standby`.
    pub fn owner(&mut self, now: Instant, view: OwnerView, live: Option<&[String]>) -> OwnerAction {
        if view == OwnerView::Lost {
            return OwnerAction::Release(ReleaseReason::LeaseLost);
        }
        if self
            .owner_since
            .is_some_and(|since| now.saturating_duration_since(since) >= STABLE_OWNERSHIP)
        {
            self.failures = 0;
        }
        let Some(after) = self.fail_back_after else {
            return OwnerAction::Keep;
        };
        match live {
            // The preference must hold without interruption, and an
            // unreadable candidate set is an interruption.
            None => self.elsewhere_since = None,
            Some(live) if self.rank(live) == 0 => self.elsewhere_since = None,
            Some(_) => {
                let since = *self.elsewhere_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= after {
                    return OwnerAction::Release(ReleaseReason::FailBack);
                }
            }
        }
        OwnerAction::Keep
    }

    /// The volume has been released for `reason`.
    pub fn released(&mut self, now: Instant, reason: ReleaseReason) {
        self.owner_since = None;
        self.elsewhere_since = None;
        self.absent_since = None;
        match reason {
            ReleaseReason::Shutdown => {}
            ReleaseReason::LeaseLost => self.failure_backoff(now),
            ReleaseReason::FailBack => {
                self.handed_back = true;
                self.not_before = Some(now + self.timings.fail_back_hold_off);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    fn timings() -> FailoverTimings {
        FailoverTimings::default()
    }
    fn machine(node: &str, timings: FailoverTimings) -> FailoverMachine {
        FailoverMachine::new("volume", node, timings).unwrap()
    }
    /// Two node ids ordered best-ranked first for "volume".
    fn ranked_pair() -> (String, String) {
        let ranked = placement::ranking("volume", ["node-a", "node-b"]);
        (ranked[0].to_owned(), ranked[1].to_owned())
    }
    fn live(nodes: &[&str]) -> Vec<String> {
        nodes.iter().map(|node| (*node).to_owned()).collect()
    }

    #[test]
    fn timings_are_bounded() {
        assert!(timings().validate().is_ok());
        for invalid in [
            FailoverTimings {
                poll: Duration::from_millis(99),
                ..timings()
            },
            FailoverTimings {
                poll: Duration::from_secs(11),
                ..timings()
            },
            FailoverTimings {
                stagger: Duration::from_secs(61),
                ..timings()
            },
            FailoverTimings {
                fail_back_after: Some(Duration::from_millis(999)),
                ..timings()
            },
            FailoverTimings {
                fail_back_after: Some(Duration::from_secs(86_401)),
                ..timings()
            },
        ] {
            assert!(invalid.validate().is_err(), "{invalid:?}");
            assert!(FailoverMachine::new("volume", "node", invalid).is_err());
        }
    }

    #[test]
    fn a_standby_opens_only_on_evidence_that_no_route_is_published() {
        let (best, _) = ranked_pair();
        let mut standby = machine(&best, timings());
        let now = Instant::now();
        let everyone = live(&["node-a", "node-b"]);
        for (route, candidates) in [
            (RouteView::Published, Some(everyone.as_slice())),
            (RouteView::Unknown, Some(everyone.as_slice())),
            (RouteView::Unknown, None),
            // The route is absent but the candidate set is unreadable.
            (RouteView::Absent, None),
        ] {
            assert_eq!(
                standby.standby(now, route, candidates),
                StandbyAction::Wait,
                "{route:?}"
            );
        }
        assert_eq!(
            standby.standby(now, RouteView::Absent, Some(&everyone)),
            StandbyAction::Open
        );
    }

    #[test]
    fn a_lower_ranked_standby_waits_its_stagger_of_uninterrupted_absence() {
        let (_, second) = ranked_pair();
        let mut standby = machine(&second, timings());
        let everyone = live(&["node-a", "node-b"]);
        let start = Instant::now();
        let mut poll =
            |after: Duration, route| standby.standby(start + after, route, Some(&everyone));
        assert_eq!(poll(Duration::ZERO, RouteView::Absent), StandbyAction::Wait);
        assert_eq!(poll(SECOND, RouteView::Absent), StandbyAction::Wait);
        // One failed read restarts the wait: the absence was not continuous.
        assert_eq!(poll(SECOND * 2, RouteView::Unknown), StandbyAction::Wait);
        assert_eq!(poll(SECOND * 3, RouteView::Absent), StandbyAction::Wait);
        assert_eq!(poll(SECOND * 4, RouteView::Absent), StandbyAction::Wait);
        assert_eq!(poll(SECOND * 5, RouteView::Absent), StandbyAction::Open);
        // Alone among the live candidates it goes at once.
        let mut alone = machine(&second, timings());
        let only = live(&[&second]);
        assert_eq!(
            alone.standby(start, RouteView::Absent, Some(&only)),
            StandbyAction::Open
        );
        // So does a node that the set does not list at all.
        let mut unlisted = machine(&second, timings());
        assert_eq!(
            unlisted.standby(start, RouteView::Absent, Some(&[])),
            StandbyAction::Open
        );
    }

    #[test]
    fn a_held_lease_is_retried_with_a_capped_delay_and_no_loss_of_candidacy() {
        let (best, _) = ranked_pair();
        let mut standby = machine(&best, timings());
        let everyone = live(&["node-a", "node-b"]);
        let mut now = Instant::now();
        let mut waits = Vec::new();
        for _ in 0..6 {
            assert_eq!(
                standby.standby(now, RouteView::Absent, Some(&everyone)),
                StandbyAction::Open
            );
            standby.opened(now, OpenOutcome::LeaseHeld);
            assert!(standby.is_candidate(now));
            let mut waited = Duration::ZERO;
            while standby.standby(now, RouteView::Absent, Some(&everyone)) == StandbyAction::Wait {
                now += Duration::from_millis(100);
                waited += Duration::from_millis(100);
            }
            waits.push(waited.as_millis());
        }
        assert_eq!(waits, [1000, 2000, 4000, 5000, 5000, 5000]);
        // A published route ends the episode; the next starts from one poll.
        standby.opened(now, OpenOutcome::LeaseHeld);
        now += SECOND * 5;
        assert_eq!(
            standby.standby(now, RouteView::Published, Some(&everyone)),
            StandbyAction::Wait
        );
        assert_eq!(
            standby.standby(now, RouteView::Absent, Some(&everyone)),
            StandbyAction::Open
        );
        standby.opened(now, OpenOutcome::LeaseHeld);
        assert_eq!(
            standby.standby(now + SECOND, RouteView::Absent, Some(&everyone)),
            StandbyAction::Open
        );
    }

    #[test]
    fn a_failed_open_backs_off_and_withdraws_candidacy_until_it_ends() {
        let (best, _) = ranked_pair();
        let mut standby = machine(&best, timings());
        let everyone = live(&["node-a", "node-b"]);
        let mut now = Instant::now();
        let mut waits = Vec::new();
        for _ in 0..8 {
            assert_eq!(
                standby.standby(now, RouteView::Absent, Some(&everyone)),
                StandbyAction::Open
            );
            standby.opened(now, OpenOutcome::Failed);
            assert!(!standby.is_candidate(now));
            let mut waited = 0;
            while standby.standby(now, RouteView::Absent, Some(&everyone)) == StandbyAction::Wait {
                assert!(!standby.is_candidate(now));
                now += SECOND;
                waited += 1;
            }
            assert!(standby.is_candidate(now));
            waits.push(waited);
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
        // A minute of ownership clears the back-off.
        standby.opened(now, OpenOutcome::Acquired);
        assert_eq!(
            standby.owner(now + SECOND * 60, OwnerView::Held, Some(&everyone)),
            OwnerAction::Keep
        );
        standby.released(now + SECOND * 61, ReleaseReason::LeaseLost);
        assert!(!standby.is_candidate(now + SECOND * 61));
        assert!(standby.is_candidate(now + SECOND * 62));
    }

    #[test]
    fn an_owner_keeps_its_volume_through_a_registry_outage_and_releases_a_lost_lease() {
        let (_, second) = ranked_pair();
        let mut owner = machine(&second, timings());
        let now = Instant::now();
        owner.opened(now, OpenOutcome::Acquired);
        let everyone = live(&["node-a", "node-b"]);
        // Not preferred, registry readable or not: without fail-back it keeps.
        for tick in 0..10_000 {
            let candidates = (tick % 3 != 0).then_some(everyone.as_slice());
            assert_eq!(
                owner.owner(now + SECOND * tick, OwnerView::Held, candidates),
                OwnerAction::Keep
            );
        }
        assert_eq!(
            owner.owner(now, OwnerView::Lost, None),
            OwnerAction::Release(ReleaseReason::LeaseLost)
        );
        owner.released(now, ReleaseReason::LeaseLost);
        assert!(!owner.is_candidate(now));
        assert_eq!(
            owner.standby(now, RouteView::Absent, Some(&[])),
            StandbyAction::Wait
        );
        assert_eq!(
            owner.standby(now + SECOND, RouteView::Absent, Some(&[])),
            StandbyAction::Open
        );
    }

    #[test]
    fn fail_back_needs_an_uninterrupted_preference_and_then_holds_off() {
        let (best, second) = ranked_pair();
        let fail_back = FailoverTimings {
            fail_back_after: Some(SECOND * 30),
            ..timings()
        };
        let mut owner = machine(&second, fail_back);
        let start = Instant::now();
        owner.opened(start, OpenOutcome::Acquired);
        let both = live(&[&best, &second]);
        let alone = live(&[&second]);
        // Alone, it is preferred and keeps the volume for ever.
        assert_eq!(
            owner.owner(start + SECOND * 500, OwnerView::Held, Some(&alone)),
            OwnerAction::Keep
        );
        let t = start + SECOND * 1000;
        assert_eq!(
            owner.owner(t, OwnerView::Held, Some(&both)),
            OwnerAction::Keep
        );
        assert_eq!(
            owner.owner(t + SECOND * 29, OwnerView::Held, Some(&both)),
            OwnerAction::Keep
        );
        // The better node vanishing, or an unreadable set, restarts the count.
        for interruption in [Some(alone.as_slice()), None] {
            assert_eq!(
                owner.owner(t + SECOND * 29, OwnerView::Held, interruption),
                OwnerAction::Keep
            );
            assert_eq!(
                owner.owner(t + SECOND * 30, OwnerView::Held, Some(&both)),
                OwnerAction::Keep
            );
            assert_eq!(
                owner.owner(t + SECOND * 59, OwnerView::Held, Some(&both)),
                OwnerAction::Keep
            );
        }
        let t = t + SECOND * 60;
        assert_eq!(
            owner.owner(t, OwnerView::Held, Some(&both)),
            OwnerAction::Release(ReleaseReason::FailBack)
        );
        owner.released(t, ReleaseReason::FailBack);
        // It stays a candidate but leaves the volume alone for the hold-off,
        // however long the route has been absent.
        assert!(owner.is_candidate(t));
        for second in 0..15 {
            assert_eq!(
                owner.standby(t + SECOND * second, RouteView::Absent, Some(&both)),
                StandbyAction::Wait,
                "{second}"
            );
        }
        assert_eq!(
            owner.standby(t + SECOND * 15, RouteView::Absent, Some(&both)),
            StandbyAction::Open
        );
    }

    #[test]
    fn a_hand_back_nobody_takes_doubles_the_wait_until_one_is_taken() {
        let (best, second) = ranked_pair();
        let fail_back = FailoverTimings {
            fail_back_after: Some(SECOND * 30),
            ..timings()
        };
        let mut owner = machine(&second, fail_back);
        let both = live(&[&best, &second]);
        let mut now = Instant::now();
        owner.opened(now, OpenOutcome::Acquired);
        let mut held_for = Vec::new();
        for _ in 0..3 {
            let since = now;
            while owner.owner(now, OwnerView::Held, Some(&both)) == OwnerAction::Keep {
                now += SECOND;
            }
            held_for.push(now.duration_since(since).as_secs());
            owner.released(now, ReleaseReason::FailBack);
            // The preferred node never takes it; this one does after the hold-off.
            while owner.standby(now, RouteView::Absent, Some(&both)) == StandbyAction::Wait {
                now += SECOND;
            }
            owner.opened(now, OpenOutcome::Acquired);
        }
        assert_eq!(held_for, [30, 60, 120]);
        // This time the preferred node publishes: the wait returns to 30 s.
        while owner.owner(now, OwnerView::Held, Some(&both)) == OwnerAction::Keep {
            now += SECOND;
        }
        owner.released(now, ReleaseReason::FailBack);
        assert_eq!(
            owner.standby(now + SECOND, RouteView::Published, Some(&both)),
            StandbyAction::Wait
        );
        // Much later that node stops and this one takes the volume again.
        now += SECOND * 100;
        while owner.standby(now, RouteView::Absent, Some(&both)) == StandbyAction::Wait {
            now += SECOND;
        }
        owner.opened(now, OpenOutcome::Acquired);
        let since = now;
        while owner.owner(now, OwnerView::Held, Some(&both)) == OwnerAction::Keep {
            now += SECOND;
        }
        assert_eq!(now.duration_since(since).as_secs(), 30);
    }

    /// A lease, a route and a candidate set shared by simulated nodes, with
    /// the lease as the only authority, as in a real cluster.
    struct World {
        lease: Option<(usize, Instant)>,
        route: Option<(usize, Instant)>,
        announced: Vec<Option<Instant>>,
        owner: Vec<bool>,
        alive: Vec<bool>,
        machines: Vec<FailoverMachine>,
        names: Vec<String>,
        registry_up: bool,
        takeovers: Vec<(Instant, usize)>,
    }
    const ROUTE_TTL: Duration = Duration::from_secs(15);
    const LEASE_TTL: Duration = Duration::from_secs(20);
    impl World {
        fn new(nodes: usize, timings: FailoverTimings) -> Self {
            let names: Vec<String> = (0..nodes).map(|n| format!("node-{n}")).collect();
            Self {
                lease: None,
                route: None,
                announced: vec![None; nodes],
                owner: vec![false; nodes],
                alive: vec![true; nodes],
                machines: names.iter().map(|name| machine(name, timings)).collect(),
                names,
                registry_up: true,
                takeovers: Vec::new(),
            }
        }
        fn live(&self, now: Instant) -> Vec<String> {
            (0..self.names.len())
                .filter(|&n| self.announced[n].is_some_and(|until| now < until))
                .map(|n| self.names[n].clone())
                .collect()
        }
        /// One poll of every live node, in index order.
        fn tick(&mut self, now: Instant) {
            if self.lease.is_some_and(|(_, until)| now >= until) {
                self.lease = None;
            }
            if self.route.is_some_and(|(_, until)| now >= until) {
                self.route = None;
            }
            for node in 0..self.names.len() {
                if !self.alive[node] {
                    continue;
                }
                if self.registry_up && self.machines[node].is_candidate(now) {
                    self.announced[node] = Some(now + ROUTE_TTL);
                }
                let live = self.registry_up.then(|| self.live(now));
                if self.owner[node] {
                    // A live owner renews its lease and refreshes its route.
                    self.lease = Some((node, now + LEASE_TTL));
                    if self.registry_up {
                        self.route = Some((node, now + ROUTE_TTL));
                    }
                    if let OwnerAction::Release(reason) =
                        self.machines[node].owner(now, OwnerView::Held, live.as_deref())
                    {
                        self.owner[node] = false;
                        self.lease = None;
                        if self.registry_up {
                            self.route = None;
                        }
                        self.machines[node].released(now, reason);
                    }
                    continue;
                }
                let route = match (self.registry_up, self.route) {
                    (false, _) => RouteView::Unknown,
                    (true, Some((holder, _))) if holder != node => RouteView::Published,
                    (true, _) => RouteView::Absent,
                };
                if self.machines[node].standby(now, route, live.as_deref()) == StandbyAction::Open {
                    let outcome = if self.lease.is_none() {
                        self.lease = Some((node, now + LEASE_TTL));
                        self.route = Some((node, now + ROUTE_TTL));
                        self.owner[node] = true;
                        self.takeovers.push((now, node));
                        OpenOutcome::Acquired
                    } else {
                        OpenOutcome::LeaseHeld
                    };
                    self.machines[node].opened(now, outcome);
                }
                assert!(
                    self.owner.iter().filter(|owner| **owner).count() <= 1,
                    "two owners at once"
                );
            }
        }
        fn crash(&mut self, node: usize) {
            self.alive[node] = false;
            self.owner[node] = false;
        }
        fn run(&mut self, from: Instant, seconds: u64) -> Instant {
            for second in 0..seconds {
                self.tick(from + SECOND * second as u32);
            }
            from + SECOND * seconds as u32
        }
        fn current_owner(&self) -> Option<usize> {
            self.owner.iter().position(|owner| *owner)
        }
    }

    #[test]
    fn simulated_cluster_fails_over_within_the_bound_and_never_has_two_owners() {
        let mut world = World::new(3, timings());
        let mut now = world.run(Instant::now(), 5);
        let first = world.current_owner().expect("an owner at startup");
        assert_eq!(world.takeovers.len(), 1);
        // The preferred node took it: all three started together.
        assert_eq!(world.machines[first].rank(&world.live(now)), 0);
        for _ in 0..2 {
            let dead = world.current_owner().unwrap();
            let crashed_at = now;
            world.crash(dead);
            let before = world.takeovers.len();
            now = world.run(now, 60);
            assert_eq!(world.takeovers.len(), before + 1, "exactly one takeover");
            let (at, by) = *world.takeovers.last().unwrap();
            assert_ne!(by, dead);
            // max(R + k x S, L) + 5P with k <= 1 here: 25 s.
            let took = at.duration_since(crashed_at);
            assert!(took <= Duration::from_secs(25), "{took:?}");
            assert!(
                took >= LEASE_TTL - SECOND,
                "not before the lease expired: {took:?}"
            );
        }
        // The last node keeps serving alone.
        let survivor = world.current_owner().unwrap();
        now = world.run(now, 120);
        assert_eq!(world.current_owner(), Some(survivor));
        assert_eq!(world.takeovers.len(), 3);
        let _ = now;
    }

    #[test]
    fn simulated_registry_outage_changes_nothing() {
        let mut world = World::new(3, timings());
        let mut now = world.run(Instant::now(), 5);
        let owner = world.current_owner().unwrap();
        world.registry_up = false;
        now = world.run(now, 300);
        assert_eq!(world.current_owner(), Some(owner));
        assert_eq!(world.takeovers.len(), 1);
        // The registry returns empty: routes and announcements are gone.
        world.route = None;
        world.announced = vec![None; 3];
        world.registry_up = true;
        now = world.run(now, 120);
        assert_eq!(world.current_owner(), Some(owner));
        assert_eq!(world.takeovers.len(), 1);
        let _ = now;
    }

    #[test]
    fn simulated_restart_stays_put_without_fail_back_and_returns_with_it() {
        for fail_back in [None, Some(SECOND * 30)] {
            let mut world = World::new(
                2,
                FailoverTimings {
                    fail_back_after: fail_back,
                    ..timings()
                },
            );
            let mut now = world.run(Instant::now(), 5);
            let preferred = world.current_owner().unwrap();
            let other = 1 - preferred;
            // A clean stop of the preferred node: it releases as it goes.
            world.crash(preferred);
            world.lease = None;
            world.route = None;
            world.announced[preferred] = None;
            now = world.run(now, 5);
            assert_eq!(world.current_owner(), Some(other));
            // It comes back as a standby.
            world.alive[preferred] = true;
            world.machines[preferred] = machine(
                &world.names[preferred].clone(),
                world.machines[other].timings,
            );
            now = world.run(now, 120);
            let expected = if fail_back.is_some() {
                preferred
            } else {
                other
            };
            assert_eq!(world.current_owner(), Some(expected), "{fail_back:?}");
            assert_eq!(
                world.takeovers.len(),
                if fail_back.is_some() { 3 } else { 2 }
            );
            let _ = now;
        }
    }
}
