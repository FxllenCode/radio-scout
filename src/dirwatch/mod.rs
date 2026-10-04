//! **Dirwatch** (CONTEXT.md, #72, spec US 14): ingesting **Calls** a Recorder
//! drops into a folder, through the same pipeline an upload takes.
//!
//! A dropped file becomes a [`NewCall`] here and is handed to
//! [`crate::ingest::ingest_call`] — the function the upload endpoints call —
//! under [`crate::ingest::Authority::Dirwatch`], so deduplication, auto-populate,
//! channel merges, enrichment, **Mining**, forwarding and webhooks are not
//! re-implemented for files: they are the same code. Trunk Recorder's `.json` is
//! read by the native endpoint's own parser ([`crate::ingest::read_tr_meta`]),
//! so a Call that arrives by file is identical to one that arrives by
//! `uploadScript`, and `tests/dirwatch.rs` holds the two to rendering the same.
//!
//! # Improving on rdio-scanner
//!
//! rdio's `dirwatch.go` is the reference for what a watch *is* — its four
//! formats, its mask tokens, its per-watch System/Talkgroup/frequency — and for
//! almost everything it does wrong:
//!
//! | rdio | Radio-Scout |
//! | --- | --- |
//! | a watch's folder is any path an admin types, and delete-after removes whatever it ingested | the folder must resolve inside `[dirwatch] roots`, which only the TOML can set, and only audio is read ([ADR-0021]) |
//! | deletes a file once it is *queued*, before the Call is stored — a failed store loses it | deletes only once the pipeline has **answered**; a file whose ingest broke is kept and retried |
//! | a Trunk Recorder `.json` whose audio is not there yet is silently dropped, and TR writes the `.json` *first* | the `.json` waits for its audio, and only one alone past a grace is refused |
//! | a mask is a regular expression, compiled with `MustCompile` at the first file, in a goroutine with no `recover` | literal text is escaped and a mask is compiled when it is **saved**, so a bad one is a refusal on a form |
//! | a file that does not say when is filed at `time.Now()` — wrong by the whole outage for every Call a backfill reads | filed at the file's own modification time |
//! | without delete-after, files that arrived while the server was down are never read | a per-watch **watermark** backfills exactly those |
//! | a file it cannot read is a line in its event log and nothing else | one WARN per refused file naming the reason, a count on the watch's row, and the file left where it was |
//! | "dirwatch is not compatible with networked disk" | a per-watch poll for a share with no events to give |
//! | `DSDPlus` panics on a name with one field (`meta[len-2]`) | a name that names nothing names nothing |
//! | SDRTrunk's tag date — the *end* of the Call — used as its start | the tag date minus the Call's length, which is what SDRTrunk's own upload sends |
//!
//! # Where the decisions live
//!
//! - **Reading a file is pure**: [`mask`], [`dsdplus`] and [`sdrtrunk`] turn a
//!   name or a tag into a [`Described`], and [`new_call`] lays a watch's
//!   [`Routing`] over it. Every one is property-tested, because every one
//!   reads a stranger's filename.
//! - **Watching is the [`worker`]'s**: the operating system's events, the delay
//!   a file is left alone for, the watermark, delete-after, and what each file
//!   is owed. One file at a time, one Worker for every watch.
//! - **Curating is [`crate::curate::dirwatches`]'s**, which holds every write to
//!   the whole row it produces and re-arms the Worker on the same request.
//!
//! **A watch's System and Talkgroup outrank the file's**, #111's rule for a
//! Recorder that names its own System: an Operator who set them is saying where
//! these Calls go. Its frequency only fills in for a file that names none —
//! rdio's "fake frequency".
//!
//! **Times with no zone are read in this Instance's own**, through
//! `chrono::Local` — DSDPlus's folder and name, a mask's `#TIME`, SDRTrunk's
//! tag. rdio does the same. It is right wherever the Recorder and the Instance
//! share a clock, which is the install this exists for; the Docker image has
//! no zone database and needs the host's mounted, which `docs/deploy.md` says.
//! (The `time` crate refuses to read the local offset on a multi-threaded Unix
//! process, which is why this is chrono's.)
//!
//! [ADR-0021]: https://github.com/FxllenCode/radio-scout/blob/next/docs/adr/0021-dirwatch-roots-are-infrastructure.md

pub mod dsdplus;
pub mod mask;
pub mod sdrtrunk;
pub mod worker;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::db::repo::{NewCall, NewCallUnit};
use crate::worker::{Handoff, Meter, Ticket};

/// The `[dirwatch]` section (ADR-0012, #87) — and **the bound on every watch**
/// (ADR-0021).
///
/// A watch is created from the browser, so its folder is whatever an admin
/// session typed — and a watch reads every file it is pointed at and, with
/// delete-after, removes it. Unbounded, that turns an admin session into the
/// service user's whole filesystem, which is what rdio's dirwatch is. So the
/// *places* a watch may be are infrastructure, named here where only somebody
/// with a shell can name them, and the browser chooses inside them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DirwatchConfig {
    /// The folders a watch may be created inside. **Empty — what ships — means
    /// Dirwatch is unavailable**, and the admin screen says how to enable it.
    pub roots: Vec<Root>,
}

/// One of `[dirwatch] roots`: an **absolute** directory path.
///
/// Relative is refused where it is written rather than resolved against
/// whatever directory the service happened to start in, which differs between
/// a terminal, systemd and launchd — the `ProxyNet` precedent, so an unusable
/// root is a boot error naming its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root(PathBuf);

/// What a root has to look like. One string, so the file and the environment
/// cannot describe it differently.
pub const EXPECTED_ROOT: &str = "absolute directory paths, e.g. /srv/trunk-recorder";

/// A root that is not an absolute path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotARoot;

impl std::fmt::Display for NotARoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "expected {EXPECTED_ROOT}")
    }
}

impl std::error::Error for NotARoot {}

impl Root {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl FromStr for Root {
    type Err = NotARoot;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let path = PathBuf::from(text.trim());
        match path.is_absolute() {
            true => Ok(Root(path)),
            false => Err(NotARoot),
        }
    }
}

impl Serialize for Root {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0.display())
    }
}

impl<'de> Deserialize<'de> for Root {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse()
            .map_err(|err| serde::de::Error::custom(format!("{text:?}: {err}")))
    }
}

/// Which Recorder's folder a watch reads, and so how a file there is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Trunk Recorder's `captureDir`: a `.json` beside each Call's audio, the
    /// very document the native upload endpoint parses.
    TrunkRecorder,
    /// SDRTrunk's recordings folder: MP3s carrying an ID3 tag.
    SdrTrunk,
    /// DSDPlus Fast Lane's record folders: everything is in the path.
    DsdPlus,
    /// Anything else, read through an rdio filename mask.
    Mask,
}

impl Format {
    /// Every format, for a refusal that lists them.
    pub const ALL: [Format; 4] = [
        Format::TrunkRecorder,
        Format::SdrTrunk,
        Format::DsdPlus,
        Format::Mask,
    ];

    /// How it is spelled in the admin API and the database.
    pub fn slug(self) -> &'static str {
        match self {
            Format::TrunkRecorder => "trunk-recorder",
            Format::SdrTrunk => "sdrtrunk",
            Format::DsdPlus => "dsdplus",
            Format::Mask => "mask",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|format| format.slug() == slug)
    }

    /// The audio a watch of this format reads when it names none — what each
    /// Recorder writes by default.
    pub fn default_extension(self) -> &'static str {
        match self {
            Format::TrunkRecorder | Format::Mask => "wav",
            Format::SdrTrunk | Format::DsdPlus => "mp3",
        }
    }
}

/// The audio containers a watch may read, and the type each is served as.
///
/// **A closed list, and the second half of the path bound** (ADR-0021): the
/// TOML's roots say *where* a browser may point a watch, and this says *what*
/// it may pick up there — so an admin session can never have `/etc/shadow`
/// ingested as a Call and downloaded back as its audio.
pub const AUDIO: [(&str, &str); 7] = [
    ("wav", "audio/wav"),
    ("mp3", "audio/mpeg"),
    ("m4a", "audio/mp4"),
    ("aac", "audio/aac"),
    ("ogg", "audio/ogg"),
    ("opus", "audio/ogg"),
    ("flac", "audio/flac"),
];

/// The type an extension on [`AUDIO`] is served as, or `None` for one that is
/// not audio a watch may read.
pub fn audio_mime(extension: &str) -> Option<&'static str> {
    AUDIO
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(extension))
        .map(|(_, mime)| *mime)
}

/// A System, as a file names it: by Ref, or by name for the Instance to look up
/// — the way Trunk Recorder's `short_name` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    Ref(i64),
    Label(String),
}

/// What a dropped file says about its Call, before the watch's own settings are
/// laid over it. Every format's reader produces one; [`new_call`] is the one
/// place any of them becomes a Call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Described {
    pub system: Option<Named>,
    pub site_ref: Option<i64>,
    pub site_label: Option<String>,
    pub talkgroup_ref: Option<i64>,
    pub talkgroup_label: Option<String>,
    pub talkgroup_tag: Option<String>,
    pub talkgroup_group: Option<String>,
    /// Hertz.
    pub frequency: Option<i64>,
    /// The radio that keyed, and its alias where the file gave one.
    pub unit: Option<(i64, Option<String>)>,
    /// Unix milliseconds — `None` where the file does not say, and the file's
    /// own modification time decides.
    pub call_at_ms: Option<i64>,
    pub patches: Vec<i64>,
}

/// What an Operator configured a watch to say about every file in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Routing {
    /// The System every file here is filed under. **Outranks the file**: an
    /// Operator who set it is saying where these Calls go, the precedent #111
    /// set for a Trunk Recorder that names its own System.
    pub system_ref: Option<i64>,
    /// The Talkgroup, likewise — rdio's "Talkgroup to where the audio files
    /// should go".
    pub talkgroup_ref: Option<i64>,
    /// A frequency for files that carry none — rdio's "fake frequency". Fills,
    /// never overwrites.
    pub frequency: Option<i64>,
}

/// Why a dropped file did not become a Call — the closed vocabulary its line
/// carries, the [`crate::failure::Reason`] precedent applied one surface along.
///
/// The slugs an upload already spends on the same condition are spelled the
/// same (`no-talkgroup`, `invalid-meta`, `no-audio`), so one grep finds a Call
/// refused either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreadable {
    /// Neither the file nor the watch says which Talkgroup.
    NoTalkgroup,
    /// Neither the file nor the watch says which System.
    NoSystem,
    /// A Trunk Recorder `.json` that is not the document it should be.
    InvalidMeta,
    /// Empty audio, or a Trunk Recorder `.json` whose audio never arrived.
    NoAudio,
    /// A mask watch's file whose name the mask does not describe.
    NoMatch,
    /// Larger than [`MAX_FILE_BYTES`] — read into memory, it could take a Pi
    /// down with it.
    TooLarge,
    /// The file could not be read at all.
    CouldNotRead,
}

impl Unreadable {
    pub fn slug(self) -> &'static str {
        match self {
            Unreadable::NoTalkgroup => "no-talkgroup",
            Unreadable::NoSystem => "no-system",
            Unreadable::InvalidMeta => "invalid-meta",
            Unreadable::NoAudio => "no-audio",
            Unreadable::NoMatch => "no-match",
            Unreadable::TooLarge => "too-large",
            Unreadable::CouldNotRead => "could-not-read",
        }
    }
}

/// What the Dirwatch Worker is asked to do from outside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    /// The roster changed: re-read it, and start, stop or restart watches to
    /// match. Sent by every curation write, on the same request (the
    /// `Tones::rearm` rule — a stale roster is a watch that silently is not).
    Rearm,
    /// Look in one watch's folder now, a little behind its watermark — the
    /// admin screen's "scan now", for a share that sends no events or an
    /// Operator who wants to be sure. A file refused this run is **not** read
    /// again unless it has changed: the same bytes get the same answer.
    Scan(i64),
}

/// Dirwatch's half of the Instance: the Worker's inbox, what it owes, and what
/// each watch is doing — the reading an admin screen needs and could never get
/// from a Worker it does not own.
///
/// A watch is a **row**, so there is no disabled form beyond `[dirwatch] roots`
/// being empty: an Instance with no watches has an empty roster, and the Worker
/// sleeps on its inbox.
#[derive(Clone)]
pub struct Dirwatch(Arc<Inner>);

struct Inner {
    config: DirwatchConfig,
    commands: mpsc::UnboundedSender<(Command, Ticket)>,
    /// Taken once, by [`worker::spawn`]. A second spawn finds it gone and
    /// starts no second Worker.
    inbox: Handoff<mpsc::UnboundedReceiver<(Command, Ticket)>>,
    meter: Arc<Meter>,
    health: Mutex<HashMap<i64, Health>>,
}

impl Default for Dirwatch {
    fn default() -> Self {
        Dirwatch::new(DirwatchConfig::default())
    }
}

impl Dirwatch {
    pub fn new(config: DirwatchConfig) -> Self {
        let (commands, inbox) = mpsc::unbounded_channel();
        Dirwatch(Arc::new(Inner {
            config,
            commands,
            inbox: Handoff::new(inbox),
            meter: Meter::new(),
            health: Mutex::new(HashMap::new()),
        }))
    }

    /// The roots every watch is bounded by.
    pub fn config(&self) -> &DirwatchConfig {
        &self.0.config
    }

    /// The roster changed — tell the Worker, owing it the work of catching up
    /// so nothing reads idle before it has.
    pub fn rearm(&self) {
        self.send(Command::Rearm);
    }

    /// Look in watch `id`'s folder now.
    pub fn scan(&self, id: i64) {
        self.send(Command::Scan(id));
    }

    fn send(&self, command: Command) {
        // Admitted *before* it is handed over (#93's rule), so an Instance is
        // never observed idle between the request and the Worker waking. A
        // closed inbox is a Worker that has stopped, which is an Instance
        // shutting down — and the ticket settles on the way out either way.
        let _ = self.0.commands.send((command, self.0.meter.admit()));
    }

    /// What watch `id` is doing — `None` for one the Worker has never looked at.
    pub fn health(&self, id: i64) -> Option<Health> {
        self.0.health.lock().expect("health").get(&id).cloned()
    }

    fn set_health(&self, id: i64, edit: impl FnOnce(&mut Health)) {
        edit(self.0.health.lock().expect("health").entry(id).or_default());
    }

    fn forget_health(&self, keep: impl Fn(i64) -> bool) {
        self.0
            .health
            .lock()
            .expect("health")
            .retain(|id, _| keep(*id));
    }
}

/// What one watch is doing, as an Operator is shown it.
///
/// Counters since this run started, held in memory and never written down:
/// they answer "is it working?", and the Archive and the log already answer
/// everything else.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    /// `watching`, or why it is not — see [`Status`].
    pub status: Status,
    /// Files that became, or improved, a Call.
    pub ingested: u64,
    /// Files refused for something about the file itself.
    pub refused: u64,
    pub last_ingest_ms: Option<i64>,
    /// The last refusal, as `<reason> <file name>`.
    pub last_refusal: Option<String>,
}

/// Whether a watch is running, and if not, the one thing to fix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Not yet looked at by the Worker.
    #[default]
    Starting,
    Watching,
    Disabled,
    /// The folder is no longer inside any `[dirwatch] roots` — the TOML changed
    /// under a row that was fine when it was saved.
    OutsideRoots,
    NoSuchDirectory,
    /// The operating system would not watch it.
    CannotWatch,
    /// A row a newer or older release wrote that this one cannot read.
    Unreadable,
}

/// Where `directory` really is, if it is a directory inside one of `roots` —
/// both sides resolved, so neither `..` nor a symlink can walk a watch out of
/// the folder it appears to be in.
///
/// [`Bound`] says which of the two ways it failed, because "that folder does
/// not exist" and "you may not watch there" are different things to fix.
pub fn within_roots(directory: &Path, roots: &[Root]) -> Result<PathBuf, Bound> {
    let real = std::fs::canonicalize(directory).map_err(|_| Bound::NoSuchDirectory)?;
    if !real.is_dir() {
        return Err(Bound::NoSuchDirectory);
    }
    roots
        .iter()
        .filter_map(|root| std::fs::canonicalize(root.path()).ok())
        .any(|root| real.starts_with(root))
        .then_some(real)
        .ok_or(Bound::OutsideRoots)
}

/// Why a folder cannot be watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    NoSuchDirectory,
    OutsideRoots,
}

/// The largest file a watch will read. A sanity bound, not a policy: a Call is
/// megabytes at most, and a stray multi-gigabyte file in a watched folder must
/// cost a refusal rather than the process.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// What a file described, under its watch's [`Routing`], as the Call the
/// pipeline is handed.
///
/// `written_ms` is the file's modification time, which is **when** wherever the
/// file itself does not say — not "now", which is rdio's answer and is wrong by
/// exactly as long as the Instance was down for every Call a backfill picks up.
///
/// A System named only by label comes back as Ref `0` with the label beside it,
/// for the Instance to look up the way it looks up Trunk Recorder's
/// `short_name` — the one step here that needs a database.
pub fn new_call(
    described: Described,
    routing: &Routing,
    written_ms: i64,
    audio_name: &str,
    audio_mime: &str,
) -> Result<NewCall, Unreadable> {
    let talkgroup_ref = routing
        .talkgroup_ref
        .or(described.talkgroup_ref)
        .ok_or(Unreadable::NoTalkgroup)?;
    let (system_ref, system_label) = match (routing.system_ref, described.system) {
        (Some(system_ref), _) | (None, Some(Named::Ref(system_ref))) => (system_ref, None),
        (None, Some(Named::Label(label))) => (0, Some(label)),
        (None, None) => return Err(Unreadable::NoSystem),
    };
    // A watch that routes these files to another Talkgroup than the one a
    // file names is saying where the Call goes, not what that channel is
    // called — so the file's names for its own Talkgroup are not kept, as a
    // file's System label is not when the watch names the System.
    let rerouted = described
        .talkgroup_ref
        .is_some_and(|named| named != talkgroup_ref);
    let named = |name: Option<String>| name.filter(|_| !rerouted);
    Ok(NewCall {
        system_label,
        talkgroup_label: named(described.talkgroup_label),
        talkgroup_tag: named(described.talkgroup_tag),
        talkgroup_groups: named(described.talkgroup_group).into_iter().collect(),
        frequency: described.frequency.or(routing.frequency),
        site_ref: described.site_ref,
        site_label: described.site_label,
        patches: described.patches,
        units: described
            .unit
            .map(|(unit_ref, label)| NewCallUnit {
                unit_ref,
                label,
                ..Default::default()
            })
            .into_iter()
            .collect(),
        audio_name: Some(audio_name.to_owned()),
        audio_mime: Some(audio_mime.to_owned()),
        ..NewCall::new(
            system_ref,
            talkgroup_ref,
            described.call_at_ms.unwrap_or(written_ms),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn described() -> Described {
        Described {
            system: Some(Named::Ref(11)),
            talkgroup_ref: Some(54241),
            call_at_ms: Some(1_000),
            ..Described::default()
        }
    }

    #[test]
    fn a_described_file_becomes_the_call_it_describes() {
        let call = new_call(
            Described {
                talkgroup_label: Some("Fire".into()),
                talkgroup_tag: Some("Fire Dispatch".into()),
                talkgroup_group: Some("Fire".into()),
                frequency: Some(851_012_500),
                site_ref: Some(3),
                site_label: Some("North".into()),
                patches: vec![1, 2],
                unit: Some((77, Some("Engine 7".into()))),
                ..described()
            },
            &Routing::default(),
            9_999,
            "a.wav",
            "audio/wav",
        )
        .expect("a call");

        assert_eq!((call.system_ref, call.talkgroup_ref), (11, 54241));
        assert_eq!(call.call_at_ms, 1_000);
        assert_eq!(call.talkgroup_label.as_deref(), Some("Fire"));
        assert_eq!(call.talkgroup_tag.as_deref(), Some("Fire Dispatch"));
        assert_eq!(call.talkgroup_groups, vec!["Fire".to_string()]);
        assert_eq!(call.frequency, Some(851_012_500));
        assert_eq!(
            (call.site_ref, call.site_label.as_deref()),
            (Some(3), Some("North"))
        );
        assert_eq!(call.patches, vec![1, 2]);
        assert_eq!(call.units[0].unit_ref, 77);
        assert_eq!(call.units[0].label.as_deref(), Some("Engine 7"));
        assert_eq!(call.audio_name.as_deref(), Some("a.wav"));
        assert_eq!(call.audio_mime.as_deref(), Some("audio/wav"));
    }

    /// rdio stamps a file it cannot date with `time.Now()` — so every Call a
    /// backfill picks up is filed at the moment the Instance came back.
    #[test]
    fn a_file_that_does_not_say_when_is_when_it_was_written() {
        let call = new_call(
            Described {
                call_at_ms: None,
                ..described()
            },
            &Routing::default(),
            9_999,
            "a.wav",
            "audio/wav",
        )
        .expect("a call");

        assert_eq!(call.call_at_ms, 9_999);
    }

    /// The watch outranks the file for where a Call goes, and only fills in
    /// for what it carries.
    #[rstest]
    #[case::watch_routes(Routing { system_ref: Some(22), talkgroup_ref: Some(5), frequency: Some(1) }, (22, 5), Some(851))]
    #[case::file_routes(Routing::default(), (11, 54241), Some(851))]
    #[case::watch_frequency_fills_only(Routing { frequency: Some(1), ..Routing::default() }, (11, 54241), Some(851))]
    fn the_watch_routes_and_the_file_describes(
        #[case] routing: Routing,
        #[case] routed: (i64, i64),
        #[case] frequency: Option<i64>,
    ) {
        let call = new_call(
            Described {
                frequency: Some(851),
                ..described()
            },
            &routing,
            0,
            "a.wav",
            "audio/wav",
        )
        .expect("a call");

        assert_eq!((call.system_ref, call.talkgroup_ref), routed);
        assert_eq!(call.frequency, frequency);
    }

    /// A System named by label is looked up by the Instance; one named by a
    /// watch drops the file's label, which would otherwise rename it.
    #[rstest]
    #[case::label(None, (0, Some("Fulton")))]
    #[case::watch_drops_the_label(Some(22), (22, None))]
    fn a_system_named_by_label_is_left_for_the_instance_to_find(
        #[case] watch: Option<i64>,
        #[case] expected: (i64, Option<&str>),
    ) {
        let call = new_call(
            Described {
                system: Some(Named::Label("Fulton".into())),
                ..described()
            },
            &Routing {
                system_ref: watch,
                ..Routing::default()
            },
            0,
            "a.wav",
            "audio/wav",
        )
        .expect("a call");

        assert_eq!((call.system_ref, call.system_label.as_deref()), expected);
    }

    #[rstest]
    #[case::no_talkgroup(Described { talkgroup_ref: None, ..described() }, Unreadable::NoTalkgroup)]
    #[case::no_system(Described { system: None, ..described() }, Unreadable::NoSystem)]
    fn a_file_nothing_routes_is_refused_by_name(
        #[case] described: Described,
        #[case] refusal: Unreadable,
    ) {
        assert_eq!(
            new_call(described, &Routing::default(), 0, "a.wav", "audio/wav").expect_err("refused"),
            refusal
        );
    }

    #[rstest]
    #[case("wav", Some("audio/wav"))]
    #[case("MP3", Some("audio/mpeg"))]
    #[case("m4a", Some("audio/mp4"))]
    #[case("json", None)]
    #[case("conf", None)]
    #[case("", None)]
    fn only_audio_has_a_type(#[case] extension: &str, #[case] mime: Option<&str>) {
        assert_eq!(audio_mime(extension), mime);
    }

    #[test]
    fn every_format_round_trips_its_slug() {
        for format in Format::ALL {
            assert_eq!(Format::from_slug(format.slug()), Some(format));
            assert!(audio_mime(format.default_extension()).is_some());
        }
        assert_eq!(Format::from_slug("default"), None);
    }

    /// The conditions a file and an upload share are refused under **one
    /// spelling**, so one grep finds a Call refused either way — held here,
    /// because the two vocabularies are two enums.
    #[rstest]
    #[case(
        Unreadable::NoTalkgroup,
        crate::failure::Reason::Incomplete(crate::failure::Incomplete::NoTalkgroup)
    )]
    #[case(
        Unreadable::NoAudio,
        crate::failure::Reason::Incomplete(crate::failure::Incomplete::NoAudio)
    )]
    #[case(Unreadable::InvalidMeta, crate::failure::Reason::InvalidMeta)]
    fn a_refusal_an_upload_shares_is_spelled_as_the_upload_spells_it(
        #[case] file: Unreadable,
        #[case] upload: crate::failure::Reason,
    ) {
        assert_eq!(file.slug(), upload.slug());
    }

    /// A watch that routes a file to another Talkgroup drops the file's names
    /// for its own — and keeps a name the file gives with no Ref of its own,
    /// which can only be the routed channel's.
    #[rstest]
    #[case::rerouted(Some(54241), None)]
    #[case::same_channel(Some(5), Some("Fire"))]
    #[case::name_with_no_ref(None, Some("Fire"))]
    fn a_rerouted_file_does_not_rename_the_channel_it_is_routed_to(
        #[case] named: Option<i64>,
        #[case] kept: Option<&str>,
    ) {
        let call = new_call(
            Described {
                talkgroup_ref: named,
                talkgroup_label: Some("Fire".into()),
                talkgroup_tag: Some("Fire".into()),
                talkgroup_group: Some("Fire".into()),
                ..described()
            },
            &Routing {
                talkgroup_ref: Some(5),
                ..Routing::default()
            },
            0,
            "a.wav",
            "audio/wav",
        )
        .expect("a call");

        assert_eq!(call.talkgroup_ref, 5);
        assert_eq!(call.talkgroup_label.as_deref(), kept);
        assert_eq!(call.talkgroup_tag.as_deref(), kept);
        assert_eq!(call.talkgroup_groups.len(), usize::from(kept.is_some()));
    }

    #[test]
    fn every_refusal_has_its_own_slug() {
        let all = [
            Unreadable::NoTalkgroup,
            Unreadable::NoSystem,
            Unreadable::InvalidMeta,
            Unreadable::NoAudio,
            Unreadable::NoMatch,
            Unreadable::TooLarge,
            Unreadable::CouldNotRead,
        ];
        let slugs: std::collections::HashSet<_> = all.iter().map(|r| r.slug()).collect();
        assert_eq!(slugs.len(), all.len());
    }
}
