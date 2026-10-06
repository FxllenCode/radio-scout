//! How fast a Station stream is sent (#74).
//!
//! **Real time, after a lead.** A player is handed three seconds the moment it
//! connects, so it starts at once rather than buffering in real time, and from
//! then on exactly a frame per 36 ms — never faster, because a Call that goes on
//! the air has to wait behind whatever was already sent, and every second sent
//! ahead is a second the next Call arrives late.
//!
//! **A reader that stalled is not owed the time it was gone.** It paused, or its
//! network did, and the frames it did not take were never sent. When it comes
//! back it is given a lead's worth again and carries on from now — not a minute
//! of audio in one burst, which would put every Call after it a minute behind.

use std::time::Duration;

/// Frames sent the moment a player connects.
pub(crate) const LEAD_FRAMES: u64 = 84;

/// How many frames a stream owes its listener.
#[derive(Debug, Default)]
pub(crate) struct Pace {
    /// Frames sent so far.
    sent: u64,
    /// Frames of time a stalled reader was not given back.
    forgiven: u64,
}

impl Pace {
    /// The frames owed now, `elapsed` after the stream began — counted as sent.
    ///
    /// Never more than [`LEAD_FRAMES`] at once: whatever more had fallen due is
    /// time the reader was not there for, and is written off rather than owed.
    pub(crate) fn owed(&mut self, elapsed: Duration) -> u64 {
        let played = (elapsed.as_micros() / super::encode::FRAME.as_micros()) as u64;
        let due = (LEAD_FRAMES + played).saturating_sub(self.forgiven);
        let owed = due.saturating_sub(self.sent);
        let owed = match owed > LEAD_FRAMES {
            true => {
                self.forgiven += owed - LEAD_FRAMES;
                LEAD_FRAMES
            }
            false => owed,
        };
        self.sent += owed;
        owed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station::encode::FRAME;

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    /// A player starts the moment it has some audio in hand, so it is given
    /// three seconds of it at once rather than made to wait for them.
    #[test]
    fn a_player_starts_with_three_seconds_in_hand() {
        let mut pace = Pace::default();

        let opening = pace.owed(Duration::ZERO);

        assert!((FRAME * opening as u32).abs_diff(Duration::from_secs(3)) < FRAME);
    }

    /// After that, exactly as fast as it plays: a frame per 36 ms, and nothing
    /// twice for the same moment.
    #[test]
    fn then_a_frame_for_every_frame_of_time() {
        let mut pace = Pace::default();
        pace.owed(Duration::ZERO);

        assert_eq!(pace.owed(ms(35)), 0);
        assert_eq!(pace.owed(ms(36)), 1);
        assert_eq!(pace.owed(ms(36)), 0);
        assert_eq!(pace.owed(ms(1_008)), 27);
    }

    /// **A player that stopped reading is not owed the time it was gone.** It
    /// paused, or its network stalled, and the frames it did not take were
    /// never sent — so when it comes back it is given a lead's worth again and
    /// carries on from now, rather than a minute of audio in one burst that
    /// would put every Call after it a minute behind live.
    #[test]
    fn a_reader_that_stalled_is_given_a_lead_not_the_backlog() {
        let mut pace = Pace::default();
        pace.owed(Duration::ZERO);

        assert_eq!(pace.owed(ms(60_000)), LEAD_FRAMES);
        assert_eq!(pace.owed(ms(60_036)), 1);
    }

    proptest::proptest! {
        /// Whenever it is asked, a stream is never further ahead of real time
        /// than its lead, and never owes more than a lead at once.
        #[test]
        fn never_more_than_a_lead_ahead_of_the_clock(
            mut times in proptest::collection::vec(0u64..600_000, 1..200),
        ) {
            times.sort_unstable();
            let mut pace = Pace::default();
            let mut sent = 0;
            for at in times {
                let owed = pace.owed(ms(at));
                proptest::prop_assert!(owed <= LEAD_FRAMES);
                sent += owed;
                proptest::prop_assert!(sent <= LEAD_FRAMES + at * 1_000 / FRAME.as_micros() as u64);
            }
        }
    }
}
