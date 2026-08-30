//! Real audio for the tests that need a Call to *be* something.
//!
//! Most of the suite uploads [`CallUpload::DEFAULT_AUDIO`] — eleven bytes that
//! are deliberately not audio, because ingest neither decodes nor cares. Three
//! things do care: the enhancement pipeline (#20), which decodes, the duration
//! probe (#42), which reads a header, and **Mining** (#48), which reads the
//! metadata a Recorder wrote *into* the file. All three need a file that is
//! really a file.
//!
//! [`wav`] and [`SdrTrunkMp3`] are hand-rolled rather than written with a
//! library — a test sharing its writer with the code under test cannot tell a
//! wrong header from a consistently wrong one. `src/audio_meta.rs`'s own unit
//! tests hand-roll an ID3 tag too, and the duplication is deliberate: that one
//! is about whether a frame *decodes*, this one is about what SDRTrunk *writes*,
//! and a shared fixture would let a change to either quietly satisfy both.

/// A mono 16-bit PCM WAV of `samples` at `rate`.
pub fn wav(samples: &[f32], rate: u32) -> Vec<u8> {
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

/// A silent WAV that really is `millis` long, at the 8 kHz a P25 recorder
/// produces. What a test asserting a *duration* uploads.
pub fn silence_ms(millis: i64) -> Vec<u8> {
    const RATE: u32 = 8_000;
    let samples = (millis as usize * RATE as usize) / 1000;
    wav(&vec![0.0; samples], RATE)
}

/// **A page-out, as a scanner records one** (#55) — a Quick Call II sequence at
/// two real tone-set frequencies, followed by the dispatcher.
///
/// Not a signal generator's output, deliberately. Every departure from a clean
/// sinusoid here is one a real recording has and a synthetic fixture would miss:
///
/// - **Attack and decay** on each tone, because a console keys through an audio
///   path with a rise time, and a hard edge is a click whose energy smears
///   across the whole band.
/// - **Hiss**, at a signal-to-noise ratio a scanner on a rooftop antenna
///   actually gets — deterministic, so a failure is a failure and not a Tuesday.
/// - **Voice after the tones**, harmonically rich and pitch-moving, which is the
///   part of a real page-out most likely to be mistaken for a third tone.
///
/// What it is honestly *not* is a recording of a real dispatch: there is none in
/// this repository, and there should not be — an agency's traffic is not ours to
/// commit. The near-miss negatives beside it in `src/tone/detect.rs` are what
/// stand in for the false-positive half of that testing.
pub fn page_out(a_hz: f64, b_hz: f64) -> Vec<u8> {
    let mut samples = tone(a_hz, 1_000);
    samples.extend(tone(b_hz, 3_000));
    samples.extend(voice(2_000));
    for (n, sample) in samples.iter_mut().enumerate() {
        *sample = (*sample + hiss(n)).clamp(-1.0, 1.0);
    }
    wav(&samples, PAGE_RATE)
}

/// The rate a P25 recorder writes, which is what a page-out arrives at.
pub const PAGE_RATE: u32 = 8_000;

/// A Call with no page in it: the dispatcher and nothing else, at the same
/// length and level, so a negative is a difference of *content* rather than of
/// anything else the detector could be reacting to.
pub fn routine_traffic() -> Vec<u8> {
    let mut samples = voice(6_000);
    for (n, sample) in samples.iter_mut().enumerate() {
        *sample = (*sample + hiss(n)).clamp(-1.0, 1.0);
    }
    wav(&samples, PAGE_RATE)
}

/// **A Trunk Recorder call file, as one really arrives** (#59) — two keyups
/// with the hang time between them, and the squelch tail after the last word.
///
/// This is the shape Catch-up exists for. A recorder's call file spans the whole
/// grant, so what a Listener draining a backlog sits through is not only the
/// talking: it is the seconds between one unit finishing and the next keying,
/// and the tail after the last one lets go.
///
/// The gaps are near-silence rather than digital zero, because a real recorder's
/// gap has a floor — and a detector that only found exact zeroes would find
/// nothing in any Call a radio ever produced.
pub fn two_keyups() -> Vec<u8> {
    let samples = [
        keyed(0, 1_500),
        hang(3_000),
        keyed(1_500, 1_500),
        hang(2_000),
    ]
    .concat();
    wav(&samples, PAGE_RATE)
}

/// Somebody talking, with the hiss a rooftop antenna gets. `from` keeps the
/// noise walking across the whole Call rather than repeating per keyup.
fn keyed(from_ms: usize, ms: usize) -> Vec<f32> {
    let from = from_ms * PAGE_RATE as usize / 1000;
    voice(ms)
        .into_iter()
        .enumerate()
        .map(|(n, sample)| (sample + hiss(from + n)).clamp(-1.0, 1.0))
        .collect()
}

/// The channel between keyups: a floor, not digital zero.
fn hang(ms: usize) -> Vec<f32> {
    (0..ms * PAGE_RATE as usize / 1000)
        .map(|n| hiss(n) * 0.02)
        .collect()
}

/// Where [`two_keyups`]'s gaps are, in milliseconds — what a test asserts
/// against, rather than repeating the arithmetic of the fixture it is reading.
pub const TWO_KEYUPS_GAPS: [(i64, i64); 2] = [(1_500, 4_500), (6_000, 8_000)];

fn tone(hz: f64, ms: usize) -> Vec<f32> {
    let len = ms * PAGE_RATE as usize / 1000;
    let ramp = PAGE_RATE as usize / 200;
    (0..len)
        .map(|n| {
            let envelope = (n.min(len - n - 1) as f32 / ramp as f32).min(1.0);
            let phase = std::f64::consts::TAU * hz * n as f64 / PAGE_RATE as f64;
            0.5 * envelope * phase.sin() as f32
        })
        .collect()
}

/// Speech, near enough: a harmonic stack whose pitch moves, which is what a
/// held paging tone never does.
fn voice(ms: usize) -> Vec<f32> {
    (0..ms * PAGE_RATE as usize / 1000)
        .map(|n| {
            let t = n as f64 / PAGE_RATE as f64;
            let f0 = 130.0 * (1.0 + 0.25 * (std::f64::consts::TAU * 3.0 * t).sin());
            (1..=12)
                .map(|h| (std::f64::consts::TAU * f0 * h as f64 * t).sin() / h as f64)
                .sum::<f64>() as f32
                * 0.15
        })
        .collect()
}

/// Deterministic hiss at roughly 20 dB below the tones — a seeded LCG, so this
/// fixture is the same bytes on every machine and every run.
fn hiss(n: usize) -> f32 {
    let mut state = (n as u64).wrapping_mul(6364136223846793005).wrapping_add(1);
    state ^= state >> 33;
    0.05 * ((state >> 40) as f32 / 8388608.0 - 1.0)
}

/// An MP3 shaped the way SDRTrunk uploads one (#48).
///
/// `AudioSegmentRecorder.recordMP3` writes an ID3v2.4 tag and *then* the MPEG
/// frames, and `RdioScannerBroadcaster` posts that file's bytes verbatim — so
/// this is what actually arrives on the ingest path from an SDRTrunk instance.
///
/// The default carries SDRTrunk's own [`Self::COMPOSER`] and nothing else, so a
/// test says only the frame it is about.
#[derive(Debug, Clone)]
pub struct SdrTrunkMp3 {
    frames: Vec<(String, String)>,
    mpeg_frames: usize,
}

impl SdrTrunkMp3 {
    /// How SDRTrunk stamps its own name into `TCOM`
    /// (`SystemProperties.getApplicationName()`), which is the only thing that
    /// tells its uploads from every other MP3 in an Archive.
    pub const COMPOSER: &'static str = "sdrtrunk v0.6.1";
    /// The length of the default stream: 40 Layer III frames at 44.1 kHz.
    pub const DURATION_MS: i64 = 1044;

    /// An SDRTrunk MP3 that says who wrote it and nothing more — the
    /// zero-configuration install, where no radio and no tower has a name.
    pub fn new() -> Self {
        SdrTrunkMp3 {
            frames: vec![("TCOM".into(), Self::COMPOSER.into())],
            mpeg_frames: 40,
        }
    }

    /// `TPE1`: the FROM radio, then each alias SDRTrunk had configured for it,
    /// space-separated — `1234567 Engine 1`.
    pub fn radio(self, artist: impl Into<String>) -> Self {
        self.frame("TPE1", artist)
    }

    /// `COMM`: SDRTrunk's `Key:Value;` run, holding the tower, the decoder and
    /// the frequency among others.
    pub fn comment(self, comment: impl Into<String>) -> Self {
        self.frame("COMM", comment)
    }

    /// Any frame, including ones SDRTrunk would never write — how a test says
    /// "this MP3 came from something else".
    pub fn frame(mut self, id: &str, value: impl Into<String>) -> Self {
        self.frames.retain(|(name, _)| name != id);
        self.frames.push((id.to_string(), value.into()));
        self
    }

    /// Drop the frame that names SDRTrunk, leaving an MP3 with a tag that is
    /// nobody's dialect.
    pub fn anonymous(mut self) -> Self {
        self.frames.retain(|(name, _)| name != "TCOM");
        self
    }

    /// How many MPEG frames follow the tag — 1152 samples at 44.1 kHz each, so
    /// this is how a test gives two copies of a transmission different lengths.
    pub fn mpeg_frames(mut self, frames: usize) -> Self {
        self.mpeg_frames = frames;
        self
    }

    /// The file.
    pub fn bytes(&self) -> Vec<u8> {
        let body: Vec<u8> = self
            .frames
            .iter()
            .map(|(id, value)| id3_frame(id, value))
            .collect::<Vec<_>>()
            .concat();
        let mut out = b"ID3".to_vec();
        out.extend([0x04, 0x00, 0x00]); // v2.4, revision 0, no flags
        out.extend(synchsafe(body.len() as u32));
        out.extend(body);
        out.extend(mpeg(self.mpeg_frames));
        out
    }
}

impl Default for SdrTrunkMp3 {
    fn default() -> Self {
        SdrTrunkMp3::new()
    }
}

/// ID3v2's 28-bit size: four bytes of seven bits, so a length can never contain
/// a byte that looks like an MPEG sync word.
fn synchsafe(n: u32) -> [u8; 4] {
    [
        ((n >> 21) & 0x7f) as u8,
        ((n >> 14) & 0x7f) as u8,
        ((n >> 7) & 0x7f) as u8,
        (n & 0x7f) as u8,
    ]
}

/// One ID3v2.4 frame. `COMM` carries a language and a short description ahead
/// of its text where a text frame carries only an encoding byte, which is the
/// one structural difference mp3agic writes and this has to match.
fn id3_frame(id: &str, value: &str) -> Vec<u8> {
    let mut body = vec![0x03]; // UTF-8
    if id == "COMM" {
        body.extend(b"eng");
        body.push(0); // an empty description
    }
    body.extend(value.as_bytes());

    let mut out = id.as_bytes().to_vec();
    out.extend(synchsafe(body.len() as u32));
    out.extend([0u8, 0u8]);
    out.extend(body);
    out
}

/// A constant-bitrate MPEG-1 Layer III stream — mono, 44.1 kHz, 128 kbps, and
/// deliberately no Xing header, which is the shape a recorder writing a live
/// stream produces. Frame size is `144 * 128000 / 44100 = 417` bytes.
fn mpeg(frames: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..frames {
        out.extend([0xFF, 0xFB, 0x90, 0xC0]);
        out.extend(std::iter::repeat_n(0u8, 417 - 4));
    }
    out
}
