//! Failed-login throttling per client IP (`SPEC.md` §12): after `max_failures` failures
//! within `window`, further attempts are refused until the oldest of them leaves the window.
//! Kept in memory; a daemon restart clears it.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Distinct IPs tracked before stale entries are swept.
const SWEEP_AT: usize = 10_000;

/// Per-IP failure counter.
#[derive(Debug)]
pub(crate) struct Limiter {
    max_failures: usize,
    window: Duration,
    failures: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
}

impl Limiter {
    pub(crate) fn new(max_failures: usize, window: Duration) -> Self {
        Self {
            max_failures: max_failures.max(1),
            window,
            failures: Mutex::default(),
        }
    }

    /// `Some(wait)` while `ip` is locked out.
    pub(crate) fn check(&self, ip: IpAddr, now: Instant) -> Option<Duration> {
        let mut map = self.lock();
        let entry = map.get_mut(&ip)?;
        self.prune(entry, now);
        if entry.len() < self.max_failures {
            return None;
        }
        // Locked until enough failures have aged out to fall below the limit.
        let unlock_at = entry[entry.len() - self.max_failures] + self.window;
        Some(
            unlock_at
                .saturating_duration_since(now)
                .max(Duration::from_secs(1)),
        )
    }

    /// Records a failed attempt.
    pub(crate) fn fail(&self, ip: IpAddr, now: Instant) {
        let mut map = self.lock();
        if map.len() >= SWEEP_AT {
            map.retain(|_, times| {
                times.retain(|t| now.saturating_duration_since(*t) < self.window);
                !times.is_empty()
            });
        }
        let entry = map.entry(ip).or_default();
        self.prune(entry, now);
        entry.push_back(now);
        // Only the most recent `max_failures` matter.
        while entry.len() > self.max_failures {
            entry.pop_front();
        }
    }

    /// Forgets `ip`'s failures after a successful login.
    pub(crate) fn succeed(&self, ip: IpAddr) {
        self.lock().remove(&ip);
    }

    fn prune(&self, times: &mut VecDeque<Instant>, now: Instant) {
        while times
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= self.window)
        {
            times.pop_front();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, VecDeque<Instant>>> {
        self.failures.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn locks_after_max_failures_and_unlocks_after_window() {
        let l = Limiter::new(5, 15 * MIN);
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let other: IpAddr = "192.0.2.2".parse().unwrap();
        let t0 = Instant::now();
        for i in 0..4 {
            l.fail(ip, t0 + i * MIN);
            assert_eq!(l.check(ip, t0 + i * MIN), None);
        }
        l.fail(ip, t0 + 4 * MIN);
        // The first failure at t0 leaves the window at t0 + 15 min.
        assert_eq!(l.check(ip, t0 + 5 * MIN), Some(10 * MIN));
        assert_eq!(l.check(other, t0 + 5 * MIN), None, "per IP");
        assert_eq!(l.check(ip, t0 + 15 * MIN), None);
        // One more failure locks again until the second failure ages out.
        l.fail(ip, t0 + 15 * MIN);
        assert_eq!(l.check(ip, t0 + 15 * MIN), Some(MIN));
    }

    #[test]
    fn success_resets() {
        let l = Limiter::new(2, MIN);
        let ip: IpAddr = "::1".parse().unwrap();
        let t0 = Instant::now();
        l.fail(ip, t0);
        l.fail(ip, t0);
        assert!(l.check(ip, t0).is_some());
        l.succeed(ip);
        assert!(l.check(ip, t0).is_none());
    }

    #[test]
    fn sweep_bounds_memory() {
        let l = Limiter::new(5, MIN);
        let t0 = Instant::now();
        for i in 0..SWEEP_AT as u32 {
            l.fail(IpAddr::from(i.to_be_bytes()), t0);
        }
        l.fail("::1".parse().unwrap(), t0 + 2 * MIN);
        assert_eq!(l.lock().len(), 1);
    }
}
