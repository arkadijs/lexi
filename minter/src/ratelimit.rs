//! In-memory keyed rate limiting (per replica, SPEC §5.3).

use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota};
use std::num::NonZeroU32;
use std::time::Duration;

#[derive(Debug)]
pub struct Limiter {
    inner: Inner,
}

struct Inner(DefaultKeyedRateLimiter<String>);

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeyedRateLimiter")
    }
}

impl Limiter {
    /// `per_hour` requests per hour per key, with a burst of the same size. 0 is treated as 1.
    pub fn per_hour(per_hour: u32) -> Self {
        let n = NonZeroU32::new(per_hour.max(1)).unwrap_or(NonZeroU32::MIN);
        Self {
            inner: Inner(DefaultKeyedRateLimiter::keyed(Quota::per_hour(n))),
        }
    }

    /// `Ok` if allowed, otherwise `Err(retry_after)`.
    pub fn check(&self, key: &str) -> Result<(), Duration> {
        self.inner
            .0
            .check_key(&key.to_owned())
            .map_err(|nu| nu.wait_time_from(DefaultClock::default().now()))
    }

    /// Drop keys whose state has fully recovered; call periodically to bound memory.
    pub fn gc(&self) {
        self.inner.0.retain_recent();
        self.inner.0.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_per_key() {
        let l = Limiter::per_hour(2);
        assert!(l.check("a").is_ok());
        assert!(l.check("a").is_ok());
        let wait = l.check("a").unwrap_err();
        assert!(wait > Duration::ZERO);
        assert!(l.check("b").is_ok());
    }
}
