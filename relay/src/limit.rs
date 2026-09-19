//! Rate limiting, per client address.
//!
//! The relay's whole security model is that it cannot read anything and cannot
//! tell one person from another. That is deliberate, and it leaves exactly one
//! thing it *can* be abused for: work. Anyone who can reach it can ask for a
//! nonce, open a socket and be handed a note's backlog, for ever, as fast as the
//! process will answer. None of that needs a key, because the handshake is the
//! thing that proves a key and it is the handshake being flooded.
//!
//! So the limit is on the two unauthenticated doors — `/v1/challenge` and
//! `/v1/token` — and on opening a socket. It is not on publishing: a device that
//! got through the handshake holds a key, and the envelope cap plus the store is
//! what bounds it.
//!
//! ## Why a token bucket, and why it is keyed on the socket's peer
//!
//! A bucket rather than a fixed window because a fixed window lets a client
//! spend a whole window's budget in its last millisecond and the next window's
//! in its first, which is twice the rate it was configured for at exactly the
//! moment that matters. A bucket refills continuously and has no edge.
//!
//! Keyed on the address the socket actually came from, and deliberately **not**
//! on `X-Forwarded-For`. That header is a client-supplied string: reading it
//! from an untrusted peer turns the limiter off, because every request can carry
//! a fresh one. A relay behind a proxy therefore limits the proxy, which is
//! wrong but safe; making it right means telling the relay which hop to trust,
//! and there is no deployment yet to tell it about.
//!
//! ## What it costs when nobody is attacking
//!
//! One `HashMap` lookup and a subtraction per request, under a mutex the
//! handshake path already contends for elsewhere ([`crate::auth`]). The map is
//! the part that needs care: a key per address is a memory cost an attacker
//! chooses, so it is swept of full buckets — a full bucket is one that has
//! forgotten everything anyway — whenever it grows past [`SWEEP_ABOVE`].

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How many addresses the map may hold before a request also sweeps it. Chosen
/// to be far above any real deployment's concurrent clients and far below a
/// memory problem: 10 000 entries is well under a megabyte.
const SWEEP_ABOVE: usize = 10_000;

/// One address's allowance for one kind of request.
#[derive(Clone, Copy)]
struct Bucket {
    /// Whole requests remaining, as a float because refilling is continuous.
    tokens: f64,
    last: Instant,
}

/// A token bucket per address: `burst` requests available at once, refilling to
/// full over a minute.
pub struct RateLimit {
    per_minute: f64,
    burst: f64,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

impl RateLimit {
    /// `per_minute` requests sustained, with `burst` available at once. A
    /// `per_minute` of zero means no limit at all, which is what a test and a
    /// development relay want.
    pub fn new(per_minute: u32, burst: u32) -> Self {
        Self {
            per_minute: f64::from(per_minute),
            burst: f64::from(burst.max(1)),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn unlimited() -> Self {
        Self::new(0, 1)
    }

    fn disabled(&self) -> bool {
        self.per_minute <= 0.0
    }

    /// Spend one request's worth of allowance. `false` means the caller is over
    /// its limit and the request should be refused.
    pub fn allow(&self, who: IpAddr) -> bool {
        self.allow_at(who, Instant::now())
    }

    /// The whole of [`RateLimit::allow`], with the clock passed in so that a
    /// test can assert on the refill without sleeping for a minute.
    pub fn allow_at(&self, who: IpAddr, now: Instant) -> bool {
        if self.disabled() {
            return true;
        }
        let mut buckets = self.buckets.lock().expect("rate limiter mutex");
        if buckets.len() > SWEEP_ABOVE {
            // A bucket that has refilled to full is indistinguishable from one
            // that never existed, so dropping it loses nothing.
            let full = self.burst;
            let per_second = self.per_minute / 60.0;
            buckets.retain(|_, b| {
                let refilled = b.tokens + now.duration_since(b.last).as_secs_f64() * per_second;
                refilled < full
            });
        }
        let bucket = buckets.entry(who).or_insert(Bucket {
            tokens: self.burst,
            last: now,
        });
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * (self.per_minute / 60.0)).min(self.burst);
        bucket.last = now;
        if bucket.tokens < 1.0 {
            return false;
        }
        bucket.tokens -= 1.0;
        true
    }

    /// How long a refused caller should wait, for the `Retry-After` header. One
    /// token's worth of refill, rounded up, and never zero — a `Retry-After: 0`
    /// invites the retry that was just refused.
    pub fn retry_after(&self) -> Duration {
        if self.disabled() {
            return Duration::ZERO;
        }
        Duration::from_secs((60.0 / self.per_minute).ceil().max(1.0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([127, 0, 0, n])
    }

    #[test]
    fn a_burst_is_allowed_and_then_it_is_not() {
        let limit = RateLimit::new(60, 5);
        let now = Instant::now();
        for i in 0..5 {
            assert!(limit.allow_at(ip(1), now), "request {i} should be allowed");
        }
        assert!(!limit.allow_at(ip(1), now), "the sixth is over the burst");
    }

    #[test]
    fn one_address_does_not_spend_anothers_allowance() {
        let limit = RateLimit::new(60, 2);
        let now = Instant::now();
        assert!(limit.allow_at(ip(1), now));
        assert!(limit.allow_at(ip(1), now));
        assert!(!limit.allow_at(ip(1), now));
        // A second address starts with a full bucket, which is the whole reason
        // this is keyed rather than global: one noisy client must not lock the
        // relay for everyone else.
        assert!(limit.allow_at(ip(2), now));
    }

    #[test]
    fn it_refills_over_time() {
        let limit = RateLimit::new(60, 2);
        let now = Instant::now();
        assert!(limit.allow_at(ip(1), now));
        assert!(limit.allow_at(ip(1), now));
        assert!(!limit.allow_at(ip(1), now));
        // 60 a minute is one a second.
        assert!(limit.allow_at(ip(1), now + Duration::from_secs(1)));
        assert!(!limit.allow_at(ip(1), now + Duration::from_secs(1)));
    }

    #[test]
    fn it_does_not_refill_past_the_burst() {
        let limit = RateLimit::new(60, 2);
        let now = Instant::now();
        // An hour of idleness is worth two requests, not sixty.
        assert!(limit.allow_at(ip(1), now + Duration::from_secs(3600)));
        assert!(limit.allow_at(ip(1), now + Duration::from_secs(3600)));
        assert!(!limit.allow_at(ip(1), now + Duration::from_secs(3600)));
    }

    #[test]
    fn a_limit_of_zero_is_no_limit() {
        let limit = RateLimit::unlimited();
        let now = Instant::now();
        for _ in 0..1000 {
            assert!(limit.allow_at(ip(1), now));
        }
    }
}
