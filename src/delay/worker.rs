//! The **release** Worker: what emits a Delayed Call once it is due (#73).
//!
//! # Restart-safe because it remembers nothing
//!
//! It holds no timer per Call and no schedule of its own. Each pass asks the
//! database two things — which waiting Calls are due, and when the next one is —
//! releases the first and sleeps until the second. So a restart, a reschedule
//! and a Call stored a moment ago are all the same event: something to look at
//! again. A Call that came due while the process was down is simply due on the
//! first pass after boot, and goes out then — late, never lost, never early.
//!
//! # A release is one transaction, and all of it or none
//!
//! [`release`] takes the Call's row, queues it for every **Downstream** and
//! **Webhook** it reaches, builds its live frame, and only then allocates its
//! emission and stamps it — all inside one transaction, and only after that
//! commits does the live feed hear it. Three properties, each from the order:
//!
//! - **Taking the row first** closes the race with a tone-out found at the same
//!   moment: whichever of the two writes second sees the first, so the page is
//!   posted exactly once (`ingest::queue_tone_deliveries` holds the other
//!   half). The take is guarded on the Call still waiting, so two passes
//!   cannot emit one Call twice.
//! - **Allocating the emission last** keeps the window in which a later
//!   emission can reach a Listener first as narrow as ingest's own — one
//!   statement and a commit, rather than everything a release reads. A
//!   Listener whose socket dropped inside that window and came back with the
//!   later cursor is what the window costs, so it is kept as small as ingest
//!   keeps it.
//! - **Building the frame inside it** makes the release whole: a Call that
//!   could not be shown on the live feed is not marked as having gone out, so
//!   nothing leaves half-published. A release that cannot be written in full
//!   leaves the Call waiting and is tried again after [`RETRY`] — late, never
//!   early. An emission allocated and then rolled back is a number skipped,
//!   which a cursor cannot notice: a Backfill asks for what is *after* it,
//!   never for what is *next*.

use std::sync::Arc;
use std::time::Duration;

use sea_orm::DbErr;
use tracing::{info, warn};

use crate::AppState;
use crate::call::CallId;
use crate::db::repo;
use crate::worker::Worker;

/// What this Worker is called on the status page and in the registry (#93).
pub const WORKER: &str = "delay";

/// How many due Calls one read releases before reading again — a backlog that
/// came due during a long outage goes out in bounded steps rather than as one
/// unbounded read.
pub const BATCH: u64 = 100;

/// How long a pass that could not reach the database waits before trying again.
const RETRY: Duration = Duration::from_secs(5);

/// Start the release Worker, unless one is already running.
///
/// Always started: a Delay is a column, so one written from the browser five
/// minutes from now must be honoured without a restart. With nothing waiting it
/// sleeps on its wake-up and costs nothing.
pub fn spawn(state: AppState) -> Option<Worker> {
    let delays = state.delays.clone();
    delays.wake_up().claim()?;
    // Its first pass is owed from boot: a Call that came due while the Instance
    // was down goes out on it, and nothing may read idle before it has.
    delays.wake_up().owes_a_first_pass();
    Some(Worker::start(
        WORKER,
        delays.wake_up().meter(),
        move |mut stop| async move {
            loop {
                // Read before the pass, settled after it — `WakeUp::caught_up`.
                let owed = delays.wake_up().outstanding();
                let next = match pass(&state).await {
                    Ok(next) => next,
                    Err(error) => {
                        warn!(
                            reason = %"release-failed",
                            %error,
                            "could not release the delayed Calls that are due; trying again shortly"
                        );
                        Some(
                            state
                                .clock
                                .now_ms()
                                .saturating_add(RETRY.as_millis() as i64),
                        )
                    }
                };
                delays.wake_up().caught_up(owed);
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = delays.wake_up().woken() => {}
                    _ = until(&state, next) => {}
                }
            }
        },
    ))
}

/// Sleep until the next Call is due, or for ever when nothing is waiting.
async fn until(state: &AppState, next: Option<i64>) {
    match next {
        Some(at_ms) => state.clock.sleep_until(at_ms).await,
        None => std::future::pending().await,
    }
}

/// Release everything that is due, and answer when the next waiting Call is.
async fn pass(state: &AppState) -> Result<Option<i64>, DbErr> {
    loop {
        let due = repo::due_calls(&state.db, state.clock.now_ms(), BATCH).await?;
        let more = due.len() as u64 == BATCH;
        for id in due {
            release(state, id).await?;
        }
        if !more {
            break;
        }
    }
    repo::next_due(&state.db).await
}

/// Emit one Delayed Call — see the module header for why in this order.
async fn release(state: &AppState, id: CallId) -> Result<(), DbErr> {
    let now_ms = state.clock.now_ms();
    let txn = state.db.begin().await?;
    // `None` is a Call another pass already released, or one Retention took
    // while it waited — nothing to emit either way, and the transaction rolls
    // back as it drops.
    let Some(row) = repo::take_waiting_call(&txn, id).await? else {
        return Ok(());
    };
    let owed = crate::ingest::owed_on_release(&txn, &row, now_ms).await?;
    let views = crate::archive::stored_calls(&txn, std::slice::from_ref(&row)).await?;
    let seq = state.live.next_emission();
    repo::emit_call(&txn, id, seq).await?;
    txn.commit().await?;
    owed.hand_over(state);

    for mut view in views {
        // Built from the row as it was taken — before the stamp — so it reads
        // as still waiting; it has just gone out.
        view.waiting = false;
        state.live.publish(crate::live::Emitted {
            seq,
            call: Arc::new(view),
        });
    }
    // INFO, once per Delayed Call: the Admission line said it was stored, and
    // this is the only record of when it actually reached anybody. The figure
    // is worked out before the macro, which evaluates its fields only for a
    // subscriber that wants them — `MergeChange::record`'s reason.
    let waited_ms = now_ms.saturating_sub(row.created_at_ms);
    info!(call_id = id, waited_ms, "delayed call published");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repo::{NewCall, Resolved};

    /// **Two passes cannot emit one Call twice.** The stamp is guarded on the
    /// Call still waiting, so a second release of the same id — a pass racing
    /// another, or a reschedule's wake landing mid-pass — finds nothing to do,
    /// sends no second frame and owes no sink a second delivery.
    #[tokio::test]
    async fn a_call_released_twice_goes_out_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::connect(&crate::testing::sqlite_url(&tmp))
            .await
            .expect("db");
        let call = repo::insert_call(
            &db,
            &NewCall::new(11, 54241, 1_000),
            Some(crate::blob::StoredAudio::written("aa/1.wav".into(), 3)),
            &Resolved::unresolved(),
            true,
            0,
        )
        .await
        .expect("store a call");
        repo::delay_call(&db, call.id, 600_000)
            .await
            .expect("put it on a Delay");
        let store = Arc::new(crate::BlobStore::filesystem(tmp.path().join("audio")).expect("blob"));
        let state = AppState::new(store, db, crate::ingest::IngestConfig::default());
        let mut live = state.live.subscribe();

        release(&state, call.id).await.expect("the first release");
        release(&state, call.id).await.expect("the second, a no-op");

        let frame = live.try_recv().expect("one frame");
        assert_eq!(frame.call.id, call.id);
        assert!(frame.call.delayed);
        assert!(live.try_recv().is_err(), "and only one");
    }
}
