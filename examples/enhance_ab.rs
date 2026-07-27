//! A/B renderer for the enhancement listening test (#20, ADR-0006).
//!
//! ```text
//! cargo run --example enhance_ab                          # ./radio-scout-data/audio
//! cargo run --example enhance_ab -- --from /path/to/audio --out /tmp/ab --limit 8
//! ```
//!
//! ADR-0006 calls loudness normalization the proven win and RNNoise **a
//! hypothesis** on already-vocoder-decoded P25/DMR audio — a codec's output is
//! not the noisy microphone signal RNNoise was trained on, and it may chew on
//! vocoder artefacts instead of removing hiss. No test can settle that. Ears
//! can.
//!
//! So this takes real Calls out of a real archive and writes three files for
//! each — the original, `normalize`, and `denoise` — for someone to listen to
//! back to back. Whatever they conclude goes in ADR-0006 and decides whether
//! `mode = "denoise"` stays opt-in.
//!
//! It also prints what each mode costs in bytes, which is the other half of the
//! decision on a Pi with an SD card.

// This is a hand-run CLI whose stdout *is* its product: the comparison table is
// the thing you read while listening. ADR-0011's print lint is about the
// application's own output, and `examples/feed.rs` carries the same exemption.
#![allow(clippy::print_stdout)]

use std::path::{Path, PathBuf};

use radio_scout::enhance::{EnhancementConfig, Mode, enhance};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let from = PathBuf::from(flag(&args, "--from").unwrap_or("./radio-scout-data/audio".into()));
    let out = PathBuf::from(flag(&args, "--out").unwrap_or("./enhance-ab".into()));
    let limit: usize = flag(&args, "--limit")
        .and_then(|n| n.parse().ok())
        .unwrap_or(6);

    let mut sources = audio_files(&from);
    sources.sort();
    if sources.is_empty() {
        println!("no audio under {}", from.display());
        println!("point --from at an archive, e.g. ./radio-scout-data/audio");
        return;
    }
    // Largest first: the longest Calls carry the most speech to judge by, and a
    // two-second acknowledgement tells you very little either way.
    sources.sort_by_key(|path| std::cmp::Reverse(std::fs::metadata(path).map_or(0, |m| m.len())));
    sources.truncate(limit);

    std::fs::create_dir_all(&out).expect("create the output directory");
    println!("rendering {} Calls into {}\n", sources.len(), out.display());
    println!(
        "{:<26} {:>10} {:>12} {:>12}",
        "call", "original", "normalize", "denoise"
    );
    println!("{}", "-".repeat(64));

    for source in &sources {
        let Ok(audio) = std::fs::read(source) else {
            continue;
        };
        let stem = source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("call")
            .chars()
            .take(12)
            .collect::<String>();

        // The original, copied under a matching name so the three sit together
        // in a file browser and play in order.
        let extension = source.extension().and_then(|e| e.to_str()).unwrap_or("bin");
        let _ = std::fs::write(out.join(format!("{stem}-0-original.{extension}")), &audio);

        let mut sizes = Vec::new();
        for (label, mode) in [
            ("1-normalize", Mode::Normalize),
            ("2-denoise", Mode::Denoise),
        ] {
            let config = EnhancementConfig {
                mode,
                ..EnhancementConfig::default()
            };
            match enhance(&audio, &config) {
                Ok(enhanced) => {
                    sizes.push(format!("{:>10}", human(enhanced.bytes.len() as u64)));
                    let name = format!("{stem}-{label}.{}", enhanced.extension);
                    let _ = std::fs::write(out.join(name), &enhanced.bytes);
                }
                Err(error) => sizes.push(format!("{error:>10}")),
            }
        }
        println!(
            "{:<26} {:>10} {} {}",
            stem,
            human(audio.len() as u64),
            sizes.first().map(String::as_str).unwrap_or(""),
            sizes.get(1).map(String::as_str).unwrap_or(""),
        );
    }

    println!("\nListen to each trio back to back. What to decide:");
    println!("  * does `denoise` sound clearer than `normalize`, or just duller?");
    println!("  * does it eat the quiet end of speech, or leave words intact?");
    println!("  * is anything worse than the original at all?");
    println!("\nThat verdict belongs in docs/adr/0006-*.md; denoise stays opt-in until it wins.");
}

/// `--name value` off the command line.
fn flag(args: &[String], name: &str) -> Option<String> {
    let at = args.iter().position(|arg| arg == name)?;
    args.get(at + 1).cloned()
}

/// Every audio file under `root`, recursively — an archive is sharded into
/// two-character directories (`blob::new_object_key`).
fn audio_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(audio_files(&path));
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("wav" | "m4a" | "mp3" | "mp4" | "aac")
        ) {
            found.push(path);
        }
    }
    found
}

fn human(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}
