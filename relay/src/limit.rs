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
//! `/v1/token` — and on opening a socket. Publishing has a budget of its own,
//! in bytes rather than requests: appends and attachment chunks together, per
//! address ([ADR 0028](../../docs/decisions.md#adr-0028)).
//! Device keys are free, so a budget per key would be a budget per nothing.
//!
//! ## Why a token bucket, and why it is keyed on the socket's peer
//!
//! A bucket rather than a fixed window because a fixed window lets a client
//! spend a whole window's budget in its last millisecond and the next window's
//! in its first, which is twice the rate it was configured for at exactly the
//! moment that matters. A bucket refills continuously and has no edge.
//!
//! Keyed on the address the socket actually came from, and **not** on
//! `X-Forwarded-For` unless the deployment says which peers may set it. That
//! header is a client-supplied string: reading it from an untrusted peer turns
//! the limiter off, because every request can carry a fresh one.
//!
//! But a relay behind a reverse proxy — which is how the Synology deployment
//! runs — sees one peer for every client, and then limits the proxy: every
//! device shares one bucket, a relay restart refuses all but the first twenty
//! to reconnect, and one noisy client spends everybody's allowance.
//! [`TrustedProxies`] is the fix. `RELAY_TRUSTED_PROXIES` names the addresses
//! (or ranges) the relay's own proxy connects from; a request from one of them
//! is keyed on the address that proxy says it received the request from, and a
//! request from anywhere else is keyed on its peer exactly as before.
//!
//! ## What it costs when nobody is attacking
//!
//! One `HashMap` lookup and a subtraction per request, under a mutex the
//! handshake path already contends for elsewhere ([`crate::auth`]). The map is
//! the part that needs care: a key per address is a memory cost an attacker
//! chooses, so it is swept of full buckets — a full bucket is one that has
//! forgotten everything anyway — whenever it grows past [`SWEEP_ABOVE`].

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
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
        self.spend_at(who, 1, now)
    }

    /// Spend `cost` units at once — bytes, for the write budget. A cost larger
    /// than the whole burst is charged against a full bucket rather than
    /// refused for ever, so that no single write the relay otherwise accepts
    /// can never go through.
    pub fn spend(&self, who: IpAddr, cost: u64) -> bool {
        self.spend_at(who, cost, Instant::now())
    }

    pub fn spend_at(&self, who: IpAddr, cost: u64, now: Instant) -> bool {
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
        let cost = (cost as f64).clamp(1.0, self.burst);
        if bucket.tokens < cost {
            return false;
        }
        bucket.tokens -= cost;
        true
    }

    /// How long a caller refused `cost` units should wait for them to refill:
    /// the write budget's `Retry-After`. Never zero, for the reason
    /// [`RateLimit::retry_after`] is not.
    pub fn retry_after_for(&self, cost: u64) -> Duration {
        if self.disabled() {
            return Duration::ZERO;
        }
        let cost = (cost as f64).clamp(1.0, self.burst);
        Duration::from_secs((cost * 60.0 / self.per_minute).ceil().max(1.0) as u64)
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

/// The peers allowed to say who a client is, and the reading of what they say.
///
/// Empty — the default, and what every test gets — means no peer is trusted and
/// `X-Forwarded-For` is never read.
#[derive(Debug, Default)]
pub struct TrustedProxies {
    nets: Vec<(IpAddr, u8)>,
    /// Set once a trusted peer has sent a request with no usable header, so the
    /// warning that says so is logged once rather than per request.
    warned: AtomicBool,
}

impl TrustedProxies {
    /// Parse a comma-separated list of addresses and CIDR ranges, such as
    /// `172.30.87.1` or `10.0.0.0/8, ::1`. Anything it cannot read is an error:
    /// a trust list that silently drops an entry is one that limits the proxy
    /// again without saying so.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let mut nets = Vec::new();
        for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (addr, prefix) = match entry.split_once('/') {
                Some((addr, prefix)) => (addr, Some(prefix)),
                None => (entry, None),
            };
            let addr: IpAddr = addr
                .parse()
                .map_err(|_| format!("{entry:?} is not an IP address or CIDR range"))?;
            let addr = addr.to_canonical();
            let max = if addr.is_ipv4() { 32 } else { 128 };
            let prefix = match prefix {
                None => max,
                Some(p) => p
                    .parse::<u8>()
                    .ok()
                    .filter(|p| *p <= max)
                    .ok_or_else(|| format!("{entry:?} has a prefix length outside 0-{max}"))?,
            };
            nets.push((addr, prefix));
        }
        Ok(Self {
            nets,
            warned: AtomicBool::new(false),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.nets.is_empty()
    }

    fn trusts(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        self.nets.iter().any(|&(net, prefix)| match (net, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        })
    }

    /// The address to key a request on: the peer itself, unless the peer is a
    /// trusted proxy, in which case the nearest address in `X-Forwarded-For`
    /// that is not another trusted proxy.
    ///
    /// The header is read from the right, because each proxy appends the
    /// address it received the request from: the rightmost entries were written
    /// by proxies this relay trusts, and everything to the left of the first
    /// untrusted one was written by the client and may be anything. An entry
    /// that cannot be read stops the walk at the last address that could be
    /// trusted, which limits a proxy again rather than believing a stranger.
    pub fn client<'a>(&self, peer: IpAddr, forwarded: impl Iterator<Item = &'a str>) -> IpAddr {
        let peer = peer.to_canonical();
        if !self.trusts(peer) {
            return peer;
        }
        // Several headers are one list, in order (RFC 9110).
        let entries: Vec<&str> = forwarded
            .flat_map(|h| h.split(','))
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .collect();
        if entries.is_empty() && !self.warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                %peer,
                "a trusted proxy sent a request without X-Forwarded-For: \
                 every client behind it shares one rate limit until it does"
            );
        }
        let mut client = peer;
        for entry in entries.iter().rev() {
            let Some(ip) = parse_forwarded(entry) else {
                break;
            };
            client = ip;
            if !self.trusts(ip) {
                break;
            }
        }
        client
    }
}

/// One `X-Forwarded-For` entry: a bare address, or one with a port, which some
/// proxies write (`203.0.113.9:4711`, `[2001:db8::1]:4711`).
fn parse_forwarded(entry: &str) -> Option<IpAddr> {
    entry
        .parse::<IpAddr>()
        .or_else(|_| entry.parse::<SocketAddr>().map(|a| a.ip()))
        .ok()
        .map(|ip| ip.to_canonical())
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
    fn a_byte_budget_spends_what_each_write_costs() {
        let limit = RateLimit::new(60, 1000);
        let now = Instant::now();
        assert!(limit.spend_at(ip(1), 600, now));
        assert!(!limit.spend_at(ip(1), 600, now), "only 400 are left");
        assert!(limit.spend_at(ip(1), 400, now));
        // A write larger than the whole burst waits for a full bucket rather
        // than never going through.
        assert!(limit.spend_at(ip(2), 5000, now));
        assert!(!limit.spend_at(ip(2), 1, now));
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

    fn trusting(raw: &str) -> TrustedProxies {
        TrustedProxies::parse(raw).unwrap()
    }

    fn addr(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn an_untrusted_peer_is_keyed_on_itself_whatever_it_claims() {
        let proxies = trusting("10.0.0.1");
        assert_eq!(
            proxies.client(addr("198.51.100.7"), ["203.0.113.9"].into_iter()),
            addr("198.51.100.7"),
        );
        // And with no list at all, nobody's header is read.
        assert_eq!(
            TrustedProxies::default().client(addr("10.0.0.1"), ["203.0.113.9"].into_iter()),
            addr("10.0.0.1"),
        );
    }

    #[test]
    fn a_trusted_proxy_is_keyed_on_the_client_it_names() {
        let proxies = trusting("10.0.0.1");
        assert_eq!(
            proxies.client(addr("10.0.0.1"), ["203.0.113.9"].into_iter()),
            addr("203.0.113.9"),
        );
    }

    #[test]
    fn what_the_client_wrote_to_the_left_is_ignored() {
        // The client sent `X-Forwarded-For: 1.2.3.4`, and the proxy appended the
        // address it really came from. Only the rightmost untrusted entry counts.
        let proxies = trusting("10.0.0.1");
        assert_eq!(
            proxies.client(addr("10.0.0.1"), ["1.2.3.4, 203.0.113.9"].into_iter()),
            addr("203.0.113.9"),
        );
    }

    #[test]
    fn a_chain_of_trusted_proxies_is_walked_through() {
        let proxies = trusting("10.0.0.0/8");
        assert_eq!(
            proxies.client(
                addr("10.0.0.1"),
                ["1.2.3.4, 203.0.113.9", "10.1.2.3"].into_iter()
            ),
            addr("203.0.113.9"),
        );
    }

    #[test]
    fn a_missing_or_unreadable_header_limits_the_proxy() {
        let proxies = trusting("10.0.0.1");
        assert_eq!(
            proxies.client(addr("10.0.0.1"), std::iter::empty()),
            addr("10.0.0.1"),
        );
        assert_eq!(
            proxies.client(addr("10.0.0.1"), ["203.0.113.9, unknown"].into_iter()),
            addr("10.0.0.1"),
            "an entry the proxy could not have written stops the walk",
        );
    }

    #[test]
    fn ports_brackets_and_mapped_addresses_are_understood() {
        let proxies = trusting("10.0.0.1, ::1");
        assert_eq!(
            proxies.client(addr("::ffff:10.0.0.1"), ["203.0.113.9:4711"].into_iter()),
            addr("203.0.113.9"),
        );
        assert_eq!(
            proxies.client(addr("::1"), ["[2001:db8::1]:4711"].into_iter()),
            addr("2001:db8::1"),
        );
    }

    #[test]
    fn a_trust_list_it_cannot_read_is_an_error() {
        assert!(TrustedProxies::parse("10.0.0.1, 10.0.0.300").is_err());
        assert!(TrustedProxies::parse("10.0.0.0/33").is_err());
        assert!(TrustedProxies::parse("proxy.local").is_err());
        assert!(TrustedProxies::parse("").unwrap().is_empty());
        assert!(trusting("0.0.0.0/0").trusts(addr("198.51.100.7")));
    }
}
