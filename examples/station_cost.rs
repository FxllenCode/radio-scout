//! What one Station stream costs this machine per Call (#74).
//!
//! ```text
//! cargo run --release --example station_cost
//! cargo run --release --example station_cost -- --seconds 12 --rounds 100
//! ```
//!
//! `docs/operating.md` quotes a cost per Call, and a figure a reader cannot
//! reproduce is a figure nobody can check — least of all on the Pi it matters
//! on. So this prepares a Call the way a stream does
//! (`radio_scout::station::encode::prepare`: decode, resample to 16 kHz,
//! encode to MP3) and says how long it took, and how many times faster than
//! the Call plays that is.
//!
//! The Call is 8 kHz mono WAV — what Trunk Recorder writes — holding a voice-
//! band tone under a little noise, so the encoder has something to work at.
//! **Run it `--release`**: a debug build is two orders of magnitude slower and
//! says nothing about what an Operator's binary does.

// A hand-run measurement whose stdout *is* its product, `enhance_ab`'s
// exemption from ADR-0011's print lint.
#![allow(clippy::print_stdout)]

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seconds: usize = flag(&args, "--seconds").unwrap_or(5);
    let rounds: u32 = flag(&args, "--rounds").unwrap_or(200);

    let object = call(seconds);
    let frames = radio_scout::station::encode::prepare(&object).expect("the Call prepares");

    let started = Instant::now();
    for _ in 0..rounds {
        let _ = radio_scout::station::encode::prepare(&object);
    }
    let each = started.elapsed() / rounds;

    println!(
        "a {seconds} s Call: {each:?} to prepare, {:.0}x real time, {} frames ({} bytes) on the air",
        seconds as f64 / each.as_secs_f64(),
        frames.len(),
        frames.iter().map(|frame| frame.len()).sum::<usize>(),
    );
}

/// `seconds` of 8 kHz mono 16-bit WAV: a 700 Hz tone under deterministic noise.
fn call(seconds: usize) -> Vec<u8> {
    const RATE: u32 = 8_000;
    let mut noise: u32 = 0x2545_f491;
    let data: Vec<u8> = (0..seconds * RATE as usize)
        .map(|n| {
            noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let hiss = (noise >> 8) as f32 / (1 << 24) as f32 - 0.5;
            let voice = (n as f32 * 700.0 * std::f32::consts::TAU / RATE as f32).sin();
            ((0.3 * voice + 0.05 * hiss) * i16::MAX as f32) as i16
        })
        .flat_map(i16::to_le_bytes)
        .collect();
    let mut wav = Vec::with_capacity(44 + data.len());
    wav.extend(b"RIFF");
    wav.extend(((36 + data.len()) as u32).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(RATE.to_le_bytes());
    wav.extend((RATE * 2).to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend((data.len() as u32).to_le_bytes());
    wav.extend(data);
    wav
}

fn flag<T: std::str::FromStr>(args: &[String], name: &str) -> Option<T> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|at| args.get(at + 1))
        .and_then(|value| value.parse().ok())
}
