//! The **quiet-span worker** (#59): what looking at one Call for its gaps is,
//! and the loop that does it.
//!
//! # Off the ingest path, and its own queue
//!
//! Scanning never runs inside an upload. Ingest stores, inserts, answers `200`
//! and puts the Call on the live feed; only then is the id offered here. That is
//! the bargain [`crate::enhance`] and [`crate::tone::worker`] both make, and it
//! has a consequence this feature has to live with: **the live frame is already
//! gone** by the time the spans exist, and nothing republishes one (#46). So a
//! Call sitting in a Listener's queue does not carry its spans, and Catch-up
//! pulls them for the window it is about to play (`GET /api/calls/quiet`). The
//! Archive, which is read after the fact, gets them on the Call itself.
//!
//! # What it is looking at
//!
//! **The audio as the recorder sent it**, which is why [`Subject`] carries the
//! object key rather than the worker reading the row twice — and why a
//! **Replacement** (#46) re-offers the Call. Enhancement may write a new object
//! underneath this; unlike a page-out, a quiet span *would* survive that
//! (levelling moves the whole Call, and the detector's threshold is relative),
//! so the two do not have to be ordered — but a replacement is different audio
//! entirely, and its gaps are its own.

use std::sync::Arc;

use tracing::{Instrument, Level, debug, info, span, warn};

use super::{Quiet, Span, detect};
use crate::AppState;
use crate::call::CallId;
use crate::db::entities::call::QuietState;
use crate::db::repo;
use crate::enhance::Failure;
use crate::worker::Worker;

/// What scanning needs of the world, in four questions.
///
/// A port for the reason [`crate::enhance::Archive`] is one: every arm worth
/// testing here is a failure — an object that has gone, a store that will not
/// answer, audio that will not decode — and a database and a filesystem that
/// both work can produce none of them.
#[async_trait::async_trait]
pub trait Archive: Send + Sync {
    /// Calls a previous process left mid-scan.
    async fn pending(&self) -> Result<Vec<CallId>, Failure>;

    /// The audio to look at, or `None` if the Call is no longer there.
    async fn subject(&self, call_id: CallId) -> Result<Option<Subject>, Failure>;

    /// The bytes behind an object key, or `None` if the object is gone.
    async fn read(&self, key: &str) -> Result<Option<bytes::Bytes>, Failure>;

    /// Write down what was found.
    async fn settle(&self, call_id: CallId, settled: &Settled) -> Result<(), Failure>;
}

/// One Call, and where its audio is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    /// Empty for an **Encrypted Call**, which has none.
    pub object_key: String,
}

/// How looking at one Call ended.
///
/// *Settled*, not *outcome*: CONTEXT.md reserves that word for what **Ingest**
/// decided about a Call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// Looked at. The spans found, which is very often none — most Calls are
    /// somebody saying one thing.
    Scanned(Vec<Span>),
    /// The Call is no longer there. Nothing is written: there is no row to write
    /// it on.
    Vanished,
    /// Could not be looked at. The Call keeps everything it arrived with; the
    /// only thing lost is a trim, and Catch-up falls back to the rate alone.
    Skipped {
        reason: &'static str,
        cause: Option<String>,
    },
}

impl Settled {
    /// The state the Call row is left in.
    pub fn state(&self) -> &'static str {
        match self {
            Settled::Scanned(_) => QuietState::DONE,
            Settled::Vanished => QuietState::NONE,
            Settled::Skipped { .. } => QuietState::SKIPPED,
        }
    }

    /// What goes in the column — [`super::pack`]ed, and `None` where there is
    /// nothing to say.
    pub fn packed(&self) -> Option<String> {
        match self {
            Settled::Scanned(spans) if !spans.is_empty() => Some(super::pack(spans)),
            _ => None,
        }
    }
}

/// Look at one Call.
///
/// **Pure decision, performed by the caller** — everything that could fail is a
/// question asked of [`Archive`], and everything decided is a [`Settled`]. The
/// order the questions come in is the cost order, and is
/// [`crate::tone::worker::step`]'s.
pub async fn step(archive: &dyn Archive, call_id: CallId) -> Settled {
    let subject = match archive.subject(call_id).await {
        Ok(Some(subject)) => subject,
        Ok(None) => return Settled::Vanished,
        Err(cause) => return skipped("call-unreadable", cause),
    };
    // **Reachable, despite ingest never offering an Encrypted Call.** A
    // **Replacement** (#46) rewrites the row a queued id already names, and a
    // better copy of a transmission can be an encrypted one — so the audio that
    // was there when this Call was offered may be gone by the time it is looked
    // at. Skipped rather than scanned-with-nothing: an empty span list means
    // "looked at, no gaps", and this looked at nothing.
    if subject.object_key.is_empty() {
        return Settled::Skipped {
            reason: NO_AUDIO,
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
    Settled::Scanned(detect::spans(&samples, rate))
}

/// The one skip that is not a fault — an **Encrypted Call**, which has no audio
/// to look at. Named because [`record`] reads it back to decide a level.
const NO_AUDIO: &str = "no-audio";

fn skipped(reason: &'static str, cause: impl std::fmt::Display) -> Settled {
    Settled::Skipped {
        reason,
        cause: Some(cause.to_string()),
    }
}

/// Write down what was found, and say so where an Operator would want to know.
///
/// **A scan says nothing** (ADR-0011 rule 8): this runs on every Call there is,
/// so a line per Call is exactly the hot loop that rule names. A failure is a
/// WARN, because a store that will not answer is a thing to look at — and it is
/// per Call by necessity, since unlike the Mining sweep there is no pass to
/// report once at the end of.
///
/// **Except `no-audio`, which is DEBUG** (ADR-0011 rule 7, and #92's "a refusal
/// an Operator would not act on is DEBUG"). That arm is an **Encrypted Call**
/// reaching a scanner it was never offered to, which only a **Replacement**
/// (#46) can arrange: nothing is broken, nothing can be done about it, and a
/// System whose traffic is mostly encrypted would fill a log with it. This is
/// deliberately one level quieter than [`crate::tone::worker::record`]'s
/// identically-shaped arm, and the difference is the rule rather than the shape.
async fn record(archive: &dyn Archive, call_id: CallId, settled: Settled) {
    if let Settled::Skipped { reason, cause } = &settled {
        let cause = cause.as_deref().unwrap_or("");
        if *reason == NO_AUDIO {
            debug!(%reason, "this Call has no audio to scan");
        } else {
            warn!(%reason, %cause, "this Call was not scanned for quiet spans");
        }
    }
    if let Settled::Vanished = settled {
        return;
    }
    if let Err(error) = archive.settle(call_id, &settled).await {
        warn!(
            reason = %"settle-failed",
            %error,
            "could not write down where this Call is quiet"
        );
    }
}

/// Pick up what the last process left.
///
/// [`crate::enhance::resume`]'s bargain: scanning is a background convenience
/// and must never be why a scanner refuses to come up, so an Archive that cannot
/// be asked leaves a WARN and nothing else.
async fn resume(archive: &dyn Archive, quiet: &Quiet) {
    let pending = match archive.pending().await {
        Ok(pending) => pending,
        Err(error) => {
            return warn!(
                reason = %"sweep-failed",
                %error,
                "could not look for Calls left mid-scan"
            );
        }
    };
    if pending.is_empty() {
        return;
    }
    let queued = pending
        .iter()
        .filter(|call_id| quiet.submit(**call_id))
        .count();
    let shed = pending.len() - queued;
    info!(queued, shed, "resuming quiet-span scanning after a restart");
}

/// Start the quiet-span worker. `None` means one is already running, or this
/// Instance does not scan.
///
/// **Not started at all when `[quiet] enabled = false`**, unlike
/// [`crate::tone::worker::spawn`], and the difference is what the switch means:
/// a Tone profile is a row that may be written five minutes from now, where this
/// is an Operator saying their hardware does not do this. Nothing can turn it on
/// without a restart, so nothing has to be left running in case it does.
///
/// One worker, deliberately, for [`crate::enhance::spawn`]'s reason: this is
/// CPU-bound and a Pi's four cores are also serving audio, taking uploads and
/// running a database.
pub fn spawn(state: AppState) -> Option<Worker> {
    let quiet = state.quiet.clone();
    if !quiet.is_enabled() {
        return None;
    }
    let mut inbox = quiet.0.inbox.take()?;
    let meter = quiet.0.meter.clone();
    // Owed before this returns, so an Instance can never be observed idle in the
    // window before the resume sweep has run (#93).
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
                    _ = resume(archive.as_ref(), &state.quiet) => {}
                }
            }
            loop {
                let Some((call_id, _ticket)) = (tokio::select! {
                    biased;
                    _ = stop.cancelled() => break,
                    submitted = inbox.recv() => submitted,
                }) else {
                    // Unreachable while this task runs — it owns an `AppState`
                    // holding the sending half — and left in for
                    // `enhance::spawn`'s reason: without it a closed queue would
                    // spin on `None`.
                    break;
                };
                // Cancellable *during* a Call, not merely between Calls: one
                // Call is a whole decode, and a stop that had to wait it out
                // would be a restart that appears to hang. Safe for the reason
                // it is there — the row is still `pending`, and the next boot's
                // resume picks it up.
                let work = async {
                    let settled = step(archive.as_ref(), call_id).await;
                    record(archive.as_ref(), call_id, settled).await;
                }
                .instrument(span!(Level::ERROR, "quiet", call_id));
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => break,
                    () = work => {}
                }
            }
        },
    ))
}

/// The Instance, as scanning asks questions of it.
#[async_trait::async_trait]
impl Archive for AppState {
    async fn pending(&self) -> Result<Vec<CallId>, Failure> {
        Ok(repo::calls_pending_quiet(&self.db).await?)
    }

    async fn subject(&self, call_id: CallId) -> Result<Option<Subject>, Failure> {
        Ok(repo::quiet_subject(&self.db, call_id).await?)
    }

    async fn read(&self, key: &str) -> Result<Option<bytes::Bytes>, Failure> {
        Ok(self.audio.get(key).await?)
    }

    async fn settle(&self, call_id: CallId, settled: &Settled) -> Result<(), Failure> {
        Ok(repo::settle_quiet(&self.db, call_id, settled).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::sync::Mutex;

    /// The Archive, substituted at its own interface (#37, #97) — because every
    /// arm worth testing here is a *failure*, and a database and a store that
    /// both work can produce none of them.
    #[derive(Default)]
    struct FakeArchive {
        /// The Call, if there is one: where its audio is.
        subject: Option<Subject>,
        /// The bytes behind that key, if the object is still there.
        audio: Option<Vec<u8>>,
        pending: Vec<CallId>,
        /// Which question answers with a failure, if any.
        fails: Option<&'static str>,
        /// What it was told, in order — so "it settled the Call" is an assertion
        /// rather than a log line to grep.
        settled: Mutex<Vec<Settled>>,
    }

    impl FakeArchive {
        /// An Archive holding one Call: two keyups with a long gap between them.
        fn holding_a_gap() -> Self {
            FakeArchive {
                subject: Some(Subject {
                    object_key: String::from("aa/call.wav"),
                }),
                audio: Some(two_keyups()),
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

    /// What a refused question says. One string, so an assertion about the cause
    /// reaching a log line is about *this* test's failure and not a driver's
    /// phrasing.
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

    /// Two keyups with three seconds of hang time between them, as a WAV —
    /// hand-rolled for [`crate::tone::worker`]'s reason: `tests/common/audio.rs`
    /// is about what a *recorder* writes, and this is about what the worker does
    /// with bytes.
    fn two_keyups() -> Vec<u8> {
        const RATE: u32 = 8_000;
        let mut seed = 7u32;
        let mut noise = |amplitude: f32, ms: usize| -> Vec<f32> {
            (0..ms * RATE as usize / 1_000)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    ((seed >> 8) as f32 / (1 << 23) as f32 - 1.0) * amplitude
                })
                .collect()
        };
        let samples = [noise(0.3, 1_500), noise(0.002, 3_000), noise(0.3, 1_500)].concat();

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
    async fn a_call_with_a_gap_settles_as_the_gap() {
        let archive = FakeArchive::holding_a_gap();

        let Settled::Scanned(spans) = step(&archive, 1).await else {
            panic!("expected a scan");
        };
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert!(
            spans[0].start_ms > 1_400 && spans[0].end_ms < 4_600,
            "{spans:?}"
        );
    }

    /// **Every way looking at a Call can fail**, and the slug each leaves. These
    /// are the arms an Operator meets when something is wrong, and the only
    /// reason they are reachable is that the Archive is a port.
    #[rstest]
    #[case::call(FakeArchive::holding_a_gap().failing("look up the Call"), "call-unreadable")]
    #[case::audio(FakeArchive::holding_a_gap().failing("read the audio"), "audio-unreadable")]
    #[case::undecodable(
        FakeArchive { audio: Some(b"not audio at all".to_vec()), ..FakeArchive::holding_a_gap() },
        "undecodable",
    )]
    #[case::encrypted(
        FakeArchive {
            subject: Some(Subject { object_key: String::new() }),
            ..FakeArchive::holding_a_gap()
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
    #[case::object_gone(FakeArchive { audio: None, ..FakeArchive::holding_a_gap() })]
    #[tokio::test]
    async fn a_call_that_is_gone_leaves_nothing_written(#[case] archive: FakeArchive) {
        let settled = step(&archive, 1).await;
        assert_eq!(settled, Settled::Vanished);

        record(&archive, 1, settled).await;
        assert!(archive.settled.lock().expect("settled").is_empty());
    }

    /// Each settlement leaves the row in the state that describes it, and puts
    /// in the column exactly what it found.
    ///
    /// **`Scanned` with nothing found writes `None`**, which is the ordinary
    /// answer and is what lets a re-scan after a **Replacement** (#46) *clear*
    /// what the previous copy's scan wrote.
    #[rstest]
    #[case(Settled::Scanned(vec![Span { start_ms: 100, end_ms: 2_000 }]), QuietState::DONE, Some("100-2000"))]
    #[case(Settled::Scanned(Vec::new()), QuietState::DONE, None)]
    #[case(Settled::Vanished, QuietState::NONE, None)]
    #[case(Settled::Skipped { reason: "undecodable", cause: None }, QuietState::SKIPPED, None)]
    fn a_settlement_names_the_state_and_the_column_it_leaves(
        #[case] settled: Settled,
        #[case] state: &'static str,
        #[case] packed: Option<&str>,
    ) {
        assert_eq!(settled.state(), state);
        assert_eq!(settled.packed().as_deref(), packed);
    }

    /// An Archive that will not take the answer costs the *answer*, not the
    /// process: the Call keeps everything it arrived with and the next boot
    /// finds it still `pending`.
    #[tokio::test]
    async fn an_archive_that_will_not_be_written_to_is_survived() {
        let archive = FakeArchive::holding_a_gap().failing("settle the Call");
        let capture = crate::testing::LogCapture::start();

        record(&archive, 1, Settled::Scanned(Vec::new())).await;

        let logged = capture.text();
        assert!(logged.contains("reason=settle-failed"), "{logged}");
    }

    /// A skip says why, because a store that will not answer is a thing to look
    /// at — and it is the only line this worker writes, since a line per scanned
    /// Call is exactly the hot loop ADR-0011 rule 8 forbids.
    #[tokio::test]
    async fn a_scan_says_nothing_and_a_skip_says_why() {
        let archive = FakeArchive::holding_a_gap();
        let capture = crate::testing::LogCapture::start();

        record(&archive, 1, Settled::Scanned(Vec::new())).await;
        assert_eq!(capture.text(), "", "a scan is silent");

        record(
            &archive,
            1,
            Settled::Skipped {
                reason: "undecodable",
                cause: Some(String::from("no")),
            },
        )
        .await;
        let logged = capture.text();
        assert!(logged.contains("reason=undecodable"), "{logged}");
    }

    /// **What a restart picks back up**, and what it says about it. An Archive
    /// that cannot be asked leaves a WARN and nothing else: scanning is a
    /// background convenience and must never be why a scanner refuses to boot.
    #[tokio::test]
    async fn a_restart_resumes_what_was_pending_and_survives_an_archive_that_cannot_be_asked() {
        let quiet = Quiet::default();
        let capture = crate::testing::LogCapture::start();

        resume(&FakeArchive::default(), &quiet).await;
        assert_eq!(capture.text(), "", "nothing pending says nothing");

        let archive = FakeArchive {
            pending: vec![1, 2, 3],
            ..FakeArchive::default()
        };
        resume(&archive, &quiet).await;
        let logged = capture.text();
        assert!(logged.contains("queued=3"), "{logged}");

        resume(&FakeArchive::default().failing("list pending"), &quiet).await;
        let logged = capture.text();
        assert!(logged.contains("reason=sweep-failed"), "{logged}");
    }

    /// A queue too small for what the last process left **sheds the rest**,
    /// counted separately — they are different news: work about to happen, and
    /// Calls that will never be scanned.
    #[tokio::test]
    async fn a_resume_that_does_not_fit_says_what_it_shed() {
        let quiet = Quiet::new(crate::quiet::QuietConfig {
            enabled: true,
            queue_depth: 2,
        });
        let archive = FakeArchive {
            pending: vec![1, 2, 3, 4],
            ..FakeArchive::default()
        };
        let capture = crate::testing::LogCapture::start();

        resume(&archive, &quiet).await;

        let logged = capture.text();
        assert!(
            logged.contains("queued=2") && logged.contains("shed=2"),
            "{logged}"
        );
    }
}
