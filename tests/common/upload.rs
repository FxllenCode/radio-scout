//! A synthetic Call on the wire.
//!
//! Ingest is a multipart contract with recorders (ADR-0001), so a test drives it
//! by building the same form a recorder sends — including the malformed ones. A
//! valid upload is [`CallUpload::new`]; every test that cares about one field
//! says only that field, and everything else stays a complete, valid Call.
//!
//! Fields are an ordered list rather than named struct members because the wire
//! shape *is* a list of named parts: rdio's dialect and Trunk Recorder's native
//! `meta` JSON are the same multipart body with different field names, aliases
//! (`patched_talkgroups`, `audioType`) are just more names, and part order is
//! itself load-bearing — a recorder that sends the audio part first makes its
//! metadata fields win over the part's own headers.

/// A multipart Call upload under construction.
#[derive(Debug, Clone)]
pub struct CallUpload {
    fields: Vec<(String, String)>,
    audio: Option<AudioPart>,
    audio_first: bool,
}

#[derive(Debug, Clone)]
struct AudioPart {
    bytes: Vec<u8>,
    file_name: Option<String>,
    mime: Option<String>,
}

impl AudioPart {
    /// What a recorder attaches by default: a named WAV part.
    fn wav(bytes: &[u8]) -> Self {
        AudioPart {
            bytes: bytes.to_vec(),
            file_name: Some("a.wav".into()),
            mime: Some("audio/x-wav".into()),
        }
    }

    /// What Trunk Recorder's native uploader attaches.
    fn m4a(bytes: &[u8]) -> Self {
        AudioPart {
            bytes: bytes.to_vec(),
            file_name: Some("call.m4a".into()),
            mime: Some("audio/mp4".into()),
        }
    }

    fn into_part(self) -> reqwest::multipart::Part {
        let mut part = reqwest::multipart::Part::bytes(self.bytes);
        if let Some(file_name) = self.file_name {
            part = part.file_name(file_name);
        }
        if let Some(mime) = self.mime {
            part = part.mime_str(&mime).expect("audio mime");
        }
        part
    }
}

impl CallUpload {
    /// The audio bytes a default upload carries.
    pub const DEFAULT_AUDIO: &'static [u8] = b"audio-bytes";
    /// The API key a default upload authenticates with — register it with
    /// `TestApp::with_key("k")`.
    const DEFAULT_KEY: &'static str = "k";

    /// A complete, valid rdio-dialect upload: key `k`, System 11, Talkgroup
    /// 54241, timestamp 1000, and a small named WAV part.
    pub fn new() -> Self {
        CallUpload {
            fields: vec![
                ("key".into(), Self::DEFAULT_KEY.into()),
                ("system".into(), "11".into()),
                ("talkgroup".into(), "54241".into()),
                ("timestamp".into(), "1000".into()),
            ],
            audio: Some(AudioPart::wav(Self::DEFAULT_AUDIO)),
            audio_first: false,
        }
    }

    /// A Trunk-Recorder-native upload (#6): the whole call description as one
    /// JSON `meta` part, plus the key and an M4A audio part.
    pub fn tr(meta_json: &str) -> Self {
        CallUpload {
            fields: vec![
                ("key".into(), Self::DEFAULT_KEY.into()),
                ("meta".into(), meta_json.to_string()),
            ],
            audio: Some(AudioPart::m4a(Self::DEFAULT_AUDIO)),
            audio_first: false,
        }
    }

    /// Set a text field, replacing it **in place** if it is already there, so an
    /// override never silently reorders the body.
    pub fn set(mut self, name: &str, value: impl std::fmt::Display) -> Self {
        let value = value.to_string();
        match self.fields.iter_mut().find(|(field, _)| field == name) {
            Some((_, existing)) => *existing = value,
            None => self.fields.push((name.to_string(), value)),
        }
        self
    }

    /// Drop a text field — how the incomplete-upload cases are built.
    pub fn remove(mut self, name: &str) -> Self {
        self.fields.retain(|(field, _)| field != name);
        self
    }

    /// The API key this upload authenticates with.
    pub fn key(self, key: &str) -> Self {
        self.set("key", key)
    }

    /// The System Ref (rdio `system`).
    pub fn system(self, system_ref: i64) -> Self {
        self.set("system", system_ref)
    }

    /// The Talkgroup Ref (rdio `talkgroup`).
    pub fn talkgroup(self, talkgroup_ref: i64) -> Self {
        self.set("talkgroup", talkgroup_ref)
    }

    /// When the Call happened, in unix milliseconds (rdio `timestamp`).
    pub fn at(self, call_at_ms: i64) -> Self {
        self.set("timestamp", call_at_ms)
    }

    /// Replace the audio bytes, keeping whatever filename and MIME the part
    /// already carries.
    pub fn audio(mut self, bytes: &[u8]) -> Self {
        match self.audio.as_mut() {
            Some(audio) => audio.bytes = bytes.to_vec(),
            None => self.audio = Some(AudioPart::wav(bytes)),
        }
        self
    }

    /// An audio part with an explicit filename and MIME type.
    pub fn audio_named(mut self, bytes: &[u8], file_name: &str, mime: &str) -> Self {
        self.audio = Some(AudioPart {
            bytes: bytes.to_vec(),
            file_name: Some(file_name.into()),
            mime: Some(mime.into()),
        });
        self
    }

    /// An audio part carrying neither filename nor MIME, so the `audioName` /
    /// `audioType` form fields are the only source of both.
    pub fn audio_unlabelled(mut self, bytes: &[u8]) -> Self {
        self.audio = Some(AudioPart {
            bytes: bytes.to_vec(),
            file_name: None,
            mime: None,
        });
        self
    }

    /// No audio part at all — a recorder that uploaded metadata and nothing to
    /// listen to.
    pub fn no_audio(mut self) -> Self {
        self.audio = None;
        self
    }

    /// Put the audio part before the text fields, the way Trunk Recorder does —
    /// which is what makes its trailing `audioType`/`audioName` fields win over
    /// the part's own headers.
    pub fn audio_first(mut self) -> Self {
        self.audio_first = true;
        self
    }

    /// Render the multipart body.
    pub(crate) fn into_form(self) -> reqwest::multipart::Form {
        let CallUpload {
            fields,
            audio,
            audio_first,
        } = self;
        let audio = audio.map(AudioPart::into_part);
        let (leading, trailing) = if audio_first {
            (audio, None)
        } else {
            (None, audio)
        };

        let mut form = reqwest::multipart::Form::new();
        if let Some(part) = leading {
            form = form.part("audio", part);
        }
        for (name, value) in fields {
            form = form.text(name, value);
        }
        if let Some(part) = trailing {
            form = form.part("audio", part);
        }
        form
    }
}

impl Default for CallUpload {
    fn default() -> Self {
        Self::new()
    }
}
