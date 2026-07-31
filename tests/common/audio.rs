//! Real audio for the tests that need a Call to *be* something.
//!
//! Most of the suite uploads [`CallUpload::DEFAULT_AUDIO`] — eleven bytes that
//! are deliberately not audio, because ingest neither decodes nor cares. Two
//! things do care: the enhancement pipeline (#20), which decodes, and the
//! duration probe (#42), which reads a header. Both need a file that is really
//! a file.
//!
//! [`wav`] is hand-rolled rather than written with the crate the enhancement
//! pipeline encodes with — a test sharing its writer with the code under test
//! cannot tell a wrong header from a consistently wrong one.

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
