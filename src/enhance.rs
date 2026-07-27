//! Audio enhancement (#20, spec US 33-34, [ADR-0006](../docs/adr/0006-optional-rust-native-audio-enhancement.md)
//! as amended): making a stored Call's audio clearer and consistently loud.
//!
//! **Enhancement** (CONTEXT.md) is reprocessing a Call's audio *after* it is
//! stored — denoise, voice band-pass, loudness normalization — and replacing
//! the object the Call points at. **Passthrough**, the default, keeps whatever
//! the recorder sent.
//!
//! Two things shape everything here:
//!
//! - **It is never on the ingest path.** rdio-scanner converts inline
//!   (`server/controller.go:335` calls `FFMpeg.Convert` mid-ingest), so a slow
//!   encoder slows every recorder upload. Here ingest stores, inserts and
//!   answers `200` before any of this starts, so a `200` still means the Call
//!   is durable — and a recorder that deletes its local copy on success loses
//!   nothing.
//! - **It is off unless an operator turned it on**, at build time *and* run
//!   time being two different questions. ADR-0006 originally gated this behind
//!   a cargo feature; #20 moved the gate to runtime, because the operators who
//!   run a prebuilt binary — nearly all of them — can never reach a build-time
//!   flag.

use std::fmt::Display;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use audioadapter_buffers::direct::InterleavedSlice;
use biquad::{Biquad, Coefficients, DirectForm2Transposed, ToHertz, Type};
use rubato::{FixedSync, Resampler};
use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use tokio::sync::mpsc;
use tracing::{Instrument, Level, debug, info, span, warn};

use crate::AppState;
use crate::call::CallId;
use crate::db::entities::call;
use crate::db::repo;

/// How enhancement behaves, from `[enhancement]` (ADR-0012).
///
/// Policy lives here — in the file, so a headless install can set it and a
/// deployment can version-control it. *Scope* (which Systems and Talkgroups it
/// applies to) is deliberately not here: it is a column on the rows themselves,
/// following the auto-populate precedent (#8).
#[derive(Debug, Clone, PartialEq)]
pub struct EnhancementConfig {
    /// How far the chain runs, and whether it runs at all.
    pub mode: Mode,
    /// What enhanced audio is encoded as.
    pub output: Output,
    /// The integrated loudness every enhanced Call is normalized to, in LUFS.
    pub target_lufs: f64,
    /// How many Calls may be waiting to be enhanced before an arriving one
    /// keeps its passthrough audio instead.
    pub queue_depth: usize,
}

impl EnhancementConfig {
    /// Whether a Call on this System and Talkgroup is enhanced.
    ///
    /// `None` inherits and the more specific row wins, so a Talkgroup may opt
    /// back into a System that opted out. [`Mode::Off`] is a **master switch**
    /// no row overrides: with enhancement disabled there is no mode for an
    /// eager row to run, and quietly picking one for it would be a setting
    /// nobody wrote.
    ///
    /// So scope **narrows and never widens** — which is exactly what US 34
    /// asks for, since its case is keeping one chatty System from eating a Pi.
    pub fn applies_to(&self, scope: Scope) -> bool {
        self.mode != Mode::Off && scope.talkgroup.or(scope.system).unwrap_or(true)
    }
}

/// What the rows a Call belongs to say about enhancing it.
///
/// A named pair rather than a tuple because the two fields have the same type
/// and opposite precedence: read positionally, swapping them silently inverts
/// which one wins, and every test would still pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scope {
    /// The System's column, or `None` to inherit the instance.
    pub system: Option<bool>,
    /// The Talkgroup's column, or `None` to inherit the System. More specific,
    /// so it wins.
    pub talkgroup: Option<bool>,
}

impl Default for EnhancementConfig {
    fn default() -> Self {
        EnhancementConfig {
            mode: Mode::Off,
            output: Output::Wav,
            target_lufs: DEFAULT_TARGET_LUFS,
            queue_depth: DEFAULT_QUEUE_DEPTH,
        }
    }
}

/// The loudness every enhanced Call is normalized to.
///
/// EBU R128 broadcast is -23 LUFS, which is too quiet for a phone speaker in a
/// vehicle; -16 is the speech-oriented target the research recommends and the
/// one rdio-scanner's own loud-normalization preset uses
/// (`loudnorm=I=-16:TP=-1.5:LRA=11`, `server/ffmpeg.go:103`). Matching it means
/// an operator migrating from rdio hears the same levels they are used to.
pub const DEFAULT_TARGET_LUFS: f64 = -16.0;

/// The **sample-peak** ceiling enhanced audio is held under, in dBFS.
///
/// Below 0 on purpose: normalizing to a loudness *target* can leave individual
/// peaks above full scale, and a sample that clips is worse than one that is
/// quiet. -1.5 matches rdio's preset (`TP=-1.5`) and the usual broadcast
/// allowance, so an operator migrating hears familiar levels.
///
/// **Deliberately sample-peak, not true-peak.** A true-peak measurement
/// oversamples to catch inter-sample peaks a reconstruction filter can produce
/// above the highest stored sample. That headroom matters when a signal is
/// about to be re-encoded by a lossy codec at a high sample rate; here the
/// output is 16-bit PCM band-limited to 3.4 kHz at 8 kHz, where inter-sample
/// overshoot is a fraction of a dB and the 1.5 dB of headroom absorbs it. The
/// oversampling would be pure CPU on a Pi for a peak nothing can hear. Named
/// for what it is so the gap is a decision rather than a surprise.
pub const PEAK_CEILING_DBFS: f64 = -1.5;

/// How many Calls may be waiting to be enhanced.
///
/// Deep on purpose. Because the live feed is published at ingest rather than
/// after enhancement, depth costs nothing in listener-visible latency — it only
/// decides how long a burst can outrun the worker before Calls start keeping
/// their passthrough audio. At roughly a second a Call this absorbs several
/// minutes of sustained overload, and it is a queue of row ids, so the memory
/// is a few kilobytes.
pub const DEFAULT_QUEUE_DEPTH: usize = 512;

/// What enhanced audio is written as.
///
/// There is deliberately no AAC. ADR-0006 originally made AAC-LC/M4A the
/// primary enhanced output for its universal iOS support, on the grounds that
/// a cargo feature kept the encoder out of the published binary; #20 moved the
/// gate to runtime, which removed that mitigation and would have made the
/// project a publisher of an AAC encoder — the one act the patent pool
/// licenses. Measured against real scanner audio the trade was never worth
/// making: [`Output::Wav`] at 8 kHz is already smaller than what recorders
/// send, and universal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Output {
    /// 8 kHz 16-bit mono PCM. Plays in every iOS version and every browser,
    /// pure Rust to write, no patent surface. 8 kHz is Nyquist-sufficient for
    /// the voice band the band-pass already imposes, so it discards nothing a
    /// P25 vocoder produced.
    #[default]
    Wav,
    /// Opus in Ogg — royalty-free and roughly five times smaller, but only
    /// playable in Safari from iOS 18.4. **Not built yet**: `libopus` is
    /// vendored C through cmake, so it lands with #23 alongside the arm64
    /// cross-compile work that has to prove that toolchain. Until then this
    /// parses and then refuses to boot, rather than silently writing a
    /// different format than the operator asked for.
    Opus,
}

impl Output {
    /// The spelling used in the file and the environment.
    fn as_str(self) -> &'static str {
        match self {
            Output::Wav => "wav",
            Output::Opus => "opus",
        }
    }

    /// Every spelling, so an error message can list what was expected.
    pub(crate) const ALL: [Output; 2] = [Output::Wav, Output::Opus];
}

impl std::fmt::Display for Output {
    /// Unquoted in a log line (rule 6).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Output {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Output::ALL
            .into_iter()
            .find(|output| output.as_str() == text)
            .ok_or(())
    }
}

/// How much of the chain runs.
///
/// Cumulative rather than orthogonal: [`Mode::Denoise`] is [`Mode::Normalize`]
/// plus RNNoise, because band-passing and levelling audio you are about to
/// denoise is not optional — it is what makes the denoiser's job tractable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Passthrough: audio is stored exactly as the recorder sent it.
    #[default]
    Off,
    /// Voice band-pass + EBU R128 loudness normalization — the stages ADR-0006
    /// calls the proven win, and the one that fixes the level swings between
    /// Talkgroups that make scanning fatiguing.
    Normalize,
    /// ...plus RNNoise. Opt-in on top, because ADR-0006 flags its benefit on
    /// already-vocoder-decoded P25/DMR audio as unproven — a hypothesis, not a
    /// result, until someone has listened.
    Denoise,
}

impl Mode {
    /// The spelling used in the file and the environment.
    fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Normalize => "normalize",
            Mode::Denoise => "denoise",
        }
    }

    /// Every spelling, for an error message that lists what was expected.
    pub(crate) const ALL: [Mode; 3] = [Mode::Off, Mode::Normalize, Mode::Denoise];
}

impl std::fmt::Display for Mode {
    /// Unquoted in a log line, so `enhancement=normalize` greps (rule 6).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Mode {
    type Err = ();

    /// One spelling per mode, shared by the file and the environment, so
    /// `RADIO_SCOUT_ENHANCEMENT_MODE=denoise` and `mode = "denoise"` cannot
    /// drift apart.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Mode::ALL
            .into_iter()
            .find(|mode| mode.as_str() == text)
            .ok_or(())
    }
}

// -- The chain ---------------------------------------------------------------

/// The rate the whole chain runs at.
///
/// Not a preference: RNNoise is trained on 48 kHz and consumes 10 ms /
/// 480-sample frames, so [`Mode::Denoise`] has no other option. Running the
/// other stages there too keeps one code path rather than two.
const WORK_RATE: u32 = 48_000;

/// The rate enhanced audio is written at.
///
/// Nyquist-sufficient for the voice band [`BAND_HIGH_HZ`] already imposes, so
/// downsampling to it discards nothing that survived the band-pass — and a P25
/// vocoder never produced anything above it in the first place. Writing 48 kHz
/// instead would quadruple every stored Call for content that is not there.
const OUTPUT_RATE: u32 = 8_000;

/// The voice band. Below [`BAND_LOW_HZ`] is rumble, hum and CTCSS bleed; above
/// [`BAND_HIGH_HZ`] is hiss. Neither carries speech, and both cost bits.
const BAND_LOW_HZ: f32 = 250.0;
const BAND_HIGH_HZ: f32 = 3_400.0;

/// What enhancement produced, ready to be stored in place of the original.
#[derive(Debug, Clone, PartialEq)]
pub struct Enhanced {
    /// The encoded audio.
    pub bytes: Vec<u8>,
    /// Its MIME type, for the `Content-Type` the audio endpoint serves.
    pub mime: &'static str,
    /// Its file extension, for the object key and the download filename.
    pub extension: &'static str,
    /// How long the Call actually is. Ingest never knows this — it has bytes,
    /// not samples — so an enhanced Call is the first one whose duration is a
    /// measurement rather than a `NULL`.
    pub duration_ms: i64,
}

/// Why a Call could not be enhanced.
///
/// Every variant carries a `reason` slug rather than a sentence, because what
/// it becomes is a WARN line an operator greps (ADR-0011 rules 3 and 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnhanceError {
    /// Nothing here could be recognised as audio.
    Undecodable,
    /// It decoded, but to nothing — a zero-length or silent-header file.
    Empty,
    /// A stage refused mid-flight.
    Failed(&'static str),
}

impl EnhanceError {
    /// The machine-readable slug this failure is logged under.
    pub fn reason(&self) -> &'static str {
        match self {
            EnhanceError::Undecodable => "undecodable",
            EnhanceError::Empty => "empty",
            EnhanceError::Failed(stage) => stage,
        }
    }
}

impl std::fmt::Display for EnhanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

impl std::error::Error for EnhanceError {}

/// Enhance one Call's audio: decode, denoise, band-pass, normalize, re-encode.
///
/// The whole of the DSP, behind one call. `symphonia`, `rubato`, `nnnoiseless`,
/// `biquad` and `ebur128` are implementation detail — a caller hands over the
/// bytes a recorder sent and gets back bytes to store in their place, and a
/// resampler swapped underneath changes nothing here.
///
/// Pure and synchronous: no clock, no store, no database. That is what lets the
/// worker treat it as a unit of work to be scheduled, and what lets its tests
/// assert on audio rather than on plumbing.
pub fn enhance(audio: &[u8], config: &EnhancementConfig) -> Result<Enhanced, EnhanceError> {
    let (samples, rate) = decode(audio)?;
    if samples.is_empty() {
        return Err(EnhanceError::Empty);
    }

    // To the work rate first: RNNoise accepts no other, and the band-pass is
    // better behaved with room above its corner than right against Nyquist.
    let mut samples = resample(&samples, rate, WORK_RATE)?;
    if config.mode == Mode::Denoise {
        samples = denoise(&samples);
    }
    band_pass(&mut samples, WORK_RATE);

    // Down to the storage rate *before* levelling, so what is measured and
    // limited is exactly what will be written — a gain set against 48 kHz and
    // applied to a resampled 8 kHz file could clip after the fact.
    let mut samples = resample(&samples, WORK_RATE, OUTPUT_RATE)?;
    normalize(&mut samples, OUTPUT_RATE, config.target_lufs)?;

    let duration_ms = (samples.len() as i64 * 1000) / OUTPUT_RATE as i64;
    Ok(Enhanced {
        bytes: encode_wav(&samples, OUTPUT_RATE),
        mime: "audio/wav",
        extension: "wav",
        duration_ms,
    })
}

/// Decode anything a recorder sends — WAV, MP3, AAC in M4A — to mono f32.
///
/// Channels are averaged rather than dropped: a recorder that duplicates a mono
/// source across two channels is common, and taking one would throw away 3 dB
/// for no reason.
fn decode(audio: &[u8]) -> Result<(Vec<f32>, u32), EnhanceError> {
    let source = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(audio.to_vec())),
        <_>::default(),
    );
    let mut format = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|_| EnhanceError::Undecodable)?;

    let track = format
        .default_track(symphonia::core::formats::TrackType::Audio)
        .ok_or(EnhanceError::Undecodable)?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or(EnhanceError::Undecodable)?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|_| EnhanceError::Undecodable)?;

    let mut samples = Vec::new();
    let mut rate = params.sample_rate.unwrap_or(0);
    while let Ok(Some(packet)) = format.next_packet() {
        if packet.track_id != track_id {
            continue;
        }
        // A packet that will not decode is a damaged frame, not a damaged file:
        // recorders truncate, and dropping one is better than losing the Call.
        let Ok(decoded) = decoder.decode(&packet) else {
            continue;
        };
        rate = decoded.spec().rate();
        append_mono(&decoded, &mut samples);
    }

    if rate == 0 {
        return Err(EnhanceError::Undecodable);
    }
    Ok((samples, rate))
}

/// Average a decoded buffer's channels onto the end of `out`.
fn append_mono(decoded: &GenericAudioBufferRef<'_>, out: &mut Vec<f32>) {
    let mut planes = Vec::new();
    decoded.copy_to_vec_interleaved(&mut planes);
    let channels = decoded.spec().channels().count().max(1);
    out.extend(
        planes
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32),
    );
}

/// Resample between two rates, or hand the samples straight back when they
/// already match — which is the common case for a Trunk Recorder writing 8 kHz.
fn resample(samples: &[f32], from: u32, to: u32) -> Result<Vec<f32>, EnhanceError> {
    if from == to {
        return Ok(samples.to_vec());
    }
    let mut resampler =
        rubato::Fft::<f32>::new(from as usize, to as usize, 1024, 1, FixedSync::Input)
            .map_err(|_| EnhanceError::Failed("resample"))?;

    // `process_all_into_buffer` writes at most `ceil(ratio * input)` frames,
    // plus the resampler's own delay; the slack is trimmed by the frame count
    // it reports, so over-allocating costs nothing but a moment.
    let capacity = (samples.len() as f64 * to as f64 / from as f64).ceil() as usize + 2 * 1024;
    let mut out = vec![0f32; capacity];
    let input = InterleavedSlice::new(samples, 1, samples.len())
        .map_err(|_| EnhanceError::Failed("resample"))?;
    let mut output = InterleavedSlice::new_mut(&mut out, 1, capacity)
        .map_err(|_| EnhanceError::Failed("resample"))?;
    let (_, written) = resampler
        .process_all_into_buffer(&input, &mut output, samples.len(), None)
        .map_err(|_| EnhanceError::Failed("resample"))?;
    out.truncate(written);
    Ok(out)
}

/// RNNoise, frame by frame.
///
/// Its benefit on already-vocoder-decoded P25/DMR audio is a **hypothesis**
/// (ADR-0006), which is why reaching this needs `mode = "denoise"` rather than
/// merely enabling enhancement. It works in 480-sample frames on i16-scaled
/// floats; a trailing part-frame is padded with silence and then trimmed back
/// off, so the Call does not grow by up to 10 ms every time it is enhanced.
fn denoise(samples: &[f32]) -> Vec<f32> {
    const FRAME: usize = nnnoiseless::FRAME_SIZE;
    let mut state = nnnoiseless::DenoiseState::new();
    let mut out = Vec::with_capacity(samples.len());
    let mut frame = [0f32; FRAME];
    let mut denoised = [0f32; FRAME];

    for chunk in samples.chunks(FRAME) {
        frame.fill(0.0);
        // RNNoise works in i16 units, not normalized floats.
        for (slot, sample) in frame.iter_mut().zip(chunk) {
            *slot = sample * i16::MAX as f32;
        }
        state.process_frame(&mut denoised, &frame);
        out.extend(
            denoised
                .iter()
                .take(chunk.len())
                .map(|s| s / i16::MAX as f32),
        );
    }
    out
}

/// The voice band-pass: a high-pass to kill rumble and hum, a low-pass to kill
/// hiss. Cheap — a handful of multiply-adds a sample — and it is what lets the
/// output sit at 8 kHz without losing anything.
fn band_pass(samples: &mut [f32], rate: u32) {
    let mut high_pass = filter(Type::HighPass, BAND_LOW_HZ, rate);
    let mut low_pass = filter(Type::LowPass, BAND_HIGH_HZ, rate);
    for sample in samples.iter_mut() {
        *sample = low_pass.run(high_pass.run(*sample));
    }
}

fn filter(kind: Type<f32>, cutoff: f32, rate: u32) -> DirectForm2Transposed<f32> {
    let coefficients = Coefficients::<f32>::from_params(
        kind,
        (rate as f32).hz(),
        cutoff.hz(),
        biquad::Q_BUTTERWORTH_F32,
    )
    .expect("a cutoff well inside Nyquist at both of this module's rates");
    DirectForm2Transposed::<f32>::new(coefficients)
}

/// Two-pass EBU R128 loudness normalization, held under a peak ceiling.
///
/// **The stage that earns its keep.** Measure the whole Call, apply one gain to
/// land it on the target, then scale that gain back if it would push a sample
/// past the ceiling.
///
/// One **static** gain for the whole Call, not a limiter riding the level: a
/// compressor would pump the noise floor up between words, which is the exact
/// artefact that makes cheap AGC unpleasant on scanner audio. The trade is
/// real and worth stating — a Call with one loud transient lands below
/// `target_lufs`, because the ceiling wins. Consistent-and-slightly-quiet is
/// the better failure for a scanner than consistent-and-clipped.
fn normalize(samples: &mut [f32], rate: u32, target_lufs: f64) -> Result<(), EnhanceError> {
    let mut meter = ebur128::EbuR128::new(1, rate, ebur128::Mode::I)
        .map_err(|_| EnhanceError::Failed("measure-loudness"))?;
    meter
        .add_frames_f32(samples)
        .map_err(|_| EnhanceError::Failed("measure-loudness"))?;
    let measured = meter
        .loudness_global()
        .map_err(|_| EnhanceError::Failed("measure-loudness"))?;

    // Loudness that could not be measured leaves the level alone: digital
    // silence measures as -inf, and so does a transmission too short to fill
    // one of R128's 400 ms blocks — a real thing on a busy System. There is no
    // gain that makes silence audible, and inventing one fills the archive with
    // NaN, which reaches a listener as a burst of noise.
    //
    // The **ceiling still applies**. Not clipping is not conditional on having
    // a measurement: a short, loud blip that skipped the limiter would be
    // stored at whatever level it arrived at, which is exactly the case that
    // clips.
    let mut gain = match measured.is_finite() {
        true => 10f32.powf(((target_lufs - measured) / 20.0) as f32),
        false => 1.0,
    };
    // Peaks are found after the gain would be applied, so the ceiling holds for
    // what is actually written rather than for what was measured.
    let ceiling = 10f32.powf((PEAK_CEILING_DBFS / 20.0) as f32);
    let peak = samples.iter().fold(0f32, |max, s| max.max(s.abs())) * gain;
    if peak > ceiling {
        gain *= ceiling / peak;
    }
    for sample in samples.iter_mut() {
        *sample *= gain;
    }
    Ok(())
}

/// Write mono 16-bit PCM in a canonical 44-byte-header WAV.
///
/// Hand-written rather than a dependency: this is the entire format, it has no
/// patent surface and no C, and it is the one encoder that plays on every iOS
/// version there has ever been.
fn encode_wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes())
        .collect();
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend(b"RIFF");
    out.extend(((36 + data.len()) as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes()); // PCM format chunk size
    out.extend(1u16.to_le_bytes()); // PCM
    out.extend(1u16.to_le_bytes()); // mono
    out.extend(rate.to_le_bytes());
    out.extend((rate * 2).to_le_bytes()); // byte rate: rate * channels * 2
    out.extend(2u16.to_le_bytes()); // block align
    out.extend(16u16.to_le_bytes()); // bits per sample
    out.extend(b"data");
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    out
}

// -- The enhancement queue and its worker ------------------------------------

/// The enhancement surface, cloned into every handler — the shape [`crate::push::Push`]
/// uses, for the same reason: `None` is an instance where the feature is off,
/// and nothing else in the app behaves differently.
#[derive(Clone, Default)]
pub struct Enhancer(Option<Arc<Inner>>);

struct Inner {
    config: EnhancementConfig,
    /// The **enhancement queue** (CONTEXT.md) — Call ids, never audio. The
    /// object is already written by the time anything lands here, so the worker
    /// reads it back rather than carrying megabytes through a channel.
    submissions: mpsc::Sender<CallId>,
    /// Taken once, by [`spawn`]. A second `spawn` finds it gone and starts no
    /// second worker, which is what keeps "one core" true.
    inbox: Mutex<Option<mpsc::Receiver<CallId>>>,
}

impl Enhancer {
    /// An instance that enhances, with a queue `config.queue_depth` deep.
    pub fn new(config: EnhancementConfig) -> Self {
        let (submissions, inbox) = mpsc::channel(config.queue_depth);
        Enhancer(Some(Arc::new(Inner {
            config,
            submissions,
            inbox: Mutex::new(Some(inbox)),
        })))
    }

    /// An instance that enhances nothing — the shipped default, and what
    /// `mode = "off"` resolves to.
    pub fn disabled() -> Self {
        Enhancer(None)
    }

    /// Build from configuration: [`Mode::Off`] is the master switch, so it
    /// produces a disabled enhancer rather than a worker with nothing to do.
    pub fn from_config(config: EnhancementConfig) -> Self {
        match config.mode {
            Mode::Off => Enhancer::disabled(),
            _ => Enhancer::new(config),
        }
    }

    /// Whether this instance enhances anything at all.
    ///
    /// Asked **before** any lookup that would decide *which* Calls: on the
    /// shipped default this is the whole answer, and the ingest path must not
    /// buy three SELECTs to rediscover that a disabled feature is disabled.
    pub fn is_enabled(&self) -> bool {
        self.0.is_some()
    }

    /// Whether a Call on this System and Talkgroup would be enhanced.
    pub fn applies_to(&self, scope: Scope) -> bool {
        self.0
            .as_ref()
            .is_some_and(|inner| inner.config.applies_to(scope))
    }

    /// Offer a Call to the queue. `false` means it stays passthrough.
    ///
    /// Never blocks and never waits: this is called from the ingest path, which
    /// has already answered the recorder and must not acquire a new reason to
    /// be slow. A full queue is a load signal, not an error — the Call is
    /// already stored and already playable, and the only thing lost is that it
    /// will not be levelled.
    pub fn submit(&self, call_id: CallId) -> bool {
        let Some(inner) = &self.0 else {
            return false;
        };
        match inner.submissions.try_send(call_id) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                // WARN, because something was dropped (rule 7) and a queue that
                // is persistently full is the operator's to act on — it means
                // this hardware cannot enhance at the rate this System talks.
                // `%reason`, not the default: `reason=queue-full` greps and
                // `reason="queue-full"` does not — the same rule every other
                // rejection slug follows (`ingest::rejected`, rule 6).
                warn!(
                    reason = %"queue-full",
                    call_id, "enhancement skipped; Call keeps the audio it arrived with"
                );
                false
            }
            // The worker is gone, which happens only as the process shuts down.
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

/// Start the enhancement worker: re-queue what a restart interrupted, then work
/// the queue until the process ends.
///
/// One worker, deliberately. Enhancement is CPU-bound and a Pi has four cores
/// that are also serving audio, taking uploads and running a database; a pool
/// sized to the machine would make enhancement the thing that makes everything
/// else slow, which is precisely what US 34 exists to prevent.
pub fn spawn(state: AppState) {
    let Some(inner) = state.enhancer.0.clone() else {
        return;
    };
    let Some(mut inbox) = inner.inbox.lock().expect("enhancement inbox").take() else {
        return;
    };

    tokio::spawn(async move {
        catch_up(&state).await;
        while let Some(call_id) = inbox.recv().await {
            run(&state, &inner.config, call_id).await;
        }
    });
}

/// Re-queue Calls that were pending when the process last stopped.
///
/// Only `pending` — never `none`. A Call marked `none` was ingested while
/// enhancement was off, and sweeping those up would mean that turning
/// enhancement on silently re-encoded an operator's entire archive the next
/// time they restarted. Converting an existing archive is a deliberate act, and
/// it is not this.
async fn catch_up(state: &AppState) {
    let pending = match repo::calls_pending_enhancement(&state.db).await {
        Ok(pending) => pending,
        Err(error) => {
            warn!(
                reason = %"sweep-failed",
                %error,
                "could not look for Calls left mid-enhancement"
            );
            return;
        }
    };
    if pending.is_empty() {
        return;
    }
    info!(
        calls = pending.len(),
        "resuming enhancement after a restart"
    );
    for call_id in pending {
        offer(state, call_id).await;
    }
}

/// Offer an already-`pending` Call to the queue, and record it as `skipped` if
/// the queue would not take it.
///
/// Shared by ingest and the boot sweep so the two cannot drift: a refusal that
/// left the row `pending` would be re-queued by every subsequent boot, and —
/// because a `pending` Call is deliberately served without `immutable`
/// ([`crate::audio_cache_control`]) — would stay permanently uncacheable. That
/// is exactly what happens when a restart interrupts more Calls than the queue
/// is deep.
pub(crate) async fn offer(state: &AppState, call_id: CallId) {
    if state.enhancer.submit(call_id) {
        return;
    }
    // `submit` has already said why it refused.
    if let Err(error) =
        repo::mark_enhancement(&state.db, call_id, call::EnhancementState::SKIPPED).await
    {
        warn!(
            reason = %"mark-skipped-failed",
            %error,
            "a refused Call is still marked pending and will be re-queued every boot"
        );
    }
}

/// Enhance one Call: read its audio, run the chain, and point the row at the
/// result.
async fn run(state: &AppState, config: &EnhancementConfig, call_id: CallId) {
    let span = span!(Level::ERROR, "enhance", call_id);
    async {
        let audio = match repo::get_call_audio(&state.db, call_id).await {
            Ok(Some(audio)) => audio,
            // The Call was pruned between being queued and being reached, which
            // retention is entitled to do. Nothing to say.
            Ok(None) => return,
            Err(error) => return skipped(state, call_id, "look-up-call", &error).await,
        };
        let original = match state.audio.get(&audio.object_key).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return skipped(state, call_id, "audio-missing", &"no object").await,
            Err(error) => return skipped(state, call_id, "read-audio", &error).await,
        };

        // The one blocking stage. On `spawn_blocking` because it is hundreds of
        // milliseconds of solid arithmetic, and holding a Tokio worker thread
        // for that long stalls every request scheduled behind it.
        let config = config.clone();
        let enhanced = match tokio::task::spawn_blocking(move || enhance(&original, &config)).await
        {
            Ok(Ok(enhanced)) => enhanced,
            Ok(Err(error)) => return skipped(state, call_id, error.reason(), &error).await,
            Err(error) => return skipped(state, call_id, "enhance-panicked", &error).await,
        };

        // A new key, never the old one: ADR-0002's ordering rests on objects
        // being immutable once written, and a listener mid-download of the
        // original keeps reading a file that still exists until orphan-GC
        // reclaims it.
        let object_key = crate::blob::new_object_key(enhanced.extension);
        let bytes = enhanced.bytes.len() as i64;
        if let Err(error) = state
            .audio
            .put(&object_key, bytes::Bytes::from(enhanced.bytes))
            .await
        {
            return skipped(state, call_id, "store-audio", &error).await;
        }

        if let Err(error) = repo::store_enhanced_audio(
            &state.db,
            call_id,
            repo::EnhancedAudio {
                object_key: &object_key,
                mime: enhanced.mime,
                name: format!("{call_id}.{}", enhanced.extension),
                bytes,
                duration_ms: enhanced.duration_ms,
            },
        )
        .await
        {
            // The object is written but unreferenced — an orphan, which #10's
            // sweep reclaims. The Call keeps working on its original audio.
            return skipped(state, call_id, "store-call", &error).await;
        }
        debug!(bytes, duration_ms = enhanced.duration_ms, "call enhanced");
    }
    .instrument(span)
    .await
}

/// Give up on enhancing a Call, saying why, and leave it playable.
///
/// One funnel, the shape [`crate::ingest::rejected`] uses and for the same
/// reason: a Call that quietly stopped being enhanced with nothing in the log
/// is indistinguishable from one that was never queued. The Call keeps the
/// audio it arrived with, so nothing a listener can reach is broken by this.
async fn skipped(
    state: &AppState,
    call_id: CallId,
    reason: &'static str,
    // `+ Send + Sync`, because this is a parameter of a future the worker holds
    // across an await, and a bare `dyn Display` would make that future — and so
    // the whole worker task — un-spawnable.
    cause: &(dyn Display + Send + Sync),
) {
    warn!(reason = %reason, cause = %cause, "enhancement skipped");
    if let Err(error) =
        repo::mark_enhancement(&state.db, call_id, call::EnhancementState::SKIPPED).await
    {
        warn!(
            reason = %"mark-skipped-failed",
            %error,
            "could not record that enhancement was skipped"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::LogCapture;
    use proptest::prelude::*;
    use rstest::rstest;

    /// Test signals are two seconds long because EBU R128 integrated loudness
    /// is built from 400 ms blocks — anything shorter has nothing to integrate.
    const SECONDS: f64 = 2.0;

    /// A mono 16-bit PCM WAV, built by hand.
    ///
    /// Deliberately *not* built with the crate the implementation encodes with:
    /// a test that shares its writer with the code under test cannot tell a
    /// wrong header from a consistently wrong one.
    fn wav(samples: &[f32], rate: u32) -> Vec<u8> {
        let data: Vec<u8> = samples
            .iter()
            .flat_map(|s| ((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes())
            .collect();
        let mut out = Vec::new();
        out.extend(b"RIFF");
        out.extend(((36 + data.len()) as u32).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes()); // PCM fmt chunk size
        out.extend(1u16.to_le_bytes()); // PCM
        out.extend(1u16.to_le_bytes()); // mono
        out.extend(rate.to_le_bytes());
        out.extend((rate * 2).to_le_bytes()); // byte rate
        out.extend(2u16.to_le_bytes()); // block align
        out.extend(16u16.to_le_bytes()); // bits
        out.extend(b"data");
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
        out
    }

    /// Read a mono 16-bit WAV back — a hand-rolled reader, for the same reason
    /// [`wav`] is a hand-rolled writer. It also *is* the assertion that the
    /// output is a canonical WAV: a file this cannot read is one a browser
    /// would struggle with too.
    fn read_wav(bytes: &[u8]) -> (Vec<f32>, u32) {
        assert_eq!(&bytes[0..4], b"RIFF", "not a RIFF file");
        assert_eq!(&bytes[8..12], b"WAVE", "not a WAVE file");
        let mut rate = 0;
        let mut samples = Vec::new();
        let mut at = 12;
        while at + 8 <= bytes.len() {
            let id = &bytes[at..at + 4];
            let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
            match id {
                b"fmt " => {
                    assert_eq!(u16::from_le_bytes(body[0..2].try_into().unwrap()), 1, "PCM");
                    assert_eq!(
                        u16::from_le_bytes(body[2..4].try_into().unwrap()),
                        1,
                        "mono"
                    );
                    assert_eq!(
                        u16::from_le_bytes(body[14..16].try_into().unwrap()),
                        16,
                        "16-bit"
                    );
                    rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                }
                b"data" => {
                    samples = body
                        .chunks_exact(2)
                        .map(|p| i16::from_le_bytes([p[0], p[1]]) as f32 / i16::MAX as f32)
                        .collect();
                }
                _ => {}
            }
            at += 8 + size + (size & 1); // chunks are word-aligned
        }
        assert!(rate > 0, "no fmt chunk");
        (samples, rate)
    }

    /// A sine at `amplitude`, as a recorder would have captured it.
    fn tone(freq: f64, rate: u32, amplitude: f64) -> Vec<f32> {
        tone_of(freq, rate, amplitude, SECONDS)
    }

    fn tone_of(freq: f64, rate: u32, amplitude: f64, seconds: f64) -> Vec<f32> {
        let n = (seconds * rate as f64) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / rate as f64;
                (amplitude * (std::f64::consts::TAU * freq * t).sin()) as f32
            })
            .collect()
    }

    /// Integrated loudness of an encoded result, measured independently of how
    /// the implementation chose its gain.
    fn loudness_of(encoded: &[u8]) -> f64 {
        let (samples, rate) = read_wav(encoded);
        let mut meter = ebur128::EbuR128::new(1, rate, ebur128::Mode::I).expect("meter");
        meter.add_frames_f32(&samples).expect("measure");
        meter.loudness_global().expect("integrated loudness")
    }

    fn normalizing() -> EnhancementConfig {
        EnhancementConfig {
            mode: Mode::Normalize,
            ..EnhancementConfig::default()
        }
    }

    /// A recorder mid-crash, a truncated upload, a `.wav` that is really an
    /// HTML error page. None of these may panic a worker that is holding a
    /// queue: they become a `reason` an operator can grep (ADR-0011 rule 3).
    #[rstest]
    #[case::html(b"<!doctype html><title>502</title>".to_vec(), EnhanceError::Undecodable)]
    #[case::empty(Vec::new(), EnhanceError::Undecodable)]
    #[case::truncated_header(b"RIFF".to_vec(), EnhanceError::Undecodable)]
    #[case::a_wav_with_no_audio(wav(&[], 8_000), EnhanceError::Empty)]
    fn audio_that_cannot_be_enhanced_says_why(
        #[case] audio: Vec<u8>,
        #[case] expected: EnhanceError,
    ) {
        let error = enhance(&audio, &normalizing()).expect_err("not enhanceable");

        assert_eq!(error, expected);
        assert!(!error.reason().is_empty(), "every failure has a slug");
    }

    /// A Call that is entirely silence has no loudness to correct toward —
    /// `-inf` LUFS. The gain for that is infinite, and multiplying by it fills
    /// the archive with NaN, which a browser renders as a burst of noise. It
    /// stays silent instead.
    #[test]
    fn a_silent_call_stays_silent_rather_than_becoming_noise() {
        let silence = wav(&vec![0.0; 16_000 * 2], 16_000);

        let enhanced = enhance(&silence, &normalizing()).expect("enhance");

        let (samples, _) = read_wav(&enhanced.bytes);
        assert!(
            samples.iter().all(|s| s.abs() < 0.001 && s.is_finite()),
            "silence became something else"
        );
    }

    /// Denoising is a *stage*, not a different pipeline: everything the
    /// normalize mode guarantees still holds with it switched on.
    #[test]
    fn denoising_still_produces_a_levelled_eight_kilohertz_call() {
        let config = EnhancementConfig {
            mode: Mode::Denoise,
            ..normalizing()
        };

        let enhanced = enhance(&wav(&tone(1000.0, 16_000, 0.1), 16_000), &config).expect("enhance");

        let (samples, rate) = read_wav(&enhanced.bytes);
        assert_eq!(rate, OUTPUT_RATE);
        assert!(peak_of(&enhanced.bytes) <= ceiling() + 0.001);
        // RNNoise works in 480-sample frames; a Call that is not a whole number
        // of them must not be padded out to one.
        let expected = (SECONDS * OUTPUT_RATE as f64) as usize;
        assert!(
            samples.len().abs_diff(expected) < OUTPUT_RATE as usize / 10,
            "{} samples for a {expected}-sample Call",
            samples.len()
        );
    }

    /// Every failure carries a slug an operator can grep, including the ones
    /// built from a stage name (ADR-0011 rules 3 and 6) — and the slug is what
    /// `Display` renders, so a `%error` in a log line and a `reason=` field can
    /// never say two different things.
    #[rstest]
    #[case(EnhanceError::Undecodable, "undecodable")]
    #[case(EnhanceError::Empty, "empty")]
    #[case(EnhanceError::Failed("resample"), "resample")]
    fn every_failure_has_one_greppable_name(#[case] error: EnhanceError, #[case] expected: &str) {
        assert_eq!(error.reason(), expected);
        assert_eq!(error.to_string(), expected);
    }

    /// A recorder killed mid-upload leaves a header promising more audio than
    /// the file contains. The frames that *did* arrive are still a Call worth
    /// levelling, so the damaged tail is dropped rather than the whole upload —
    /// which is the difference between a listener hearing half a transmission
    /// and hearing nothing.
    #[test]
    fn a_truncated_upload_keeps_the_audio_that_did_arrive() {
        let whole = wav(&tone(1000.0, 16_000, 0.2), 16_000);
        let mut truncated = whole.clone();
        truncated.truncate(truncated.len() / 2);

        let enhanced = enhance(&truncated, &normalizing()).expect("half a Call is still a Call");

        let (samples, rate) = read_wav(&enhanced.bytes);
        assert_eq!(rate, OUTPUT_RATE);
        // About half the original two seconds, at the storage rate — enough to
        // pin that the surviving frames were kept rather than the file being
        // padded back out or truncated to nothing.
        let expected = OUTPUT_RATE as usize;
        assert!(
            samples.len().abs_diff(expected) < expected / 4,
            "{} samples; half of a two-second Call is about {expected}",
            samples.len()
        );
    }

    // -- The queue's admission decision -------------------------------------

    fn queue_of_depth(queue_depth: usize) -> Enhancer {
        Enhancer::new(EnhancementConfig {
            queue_depth,
            ..normalizing()
        })
    }

    /// **Backpressure.** Nothing drains this queue, so the second Call meets it
    /// full — which is what a Pi that cannot enhance as fast as a System talks
    /// looks like, made deterministic.
    ///
    /// The Call is already stored, already answered and already on the live
    /// feed by the time this is reached, so a refusal costs the levelling and
    /// nothing else. What it must never do is block: `submit` is called from
    /// the ingest path.
    #[test]
    fn a_full_queue_refuses_rather_than_waiting() {
        let enhancer = queue_of_depth(1);

        assert!(enhancer.submit(1), "the first Call fits");
        assert!(
            !enhancer.submit(2),
            "the second must be refused, not queued"
        );
    }

    /// ...and says so, with a slug an operator can grep for (rule 3), because a
    /// queue that is persistently full means this hardware cannot keep up —
    /// which is theirs to act on, not ours to hide.
    #[test]
    fn a_refused_call_says_why() {
        let capture = LogCapture::start();
        let enhancer = queue_of_depth(1);

        enhancer.submit(1);
        enhancer.submit(2);

        let logged = capture.text();
        assert!(logged.contains(" WARN "), "{logged}");
        assert!(logged.contains("reason=queue-full"), "{logged}");
        assert!(logged.contains("call_id=2"), "{logged}");
    }

    /// A queue whose worker has gone — which happens as the process shuts
    /// down — refuses quietly. The Call is already stored and answered, and
    /// there is nobody left to warn.
    #[test]
    fn a_queue_whose_worker_has_gone_refuses_quietly() {
        let capture = LogCapture::start();
        let enhancer = queue_of_depth(4);
        // Take the receiver and drop it, exactly as the worker task ending does.
        drop(
            enhancer
                .0
                .as_ref()
                .expect("an enabled enhancer")
                .inbox
                .lock()
                .expect("inbox")
                .take(),
        );

        assert!(!enhancer.submit(1));
        assert_eq!(capture.text(), "", "shutdown is not an operator's problem");
    }

    /// An instance with enhancement off has no queue to offer a Call to, and
    /// must not pretend otherwise — a `true` here would leave a row marked
    /// `pending` that nothing would ever pick up.
    #[test]
    fn a_disabled_instance_accepts_nothing() {
        assert!(!Enhancer::disabled().submit(1));
        assert!(!Enhancer::default().submit(1));
        assert!(!Enhancer::disabled().applies_to(Scope::default()));
        assert!(
            !Enhancer::disabled().is_enabled(),
            "and says so before any I/O"
        );
    }

    /// `mode = "off"` is the master switch, so building from configuration must
    /// produce the disabled enhancer rather than a worker with nothing to do.
    #[test]
    fn configuration_with_enhancement_off_builds_no_queue() {
        let enhancer = Enhancer::from_config(EnhancementConfig::default());

        assert!(!enhancer.submit(1));
        assert!(!enhancer.applies_to(Scope::default()));
        // The check ingest makes *before* it will pay for a scope lookup.
        assert!(!enhancer.is_enabled());
    }

    /// ...and configuration that *is* on builds one that accepts Calls and
    /// carries the scope rules with it.
    #[test]
    fn configuration_with_enhancement_on_builds_a_working_queue() {
        let enhancer = Enhancer::from_config(normalizing());

        assert!(enhancer.is_enabled());
        assert!(enhancer.applies_to(Scope::default()));
        assert!(
            !enhancer.applies_to(Scope {
                system: Some(false),
                talkgroup: None,
            }),
            "scope still narrows"
        );
        assert!(enhancer.submit(1));
    }

    /// **Scope, resolved.** `NULL` inherits, the more specific row wins, and
    /// `mode = "off"` is the master switch that no row can override — there
    /// would be no mode for it to run.
    ///
    /// The whole point (US 34) is keeping one chatty System off a Pi, which is
    /// narrowing; a row that could *widen* would need a mode of its own to say
    /// how, and that is a knob nobody asked for.
    #[rstest]
    // Nothing said anywhere: the instance-wide setting decides.
    #[case::inherited_on(Mode::Normalize, None, None, true)]
    #[case::inherited_off(Mode::Off, None, None, false)]
    // A System opts out, and its Talkgroups follow it.
    #[case::system_opts_out(Mode::Normalize, Some(false), None, false)]
    // ...but a Talkgroup is more specific than the System it belongs to, both ways.
    #[case::talkgroup_opts_back_in(Mode::Normalize, Some(false), Some(true), true)]
    #[case::talkgroup_opts_out(Mode::Normalize, Some(true), Some(false), false)]
    // The master switch: with enhancement off there is no mode to run, so a row
    // asking for it is not silently given one.
    #[case::off_beats_an_eager_system(Mode::Off, Some(true), None, false)]
    #[case::off_beats_an_eager_talkgroup(Mode::Off, None, Some(true), false)]
    fn scope_narrows_but_never_widens(
        #[case] mode: Mode,
        #[case] system: Option<bool>,
        #[case] talkgroup: Option<bool>,
        #[case] expected: bool,
    ) {
        let config = EnhancementConfig {
            mode,
            ..EnhancementConfig::default()
        };

        assert_eq!(config.applies_to(Scope { system, talkgroup }), expected);
    }

    /// Found by `every_enhanced_call_is_finite_levelled_and_at_the_storage_rate`,
    /// and kept as a case in its own right because it is a real transmission
    /// rather than a generator artefact: a two-word blip is shorter than EBU
    /// R128's 400 ms integration block, so it has no measurable loudness at
    /// all. Skipping the gain for it is right; skipping **the ceiling** with it
    /// was not, and stored a loud blip at whatever level it arrived at.
    #[test]
    fn a_call_too_short_to_measure_is_still_kept_below_the_ceiling() {
        let blip = wav(&tone_of(1000.0, 44_100, 0.99, 0.09), 44_100);

        let enhanced = enhance(&blip, &normalizing()).expect("enhance");

        let peak = peak_of(&enhanced.bytes);
        assert!(
            peak <= ceiling() + 0.001,
            "an unmeasurable Call peaked at {peak:.4}"
        );
    }

    /// The linear amplitude [`PEAK_CEILING_DBFS`] describes.
    fn ceiling() -> f32 {
        10f32.powf((PEAK_CEILING_DBFS / 20.0) as f32)
    }

    fn peak_of(encoded: &[u8]) -> f32 {
        read_wav(encoded).0.iter().fold(0f32, |m, s| m.max(s.abs()))
    }

    /// The target is a setting, so it has to be the one that is hit — a
    /// normalizer that lands everything on a hard-coded level would still pass
    /// the "same level" test above.
    #[rstest]
    #[case(-16.0)]
    #[case(-23.0)]
    fn a_call_lands_on_the_configured_loudness(#[case] target_lufs: f64) {
        let config = EnhancementConfig {
            target_lufs,
            ..normalizing()
        };

        let enhanced = enhance(&wav(&tone(1000.0, 16_000, 0.1), 16_000), &config).expect("enhance");

        let measured = loudness_of(&enhanced.bytes);
        assert!(
            (measured - target_lufs).abs() < 1.0,
            "asked for {target_lufs} LUFS, got {measured:.2}"
        );
    }

    /// Loudness and peak are different quantities, and normalizing to a target
    /// can push a spike past full scale even when the Call as a whole is quiet.
    /// A clipped sample is a worse outcome than a slightly quiet Call, so the
    /// gain gives way to the ceiling rather than the other way round.
    #[test]
    fn a_spiky_call_is_pulled_below_the_ceiling_rather_than_clipped() {
        let mut samples = tone(1000.0, 16_000, 0.02);
        for spike in (0..samples.len()).step_by(4_000) {
            samples[spike] = 1.0;
        }

        let enhanced = enhance(&wav(&samples, 16_000), &normalizing()).expect("enhance");

        let peak = peak_of(&enhanced.bytes);
        assert!(
            peak <= ceiling() + 0.001,
            "peaked at {peak:.4}, ceiling is {:.4}",
            ceiling()
        );
    }

    /// Power at one frequency, by correlating against it directly.
    ///
    /// A single-bin DFT rather than an FFT: it is exact at any frequency
    /// without bin alignment, it is six lines, and it shares nothing with the
    /// filters under test.
    fn power_at(samples: &[f32], rate: u32, freq: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (n, sample) in samples.iter().enumerate() {
            let phase = std::f64::consts::TAU * freq * n as f64 / rate as f64;
            re += *sample as f64 * phase.cos();
            im -= *sample as f64 * phase.sin();
        }
        (re * re + im * im) / (samples.len() as f64).powi(2)
    }

    /// Two tones at equal amplitude, as a Call with something under it.
    fn two_tones(a: f64, b: f64, rate: u32, amplitude: f64) -> Vec<f32> {
        tone(a, rate, amplitude / 2.0)
            .into_iter()
            .zip(tone(b, rate, amplitude / 2.0))
            .map(|(x, y)| x + y)
            .collect()
    }

    /// Rumble, hum and CTCSS bleed sit below the voice band and carry no
    /// speech; the high-pass exists to remove them. Asserted as a *ratio*
    /// against the speech tone, because normalization applies a gain to
    /// everything and an absolute level would move with it.
    #[test]
    fn rumble_is_removed_and_speech_survives() {
        let input = wav(&two_tones(60.0, 1000.0, 16_000, 0.4), 16_000);
        let before = power_at(&two_tones(60.0, 1000.0, 16_000, 0.4), 16_000, 60.0)
            / power_at(&two_tones(60.0, 1000.0, 16_000, 0.4), 16_000, 1000.0);

        let enhanced = enhance(&input, &normalizing()).expect("enhance");

        let (samples, rate) = read_wav(&enhanced.bytes);
        let after = power_at(&samples, rate, 60.0) / power_at(&samples, rate, 1000.0);
        assert!(
            before > 0.5,
            "the two tones should start out level; started at {before:.4}"
        );
        assert!(
            after < 0.01,
            "60 Hz should be at least 20 dB down on speech; it is at {after:.4}"
        );
    }

    /// The low-pass has to run **before** the downsample, not after.
    ///
    /// 6 kHz cannot be represented at 8 kHz — it folds to 2 kHz. So if the
    /// stages were ever reordered, hiss the filter was meant to remove would
    /// reappear *inside the voice band* as an alias, which is worse than
    /// leaving it alone. Energy at 2 kHz is the tell, and nothing about a
    /// correctly-ordered chain can put any there.
    #[test]
    fn hiss_is_removed_before_it_could_alias_into_speech() {
        let input = wav(&two_tones(1000.0, 6000.0, 48_000, 0.4), 48_000);

        let enhanced = enhance(&input, &normalizing()).expect("enhance");

        let (samples, rate) = read_wav(&enhanced.bytes);
        let aliased = power_at(&samples, rate, 2000.0) / power_at(&samples, rate, 1000.0);
        assert!(
            aliased < 0.01,
            "6 kHz folded back to 2 kHz at {aliased:.4} of the speech tone"
        );
    }

    /// Whatever a recorder sends, storage gets 8 kHz mono — including the
    /// 96 kHz the maintainer's own Trunk Recorder produces, which carries no
    /// more speech than the 8 kHz a P25 vocoder emitted and costs twelve times
    /// the bytes to keep.
    ///
    /// The rate is asserted by reading the header back with a parser that
    /// shares no code with the writer, so a plausible-but-wrong header cannot
    /// agree with itself into a pass.
    #[rstest]
    #[case::trunk_recorder_wav(8_000)]
    #[case::narrowband(16_000)]
    #[case::cd_rate(44_100)]
    #[case::the_real_archive(96_000)]
    fn every_input_rate_lands_on_eight_kilohertz_mono(#[case] rate: u32) {
        let input = wav(&tone(1000.0, rate, 0.2), rate);

        let enhanced = enhance(&input, &normalizing()).expect("enhance");

        let (samples, out_rate) = read_wav(&enhanced.bytes);
        assert_eq!(out_rate, OUTPUT_RATE, "stored at the wrong rate");
        assert_eq!(enhanced.mime, "audio/wav");
        assert_eq!(enhanced.extension, "wav");
        assert!(!samples.is_empty(), "enhanced to nothing");
    }

    /// Ingest knows a Call's byte count but never its length — it has bytes,
    /// not samples — so `duration_ms` has been `NULL` for every Call ever
    /// stored. Enhancement decodes, so it is the first thing that can measure.
    #[test]
    fn an_enhanced_call_knows_how_long_it_is() {
        let input = wav(&tone(1000.0, 16_000, 0.2), 16_000);

        let enhanced = enhance(&input, &normalizing()).expect("enhance");

        let expected = (SECONDS * 1000.0) as i64;
        let drift = (enhanced.duration_ms - expected).abs();
        assert!(
            drift <= 50,
            "{}ms for a {expected}ms Call",
            enhanced.duration_ms
        );
    }

    /// **Spec US 33, stated as the listener experiences it.** A quiet Talkgroup
    /// and a loud one must come out at the same level — that is the whole
    /// complaint about scanning, and the reason loudness normalization is the
    /// stage ADR-0006 calls the proven win.
    ///
    /// Deliberately a *relative* assertion. "Output is near the target" could
    /// be satisfied by code that mirrors the gain formula; two inputs 20 dB
    /// apart landing together can only be satisfied by actually measuring and
    /// correcting each one.
    #[test]
    fn a_quiet_call_and_a_loud_one_come_out_at_the_same_level() {
        let quiet = wav(&tone(1000.0, 16_000, 0.02), 16_000);
        let loud = wav(&tone(1000.0, 16_000, 0.2), 16_000);

        let quiet = enhance(&quiet, &normalizing()).expect("enhance the quiet Call");
        let loud = enhance(&loud, &normalizing()).expect("enhance the loud Call");

        let difference = (loudness_of(&quiet.bytes) - loudness_of(&loud.bytes)).abs();
        assert!(
            difference < 1.0,
            "20 dB apart going in, {difference:.2} LU apart coming out"
        );
    }

    proptest! {
        /// A worker holding a queue must survive anything a recorder can put on
        /// the wire. Whatever these bytes are, enhancement either produces
        /// audio or names a reason — it never unwinds, and it never hangs on to
        /// a panic that would take the queue down with it.
        #[test]
        fn arbitrary_bytes_are_refused_rather_than_fatal(
            bytes in proptest::collection::vec(any::<u8>(), 0..4096),
        ) {
            // Not merely "does not panic": whichever way it goes must be
            // *usable* — audio a browser can play, or a slug an operator can
            // grep. A silent `Ok` carrying nothing would pass a panic-only
            // check and leave a Call pointing at an empty object.
            match enhance(&bytes, &normalizing()) {
                Ok(enhanced) => {
                    prop_assert!(!enhanced.bytes.is_empty());
                    prop_assert_eq!(enhanced.mime, "audio/wav");
                }
                Err(error) => prop_assert!(!error.reason().is_empty()),
            }
        }

        /// The three invariants every enhanced Call has, whatever went in:
        /// it is at the storage rate, no sample is past the ceiling, and no
        /// sample is NaN. The last matters most — a NaN reaches a listener as
        /// a burst of noise, and nothing downstream would catch it.
        #[test]
        fn every_enhanced_call_is_finite_levelled_and_at_the_storage_rate(
            raw in proptest::collection::vec(-1.0f32..1.0, 4_000..16_000),
            rate in prop_oneof![Just(8_000u32), Just(16_000), Just(22_050), Just(44_100)],
        ) {
            let enhanced = enhance(&wav(&raw, rate), &normalizing());

            // Noise this short can measure as un-gateable; that is a refusal,
            // not a failure, and the invariants are about what *is* produced.
            if let Ok(enhanced) = enhanced {
                let (samples, out_rate) = read_wav(&enhanced.bytes);
                prop_assert_eq!(out_rate, OUTPUT_RATE);
                for sample in samples {
                    prop_assert!(sample.is_finite(), "NaN reaches a listener as noise");
                    prop_assert!(sample.abs() <= ceiling() + 0.001, "{} past the ceiling", sample);
                }
            }
        }
    }
}
