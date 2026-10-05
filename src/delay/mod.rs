//! **The Delay policy** (#73, spec US 62): a Call is stored the moment it
//! arrives and published to **Listeners** only once a configured interval has
//! passed — flagged as delayed, and surviving a restart.
//!
//! CONTEXT.md: a **Delay** is per-System/Talkgroup officer-safety policy, not a
//! buffer. The Archive stays complete; what changes is *when* a Call becomes
//! something anybody outside the Instance can reach.
//!
//! # What a Delayed Call is, in two columns
//!
//! `calls.delayed_until_ms` is set when a Delay applied, and is the schedule.
//! `calls.emitted_seq` (#94) is set when the Call went out. A Call is
//! **published** when it was never delayed, or when it has been emitted
//! ([`crate::db::entities::call::Model::is_published`]) — so the release *is*
//! the emission, and "a Listener can reach it" and "it went out on the live
//! feed" are one fact, written in one transaction by [`worker`]. Nothing about
//! publication is a clock comparison at read time: a Worker that has fallen
//! behind keeps a Call back for longer, which is the direction an officer-safety
//! policy is allowed to err in, and never lets one out early.
//!
//! # Decisions, and why each one is the safe one (#73's grilling)
//!
//! - **Measured from arrival, never from the recorder's timestamp.** A Call
//!   cannot have been transmitted after it arrived, so `arrival + delay` is
//!   never earlier than `transmission + delay` — whatever a recorder's clock
//!   says. rdio-scanner measures from `call.Timestamp` (`delayer.go:157`), so a
//!   recorder whose clock runs ten minutes slow publishes a ten-minute Delay
//!   immediately, and a backlog uploaded late skips it entirely.
//! - **The longest Delay the Call reaches.** A **Patch** puts one transmission
//!   on several channels and keep-best (#46) stores it once, under whichever
//!   copy arrived first — so the Call's *own* channel is an accident of upload
//!   order, and a rule reading only it would delay a tactical transmission or
//!   not depending on which recorder was quicker. [`effective`] takes the
//!   maximum across the Call's channel and everything it is patched to, which
//!   is the same answer whichever copy won.
//! - **The policy in force decides.** A Delay raised mid-incident also covers
//!   the Calls already waiting, and a mistyped one lowered releases them on the
//!   new schedule: [`reschedule`] re-reads every waiting Call against the
//!   roster on the same request that changed it.
//! - **The sinks wait too.** A **Downstream** forward and a **Webhook** post are
//!   queued in the transaction that releases the Call, never at storage — a
//!   peer would publish it at once, and a Discord channel is usually a
//!   community's. Queuing at release also keeps a not-yet-due delivery from
//!   sitting at the head of a sink's queue, where #52's in-order rule would
//!   stop every Call behind it.
//! - **Waiting answers exactly like not there.** Every Listener-facing read
//!   leaves a waiting Call out — search, its total, the filter options, the
//!   density ribbon, the panel's activity, an export, a radio's history — and
//!   every read of one Call answers `404`, the **Access code** rule (#68) for
//!   the same reason: a `403`, or a count that moved, would announce that
//!   something just happened on a channel an Operator delayed precisely so
//!   nobody would know yet.
//!
//! # An Instance that delays nothing pays nothing
//!
//! [`Delays::is_armed`] is one cached bit — [`crate::access::Access::is_gating`]'s
//! shape — saying whether any Delay is configured or any Call is waiting. While
//! it is false no read adds a clause and ingest reads nothing extra. Stale-true
//! buys a clause and stale-false would be a leak, so every way it can be wrong
//! is arranged to be the first:
//!
//! - It is set **before** a Delayed Call is stored, so no read can run between a
//!   waiting row existing and the clause that hides it.
//! - It starts **true** and a read that fails keeps the last answer, so a boot
//!   that cannot read the roster leaves it set rather than open.
//! - It is cleared only by reading the roster — at boot, and on every write to
//!   a Delay — never by the release Worker. So the last waiting Call going out
//!   after the last Delay was lifted leaves it set until the next such write or
//!   the next boot. Clearing it from the Worker would be a read racing an
//!   ingest that has armed it and not yet committed its row, which is exactly
//!   the stale-false this is arranged against; a clause paid for a while for
//!   nothing is the price.
//!
//! # Improving on rdio-scanner
//!
//! | rdio-scanner (`delayer.go`) | Radio-Scout |
//! |---|---|
//! | measured from the recorder's timestamp | measured from arrival, so a slow clock cannot shorten it |
//! | a Talkgroup's `0` means *inherit*, so one channel can never be undelayed on a delayed System | `NULL` inherits and `0` means none |
//! | a Delayed Call is fetchable by id over the `CAL` command the whole time | every read of a waiting Call answers `404` |
//! | restart reads the `delayed` table, **deletes all of it**, then re-adds rows one by one — a crash in between publishes everything at once | the schedule is a column on the Call; a restart reads it and changes nothing |
//! | one `time.Timer` per waiting Call, in a map written from several goroutines without its mutex | one Worker, one indexed read for what is due and one for when to wake |
//! | the flag lives on the in-memory Call, so the Archive forgets a Call was ever delayed | the flag is the column, and travels with the Call wherever it is shown |
//! | the schedule is fixed when a Call arrives | the policy in force decides, so raising a Delay mid-incident covers what is already waiting |

pub mod worker;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sea_orm::{ConnectionTrait, DbErr};
use tracing::warn;

use crate::call::CallId;
use crate::worker::WakeUp;

/// The longest Delay an Operator may set, in minutes: a day.
///
/// An officer-safety Delay is minutes; one longer than a day is not a Delay
/// but an Archive nobody can listen to, and almost certainly a value typed into
/// the wrong box.
pub const MAX_MINUTES: u32 = 24 * 60;

const MINUTE_MS: i64 = 60 * 1000;

// ---------------------------------------------------------------------------
// The policy
// ---------------------------------------------------------------------------

/// One channel's Delay, in minutes: its own when it has one, else its System's,
/// else none.
///
/// `NULL` inherits at every level — the enhancement / restricted / retention
/// shape — and `0` is a Delay of nothing, which is how a channel says "not this
/// one" on a System that delays everything.
pub fn of_channel(own: Option<i64>, system: Option<i64>) -> i64 {
    own.or(system).unwrap_or(0).max(0)
}

/// A Call's Delay, in minutes: **the longest of every channel it reaches** —
/// its own, and every channel it is patched to, all on one System.
pub fn effective(system: Option<i64>, own: Option<i64>, patched: &[Option<i64>]) -> i64 {
    patched
        .iter()
        .map(|patch| of_channel(*patch, system))
        .fold(of_channel(own, system), i64::max)
}

/// When a Call that arrived at `arrival_ms` under a Delay of `minutes` is due to
/// go out — `None` when it is not delayed at all.
///
/// Saturating, because a Delay is bounded by [`MAX_MINUTES`] on every surface
/// that writes one but the arrival is whatever the clock said, and no arithmetic
/// here may be the thing that panics.
pub fn due(arrival_ms: i64, minutes: i64) -> Option<i64> {
    (minutes > 0).then(|| arrival_ms.saturating_add(minutes.saturating_mul(MINUTE_MS)))
}

// ---------------------------------------------------------------------------
// The subsystem
// ---------------------------------------------------------------------------

/// The Delay subsystem, cloned into every handler: whether anything is delayed
/// at all, and the release Worker's wake-up.
///
/// Like a **Webhook** and unlike enhancement there is no disabled form: a Delay
/// is a column, so an Instance with none has nothing switched off and pays
/// nothing.
#[derive(Clone)]
pub struct Delays(Arc<Inner>);

struct Inner {
    /// Whether any Delay is configured, or any Call is waiting. See the module
    /// header for why it is set before a waiting row exists.
    armed: AtomicBool,
    /// What the release Worker owes, and its wake-up. One unit is *"look
    /// again"* — never one waiting Call, which is owed by nobody until it is
    /// due; [`WakeUp`] carries the argument, because the senders owe the same.
    wake_up: WakeUp,
}

impl Default for Delays {
    /// **Armed**, until somebody has looked.
    ///
    /// The boot reads the roster straight away ([`Delays::rearm`]), and a read
    /// that fails keeps the last answer — so the answer it keeps has to be the
    /// safe one. Starting unarmed would make a database that could not be read
    /// at boot an Instance whose waiting Calls every search returns: stale-true
    /// costs a clause, stale-false is a leak.
    fn default() -> Self {
        Delays(Arc::new(Inner {
            armed: AtomicBool::new(true),
            wake_up: WakeUp::default(),
        }))
    }
}

impl Delays {
    /// Whether anything on this Instance may be waiting out a Delay.
    pub fn is_armed(&self) -> bool {
        self.0.armed.load(Ordering::Relaxed)
    }

    /// Set the bit directly — [`crate::access::Access::set_gating`]'s shape.
    /// Ingest sets it **before** a Delayed Call is stored, so no read can run
    /// between the row existing and the clause that hides it.
    pub fn set_armed(&self, armed: bool) {
        self.0.armed.store(armed, Ordering::Relaxed);
    }

    /// Re-read whether anything is delayed: at boot, and on every write to a
    /// Delay.
    pub async fn rearm(&self, db: &crate::db::Db) {
        self.arm(crate::db::repo::anything_delayed(db).await);
    }

    /// [`Delays::rearm`]'s decision, apart from the read. A read that fails keeps
    /// the last answer, [`crate::access::Access::arm`]'s rule: guessing `false`
    /// would be a leak.
    pub fn arm(&self, answer: Result<bool, DbErr>) {
        match answer {
            Ok(armed) => self.set_armed(armed),
            Err(error) => warn!(
                reason = %"roster-unreadable",
                %error,
                "could not re-read which channels are delayed; keeping the last answer"
            ),
        }
    }

    /// Something changed that the release Worker should look at — a Delayed
    /// Call stored, a schedule moved, time passed — so it owes one more pass.
    /// Called after the write it is about has committed ([`WakeUp::owes`]).
    pub fn wake(&self) {
        self.0.wake_up.owes(1);
    }

    /// The policy in force decides (#73's grilling): re-read whether anything is
    /// delayed, move every waiting Call onto the schedule the roster now gives
    /// it, and wake the Worker to act on it. What every write to a Delay calls,
    /// on the same request ([`crate::AppState::channels_changed`]).
    ///
    /// A reschedule that cannot be read leaves the old schedule standing and
    /// says so: the Calls stay waiting, which is the safe direction.
    pub async fn reconsider(&self, db: &crate::db::Db) {
        self.rearm(db).await;
        if let Err(error) = reschedule(db).await {
            warn!(
                reason = %"reschedule-failed",
                %error,
                "could not move the waiting Calls onto the new Delay; they keep their old schedule"
            );
        }
        self.wake();
    }

    /// The release Worker's accounting and wake-up.
    pub(crate) fn wake_up(&self) -> &WakeUp {
        &self.0.wake_up
    }
}

/// Move every waiting Call onto the schedule the roster gives it now, and
/// answer how many moved.
///
/// Reads the waiting Calls — bounded by how much traffic a Delay holds, never
/// by the Archive — the Calls' patches, and the roster, then decides each one
/// with [`effective`] and [`due`]: **the same two functions ingest decides
/// with**, so a Call rescheduled and a Call arriving under the same roster
/// cannot be given different answers.
///
/// A Delay lowered to nothing makes a waiting Call due at its own arrival —
/// which is to say now — rather than clearing its schedule: it *was* kept back,
/// and its flag says so.
pub async fn reschedule<C: ConnectionTrait>(db: &C) -> Result<usize, DbErr> {
    let waiting = crate::db::repo::waiting_calls(db).await?;
    if waiting.is_empty() {
        return Ok(0);
    }
    let roster = crate::db::repo::delay_roster(db).await?;
    let ids: Vec<CallId> = waiting.iter().map(|call| call.id).collect();
    let mut patched: HashMap<CallId, Vec<i64>> = HashMap::new();
    for (call_id, talkgroup_ref) in crate::db::repo::patches_of(db, &ids).await? {
        patched.entry(call_id).or_default().push(talkgroup_ref);
    }

    let mut moved = 0;
    for call in waiting {
        let system = roster.system(call.system_id);
        let patches: Vec<Option<i64>> = patched
            .get(&call.id)
            .into_iter()
            .flatten()
            .map(|talkgroup_ref| roster.by_ref(call.system_id, *talkgroup_ref))
            .collect();
        let minutes = effective(system, roster.by_id(call.talkgroup_id), &patches);
        let due_at = due(call.created_at_ms, minutes).unwrap_or(call.created_at_ms);
        if call.delayed_until_ms != Some(due_at) {
            crate::db::repo::delay_call(db, call.id, due_at).await?;
            moved += 1;
        }
    }
    Ok(moved)
}

/// Every Delay an Operator has written down: each System's, and each Talkgroup
/// that overrides its System.
///
/// Only the overrides, because `NULL` inherits — a Talkgroup missing from here
/// is exactly a Talkgroup whose Delay is its System's.
#[derive(Debug, Default)]
pub struct Roster {
    /// Every System's own Delay, keyed by System id — `None` where it has none.
    pub systems: HashMap<i64, Option<i64>>,
    /// Keyed by Talkgroup id, for a Call's own channel.
    pub talkgroups: HashMap<i64, i64>,
    /// Keyed by `(system_id, ref)`, for the channels a Call is patched to —
    /// which a Call stores as Refs.
    pub refs: HashMap<(i64, i64), i64>,
}

impl Roster {
    /// A System's own Delay, which every channel saying nothing inherits.
    fn system(&self, system_id: i64) -> Option<i64> {
        self.systems.get(&system_id).copied().flatten()
    }

    /// A Call's own channel's override, by the Talkgroup id it is filed
    /// under — `None` inherits.
    fn by_id(&self, talkgroup_id: i64) -> Option<i64> {
        self.talkgroups.get(&talkgroup_id).copied()
    }

    /// A patched channel's override, by the Ref a Call's patch row stores —
    /// `None` inherits.
    fn by_ref(&self, system_id: i64, talkgroup_ref: i64) -> Option<i64> {
        self.refs.get(&(system_id, talkgroup_ref)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    /// **Most specific wins, `NULL` inherits, `0` is none** — the whole of one
    /// channel's answer, as a table.
    #[rstest]
    #[case::nothing_anywhere(None, None, 0)]
    #[case::the_system_s(None, Some(10), 10)]
    #[case::the_channel_s_own(Some(5), None, 5)]
    #[case::the_channel_overrides_its_system_upward(Some(30), Some(10), 30)]
    #[case::the_channel_overrides_its_system_downward(Some(2), Some(10), 2)]
    #[case::zero_is_none_on_a_delayed_system(Some(0), Some(10), 0)]
    #[case::a_negative_value_is_none(Some(-5), Some(10), 0)]
    fn one_channel(#[case] own: Option<i64>, #[case] system: Option<i64>, #[case] expected: i64) {
        assert_eq!(of_channel(own, system), expected);
    }

    /// **The longest it reaches**: a Patch onto a delayed channel delays the
    /// Call, whichever channel it was filed under.
    #[rstest]
    #[case::no_patches(None, Some(3), &[], 3)]
    #[case::patched_onto_a_longer_one(None, Some(0), &[Some(15)], 15)]
    #[case::patched_onto_a_shorter_one(None, Some(15), &[Some(0)], 15)]
    #[case::patched_onto_one_inheriting_the_system(Some(8), Some(0), &[None], 8)]
    #[case::the_longest_of_several(Some(1), None, &[Some(4), Some(9), None], 9)]
    fn a_whole_call(
        #[case] system: Option<i64>,
        #[case] own: Option<i64>,
        #[case] patched: &[Option<i64>],
        #[case] expected: i64,
    ) {
        assert_eq!(effective(system, own, patched), expected);
    }

    #[rstest]
    #[case::undelayed(1_000, 0, None)]
    #[case::a_minute(1_000, 1, Some(61_000))]
    #[case::ten_minutes(0, 10, Some(600_000))]
    #[case::never_past_the_end_of_time(i64::MAX - 1, 10, Some(i64::MAX))]
    fn when_a_call_is_due(
        #[case] arrival: i64,
        #[case] minutes: i64,
        #[case] expected: Option<i64>,
    ) {
        assert_eq!(due(arrival, minutes), expected);
    }

    fn channel() -> impl Strategy<Value = Option<i64>> {
        proptest::option::of(-5i64..=i64::from(MAX_MINUTES))
    }

    proptest! {
        /// Whatever the roster, a Call's Delay is one of its channels' and no
        /// channel's is longer — so a Patch can lengthen a Delay and never
        /// shorten it.
        #[test]
        fn a_call_is_delayed_by_its_longest_channel(
            system in channel(),
            own in channel(),
            patched in proptest::collection::vec(channel(), 0..6),
        ) {
            let minutes = effective(system, own, &patched);
            let channels: Vec<i64> = std::iter::once(of_channel(own, system))
                .chain(patched.iter().map(|patch| of_channel(*patch, system)))
                .collect();
            prop_assert!(channels.contains(&minutes));
            prop_assert!(channels.iter().all(|each| *each <= minutes));
        }

        /// A Delayed Call is never due before it arrived plus its Delay.
        #[test]
        fn a_call_is_never_due_early(arrival in 0i64..=4_000_000_000_000, minutes in 1i64..=1440) {
            prop_assert_eq!(due(arrival, minutes), Some(arrival + minutes * 60_000));
        }
    }

    /// Armed until somebody has looked, and a look that fails keeps the last
    /// answer — so an unreadable roster is never an undelayed one, at boot or
    /// after it.
    #[test]
    fn a_read_that_fails_keeps_the_last_answer() {
        let delays = Delays::default();
        assert!(delays.is_armed(), "armed until the roster has been read");

        delays.arm(Err(DbErr::Custom(String::from("gone"))));
        assert!(
            delays.is_armed(),
            "an unreadable roster is not an undelayed one"
        );

        delays.arm(Ok(false));
        assert!(!delays.is_armed());
        delays.arm(Err(DbErr::Custom(String::from("gone"))));
        assert!(!delays.is_armed(), "and the last answer is kept either way");
    }
}
