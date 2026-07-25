//! Synthetic recorder — feeds a running Radio-Scout the way Trunk Recorder
//! would, so the live-feed loop can be exercised without waiting on real RF.
//!
//! ```text
//! cargo run --example feed                       # key from .env
//! cargo run --example feed -- --burst 5          # fill the listening queue
//! cargo run --example feed -- --patches 54241:54242
//! ```
//!
//! The key comes from `RADIO_SCOUT_API_KEY` (environment or `.env`, the same
//! one the binary registers on boot), so a live test needs nothing copy-pasted;
//! `--key` overrides it.
//!
//! It posts to `POST /api/call-upload` — the same generic rdio endpoint Trunk
//! Recorder's `rdioscanner_uploader` plugin actually uses (verified in
//! `trunk-recorder/plugins/rdioscanner_uploader/rdioscanner_uploader.cc`, which
//! appends `/api/call-upload` to the configured `server`) — with the same field
//! names, so what this exercises is the real ingest path.
//!
//! **The audio is a real WAV tone**, pitched by Talkgroup Ref, not a stub: a
//! browser has to decode and play it for the player, the waveform and the
//! Media Session to mean anything, and a distinct pitch per Talkgroup makes a
//! wrong-Call bug audible.
//!
//! See `docs/agents/live-testing.md`. This is a development tool, not shipped
//! code — its correctness is proven by the loop it enables, in a browser.

use std::time::Duration;

/// Sample rate. 8 kHz mono is what a trunked-radio recorder produces.
const SAMPLE_RATE: u32 = 8_000;

/// How far apart burst Calls are timestamped. Comfortably past the ±500 ms
/// dedup window in `IngestConfig`, so a burst lands instead of being rejected.
const DEDUP_CLEARANCE_MS: i64 = 2_000;

struct Options {
    server: String,
    key: String,
    system: i64,
    system_label: String,
    talkgroups: Vec<i64>,
    /// Gap between Calls. Zero for a burst.
    interval: Duration,
    /// How many Calls to send; `None` runs until interrupted.
    count: Option<usize>,
    /// Seconds of audio per Call.
    seconds: f32,
    /// `a:b` — every Call on `a` is also patched to `b`.
    patches: Option<(i64, i64)>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            server: "http://127.0.0.1:3000".into(),
            key: String::new(),
            system: 411,
            system_label: "Fulton County".into(),
            talkgroups: vec![54241, 54242, 1001],
            interval: Duration::from_secs(4),
            count: None,
            seconds: 3.0,
            patches: None,
        }
    }
}

/// A plausible name and category per Talkgroup, so the display has something
/// real to render (auto-populate, #8, stores whatever the recorder sends).
/// Keyed by *position* in the configured list, so a feed of three Talkgroups
/// always shows three different names — telling them apart is the point.
fn talkgroup_labels(slot: usize) -> (&'static str, &'static str, &'static str) {
    const NAMES: [(&str, &str, &str); 6] = [
        ("FD Dispatch", "Fire Dispatch", "Fire"),
        ("PD Dispatch", "Law Dispatch", "Law"),
        ("EMS Ops", "EMS Dispatch", "EMS"),
        ("FD Tac 2", "Fire Tac", "Fire"),
        ("PD Traffic", "Law Talk", "Law"),
        ("Public Works", "Public Works", "Services"),
    ];
    NAMES[slot % NAMES.len()]
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    println!(
        "feeding {}  (system {} · talkgroups {:?})",
        options.server, options.system, options.talkgroups
    );

    let client = reqwest::Client::new();
    let mut sent = 0usize;
    loop {
        for slot in 0..options.talkgroups.len() {
            if options.count.is_some_and(|limit| sent >= limit) {
                return Ok(());
            }
            send_call(&client, &options, slot, sent).await?;
            sent += 1;
            if !options.interval.is_zero() {
                tokio::time::sleep(options.interval).await;
            }
        }
    }
}

async fn send_call(
    client: &reqwest::Client,
    options: &Options,
    slot: usize,
    index: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let talkgroup = options.talkgroups[slot];
    let (label, tag, group) = talkgroup_labels(slot);
    let pitch = pitch_for(talkgroup);
    let audio = wav_tone(pitch, options.seconds);
    let bytes = audio.len();

    // Unix millis. `timestamp` is what rdio's parser reads; the recorder sends
    // seconds *or* millis and our ingest normalizes (see src/ingest.rs).
    //
    // Staggered by index: dedup rejects a second Call on the same
    // System+Talkgroup within ±500 ms of the *supplied* time (ADR-0001), so a
    // burst sent in one breath would be thrown away as duplicates. Spacing them
    // like real transmissions is what makes `--burst` fill a queue.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64
        + index as i64 * DEDUP_CLEARANCE_MS;

    let mut form = reqwest::multipart::Form::new()
        .text("key", options.key.clone())
        .text("system", options.system.to_string())
        .text("systemLabel", options.system_label.clone())
        .text("talkgroup", talkgroup.to_string())
        .text("talkgroupLabel", label.to_string())
        .text("talkgroupTag", tag.to_string())
        .text("talkgroupGroup", group.to_string())
        // A plausible 800 MHz trunked channel (25 kHz steps within the band),
        // and a unit that changes per Call so the Unit ID isn't always the same.
        .text(
            "frequency",
            (853_412_500 + (slot as i64 % 20) * 25_000).to_string(),
        )
        .text("source", (1_600_000 + index as i64).to_string())
        .text("timestamp", now_ms.to_string())
        .part(
            "audio",
            reqwest::multipart::Part::bytes(audio)
                .file_name(format!("{}-{talkgroup}-{now_ms}.wav", options.system))
                .mime_str("audio/x-wav")?,
        );
    if let Some((from, to)) = options.patches
        && from == talkgroup
    {
        form = form.text("patches", to.string_list());
    }

    let response = client
        .post(format!("{}/api/call-upload", options.server))
        .multipart(form)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    println!(
        "  → sys {} tg {talkgroup} {label:<14} {:.1}s {pitch:>4} Hz  {bytes:>6} B  {} {}",
        options.system,
        options.seconds,
        status.as_u16(),
        body.trim(),
    );
    Ok(())
}

/// Pitch for a Talkgroup: a fixed spread of musical-ish tones so two Talkgroups
/// are told apart by ear, and the same Talkgroup always sounds the same.
fn pitch_for(talkgroup: i64) -> u32 {
    const TONES: [u32; 6] = [440, 523, 587, 659, 784, 880];
    TONES[(talkgroup.unsigned_abs() as usize) % TONES.len()]
}

/// A mono 16-bit PCM WAV of a `pitch` Hz sine, `seconds` long, with a short
/// fade at each end so it doesn't click. Hand-rolled because the alternative is
/// a codec dependency in the dev tree for something this small.
fn wav_tone(pitch: u32, seconds: f32) -> Vec<u8> {
    let samples = (SAMPLE_RATE as f32 * seconds) as u32;
    let data_len = samples * 2; // 16-bit mono
    let mut wav = Vec::with_capacity(44 + data_len as usize);

    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());

    let fade = (SAMPLE_RATE / 50).max(1); // 20 ms
    for sample in 0..samples {
        let phase = sample as f32 / SAMPLE_RATE as f32 * pitch as f32 * std::f32::consts::TAU;
        let envelope = (sample.min(samples.saturating_sub(sample)).min(fade) as f32 / fade as f32)
            .clamp(0.0, 1.0);
        let value = (phase.sin() * envelope * 8_000.0) as i16;
        wav.extend_from_slice(&value.to_le_bytes());
    }
    wav
}

trait StringList {
    fn string_list(&self) -> String;
}

impl StringList for i64 {
    /// rdio's `patches` field is a JSON array of Talkgroup Refs.
    fn string_list(&self) -> String {
        format!("[{self}]")
    }
}

const USAGE: &str = "\
usage: cargo run --example feed -- [options]

  --server <URL>        default http://127.0.0.1:3000 (honors RADIO_SCOUT_PORT)
  --key <KEY>           ingest API key; defaults to RADIO_SCOUT_API_KEY / .env
  --system <REF>        system Ref, default 411
  --talkgroups <A,B,C>  default 54241,54242,1001
  --interval <SECONDS>  gap between Calls, default 4 (0 = as fast as possible)
  --burst <N>           send N Calls back to back and exit (--interval 0)
  --count <N>           send N Calls and exit
  --seconds <S>         audio per Call, default 3
  --patches <A:B>       Calls on A are patched to B";

fn parse_args() -> Result<Options, String> {
    // Same `.env` the binary reads, so a live test needs no copy-pasted key.
    let _ = dotenvy::dotenv();

    let mut options = Options::default();
    if let Ok(key) = std::env::var("RADIO_SCOUT_API_KEY") {
        options.key = key.trim().to_string();
    }
    if let Ok(port) = std::env::var("RADIO_SCOUT_PORT") {
        options.server = format!("http://127.0.0.1:{}", port.trim());
    }
    let mut args = std::env::args().skip(1);

    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--server" => options.server = value()?,
            "--key" => options.key = value()?,
            "--system" => options.system = value()?.parse().map_err(|_| "bad --system")?,
            "--system-label" => options.system_label = value()?,
            "--talkgroups" => {
                options.talkgroups = value()?
                    .split(',')
                    .map(|ref_| ref_.trim().parse().map_err(|_| "bad --talkgroups"))
                    .collect::<Result<_, _>>()?;
            }
            "--interval" => {
                let seconds: f32 = value()?.parse().map_err(|_| "bad --interval")?;
                options.interval = Duration::from_secs_f32(seconds);
            }
            "--burst" => {
                options.count = Some(value()?.parse().map_err(|_| "bad --burst")?);
                options.interval = Duration::ZERO;
            }
            "--count" => options.count = Some(value()?.parse().map_err(|_| "bad --count")?),
            "--seconds" => options.seconds = value()?.parse().map_err(|_| "bad --seconds")?,
            "--patches" => {
                let spec = value()?;
                let (from, to) = spec.split_once(':').ok_or("--patches wants A:B")?;
                options.patches = Some((
                    from.trim().parse().map_err(|_| "bad --patches")?,
                    to.trim().parse().map_err(|_| "bad --patches")?,
                ));
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }

    if options.key.is_empty() {
        return Err(
            "no API key: set RADIO_SCOUT_API_KEY in .env (see .env.example) or pass --key".into(),
        );
    }
    if options.talkgroups.is_empty() {
        return Err("--talkgroups needs at least one Ref".into());
    }
    Ok(options)
}
