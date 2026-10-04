//! The **Dirwatch** Worker: every watch's folder, the files arriving in them,
//! and the one-at-a-time walk of each through the ingest pipeline.
//!
//! # One unit of work
//!
//! **A file owed an answer.** The ticket is admitted where the file is handed
//! over — inside the operating system's callback, or by a scan finding it — and
//! rides with the file until it has been ingested or refused (#93's rule). Two
//! states deliberately hold no ticket, because nothing is owed *yet* and a
//! ticket held through them would hang every `settle()` in the suite:
//!
//! - a Trunk Recorder `.json` **waiting for its audio** — TR writes the
//!   document *before* it renders the audio (`call_concluder.cc`:
//!   `create_call_json` precedes `render_call_audio_artifacts`), so for a
//!   moment every Call is a `.json` alone. rdio reads it then, finds no audio,
//!   and returns `nil` — the Call is gone, silently. Here the audio's own
//!   arrival wakes it, and only a `.json` alone for [`PAIR_GRACE`] is refused.
//! - a file whose ingest **broke** (the database, the store) — it is retried
//!   after [`RETRY`] rather than spun on, and it is never deleted.
//!
//! # The watermark
//!
//! A watch that keeps its files has to know which ones it already ingested, or
//! every restart re-ingests the folder. `seen_through_ms` says *every file
//! written at or before this instant has been handled*, and it only advances as
//! far as the oldest file still pending — so it never passes one that is owed.
//! A boot reads what is newer; a watch that deletes as it goes needs no
//! watermark at all, since whatever is still there is still owed.
//!
//! What it cannot see is a file the operating system never told us about — an
//! inotify queue overflow. That case announces itself (`need_rescan`), and the
//! rescan looks back [`LOOKBACK_MS`] behind the watermark, skipping what this
//! run already handled; a `cp -p` of an old recording, whose preserved
//! timestamp is already under the watermark, is caught live by its own event.
//!
//! # Events this ignores
//!
//! Reading a file is itself an event on Linux (`IN_OPEN`, `IN_CLOSE_NOWRITE`),
//! and so is the access-time update. A watcher that reacted to those would read
//! a file, be told it was read, and read it again, for ever — so only a file
//! being created, written, closed after writing or renamed into place counts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind};
use notify::{Event, EventKind, RecursiveMode};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use tokio::sync::mpsc;
use tracing::{Instrument, Level, info, span, warn};

use super::mask::Mask;
use super::{
    AUDIO, Bound, Command, Format, MAX_FILE_BYTES, Root, Routing, Status, Unreadable, audio_mime,
    dsdplus, sdrtrunk, within_roots,
};
use crate::AppState;
use crate::db::entities::dirwatch;
use crate::db::repo::{self, NewCall};
use crate::failure::{Failure, Stage};
use crate::ingest::{self, Authority};
use crate::worker::{Meter, Ticket, Worker};

/// What this Worker is known by on the status surface.
pub const WORKER: &str = "dirwatch";

/// How long a Trunk Recorder `.json` may wait for its audio before it is refused
/// `no-audio`. Generous: on a Pi, TR's render of a long Call (sox, then ffmpeg
/// for `compressWav`) can take seconds.
const PAIR_GRACE: Duration = Duration::from_secs(60);

/// How long a file whose ingest broke waits before it is tried again.
const RETRY: Duration = Duration::from_secs(30);

/// How often a `poll` watch looks, for a share with no events to give.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How far behind the watermark a rescan looks for a file an overflow hid.
const LOOKBACK_MS: i64 = 10 * 60 * 1000;

/// One operating-system event, for watch `id`, owed by the ticket beside it.
type Delivery = (i64, notify::Result<Event>, Ticket);

/// What the operating system calls with each event for one watch.
type Callback = Box<dyn FnMut(notify::Result<Event>) + Send>;

/// The operating system's half of a watch: how a folder comes to be watched.
///
/// A parameter rather than a call, so that the one failure no test can arrange
/// for real — the OS refusing, as Linux does once `fs.inotify.max_user_watches`
/// is spent on a Pi watching a deep archive — is still one a test can be.
type Start = fn(&Path, bool, Callback) -> notify::Result<Box<dyn notify::Watcher + Send>>;

/// Watch `directory` and everything under it: with the operating system's own
/// mechanism, or — for a network share, which has none to give — by polling.
fn start_watching(
    directory: &Path,
    poll: bool,
    callback: Callback,
) -> notify::Result<Box<dyn notify::Watcher + Send>> {
    let polling = notify::Config::default().with_poll_interval(POLL_INTERVAL);
    let mut watcher: Box<dyn notify::Watcher + Send> = match poll {
        true => Box::new(notify::PollWatcher::new(callback, polling)?),
        false => Box::new(notify::recommended_watcher(callback)?),
    };
    watcher.watch(directory, RecursiveMode::Recursive)?;
    Ok(watcher)
}

/// Start the Worker. `None` if it already runs.
pub fn spawn(state: AppState) -> Option<Worker> {
    let dirwatch = state.dirwatch.clone();
    let mut commands = dirwatch.0.inbox.take()?;
    let meter = dirwatch.0.meter.clone();
    // Owed before this returns, so the Instance is never observed idle before
    // the boot scan has looked at every watch (#93).
    let starting = meter.admit();

    Some(Worker::start(WORKER, meter.clone(), move |mut stop| {
        async move {
            let (deliver, mut events) = mpsc::unbounded_channel();
            let mut watches = Watches {
                state,
                meter,
                deliver,
                running: HashMap::new(),
                start: start_watching,
            };
            {
                let _starting = starting;
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => return,
                    () = watches.rearm() => {}
                }
            }
            loop {
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => break,
                    Some((command, _ticket)) = commands.recv() => watches.command(command).await,
                    Some((id, event, _ticket)) = events.recv() => watches.on_event(id, event).await,
                    // One file per turn of the loop, so a stop is never kept
                    // waiting behind a folder's worth of backlog.
                    next = when_due(watches.next()) => watches.work(next).await,
                }
            }
        }
    }))
}

/// The file owed an answer soonest, once it is due — and never, while nothing
/// is owed, which is what keeps an idle Worker asleep.
async fn when_due(next: Option<Next>) -> Next {
    match next {
        Some(next) => {
            tokio::time::sleep_until(next.due.into()).await;
            next
        }
        None => std::future::pending().await,
    }
}

/// Every running watch, and how to reach the Instance they ingest into.
struct Watches {
    state: AppState,
    meter: Arc<Meter>,
    deliver: mpsc::UnboundedSender<Delivery>,
    running: HashMap<i64, Watch>,
    start: Start,
}

/// The file owed an answer soonest.
struct Next {
    due: Instant,
    id: i64,
    path: PathBuf,
}

impl Watches {
    async fn command(&mut self, command: Command) {
        match command {
            Command::Rearm => self.rearm().await,
            // "Scan now", from the admin screen. A file refused this run is
            // **not** refused again unless it has changed: the same bytes get
            // the same answer, and an Operator who fixes the watch instead
            // re-arms it, which starts it afresh. An id no longer running —
            // deleted between the request and now — has nothing to look at.
            Command::Scan(id) => {
                if let Some(watch) = self.running.get_mut(&id) {
                    watch
                        .scan(Scope::Rescan, &self.meter)
                        .instrument(span!(Level::ERROR, "dirwatch", watch_id = id))
                        .await;
                }
            }
        }
    }

    /// Read the roster and make the running set match it.
    ///
    /// A watch whose row is unchanged keeps running untouched — its pending
    /// files, what it has seen — so an edit to one watch does not restart the
    /// others.
    async fn rearm(&mut self) {
        let rows = match dirwatch::Entity::find()
            .order_by_asc(dirwatch::Column::Id)
            .all(&self.state.db)
            .await
        {
            Ok(rows) => rows,
            Err(cause) => {
                self.state
                    .metrics
                    .fail(&Failure::broke(Stage::Dirwatch, cause));
                return;
            }
        };
        let dirwatch = self.state.dirwatch.clone();
        dirwatch.forget_health(|id| rows.iter().any(|row| row.id == id));
        self.running.retain(|id, watch| {
            rows.iter()
                .any(|row| row.id == *id && same_watch(row, &watch.row))
        });

        for row in rows {
            if self.running.contains_key(&row.id) {
                continue;
            }
            if row.disabled {
                dirwatch.set_health(row.id, |health| health.status = Status::Disabled);
                continue;
            }
            let id = row.id;
            let span = span!(Level::ERROR, "dirwatch", watch_id = id);
            let callback = self.callback(id);
            let roots = &dirwatch.config().roots;
            match span.in_scope(|| open(row, roots, callback, self.start)) {
                Ok(mut watch) => {
                    dirwatch.set_health(id, |health| health.status = Status::Watching);
                    watch.scan(Scope::Boot, &self.meter).instrument(span).await;
                    self.running.insert(id, watch);
                }
                Err(status) => dirwatch.set_health(id, |health| health.status = status),
            }
        }
    }

    /// What the operating system calls for watch `id`: hand the event to the
    /// loop with a ticket admitted **on the operating system's own thread**, the
    /// instant it tells us anything, so nothing reads idle while an event is
    /// still on its way.
    fn callback(&self, id: i64) -> Callback {
        let deliver = self.deliver.clone();
        let meter = self.meter.clone();
        Box::new(move |event| {
            let _ = deliver.send((id, event, meter.admit()));
        })
    }

    /// An event for watch `id` — nobody's, if the watch has stopped since the
    /// operating system queued it.
    async fn on_event(&mut self, id: i64, event: notify::Result<Event>) {
        if let Some(watch) = self.running.get_mut(&id) {
            watch
                .on_event(event, &self.meter)
                .instrument(span!(Level::ERROR, "dirwatch", watch_id = id))
                .await;
        }
    }

    /// The file owed an answer soonest, across every watch.
    fn next(&self) -> Option<Next> {
        self.running
            .iter()
            .flat_map(|(id, watch)| {
                watch.pending.iter().map(move |(path, pending)| Next {
                    due: pending.due,
                    id: *id,
                    path: path.clone(),
                })
            })
            .min_by_key(|next| next.due)
    }

    /// Answer one file, and settle what it is owed.
    async fn work(&mut self, next: Next) {
        let Watches { state, running, .. } = self;
        if let Some(watch) = running.get_mut(&next.id)
            && let Some(pending) = watch.pending.remove(&next.path)
        {
            let span = span!(
                Level::ERROR,
                "dirwatch",
                watch_id = next.id,
                file = %file_name(&next.path),
            );
            let outcome = watch
                .answer(state, &next.path)
                .instrument(span.clone())
                .await;
            watch
                .settle(state, next.path, pending, outcome)
                .instrument(span)
                .await;
        }
    }
}

/// Start watching what `row` describes, inside `roots` — or say why it cannot
/// be watched, as the status its row will show.
fn open(
    row: dirwatch::Model,
    roots: &[Root],
    callback: Callback,
    start: Start,
) -> Result<Watch, Status> {
    let (reader, extension) = readable(&row)?;
    // Checked on every start, not only when the row was saved: the TOML is the
    // authority, and it may have changed since (ADR-0021).
    let directory = match within_roots(Path::new(&row.directory), roots) {
        Ok(directory) => directory,
        Err(Bound::OutsideRoots) => {
            warn!(directory = %row.directory, "dirwatch is outside every root; not watching");
            return Err(Status::OutsideRoots);
        }
        Err(Bound::NoSuchDirectory) => {
            warn!(directory = %row.directory, "dirwatch directory does not exist; not watching");
            return Err(Status::NoSuchDirectory);
        }
    };
    let watcher = start(&directory, row.poll, callback).map_err(|error| {
        warn!(%error, "the operating system would not watch the dirwatch directory");
        Status::CannotWatch
    })?;
    info!(directory = %directory.display(), format = %row.format, "dirwatch watching");

    Ok(Watch {
        reader,
        extension,
        directory,
        routing: Routing {
            system_ref: row.system_ref,
            talkgroup_ref: row.talkgroup_ref,
            frequency: row.frequency,
        },
        delay: Duration::from_millis(row.delay_ms.max(0) as u64),
        _watcher: std::sync::Mutex::new(watcher),
        pending: HashMap::new(),
        seen: HashMap::new(),
        newest_ms: row.seen_through_ms,
        started_through_ms: row.seen_through_ms,
        row,
    })
}

/// How a row's files are read, and which audio it reads — or
/// [`Status::Unreadable`] for a row this release cannot run: one written by
/// another release, or by hand. The curation surface refuses all of these, so
/// only a row that did not come through it reaches here.
fn readable(row: &dirwatch::Model) -> Result<(Reader, String), Status> {
    let reader = match (Format::from_slug(&row.format), row.mask.as_deref()) {
        (Some(Format::TrunkRecorder), _) => Reader::TrunkRecorder,
        (Some(Format::SdrTrunk), _) => Reader::SdrTrunk,
        (Some(Format::DsdPlus), _) => Reader::DsdPlus,
        (Some(Format::Mask), Some(text)) => {
            Reader::Mask(Mask::compile(text).map_err(|_| Status::Unreadable)?)
        }
        (Some(Format::Mask), None) | (None, _) => return Err(Status::Unreadable),
    };
    let extension = row
        .extension
        .clone()
        .unwrap_or_else(|| reader.format().default_extension().to_owned())
        .to_ascii_lowercase();
    match audio_mime(&extension) {
        Some(_) => Ok((reader, extension)),
        None => Err(Status::Unreadable),
    }
}

/// How one watch's files are read.
enum Reader {
    TrunkRecorder,
    SdrTrunk,
    DsdPlus,
    Mask(Mask),
}

impl Reader {
    fn format(&self) -> Format {
        match self {
            Reader::TrunkRecorder => Format::TrunkRecorder,
            Reader::SdrTrunk => Format::SdrTrunk,
            Reader::DsdPlus => Format::DsdPlus,
            Reader::Mask(_) => Format::Mask,
        }
    }
}

/// One watch, running.
struct Watch {
    row: dirwatch::Model,
    reader: Reader,
    extension: String,
    directory: PathBuf,
    routing: Routing,
    delay: Duration,
    /// Held so the operating system keeps telling us; dropped to stop. Behind a
    /// lock only so a `&Watch` may be held across an await — nothing ever
    /// takes it.
    _watcher: std::sync::Mutex<Box<dyn notify::Watcher + Send>>,
    pending: HashMap<PathBuf, Pending>,
    /// Files handled or refused this run, and the stamp they had — so a late
    /// or repeated event for the same bytes is not a second ingest, and a
    /// refused file is not re-refused until it changes.
    seen: HashMap<PathBuf, Stamp>,
    /// The newest modification time handled this run.
    newest_ms: i64,
    /// The watermark as it stood when this run of the watch started —
    /// everything at or before it was settled by an earlier run, or predates
    /// the watch altogether.
    started_through_ms: i64,
}

/// A file owed an answer.
struct Pending {
    due: Instant,
    written_ms: i64,
    ticket: Option<Ticket>,
}

/// What a file looked like: when it was last written, and how big.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    written_ms: i64,
    bytes: u64,
}

impl Stamp {
    /// Two files that are one Call, as one stamp: written when the later was,
    /// and as big as both. So a Trunk Recorder Call changes when *either* half
    /// does — its audio still growing after the `.json` was read is a reason
    /// to read it again — and it is as new as its newest half, which is the
    /// half its watermark has to have passed.
    fn with(self, other: Stamp) -> Stamp {
        Stamp {
            written_ms: self.written_ms.max(other.written_ms),
            bytes: self.bytes.saturating_add(other.bytes),
        }
    }
}

/// How far a scan looks back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// A watch starting: what is newer than its watermark.
    Boot,
    /// Something may have been missed: a little further back than that.
    Rescan,
}

/// What became of one file.
#[derive(Debug)]
enum Outcome {
    /// Gone before it could be read — deleted, or never a file.
    Gone,
    /// Not yet. `owed` keeps the ticket — still being written — where a `.json`
    /// waiting for its audio owes nothing until the audio comes.
    Wait { until: Instant, owed: bool },
    /// The pipeline broke; try again later, and never delete it.
    Broke(Failure),
    /// Something about the file itself.
    Refused { reason: Unreadable, stamp: Stamp },
    /// The pipeline decided — stored, replaced, a duplicate, dropped. Every one
    /// of those is an answer, so every one deletes under delete-after.
    Answered {
        admission: ingest::Admission,
        stamp: Stamp,
        files: Vec<PathBuf>,
    },
}

impl Watch {
    fn id(&self) -> i64 {
        self.row.id
    }

    /// Look for files this watch is owed, on disk rather than from events.
    async fn scan(&mut self, scope: Scope, meter: &Arc<Meter>) {
        let floor = match (self.row.delete_after, scope) {
            // Whatever a deleting watch still holds is owed, whatever its age.
            (true, _) => i64::MIN,
            (false, Scope::Boot) => self.started_through_ms,
            // What an overflow can have hidden is what arrived during this run,
            // so a rescan reaches back behind the watermark — and never past
            // where it stood when the watch started: a watch created or moved a
            // minute ago must not import a minute of the folder's history.
            (false, Scope::Rescan) => self
                .row
                .seen_through_ms
                .saturating_sub(LOOKBACK_MS)
                .max(self.started_through_ms),
        };
        for (path, stamp) in walked(&self.directory).await {
            self.enqueue(&path, Some(stamp), floor, meter);
        }
    }

    /// What the operating system said.
    async fn on_event(&mut self, event: notify::Result<Event>, meter: &Arc<Meter>) {
        let event = match event {
            Ok(event) if !event.need_rescan() => event,
            // An error, or the operating system saying it dropped events:
            // either way, a look on disk is the answer to it.
            Ok(_) => {
                warn!("the operating system dropped dirwatch events; rescanning");
                return self.scan(Scope::Rescan, meter).await;
            }
            Err(error) => {
                warn!(%error, "dirwatch watcher error; rescanning");
                return self.scan(Scope::Rescan, meter).await;
            }
        };
        match event.kind {
            EventKind::Remove(_) => {
                for path in &event.paths {
                    self.pending.remove(path);
                }
            }
            // A new folder (TR's `YYYY/M/D` at midnight): the watch on it was
            // added a moment after it existed, and a file written in that moment
            // sent its event to nobody — so look inside.
            EventKind::Create(CreateKind::Folder) => {
                for folder in &event.paths {
                    self.scan_folder(folder, meter).await;
                }
            }
            kind if is_a_write(kind) => {
                for path in &event.paths {
                    match path.is_dir() {
                        // A folder renamed into place, files and all.
                        true => self.scan_folder(path, meter).await,
                        false => self.enqueue(path, None, i64::MIN, meter),
                    }
                }
            }
            _ => {}
        }
    }

    async fn scan_folder(&mut self, folder: &Path, meter: &Arc<Meter>) {
        for (path, stamp) in walked(folder).await {
            self.enqueue(&path, Some(stamp), i64::MIN, meter);
        }
    }

    /// Owe an answer about `path` — the file it stands for, once it has been
    /// left alone for the watch's delay — unless what it stands for was written
    /// at or before `floor`.
    ///
    /// `walked` is the stamp a scan already read for `path`, spared a second
    /// look where `path` is the whole of what it stands for.
    fn enqueue(&mut self, path: &Path, walked: Option<Stamp>, floor: i64, meter: &Arc<Meter>) {
        let Some(subject) = self.subject(path) else {
            return;
        };
        // The stamp of the Call the answer is *about*, which for Trunk
        // Recorder is both halves of a pair, whichever half was walked past —
        // so the floor is judged against the same stamp the watermark was.
        let stamp = match self.reader {
            Reader::TrunkRecorder => self.pair_stamp(&subject),
            _ => walked.or_else(|| stamp_of(&subject)),
        };
        let Some(stamp) = stamp.filter(|stamp| stamp.written_ms > floor) else {
            return;
        };
        if self.seen.get(&subject) == Some(&stamp) {
            return;
        }
        let due = due_after(stamp.written_ms, self.delay);
        let pending = self.pending.entry(subject).or_insert_with(|| Pending {
            due,
            written_ms: stamp.written_ms,
            ticket: None,
        });
        // From the file as it is now, whatever the entry was waiting for: news
        // of a file is a reason to look at it again — the audio a `.json` was
        // waiting for, or a file that broke being touched by an Operator.
        pending.due = due;
        pending.written_ms = pending.written_ms.min(stamp.written_ms);
        pending.ticket.get_or_insert_with(|| meter.admit());
    }

    /// A Trunk Recorder `.json` and its audio, as one [`Stamp`] — the `.json`
    /// alone while its audio has not arrived.
    fn pair_stamp(&self, json: &Path) -> Option<Stamp> {
        let json_stamp = stamp_of(json)?;
        Some(match stamp_of(&json.with_extension(&self.extension)) {
            Some(audio) => json_stamp.with(audio),
            None => json_stamp,
        })
    }

    /// The file `path` stands for in this watch, or `None` for one it ignores.
    ///
    /// For Trunk Recorder that is always the `.json`: its audio's arrival is
    /// what wakes a `.json` that was waiting for it.
    fn subject(&self, path: &Path) -> Option<PathBuf> {
        let name = path.file_name()?.to_str()?;
        if name.starts_with('.') {
            return None;
        }
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match self.reader {
            Reader::TrunkRecorder if extension == "json" => Some(path.to_path_buf()),
            Reader::TrunkRecorder if extension == self.extension => {
                Some(path.with_extension("json"))
            }
            _ if extension == self.extension => Some(path.to_path_buf()),
            _ => None,
        }
    }

    /// Work out what `path` is owed.
    async fn answer(&self, state: &AppState, path: &Path) -> Outcome {
        match &self.reader {
            Reader::TrunkRecorder => self.answer_pair(state, path).await,
            Reader::SdrTrunk => {
                self.answer_file(state, path, |audio, _| {
                    Ok(sdrtrunk::read(
                        &crate::audio_meta::read(audio),
                        &chrono::Local,
                    ))
                })
                .await
            }
            Reader::DsdPlus => {
                self.answer_file(state, path, |_, path| {
                    Ok(dsdplus::read(
                        &folder_of(path),
                        &stem_of(path),
                        &chrono::Local,
                    ))
                })
                .await
            }
            Reader::Mask(mask) => {
                self.answer_file(state, path, |_, path| {
                    mask.read(&stem_of(path), &chrono::Local)
                        .ok_or(Unreadable::NoMatch)
                })
                .await
            }
        }
    }

    /// A Trunk Recorder Call: its `.json` and the audio beside it.
    async fn answer_pair(&self, state: &AppState, json: &Path) -> Outcome {
        let Some(json_stamp) = stamp_of(json) else {
            return Outcome::Gone;
        };
        if let Some(wait) = self.still_being_written(json_stamp) {
            return wait;
        }
        let audio = json.with_extension(&self.extension);
        let Some(audio_stamp) = stamp_of(&audio) else {
            // TR writes the `.json` first. Its audio's own event will wake this;
            // only a `.json` alone for longer than any render takes is refused.
            return match age(json_stamp) > PAIR_GRACE {
                true => refused(Unreadable::NoAudio, json_stamp),
                false => Outcome::Wait {
                    until: Instant::now() + self.delay.max(Duration::from_secs(1)),
                    owed: false,
                },
            };
        };
        if let Some(wait) = self.still_being_written(audio_stamp) {
            return wait;
        }
        let pair = json_stamp.with(audio_stamp);
        if json_stamp.bytes > MAX_FILE_BYTES || audio_stamp.bytes > MAX_FILE_BYTES {
            return refused(Unreadable::TooLarge, pair);
        }
        let (Ok(meta), Ok(bytes)) = (tokio::fs::read(json).await, tokio::fs::read(&audio).await)
        else {
            return refused(Unreadable::CouldNotRead, pair);
        };
        if bytes.is_empty() {
            return refused(Unreadable::NoAudio, pair);
        }
        let tr_call = match String::from_utf8(meta)
            .map_err(|_| ingest::TrMetaRefused::Invalid)
            .and_then(|meta| ingest::read_tr_meta(&meta))
        {
            Ok(tr_call) => tr_call,
            Err(ingest::TrMetaRefused::Invalid) => {
                return refused(Unreadable::InvalidMeta, pair);
            }
            Err(ingest::TrMetaRefused::NoTalkgroup) => {
                return refused(Unreadable::NoTalkgroup, pair);
            }
        };
        let mut new_call = match ingest::tr_new_call(
            state,
            tr_call,
            self.routing.system_ref,
            Some(file_name(&audio)),
            audio_mime(&self.extension).map(str::to_owned),
        )
        .await
        {
            Ok(new_call) => new_call,
            Err(failure) => return Outcome::Broke(failure),
        };
        new_call.frequency = new_call.frequency.or(self.routing.frequency);
        // The watch's Talkgroup outranks the file's, as it does for every
        // format — and the file's names for its *own* Talkgroup go with it,
        // or they would rename the channel the watch routes to.
        if let Some(talkgroup_ref) = self.routing.talkgroup_ref
            && talkgroup_ref != new_call.talkgroup_ref
        {
            new_call = NewCall {
                talkgroup_label: None,
                talkgroup_name: None,
                talkgroup_tag: None,
                talkgroup_groups: Vec::new(),
                talkgroup_ref,
                ..new_call
            };
        }
        // The Call's whole file set: the `.json`, and every audio rendering of
        // it TR left — a `compressWav` Call has both a `.wav` and an `.m4a`.
        let mut files = vec![json.to_path_buf()];
        files.extend(
            AUDIO
                .iter()
                .map(|(extension, _)| json.with_extension(extension))
                .filter(|sibling| sibling.exists()),
        );
        ingested(state, new_call, bytes, pair, files).await
    }

    /// Any other format: the audio file alone, described by `describe` from its
    /// bytes and its path.
    async fn answer_file(
        &self,
        state: &AppState,
        path: &Path,
        describe: impl FnOnce(&[u8], &Path) -> Result<super::Described, Unreadable>,
    ) -> Outcome {
        let Some(stamp) = stamp_of(path) else {
            return Outcome::Gone;
        };
        if let Some(wait) = self.still_being_written(stamp) {
            return wait;
        }
        if stamp.bytes > MAX_FILE_BYTES {
            return refused(Unreadable::TooLarge, stamp);
        }
        let Ok(bytes) = tokio::fs::read(path).await else {
            return refused(Unreadable::CouldNotRead, stamp);
        };
        if bytes.is_empty() {
            return refused(Unreadable::NoAudio, stamp);
        }
        let mime = audio_mime(&self.extension).unwrap_or_default();
        let mut new_call = match describe(&bytes, path).and_then(|described| {
            super::new_call(
                described,
                &self.routing,
                stamp.written_ms,
                &file_name(path),
                mime,
            )
        }) {
            Ok(new_call) => new_call,
            Err(reason) => return refused(reason, stamp),
        };
        // A System named only by its label is found the way Trunk Recorder's
        // `short_name` is — the one step of reading a file that needs the
        // database.
        if new_call.system_ref == 0
            && let Some(label) = &new_call.system_label
        {
            match repo::system_ref_for_short_name(&state.db, label).await {
                Ok(system_ref) => new_call.system_ref = system_ref,
                Err(cause) => return Outcome::Broke(Failure::broke(Stage::ResolveSystem, cause)),
            }
        }
        ingested(state, new_call, bytes, stamp, vec![path.to_path_buf()]).await
    }

    /// `Some(wait)` while a file is still within its delay of its last write.
    fn still_being_written(&self, stamp: Stamp) -> Option<Outcome> {
        (age(stamp) < self.delay).then(|| Outcome::Wait {
            until: due_after(stamp.written_ms, self.delay),
            owed: true,
        })
    }

    /// Record what became of a file, and stop owing it — or keep owing it.
    async fn settle(
        &mut self,
        state: &AppState,
        path: PathBuf,
        pending: Pending,
        outcome: Outcome,
    ) {
        let id = self.id();
        match outcome {
            Outcome::Gone => {}
            Outcome::Wait { until, owed } => {
                self.pending.insert(
                    path,
                    Pending {
                        due: until,
                        ticket: owed.then_some(pending.ticket).flatten(),
                        ..pending
                    },
                );
            }
            Outcome::Broke(failure) => {
                state.metrics.fail(&failure);
                self.pending.insert(
                    path,
                    Pending {
                        due: Instant::now() + RETRY,
                        ticket: None,
                        ..pending
                    },
                );
            }
            Outcome::Refused { reason, stamp } => {
                warn!(reason = %reason.slug(), file = %path.display(), "file refused");
                let name = file_name(&path);
                state.dirwatch.set_health(id, |health| {
                    health.refused += 1;
                    health.last_refusal = Some(format!("{} {name}", reason.slug()));
                });
                self.seen.insert(path, stamp);
                self.handled(stamp.written_ms);
            }
            Outcome::Answered {
                admission,
                stamp,
                files,
            } => {
                // Counted here because nothing renders it: an upload's
                // Admission is counted by the middleware off its response.
                state.metrics.admitted(admission.slug());
                if matches!(
                    admission,
                    ingest::Admission::Stored { .. } | ingest::Admission::Replaced { .. }
                ) {
                    let at = state.clock.now_ms();
                    state.dirwatch.set_health(id, |health| {
                        health.ingested += 1;
                        health.last_ingest_ms = Some(at);
                    });
                }
                if self.row.delete_after {
                    for file in files {
                        if let Err(error) = tokio::fs::remove_file(&file).await {
                            warn!(%error, file = %file.display(), "could not delete an ingested file");
                        }
                    }
                }
                self.seen.insert(path, stamp);
                self.handled(stamp.written_ms);
            }
        }
        if let Some(through) = self.advance() {
            // Keyed on the id rather than saved as a model, so a watch deleted
            // a moment ago is an update of nothing rather than an error.
            if let Err(cause) = dirwatch::Entity::update_many()
                .col_expr(dirwatch::Column::SeenThroughMs, Expr::value(through))
                .filter(dirwatch::Column::Id.eq(id))
                .exec(&state.db)
                .await
            {
                state.metrics.fail(&Failure::broke(Stage::Dirwatch, cause));
            }
            // **Forgotten once no scan can reach it**: a rescan never looks
            // further back than [`LOOKBACK_MS`] behind the watermark, so a
            // record of having seen anything older answers a question nothing
            // will ask — and kept for the life of the process, it is a Pi's
            // memory spent at a file per Call, for months.
            let horizon = through.saturating_sub(LOOKBACK_MS);
            self.seen.retain(|_, stamp| stamp.written_ms >= horizon);
        }
    }

    /// A file was answered: the newest one handled moves on, if it is newer.
    fn handled(&mut self, written_ms: i64) {
        self.newest_ms = self.newest_ms.max(written_ms);
    }

    /// The watermark this watch can now claim, if it is further on than the
    /// stored one: as far as the newest file handled, but never past the oldest
    /// one still owed. Clamped to now, so a file stamped in the future by a
    /// wrong clock cannot pull it ahead of every real one.
    fn advance(&mut self) -> Option<i64> {
        let owed = self
            .pending
            .values()
            .map(|pending| pending.written_ms.saturating_sub(1))
            .min();
        let through = owed
            .map_or(self.newest_ms, |owed| owed.min(self.newest_ms))
            .min(unix_ms(SystemTime::now()));
        (through > self.row.seen_through_ms).then(|| {
            self.row.seen_through_ms = through;
            through
        })
    }
}

/// Hand a Call to the pipeline — the very one an upload takes.
async fn ingested(
    state: &AppState,
    new_call: NewCall,
    audio: Vec<u8>,
    stamp: Stamp,
    files: Vec<PathBuf>,
) -> Outcome {
    match ingest::ingest_call(state, Authority::Dirwatch, new_call, audio).await {
        Ok(recorded) => Outcome::Answered {
            admission: recorded.admission().clone(),
            stamp,
            files,
        },
        Err(failure) => Outcome::Broke(failure),
    }
}

fn refused(reason: Unreadable, stamp: Stamp) -> Outcome {
    Outcome::Refused { reason, stamp }
}

/// Whether an event says a file's contents arrived — created, written, closed
/// after writing, or renamed into place. Opening, reading and access times are
/// not, or reading a file would re-trigger it.
fn is_a_write(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_)
            | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Name(_) | ModifyKind::Any)
            | EventKind::Access(AccessKind::Close(AccessMode::Write))
            | EventKind::Any
    )
}

/// [`walk`], off the async runtime: a deep folder is many blocking reads.
async fn walked(root: &Path) -> Vec<(PathBuf, Stamp)> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || walk(&root))
        .await
        .unwrap_or_default()
}

/// Every regular file under `root`, and its stamp. Symlinks are not followed —
/// a watch reads what is in its folder, not what is linked from it — and a
/// folder that cannot be read is passed over rather than ending the walk.
fn walk(root: &Path) -> Vec<(PathBuf, Stamp)> {
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => folders.push(path),
                Ok(kind) if kind.is_file() => {
                    found.extend(stamp_of(&path).map(|stamp| (path, stamp)));
                }
                _ => {}
            }
        }
    }
    found
}

/// A regular file's stamp, or `None` for anything else — gone, a folder, a
/// symlink.
fn stamp_of(path: &Path) -> Option<Stamp> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    metadata.is_file().then(|| Stamp {
        written_ms: metadata.modified().map(unix_ms).unwrap_or_default(),
        bytes: metadata.len(),
    })
}

fn unix_ms(at: SystemTime) -> i64 {
    match at.duration_since(UNIX_EPOCH) {
        Ok(since) => since.as_millis() as i64,
        Err(before) => -(before.duration().as_millis() as i64),
    }
}

/// How long ago a file was last written, by the clock files are stamped with.
fn age(stamp: Stamp) -> Duration {
    let now = unix_ms(SystemTime::now());
    Duration::from_millis(now.saturating_sub(stamp.written_ms).max(0) as u64)
}

/// The moment a file written at `written_ms` has been left alone for `delay`.
fn due_after(written_ms: i64, delay: Duration) -> Instant {
    let quiet_at = written_ms.saturating_add(delay.as_millis() as i64);
    let wait = quiet_at.saturating_sub(unix_ms(SystemTime::now())).max(0);
    Instant::now() + Duration::from_millis(wait as u64)
}

/// A file's name without its extension — what a mask and DSDPlus read.
fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The name of the folder a file is in — DSDPlus's date.
fn folder_of(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .map(|folder| folder.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Whether a stored row still describes the watch already running — everything
/// but the watermark, which the running watch itself moves, and the label,
/// which means nothing to it: renaming a watch must not restart it.
fn same_watch(row: &dirwatch::Model, running: &dirwatch::Model) -> bool {
    dirwatch::Model {
        seen_through_ms: running.seen_through_ms,
        label: running.label.clone(),
        ..row.clone()
    } == *running
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{DataChange, MetadataKind, RemoveKind, RenameMode};
    use rstest::rstest;

    /// A watch row as the curation surface would have saved it.
    fn row(directory: &Path, format: &str) -> dirwatch::Model {
        dirwatch::Model {
            id: 7,
            label: None,
            directory: directory.display().to_string(),
            format: format.to_owned(),
            extension: None,
            mask: None,
            system_ref: Some(11),
            talkgroup_ref: None,
            frequency: None,
            delay_ms: 0,
            delete_after: false,
            poll: false,
            disabled: false,
            seen_through_ms: 0,
            created_at_ms: 0,
        }
    }

    fn root(directory: &Path) -> Vec<Root> {
        vec![directory.display().to_string().parse().expect("a root")]
    }

    fn nobody() -> Callback {
        Box::new(|_| {})
    }

    /// The operating system, agreeing to watch anything and telling nobody.
    fn agreeable(
        _: &Path,
        _: bool,
        _: Callback,
    ) -> notify::Result<Box<dyn notify::Watcher + Send>> {
        Ok(Box::new(notify::NullWatcher))
    }

    /// The operating system, out of watches — `fs.inotify.max_user_watches`
    /// spent on a deep archive.
    fn exhausted(
        _: &Path,
        _: bool,
        _: Callback,
    ) -> notify::Result<Box<dyn notify::Watcher + Send>> {
        Err(notify::Error::new(notify::ErrorKind::MaxFilesWatch))
    }

    fn opened(directory: &Path, row: dirwatch::Model) -> Watch {
        open(row, &root(directory), nobody(), agreeable)
            .unwrap_or_else(|status| panic!("{status:?}"))
    }

    async fn state(directory: &Path) -> AppState {
        let db = crate::db::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.join("t.db").display()
        ))
        .await
        .expect("db");
        let store = Arc::new(crate::BlobStore::filesystem(directory.join("audio")).expect("blob"));
        AppState::new(store, db, crate::IngestConfig::default())
    }

    /// Reading a file is an event too, on Linux; reacting to it would read the
    /// file again, for ever.
    #[rstest]
    #[case::created(EventKind::Create(CreateKind::File), true)]
    #[case::written(EventKind::Modify(ModifyKind::Data(DataChange::Content)), true)]
    #[case::renamed_in(EventKind::Modify(ModifyKind::Name(RenameMode::To)), true)]
    #[case::closed_after_writing(EventKind::Access(AccessKind::Close(AccessMode::Write)), true)]
    #[case::unspecified(EventKind::Any, true)]
    #[case::opened(EventKind::Access(AccessKind::Open(AccessMode::Read)), false)]
    #[case::closed_after_reading(EventKind::Access(AccessKind::Close(AccessMode::Read)), false)]
    #[case::read(EventKind::Access(AccessKind::Read), false)]
    #[case::access_time(
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)),
        false
    )]
    #[case::removed(EventKind::Remove(RemoveKind::File), false)]
    fn only_a_write_wakes_a_watch(#[case] kind: EventKind, #[case] wakes: bool) {
        assert_eq!(is_a_write(kind), wakes);
    }

    #[test]
    fn a_time_before_the_epoch_is_negative_rather_than_a_panic() {
        assert_eq!(unix_ms(UNIX_EPOCH - Duration::from_millis(5)), -5);
        assert_eq!(unix_ms(UNIX_EPOCH + Duration::from_millis(5)), 5);
    }

    /// A row the curation surface would have refused — another release's, or
    /// one written by hand — is a watch that says it cannot run, never one that
    /// panics or runs wrongly.
    #[rstest]
    #[case::unknown_format("default", None, None, false)]
    #[case::mask_with_no_mask("mask", None, None, false)]
    #[case::mask_that_will_not_compile("mask", Some("#TG_#TG"), None, false)]
    #[case::not_audio("sdrtrunk", None, Some("conf"), false)]
    #[case::mask("mask", Some("#SYS_#TG"), None, true)]
    #[case::shouted_extension("trunk-recorder", None, Some("M4A"), true)]
    #[case::dsdplus("dsdplus", None, None, true)]
    fn a_row_this_release_cannot_run_is_unreadable(
        #[case] format: &str,
        #[case] mask: Option<&str>,
        #[case] extension: Option<&str>,
        #[case] runs: bool,
    ) {
        let row = dirwatch::Model {
            mask: mask.map(str::to_owned),
            extension: extension.map(str::to_owned),
            ..row(Path::new("/"), format)
        };
        match readable(&row) {
            Ok((reader, extension)) => {
                assert!(runs, "{format} should not run");
                assert_eq!(reader.format().slug(), format);
                assert_eq!(extension, extension.to_ascii_lowercase());
            }
            Err(status) => {
                assert!(!runs, "{format} should run");
                assert_eq!(status, Status::Unreadable);
            }
        }
    }

    /// The one failure no folder can arrange: the operating system refusing to
    /// watch. It is a status an Operator can see, not a Worker that stops.
    #[test]
    fn a_folder_the_operating_system_will_not_watch_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let refused = open(
            row(tmp.path(), "sdrtrunk"),
            &root(tmp.path()),
            nobody(),
            exhausted,
        );

        assert_eq!(refused.err(), Some(Status::CannotWatch));
    }

    #[test]
    fn a_folder_that_is_gone_or_outside_the_roots_is_not_watched() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();

        let gone = open(
            row(&tmp.path().join("gone"), "sdrtrunk"),
            &root(tmp.path()),
            nobody(),
            agreeable,
        );
        let outside = open(
            row(elsewhere.path(), "sdrtrunk"),
            &root(tmp.path()),
            nobody(),
            agreeable,
        );

        assert_eq!(gone.err(), Some(Status::NoSuchDirectory));
        assert_eq!(outside.err(), Some(Status::OutsideRoots));
    }

    /// A file gone before it could be read — deleted between the event and the
    /// answer — is nobody's, and is owed nothing.
    #[tokio::test]
    async fn a_file_gone_before_it_is_read_is_owed_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path()).await;
        for format in ["trunk-recorder", "sdrtrunk"] {
            let mut watch = opened(tmp.path(), row(tmp.path(), format));
            let missing = tmp.path().join("missing.json");

            let outcome = watch.answer(&state, &missing).await;
            assert!(matches!(outcome, Outcome::Gone), "{format}: {outcome:?}");

            let pending = Pending {
                due: Instant::now(),
                written_ms: 0,
                ticket: None,
            };
            watch.settle(&state, missing, pending, outcome).await;
            assert!(watch.pending.is_empty());
        }
    }

    /// A file written within the watch's delay is still being written: owed,
    /// and looked at again once it has been left alone. For Trunk Recorder that
    /// is true of either half of the pair.
    #[rstest]
    #[case::a_file("mask", &["5.wav"])]
    #[case::a_json("trunk-recorder", &["c.json"])]
    #[case::the_audio_beside_an_old_json("trunk-recorder", &["c.json", "c.wav"])]
    #[tokio::test]
    async fn a_file_still_being_written_is_waited_for(
        #[case] format: &str,
        #[case] files: &[&str],
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path()).await;
        let watch = opened(
            tmp.path(),
            dirwatch::Model {
                mask: Some("#TG".into()),
                delay_ms: 60_000,
                ..row(tmp.path(), format)
            },
        );
        for (index, file) in files.iter().enumerate() {
            let path = tmp.path().join(file);
            std::fs::write(&path, b"RIFF").unwrap();
            // Every file but the last is long since written.
            if index + 1 < files.len() {
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(SystemTime::now() - Duration::from_secs(3600))
                    .unwrap();
            }
        }

        let outcome = watch.answer(&state, &tmp.path().join(files[0])).await;
        assert!(
            matches!(outcome, Outcome::Wait { owed: true, until } if until > Instant::now() + Duration::from_secs(30)),
            "{outcome:?}"
        );
    }

    /// A Trunk Recorder pair too large to be a Call is refused without either
    /// half being read. Sparse, so the test costs no disk.
    #[tokio::test]
    async fn a_trunk_recorder_pair_too_large_to_be_a_call_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path()).await;
        let watch = opened(tmp.path(), row(tmp.path(), "trunk-recorder"));
        std::fs::write(tmp.path().join("c.json"), "{}").unwrap();
        std::fs::File::create(tmp.path().join("c.wav"))
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();

        let outcome = watch.answer(&state, &tmp.path().join("c.json")).await;
        assert!(
            matches!(
                outcome,
                Outcome::Refused {
                    reason: Unreadable::TooLarge,
                    ..
                }
            ),
            "{outcome:?}"
        );
    }

    /// A `.json` that is not even text is not Trunk Recorder's document.
    #[tokio::test]
    async fn a_trunk_recorder_json_that_is_not_text_is_invalid_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path()).await;
        let watch = opened(tmp.path(), row(tmp.path(), "trunk-recorder"));
        std::fs::write(tmp.path().join("c.json"), [0xFF, 0xFE, 0xFD]).unwrap();
        std::fs::write(tmp.path().join("c.wav"), b"RIFF").unwrap();

        let outcome = watch.answer(&state, &tmp.path().join("c.json")).await;
        assert!(
            matches!(
                outcome,
                Outcome::Refused {
                    reason: Unreadable::InvalidMeta,
                    ..
                }
            ),
            "{outcome:?}"
        );
    }

    /// A file the service user may not read is refused by name — the
    /// permissions an Operator has to fix — and so is a pair with either half
    /// unreadable.
    #[cfg(unix)]
    #[rstest]
    #[case::a_file("mask", "5.wav", "5.wav")]
    #[case::a_pairs_audio("trunk-recorder", "c.json", "c.wav")]
    #[tokio::test]
    async fn a_file_that_cannot_be_read_is_refused(
        #[case] format: &str,
        #[case] subject: &str,
        #[case] locked: &str,
    ) {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path()).await;
        let watch = opened(
            tmp.path(),
            dirwatch::Model {
                mask: Some("#TG".into()),
                ..row(tmp.path(), format)
            },
        );
        std::fs::write(tmp.path().join("c.json"), r#"{"talkgroup":5}"#).unwrap();
        for file in ["5.wav", "c.wav"] {
            std::fs::write(tmp.path().join(file), b"RIFF").unwrap();
        }
        let locked = tmp.path().join(locked);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&locked).is_ok() {
            // Root reads anything, so there is nothing to prove here.
            return;
        }

        let outcome = watch.answer(&state, &tmp.path().join(subject)).await;
        assert!(
            matches!(
                outcome,
                Outcome::Refused {
                    reason: Unreadable::CouldNotRead,
                    ..
                }
            ),
            "{outcome:?}"
        );
    }

    /// A folder the walk cannot read is passed over; the rest of the tree is
    /// still found.
    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_read_does_not_end_the_walk() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("hidden.wav"), b"x").unwrap();
        std::fs::write(tmp.path().join("found.wav"), b"x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let found: Vec<_> = walk(tmp.path())
            .into_iter()
            .map(|(path, _)| file_name(&path))
            .collect();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(found.contains(&"found.wav".to_string()), "{found:?}");
    }

    /// The watermark goes as far as the newest file answered and never past the
    /// oldest one still owed — and never into the future.
    #[test]
    fn the_watermark_never_passes_a_file_still_owed() {
        let tmp = tempfile::tempdir().unwrap();
        let mut watch = opened(tmp.path(), row(tmp.path(), "sdrtrunk"));
        let pending = |written_ms| Pending {
            due: Instant::now(),
            written_ms,
            ticket: None,
        };

        watch.handled(5_000);
        watch.pending.insert(PathBuf::from("older"), pending(3_000));
        assert_eq!(watch.advance(), Some(2_999), "held behind what is owed");
        assert_eq!(watch.advance(), None, "and not moved twice");

        watch.pending.clear();
        assert_eq!(watch.advance(), Some(5_000));

        watch.handled(i64::MAX);
        let now = unix_ms(SystemTime::now());
        assert!(
            watch
                .advance()
                .is_some_and(|through| through <= now + 1_000)
        );
    }

    /// What a watch remembers having seen is forgotten once no scan could reach
    /// it — or a watch that keeps its files would hold a record per Call for as
    /// long as the process runs.
    #[tokio::test]
    async fn a_watch_forgets_what_no_scan_can_reach() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path()).await;
        let mut watch = opened(tmp.path(), row(tmp.path(), "sdrtrunk"));
        let now = unix_ms(SystemTime::now());
        let stamp = |written_ms| Stamp {
            written_ms,
            bytes: 1,
        };
        watch
            .seen
            .insert(PathBuf::from("ancient"), stamp(now - 2 * LOOKBACK_MS));
        watch
            .seen
            .insert(PathBuf::from("recent"), stamp(now - 1_000));
        let pending = Pending {
            due: Instant::now(),
            written_ms: now - 500,
            ticket: None,
        };
        let refused = Outcome::Refused {
            reason: Unreadable::NoMatch,
            stamp: stamp(now - 500),
        };

        watch
            .settle(&state, PathBuf::from("latest"), pending, refused)
            .await;

        let mut kept: Vec<_> = watch.seen.keys().cloned().collect();
        kept.sort();
        assert_eq!(kept, [PathBuf::from("latest"), PathBuf::from("recent")]);
    }

    fn watches(state: AppState) -> Watches {
        let (deliver, _) = mpsc::unbounded_channel();
        Watches {
            state,
            meter: Meter::new(),
            deliver,
            running: HashMap::new(),
            start: agreeable,
        }
    }

    /// The operating system queues an event a moment before a watch is
    /// stopped, and the loop reads it a moment after: it is nobody's. The
    /// same for "scan now" on a watch that is disabled, and a file due on a
    /// watch since removed.
    #[tokio::test]
    async fn a_delivery_for_a_watch_that_has_stopped_is_nobodys() {
        let tmp = tempfile::tempdir().unwrap();
        let mut watches = watches(state(tmp.path()).await);

        watches
            .on_event(42, Ok(Event::new(EventKind::Create(CreateKind::File))))
            .await;
        watches.command(Command::Scan(42)).await;
        watches
            .work(Next {
                due: Instant::now(),
                id: 42,
                path: tmp.path().join("gone.wav"),
            })
            .await;

        assert!(watches.running.is_empty());
        assert!(watches.next().is_none());
    }

    /// **The operating system saying it lost events — or failing outright — is
    /// answered by looking on disk**, so an inotify overflow costs a scan
    /// rather than the Calls whose events it dropped.
    #[rstest]
    #[case::dropped_events(Ok(Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan)))]
    #[case::watcher_error(Err(notify::Error::generic("the watch was lost")))]
    #[tokio::test]
    async fn lost_events_are_answered_by_a_scan(#[case] event: notify::Result<Event>) {
        let tmp = tempfile::tempdir().unwrap();
        let mut watch = opened(
            tmp.path(),
            dirwatch::Model {
                delete_after: true,
                ..row(tmp.path(), "sdrtrunk")
            },
        );
        std::fs::write(tmp.path().join("missed.mp3"), b"ID3").unwrap();

        watch.on_event(event, &Meter::new()).await;

        // Keyed where the watch really is — the folder resolved, as it was
        // stored.
        let missed = watch.directory.join("missed.mp3");
        assert!(
            watch.pending.contains_key(&missed),
            "{:?}",
            watch.pending.keys()
        );
    }

    /// An event that says nothing arrived — a file read, a file opened — wakes
    /// nothing.
    #[tokio::test]
    async fn an_event_that_is_not_a_write_wakes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut watch = opened(tmp.path(), row(tmp.path(), "sdrtrunk"));
        let file = tmp.path().join("read.mp3");
        std::fs::write(&file, b"ID3").unwrap();

        watch
            .on_event(
                Ok(
                    Event::new(EventKind::Access(AccessKind::Open(AccessMode::Read)))
                        .add_path(file),
                ),
                &Meter::new(),
            )
            .await;

        assert!(watch.pending.is_empty());
    }
}
