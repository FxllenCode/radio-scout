//! The **tone-out worker** (#55): what looking at one Call for a page actually
//! is, and the loop that does it.
//!
//! # Off the ingest path, and its own queue
//!
//! Detection never runs inside an upload. Ingest stores, inserts, answers `200`
//! and puts the Call on the live feed; only then is the id offered here, and a
//! recorder that deletes its local copy on success has already been told the
//! Call is durable. That is the same bargain [`crate::enhance`] makes, and the
//! criterion the ticket states as *a 200 never waits on it*.
//!
//! It is a **separate** worker from enhancement rather than a stage inside it,
//! and the reason is what ships: `[enhancement] mode` is `off` by default, so a
//! detection folded into that worker would never run on a stock install. The
//! cost of the split is one extra decode on an Instance running both, which is
//! the trade — and the queue this drains is much the faster of the two, since
//! detection decodes and transforms where enhancement decodes, resamples twice,
//! filters, measures loudness and re-encodes.
//!
//! # What it is looking at
//!
//! **The audio as the recorder sent it**, which is why [`Subject`] carries the
//! object key rather than the worker reading the row twice. Enhancement may
//! replace a Call's object underneath this — it writes a new key and leaves the
//! old one for orphan-GC, which does not touch anything younger than
//! `[retention] orphan_grace` — and the levelled result is not what a page
//! should be looked for in: RNNoise is trained on speech and a held sinusoid is
//! exactly what it is built to remove.
//!
//! # What is deliberately not here
//!
//! **Nothing goes back over the Archive.** A profile written today marks the
//! Calls that follow it, and [`crate::db::entities::call::ToneState::NONE`] is
//! never re-queued — [`crate::enhance`]'s rule, for its reason: an Operator
//! adding a profile must not find their whole Archive being re-read on the next
//! boot. What a restart *does* pick up is `pending`, which is work this Instance
//! already accepted and had not finished.

use std::sync::Arc;

use tracing::{Instrument, Level, info, span, warn};

use super::{Profile, ToneMatch, Tones, detect, matched};
use crate::AppState;
use crate::call::CallId;
use crate::db::entities::call::ToneState;
use crate::db::repo;
use crate::enhance::Failure;
use crate::worker::Worker;

/// What detection needs of the world, in four questions.
///
/// A port for the reason [`crate::enhance::Archive`] is one: every arm worth
/// testing here is a failure — an object that has gone, a store that will not
/// answer, audio that will not decode — and a database and a filesystem that
/// both work can produce none of them.
#[async_trait::async_trait]
pub trait Archive: Send + Sync {
    /// Calls a previous process left mid-detection.
    async fn pending(&self) -> Result<Vec<CallId>, Failure>;

    /// The audio to look at and the profiles to look for, or `None` if the Call
    /// is no longer there.
    async fn subject(&self, call_id: CallId) -> Result<Option<Subject>, Failure>;

    /// The bytes behind an object key, or `None` if the object is gone.
    async fn read(&self, key: &str) -> Result<Option<bytes::Bytes>, Failure>;

    /// Write down how it ended.
    async fn settle(&self, call_id: CallId, settled: &Settled) -> Result<(), Failure>;
}

/// One Call, and what is worth looking for in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Subject {
    /// Where the audio is. Empty for an **Encrypted Call**, which has none.
    pub object_key: String,
    /// The enabled profiles on this Call's Talkgroup.
    pub profiles: Vec<Profile>,
}

/// How looking at one Call ended.
///
/// *Settled*, not *outcome*: CONTEXT.md reserves that word for what **Ingest**
/// decided about a Call. [`crate::enhance::Settled`]'s shape, with the arm that
/// makes this feature what it is — [`Settled::Paged`] carries the profiles that
/// fired, because a mark that could not say which station was paged answers the
/// wrong question.
#[derive(Debug, Clone, PartialEq)]
pub enum Settled {
    /// Looked at, and it held no page.
    Clear,
    /// Paged — these profiles, in the order they are configured.
    Paged(Vec<ToneMatch>),
    /// The Call is no longer there. Nothing is written: there is no row to
    /// write it on.
    Vanished,
    /// Could not be looked at. The Call keeps everything it arrived with; the
    /// only thing lost is knowing whether it held a page.
    Skipped {
        reason: &'static str,
        cause: Option<String>,
    },
}

impl Settled {
    /// The state the Call row is left in.
    pub fn state(&self) -> &'static str {
        match self {
            Settled::Clear => ToneState::CLEAR,
            Settled::Paged(_) => ToneState::MATCHED,
            Settled::Vanished => ToneState::NONE,
            Settled::Skipped { .. } => ToneState::SKIPPED,
        }
    }
}

/// Look at one Call.
///
/// **Pure decision, performed by the caller** — everything that could fail is a
/// question asked of [`Archive`], and everything decided is a [`Settled`]. The
/// order the questions come in is the cost order, and matters:
///
/// 1. The Call, with its channel's profiles. A Talkgroup with none is **clear
///    without the object ever being read** — which is most Calls on an Instance
///    that pages one channel out of four hundred.
/// 2. The audio. An **Encrypted Call** has none and is skipped rather than
///    called clear: nothing was looked at, and saying otherwise would be a
///    claim this made about audio it never had.
/// 3. The decode, then the transform, then the profiles.
pub async fn step(archive: &dyn Archive, call_id: CallId) -> Settled {
    let subject = match archive.subject(call_id).await {
        Ok(Some(subject)) => subject,
        Ok(None) => return Settled::Vanished,
        Err(cause) => return skipped("call-unreadable", cause),
    };
    if subject.profiles.is_empty() {
        return Settled::Clear;
    }
    // **Reachable, despite ingest never offering an Encrypted Call.** A
    // **Replacement** (#46) rewrites the row a queued id already names, and a
    // better copy of a transmission can be an encrypted one — so the audio that
    // was there when this Call was offered may be gone by the time it is looked
    // at. Skipped rather than called clear: nothing was looked at, and saying
    // otherwise would be a claim about audio this never had.
    if subject.object_key.is_empty() {
        return Settled::Skipped {
            reason: "no-audio",
            cause: None,
        };
    }
    let audio = match archive.read(&subject.object_key).await {
        Ok(Some(audio)) => audio,
        Ok(None) => return Settled::Vanished,
        Err(cause) => return skipped("audio-unreadable", cause),
    };
    let (samples, rate) = match crate::enhance::decode(&audio) {
        Ok(decoded) => decoded,
        Err(cause) => return skipped("undecodable", cause),
    };

    match matched(&detect::runs(&samples, rate), &subject.profiles) {
        found if found.is_empty() => Settled::Clear,
        found => Settled::Paged(found),
    }
}

fn skipped(reason: &'static str, cause: impl std::fmt::Display) -> Settled {
    Settled::Skipped {
        reason,
        cause: Some(cause.to_string()),
    }
}

/// Write down how it ended, and say so.
///
/// A page is **INFO** rather than WARN (ADR-0011 rule 7): it is a notable normal
/// event — the feature working — and not something an Operator must act on. The
/// station is named, because "a page was detected" without saying whose is the
/// line that sends somebody to the database.
async fn record(archive: &dyn Archive, call_id: CallId, settled: Settled) {
    if let Settled::Paged(found) = &settled {
        // Built before the macro rather than inside it, the way
        // `curate::document::import` does and for its reason: a `tracing` field
        // expression only runs when a subscriber is interested, which makes it
        // look unreachable to coverage.
        let profiles = found
            .iter()
            .map(|page| page.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let at_ms = found.first().map(|page| page.at_ms).unwrap_or_default();
        info!(%profiles, at_ms, "tone-out detected");
    }
    if let Settled::Skipped { reason, cause } = &settled {
        let cause = cause.as_deref().unwrap_or("");
        warn!(%reason, %cause, "this Call was not checked for a page");
    }
    if let Settled::Vanished = settled {
        return;
    }
    if let Err(error) = archive.settle(call_id, &settled).await {
        warn!(
            reason = %"settle-failed",
            %error,
            "could not write down what tone-out detection found"
        );
    }
}

/// Pick up what the last process left, saying what was found.
///
/// [`crate::enhance::resume`]'s bargain: detection is a background convenience
/// and must never be why a scanner refuses to come up, so an Archive that
/// cannot be asked leaves a WARN and nothing else.
async fn resume(archive: &dyn Archive, tones: &Tones) {
    let pending = match archive.pending().await {
        Ok(pending) => pending,
        Err(error) => {
            return warn!(
                reason = %"sweep-failed",
                %error,
                "could not look for Calls left mid-detection"
            );
        }
    };
    if pending.is_empty() {
        return;
    }
    // Counted separately, because they are different news: the first is work
    // about to happen, the second is Calls that will never be checked because
    // this queue could not hold them all.
    let queued = pending
        .iter()
        .filter(|call_id| tones.submit(**call_id))
        .count();
    let shed = pending.len() - queued;
    info!(queued, shed, "resuming tone-out detection after a restart");
}

/// Start the tone-out worker. `None` means one is already running.
///
/// **Started whatever the roster says**, unlike enhancement's: a Tone profile is
/// a row, so an Instance with none has an empty roster rather than a feature
/// switched off, and one written in the browser five minutes from now must be
/// detected against without a restart. With none configured
/// [`Tones::submit`] refuses everything and this sleeps on an empty queue,
/// costing nothing.
///
/// One worker, deliberately, for [`crate::enhance::spawn`]'s reason: this is
/// CPU-bound and a Pi's four cores are also serving audio, taking uploads and
/// running a database.
pub fn spawn(state: AppState) -> Option<Worker> {
    let tones = state.tones.clone();
    let mut inbox = tones.0.inbox.take()?;
    let meter = tones.0.meter.clone();
    // Owed before this returns, so an Instance can never be observed idle in
    // the window before the resume sweep has run (#93).
    let catching_up = meter.admit();

    Some(Worker::start(
        super::WORKER,
        meter,
        move |mut stop| async move {
            let archive: Arc<dyn Archive> = Arc::new(state.clone());
            {
                let _catching_up = catching_up;
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => return,
                    _ = resume(archive.as_ref(), &state.tones) => {}
                }
            }
            loop {
                let Some((call_id, _ticket)) = (tokio::select! {
                    biased;
                    _ = stop.cancelled() => break,
                    submitted = inbox.recv() => submitted,
                }) else {
                    // Unreachable while this task runs — it owns an `AppState`
                    // holding the sending half — and left in for `enhance::spawn`'s
                    // reason: without it a closed queue would spin on `None`.
                    break;
                };
                // Cancellable *during* a Call, not merely between Calls: one Call is
                // a whole decode and transform, and a stop that had to wait it out
                // would be a restart that appears to hang. Safe for the reason it is
                // there — the row is still `pending`, and the next boot's resume
                // picks it up. The Ticket is dropped either way, so a Call cut short
                // leaves no depth behind claiming it is still being worked on.
                let work = async {
                    let settled = step(archive.as_ref(), call_id).await;
                    record(archive.as_ref(), call_id, settled).await;
                }
                .instrument(span!(Level::ERROR, "tone", call_id));
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => break,
                    () = work => {}
                }
            }
        },
    ))
}

/// The Instance, as detection asks questions of it.
#[async_trait::async_trait]
impl Archive for AppState {
    async fn pending(&self) -> Result<Vec<CallId>, Failure> {
        Ok(repo::calls_pending_tone(&self.db).await?)
    }

    async fn subject(&self, call_id: CallId) -> Result<Option<Subject>, Failure> {
        Ok(repo::tone_subject(&self.db, call_id).await?)
    }

    async fn read(&self, key: &str) -> Result<Option<bytes::Bytes>, Failure> {
        Ok(self.audio.get(key).await?)
    }

    async fn settle(&self, call_id: CallId, settled: &Settled) -> Result<(), Failure> {
        match settled {
            Settled::Paged(found) => {
                repo::record_tone_matches(&self.db, call_id, found).await?;
                // A page is a **Mark**, and a Webhook fires on one (#54) — the
                // half of this feature that leaves the Instance. Queued here
                // rather than at ingest because that is the only place the
                // answer is known: at ingest this Call had not been looked at.
                crate::ingest::enqueue_tone_webhooks(self, call_id).await;
            }
            _ => repo::mark_tone(&self.db, call_id, settled.state()).await?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tone::Step;
    use rstest::rstest;
    use std::sync::Mutex;

    /// The Archive, substituted at its own interface (#37, #97) — because every
    /// arm worth testing here is a *failure*, and a database and a store that
    /// both work can produce none of them.
    #[derive(Default)]
    struct FakeArchive {
        /// The Call, if there is one: where its audio is, and what to look for.
        subject: Option<Subject>,
        /// The bytes behind that key, if the object is still there.
        audio: Option<Vec<u8>>,
        pending: Vec<CallId>,
        /// Which question answers with a failure, if any.
        fails: Option<&'static str>,
        /// What it was told, in order — so "it settled the Call" is an
        /// assertion rather than a log line to grep.
        settled: Mutex<Vec<Settled>>,
    }

    impl FakeArchive {
        /// An Archive holding one Call, with a real Quick Call page-out behind
        /// it and a profile that matches it.
        fn holding_a_page() -> Self {
            FakeArchive {
                subject: Some(Subject {
                    object_key: String::from("aa/page.wav"),
                    profiles: vec![station_12()],
                }),
                audio: Some(page_out()),
                ..FakeArchive::default()
            }
        }

        fn failing(mut self, question: &'static str) -> Self {
            self.fails = Some(question);
            self
        }

        fn refuse(&self, question: &'static str) -> Result<(), FakeFailure> {
            match self.fails == Some(question) {
                true => Err(FakeFailure(question)),
                false => Ok(()),
            }
        }
    }

    /// What a refused question says. One string, so an assertion about the
    /// cause reaching a log line is about *this* test's failure and not a
    /// driver's phrasing.
    #[derive(Debug)]
    struct FakeFailure(&'static str);

    impl std::fmt::Display for FakeFailure {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "the fake archive refused to {}", self.0)
        }
    }

    impl std::error::Error for FakeFailure {}

    #[async_trait::async_trait]
    impl Archive for FakeArchive {
        async fn pending(&self) -> Result<Vec<CallId>, Failure> {
            self.refuse("list pending")?;
            Ok(self.pending.clone())
        }

        async fn subject(&self, _call_id: CallId) -> Result<Option<Subject>, Failure> {
            self.refuse("look up the Call")?;
            Ok(self.subject.clone())
        }

        async fn read(&self, _key: &str) -> Result<Option<bytes::Bytes>, Failure> {
            self.refuse("read the audio")?;
            Ok(self.audio.clone().map(bytes::Bytes::from))
        }

        async fn settle(&self, _call_id: CallId, settled: &Settled) -> Result<(), Failure> {
            self.refuse("settle the Call")?;
            self.settled.lock().expect("settled").push(settled.clone());
            Ok(())
        }
    }

    fn station_12() -> Profile {
        Profile {
            id: 7,
            label: String::from("Station 12"),
            tolerance_pct: crate::tone::DEFAULT_TOLERANCE_PCT,
            gap_max_ms: crate::tone::DEFAULT_GAP_MAX_MS,
            steps: vec![
                Step {
                    hz: 1122.5,
                    min_ms: 800,
                },
                Step {
                    hz: 1465.6,
                    min_ms: 2000,
                },
            ],
        }
    }

    /// A Quick Call II page, as a WAV — hand-rolled rather than shared with
    /// `tests/common/audio.rs`, because that fixture is about what a *recorder*
    /// writes and this one is about what the worker does with bytes.
    fn page_out() -> Vec<u8> {
        const RATE: u32 = 8_000;
        let tone = |hz: f64, ms: usize| -> Vec<f32> {
            (0..ms * RATE as usize / 1000)
                .map(|n| 0.5 * (std::f64::consts::TAU * hz * n as f64 / RATE as f64).sin() as f32)
                .collect()
        };
        let samples = [tone(1122.5, 1000), tone(1465.6, 3000)].concat();

        let data: Vec<u8> = samples
            .iter()
            .flat_map(|s| ((s * i16::MAX as f32) as i16).to_le_bytes())
            .collect();
        let mut out = b"RIFF".to_vec();
        out.extend(((36 + data.len()) as u32).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(RATE.to_le_bytes());
        out.extend((RATE * 2).to_le_bytes());
        out.extend(2u16.to_le_bytes());
        out.extend(16u16.to_le_bytes());
        out.extend(b"data");
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
        out
    }

    /// The whole errand, over a working Archive.
    #[tokio::test]
    async fn a_page_settles_as_the_profile_that_fired() {
        let archive = FakeArchive::holding_a_page();

        assert_eq!(
            step(&archive, 1).await,
            Settled::Paged(vec![ToneMatch {
                profile_id: 7,
                label: String::from("Station 12"),
                at_ms: 0,
            }])
        );
    }

    /// **A Talkgroup with no profile is clear without the object ever being
    /// read** — which is most Calls on an Instance that pages one channel out of
    /// four hundred, and is why the profiles are asked for before the audio.
    ///
    /// Proved by taking the audio away: if the object were read, this would be
    /// `Vanished`.
    #[tokio::test]
    async fn a_channel_with_no_profile_costs_no_object_read() {
        let archive = FakeArchive {
            subject: Some(Subject {
                object_key: String::from("aa/page.wav"),
                profiles: Vec::new(),
            }),
            audio: None,
            ..FakeArchive::default()
        };

        assert_eq!(step(&archive, 1).await, Settled::Clear);
    }

    /// A page-out on a channel whose profile names other tones is somebody
    /// else's page.
    #[tokio::test]
    async fn audio_holding_no_page_is_clear() {
        let archive = FakeArchive {
            subject: Some(Subject {
                object_key: String::from("aa/page.wav"),
                profiles: vec![Profile {
                    steps: vec![Step {
                        hz: 602.6,
                        min_ms: 800,
                    }],
                    ..station_12()
                }],
            }),
            ..FakeArchive::holding_a_page()
        };

        assert_eq!(step(&archive, 1).await, Settled::Clear);
    }

    /// **Every way looking at a Call can fail**, and the slug each leaves.
    ///
    /// These are the arms an Operator meets when something is wrong, and the
    /// only reason they are reachable is that the Archive is a port: a real
    /// database that answers and a real store that holds the object can produce
    /// none of them.
    #[rstest]
    #[case::call(FakeArchive::holding_a_page().failing("look up the Call"), "call-unreadable")]
    #[case::audio(FakeArchive::holding_a_page().failing("read the audio"), "audio-unreadable")]
    #[case::undecodable(
        FakeArchive { audio: Some(b"not audio at all".to_vec()), ..FakeArchive::holding_a_page() },
        "undecodable",
    )]
    #[case::encrypted(
        FakeArchive {
            subject: Some(Subject { object_key: String::new(), profiles: vec![station_12()] }),
            ..FakeArchive::holding_a_page()
        },
        "no-audio",
    )]
    #[tokio::test]
    async fn a_call_that_cannot_be_looked_at_is_skipped_with_a_reason(
        #[case] archive: FakeArchive,
        #[case] expected: &str,
    ) {
        let Settled::Skipped { reason, .. } = step(&archive, 1).await else {
            panic!("expected a skip");
        };
        assert_eq!(reason, expected);
    }

    /// **A Call that is gone is not skipped, it is nothing at all** — there is
    /// no row to write a state on, and `Vanished` is the only arm [`record`]
    /// declines to settle.
    #[rstest]
    #[case::row_gone(FakeArchive::default())]
    #[case::object_gone(FakeArchive { audio: None, ..FakeArchive::holding_a_page() })]
    #[tokio::test]
    async fn a_call_that_is_gone_leaves_nothing_written(#[case] archive: FakeArchive) {
        let settled = step(&archive, 1).await;
        assert_eq!(settled, Settled::Vanished);

        record(&archive, 1, settled).await;
        assert!(archive.settled.lock().expect("settled").is_empty());
    }

    /// Each settlement leaves the row in the state that describes it.
    #[rstest]
    #[case(Settled::Clear, ToneState::CLEAR)]
    #[case(Settled::Paged(Vec::new()), ToneState::MATCHED)]
    #[case(Settled::Vanished, ToneState::NONE)]
    #[case(Settled::Skipped { reason: "undecodable", cause: None }, ToneState::SKIPPED)]
    fn a_settlement_names_the_state_it_leaves(
        #[case] settled: Settled,
        #[case] expected: &'static str,
    ) {
        assert_eq!(settled.state(), expected);
    }

    /// An Archive that will not take the answer costs the *answer*, not the
    /// process: the Call keeps everything it arrived with and the next boot
    /// finds it still `pending`.
    #[tokio::test]
    async fn an_archive_that_will_not_be_written_to_is_survived() {
        let archive = FakeArchive::holding_a_page().failing("settle the Call");

        record(&archive, 1, Settled::Clear).await;

        assert!(archive.settled.lock().expect("settled").is_empty());
    }

    /// A skip says so, once, with the cause — which is what an Operator reads
    /// when their pages stop being caught.
    #[tokio::test]
    async fn a_skip_is_written_down_with_its_cause() {
        let archive = FakeArchive::holding_a_page();

        record(
            &archive,
            1,
            Settled::Skipped {
                reason: "undecodable",
                cause: Some(String::from("no")),
            },
        )
        .await;

        assert_eq!(
            archive.settled.lock().expect("settled").len(),
            1,
            "still settled, so the Call is not looked at again"
        );
    }

    /// A restart puts back what the last process had accepted. What it must
    /// **not** do is go looking for anything else — `pending` is the whole of
    /// the question, and the Archive answers it.
    #[tokio::test]
    async fn a_resume_re_queues_what_was_left_and_counts_what_it_could_not() {
        let tones = Tones::new(crate::tone::ToneConfig { queue_depth: 1 });
        tones
            .0
            .armed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let archive = FakeArchive {
            pending: vec![1, 2, 3],
            ..FakeArchive::default()
        };

        resume(&archive, &tones).await;

        // One fits; the rest are shed, which is the honest outcome of a queue
        // this small and is exactly what the log line separates.
        assert_eq!(tones.0.meter.load().depth, 1);
    }

    /// An Archive that cannot be asked leaves a WARN and nothing else — a
    /// background convenience must never be why a scanner refuses to come up.
    #[rstest]
    #[case::unreadable(FakeArchive::default().failing("list pending"))]
    #[case::nothing_left(FakeArchive::default())]
    #[tokio::test]
    async fn a_resume_that_finds_nothing_queues_nothing(#[case] archive: FakeArchive) {
        let tones = Tones::default();

        resume(&archive, &tones).await;

        assert_eq!(tones.0.meter.load().depth, 0);
    }

    /// A page says which station, at INFO — "a tone-out was detected" without
    /// naming whose is the line that sends somebody to the database.
    #[tokio::test]
    async fn a_page_is_written_down_naming_the_station() {
        let archive = FakeArchive::holding_a_page();
        let capture = crate::testing::LogCapture::start();

        record(
            &archive,
            1,
            Settled::Paged(vec![ToneMatch {
                profile_id: 7,
                label: String::from("Station 12"),
                at_ms: 1840,
            }]),
        )
        .await;

        let logged = capture.text();
        assert!(logged.contains("Station 12"), "{logged}");
        assert!(logged.contains("tone-out detected"), "{logged}");
    }
}
