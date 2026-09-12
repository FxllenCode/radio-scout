//! **Lockout** (CONTEXT.md): the refusal to check any secret from an address
//! that has spent its budget of failed attempts, until a cooldown measured from
//! its last attempt has passed.
//!
//! Two surfaces guard a secret with one — the **admin password** (#19) and an
//! **Access code**'s unlock (#68) — and they do it for the same reason and with
//! the same failure mode: both verify with Argon2id, which is memory-hard on
//! purpose, so an unbounded guesser is *also* a way to exhaust a Pi. What
//! differs is only the budget, which is why that is the parameter and everything
//! else is here once.
//!
//! It is deliberately **not** a rate limiter over requests, and it is not a
//! record of who listened (ADR-0011 rule 5): the only addresses in it are ones
//! that presented a wrong secret, they are forgotten as soon as their cooldown
//! elapses, and nothing is ever written down about an address that got it right.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// What one surface allows before it stops answering.
///
/// Copied rather than borrowed: it is two machine words, and a `&Config` here
/// would make the ledger generic over whichever configuration type happened to
/// own it — which is exactly the coupling that kept this inside `admin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Budget {
    /// Failed attempts one address may spend.
    pub attempts: u32,
    /// How long a spent address stays locked out, measured from its **last**
    /// attempt — so hammering a locked address keeps it locked, and walking away
    /// clears it.
    pub cooldown: Duration,
}

/// Failed attempts, per address.
///
/// rdio-scanner has one of these and it does not work. Its cooldown is
/// `time.Duration(time.Duration.Minutes(10))` (`admin.go:64`) — a method
/// expression that evaluates to **zero**, not ten minutes — so failures never
/// decay and three wrong passwords lock an address until the process restarts;
/// and the only thing that ever clears the ledger is a successful login, which
/// clears it for *everybody*. Both are fixed here, and the decay is the fix
/// that matters: a lockout an operator cannot wait out is a lockout they have
/// to restart the scanner to escape.
#[derive(Default)]
pub(crate) struct Lockout {
    failures: HashMap<IpAddr, Failures>,
}

/// One address's record: how many failures, and when the last one was.
struct Failures {
    count: u32,
    last: Instant,
}

impl Lockout {
    /// How long `addr` must wait before it may try again, or `None` if it may
    /// try now.
    ///
    /// Checked *before* the password is verified, so a locked-out address costs
    /// no Argon2 work — the memory-hard hash that makes each guess expensive
    /// would otherwise make a lockout a way to exhaust a Pi's memory.
    pub(crate) fn locked_for(
        &self,
        addr: IpAddr,
        now: Instant,
        budget: Budget,
    ) -> Option<Duration> {
        let record = self.failures.get(&addr)?;
        if record.count < budget.attempts {
            return None;
        }
        match budget.cooldown.checked_sub(now.duration_since(record.last)) {
            // Rounded up, so a caller told to wait never gets 0 — and never
            // retries a fraction of a second too early.
            Some(left) if !left.is_zero() => Some(Duration::from_secs(left.as_secs() + 1)),
            _ => None,
        }
    }

    /// Charge `addr` for a wrong password. Returns the running count, so the
    /// line that reports the lockout can be written exactly once.
    pub(crate) fn record_failure(&mut self, addr: IpAddr, now: Instant, budget: Budget) -> u32 {
        // This is also what makes failures *decay*: an address that walked away
        // for the full cooldown has its record dropped here, so the `or_insert`
        // below starts it over at zero rather than resuming. One mechanism, not
        // a sweep plus a reset that would each have to be right on their own.
        self.forget_spent(now, budget);
        let record = self.failures.entry(addr).or_insert(Failures {
            count: 0,
            last: now,
        });
        record.count += 1;
        record.last = now;
        record.count
    }

    /// Forget `addr`'s failures — and only `addr`'s.
    pub(crate) fn clear(&mut self, addr: IpAddr) {
        self.failures.remove(&addr);
    }

    /// Drop what the ledger no longer needs, so guessing from a fresh address
    /// each time cannot grow it without bound — free for anyone holding an IPv6
    /// /64, and a Pi is the machine that would notice.
    ///
    /// A record whose cooldown has fully elapsed carries no information:
    /// [`Lockout::record_failure`] would reset its count anyway, so remembering
    /// it and forgetting it are the same behaviour. Only if that leaves the
    /// ledger still at its cap does a *live* record go, and then the least
    /// recently seen — which at worst hands the most idle attacker its budget
    /// back. Run on failure rather than on a timer: an attempt is already
    /// paying for an Argon2 hash, so a walk of a small map is free beside it,
    /// and nothing grows while nobody is guessing.
    fn forget_spent(&mut self, now: Instant, budget: Budget) {
        self.failures
            .retain(|_, record| now.duration_since(record.last) < budget.cooldown);
        if self.failures.len() < MAX_TRACKED_ADDRESSES {
            return;
        }
        while self.failures.len() >= MAX_TRACKED_ADDRESSES {
            let stalest = self
                .failures
                .iter()
                .min_by_key(|(_, record)| record.last)
                .map(|(addr, _)| *addr)
                // The loop runs only while the map is at a non-zero cap.
                .expect("a full ledger has a least recently seen address");
            self.failures.remove(&stalest);
        }
        tracing::warn!(
            limit = MAX_TRACKED_ADDRESSES,
            "lockout ledger is full; the least recently seen addresses were forgotten"
        );
    }
}

/// How many addresses the lockout ledger tracks at once.
///
/// Far above what a real instance sees — an operator mistyping their password
/// is one address — and small enough that the whole map is a rounding error on
/// a Pi. It exists so that the ledger is bounded by *something* even inside a
/// single cooldown window, when [`Lockout::forget_spent`]'s expiry sweep has
/// nothing to reclaim.
const MAX_TRACKED_ADDRESSES: usize = 1_024;

#[cfg(test)]
mod tests {
    use super::*;

    /// What the admin login allowed before #68 made the budget a parameter, so
    /// the tests below read exactly as they did when they lived in `admin.rs`.
    pub(super) fn budget() -> Budget {
        Budget {
            attempts: 3,
            cooldown: Duration::from_secs(60),
        }
    }

    fn addr(last: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, last])
    }

    /// A distinct address per `n`, past the 256 a single octet can spell — the
    /// ledger's cap is larger than that.
    fn addr_n(n: u64) -> IpAddr {
        IpAddr::from([10, 1, (n >> 8) as u8, n as u8])
    }

    /// The budget is the only thing that differs between the two surfaces
    /// holding one of these (#68), so it is the only parameter — and a ledger
    /// read against a budget nobody has spent is not locked.
    #[test]
    fn the_budget_is_what_decides_when_a_lockout_bites() {
        let now = Instant::now();
        let mut lockout = Lockout::default();
        let generous = Budget {
            attempts: 10,
            ..budget()
        };

        for _ in 0..budget().attempts {
            lockout.record_failure(addr(1), now, budget());
        }

        assert!(lockout.locked_for(addr(1), now, budget()).is_some());
        assert_eq!(
            lockout.locked_for(addr(1), now, generous),
            None,
            "the same ledger, read against a budget that has not been spent"
        );
    }

    /// The budget, then the wall.
    #[test]
    fn an_address_is_locked_out_only_once_its_budget_is_spent() {
        let now = Instant::now();
        let mut lockout = Lockout::default();

        for attempt in 1..=2 {
            lockout.record_failure(addr(1), now, budget());
            assert_eq!(
                lockout.locked_for(addr(1), now, budget()),
                None,
                "attempt {attempt} is still inside the budget"
            );
        }
        lockout.record_failure(addr(1), now, budget());

        assert!(lockout.locked_for(addr(1), now, budget()).is_some());
    }

    /// The fix for rdio's zero-length cooldown: an operator who locks themselves
    /// out can wait it out instead of restarting the scanner.
    #[test]
    fn a_lockout_expires_on_its_own() {
        let start = Instant::now();
        let mut lockout = Lockout::default();
        for _ in 0..3 {
            lockout.record_failure(addr(1), start, budget());
        }

        assert!(
            lockout
                .locked_for(addr(1), start + Duration::from_secs(59), budget())
                .is_some(),
            "still inside the cooldown"
        );
        assert_eq!(
            lockout.locked_for(addr(1), start + Duration::from_secs(60), budget()),
            None,
            "the cooldown is over"
        );
    }

    /// ...and waiting it out returns the whole budget, not one more try.
    #[test]
    fn waiting_out_a_lockout_restores_the_whole_budget() {
        let start = Instant::now();
        let mut lockout = Lockout::default();
        for _ in 0..3 {
            lockout.record_failure(addr(1), start, budget());
        }
        let later = start + Duration::from_secs(60);

        assert_eq!(lockout.record_failure(addr(1), later, budget()), 1);
        assert_eq!(lockout.locked_for(addr(1), later, budget()), None);
    }

    /// Hammering a locked address keeps it locked: the cooldown runs from the
    /// last attempt, not the one that spent the budget.
    #[test]
    fn attempts_during_a_lockout_extend_it() {
        let start = Instant::now();
        let mut lockout = Lockout::default();
        for _ in 0..3 {
            lockout.record_failure(addr(1), start, budget());
        }

        lockout.record_failure(addr(1), start + Duration::from_secs(59), budget());

        assert!(
            lockout
                .locked_for(addr(1), start + Duration::from_secs(90), budget())
                .is_some(),
            "the cooldown restarted from the last attempt"
        );
    }

    /// One address's failures are its own. rdio keys the ledger on a spoofable
    /// header *and* clears the whole thing on any success; both of those let one
    /// client spend or restore another's budget.
    #[test]
    fn addresses_do_not_share_a_budget() {
        let now = Instant::now();
        let mut lockout = Lockout::default();
        for _ in 0..3 {
            lockout.record_failure(addr(1), now, budget());
        }

        assert!(lockout.locked_for(addr(1), now, budget()).is_some());
        assert_eq!(
            lockout.locked_for(addr(2), now, budget()),
            None,
            "a second address starts with a full budget"
        );

        lockout.clear(addr(2));

        assert!(
            lockout.locked_for(addr(1), now, budget()).is_some(),
            "clearing one address must not free another"
        );
    }

    /// The ledger is bounded, or it is a way to exhaust a Pi's memory by
    /// guessing from a fresh address each time — which an IPv6 /64 makes free.
    /// A record whose cooldown has fully elapsed carries no information (its
    /// budget is already whole again), so it is dropped rather than kept.
    #[test]
    fn a_ledger_forgets_addresses_whose_cooldown_has_passed() {
        let start = Instant::now();
        let mut lockout = Lockout::default();
        for host in 0..50 {
            lockout.record_failure(addr(host), start, budget());
        }
        assert_eq!(lockout.failures.len(), 50);

        // One more, a full cooldown later: the other 50 are spent history.
        lockout.record_failure(addr(200), start + Duration::from_secs(60), budget());

        assert_eq!(
            lockout.failures.len(),
            1,
            "only the address still inside its window should be remembered"
        );
    }

    /// ...and even inside one window the ledger cannot grow without limit: past
    /// the cap the least recently seen address is forgotten, which at worst
    /// hands a long-idle attacker its budget back.
    #[test]
    fn a_ledger_is_capped_even_when_nothing_has_expired() {
        // A cooldown long enough that nothing expires over the run, so this is
        // about the cap alone and not the sweep the test above covers.
        let budget = || Budget {
            cooldown: Duration::from_secs(24 * 60 * 60),
            ..super::tests::budget()
        };
        let start = Instant::now();
        let mut lockout = Lockout::default();

        // Every attempt a second apart, so "least recently seen" is unambiguous.
        for host in 0..=(MAX_TRACKED_ADDRESSES as u64) {
            lockout.record_failure(addr_n(host), start + Duration::from_secs(host), budget());
        }

        assert_eq!(lockout.failures.len(), MAX_TRACKED_ADDRESSES);
        assert!(
            !lockout.failures.contains_key(&addr_n(0)),
            "the least recently seen address should have been dropped"
        );
        assert!(
            lockout
                .failures
                .contains_key(&addr_n(MAX_TRACKED_ADDRESSES as u64)),
            "the newest attempt must be recorded"
        );
    }

    /// A `Retry-After` of 0 tells a client to try again immediately, which is
    /// the one thing a lockout must never say.
    #[test]
    fn a_lockout_never_reports_zero_seconds_left() {
        let start = Instant::now();
        let mut lockout = Lockout::default();
        for _ in 0..3 {
            lockout.record_failure(addr(1), start, budget());
        }

        for elapsed_ms in [0, 1, 999, 59_001, 59_999] {
            let left = lockout
                .locked_for(addr(1), start + Duration::from_millis(elapsed_ms), budget())
                .expect("locked");
            assert!(!left.is_zero(), "elapsed_ms={elapsed_ms} left={left:?}");
        }
    }

    proptest::proptest! {
        /// However the attempts fall, an address inside its budget is never
        /// locked out and an address that has spent it always is.
        #[test]
        fn the_budget_alone_decides_whether_an_address_is_locked(attempts in 0u32..10) {
            let now = Instant::now();
            let mut lockout = Lockout::default();
            for _ in 0..attempts {
                lockout.record_failure(addr(7), now, budget());
            }
            proptest::prop_assert_eq!(
                lockout.locked_for(addr(7), now, budget()).is_some(),
                attempts >= budget().attempts,
                "after {} attempts", attempts
            );
        }
    }
}
