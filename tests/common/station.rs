//! Listening to a **Station stream** (#74) the way a player does.
//!
//! A stream never ends, so nothing here waits for a body: [`Tuned`] reads what
//! has arrived so far and keeps reading until what it has heard says something
//! — a title announced, an end reached — or a budget runs out. That is a wait
//! on the stream's own bytes, not a sleep: the budget only ever bounds how long
//! a broken stream takes to fail.
//!
//! It asks for ICY metadata (`Icy-MetaData: 1`) the way VLC and a Sonos do, and
//! strips it back out at the interval the response stated — which is itself
//! the first thing it checks, since a player that cannot find the blocks plays
//! them as noise.

use std::time::Duration;

/// A Station stream, being listened to.
pub struct Tuned {
    response: reqwest::Response,
    /// The `icy-metaint` the response stated, if it is announcing.
    metaint: Option<usize>,
    received: Vec<u8>,
    ended: bool,
}

/// What a stream has carried so far: the audio, and every title it announced.
#[derive(Debug, Default)]
pub struct Heard {
    pub audio: Vec<u8>,
    /// Every title a block carried, in order. A block with nothing new to say
    /// is left out, so this reads as the sequence of what went on the air.
    pub titles: Vec<String>,
}

impl Heard {
    /// The audio, through the process's own decoder — which is not the
    /// stream's encoder, so a stream that only *looks* like MP3 fails here.
    pub fn decoded(&self) -> (Vec<f32>, u32) {
        radio_scout::enhance::decode(&self.audio).expect("the stream decodes as audio")
    }
}

impl Tuned {
    pub fn new(response: reqwest::Response) -> Self {
        let metaint = response.headers().get("icy-metaint").map(|value| {
            value
                .to_str()
                .expect("ASCII")
                .parse()
                .expect("a byte count")
        });
        Tuned {
            response,
            metaint,
            received: Vec::new(),
            ended: false,
        }
    }

    /// Read whatever arrives within `budget`, and say whether the stream is
    /// still going.
    pub async fn listen_for(&mut self, budget: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + budget;
        while !self.ended {
            match tokio::time::timeout_at(deadline, self.response.chunk()).await {
                Ok(Ok(Some(chunk))) => self.received.extend(chunk),
                Ok(_) => self.ended = true,
                Err(_) => break,
            }
        }
        !self.ended
    }

    /// Read until what has been heard satisfies `until`, failing the test —
    /// naming `what` — if `budget` passes first.
    pub async fn listen_until(
        &mut self,
        budget: Duration,
        what: &str,
        until: impl Fn(&Heard) -> bool,
    ) -> Heard {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            let heard = self.heard();
            if until(&heard) {
                return heard;
            }
            assert!(
                !self.ended,
                "the stream ended before {what}: {:?}",
                heard.titles
            );
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(!left.is_zero(), "never heard {what}: {:?}", heard.titles);
            match tokio::time::timeout(left, self.response.chunk()).await {
                Ok(Ok(Some(chunk))) => self.received.extend(chunk),
                Ok(_) => self.ended = true,
                Err(_) => {}
            }
        }
    }

    /// Whether the server ended the stream within `budget`.
    pub async fn ends_within(&mut self, budget: Duration) -> bool {
        self.listen_for(budget).await;
        self.ended
    }

    /// What has arrived, with the metadata taken back out.
    pub fn heard(&self) -> Heard {
        let Some(metaint) = self.metaint else {
            return Heard {
                audio: self.received.clone(),
                titles: Vec::new(),
            };
        };
        let mut heard = Heard::default();
        let mut rest = &self.received[..];
        while rest.len() > metaint {
            heard.audio.extend(&rest[..metaint]);
            let length = rest[metaint] as usize * 16;
            let Some(block) = rest.get(metaint + 1..metaint + 1 + length) else {
                // The block is still arriving; what follows it has not.
                return heard;
            };
            if length > 0 {
                let text = String::from_utf8(block.to_vec()).expect("a UTF-8 block");
                let title = text
                    .trim_end_matches('\0')
                    .strip_prefix("StreamTitle='")
                    .and_then(|title| title.strip_suffix("';"))
                    .unwrap_or_else(|| panic!("not a StreamTitle block: {text:?}"));
                heard.titles.push(title.to_string());
            }
            rest = &rest[metaint + 1 + length..];
        }
        heard.audio.extend(rest);
        heard
    }
}
