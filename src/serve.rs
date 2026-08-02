//! Serving a Call's audio back to a listener (ADR-0002, #31).
//!
//! Two backends, one route. The filesystem store **proxies** the bytes with HTTP
//! range support, which iOS's `<audio>` element requires; an S3-shaped store
//! instead **redirects** to a short-lived presigned URL, so the audio never
//! crosses the Pi at all.
//!
//! The decision is a value ([`Serve`]) rather than six branches that each build
//! their own response. That matters because the branches are exactly the ones
//! hardest to reach over a socket — a presigned redirect needs a real object
//! store, an unsatisfiable range needs a real object of a known size, and "the
//! row exists but the object is gone" needs a store made to lie — and because
//! the *cache* decision differs per branch in a way no status code shows: a Call
//! still queued for enhancement is one the worker is about to point at a
//! different object, so `immutable` would be a promise we are about to break.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::call::CallId;
use crate::db::repo;
use crate::failure::{Failure, Reason, Stage};

/// How long a client may keep a Call's audio. The bytes behind a settled Call
/// never change, so this is `immutable`; `private` keeps it out of shared
/// proxies, since listening will become access-scoped (ADR-0008). A week is long
/// enough for the client's next-Call prefetch (#14) and for re-listening within
/// a session, without pinning audio that retention has since pruned.
pub(crate) const AUDIO_CACHE_CONTROL: &str = "private, max-age=604800, immutable";

/// ...and how long it may keep audio that is **queued for enhancement** (#20).
///
/// `immutable` is a promise the bytes behind this URL will never change, and for
/// a pending Call that promise is exactly false: the worker is about to point
/// the row at a different object. A client that cached it in the window would
/// keep the un-levelled version for a week and never learn otherwise, so the
/// promise is withheld until there is nothing left to replace. Short rather than
/// `no-store`, because the Call still has to play now — and a range request
/// mid-playback should not re-fetch the whole object.
const PENDING_AUDIO_CACHE_CONTROL: &str = "private, max-age=30";

/// ...as a number, because the S3 backend's redirect has to take the smaller of
/// this and what its signature has left (#31).
const PENDING_AUDIO_MAX_AGE_SECS: u64 = 30;

/// How long a *redirect* to this Call's audio may be cached: the signature's
/// budget, but never past the point the object key itself may change (#31).
///
/// The two limits are unrelated and both bind. `signature_budget` is how long
/// the presigned URL stays usable — exceed it and the listener gets a 403. The
/// enhancement state is about the row: a pending Call is one the worker is about
/// to point at a *different object*, so a redirect cached for the signature's
/// full life would keep sending the listener to the un-levelled audio long after
/// the levelled version existed. A settled Call has no second limit, because the
/// key behind it never changes again.
fn redirect_max_age(enhancement: &str, signature_budget: u64) -> u64 {
    match is_pending(enhancement) {
        true => signature_budget.min(PENDING_AUDIO_MAX_AGE_SECS),
        false => signature_budget,
    }
}

/// The `Cache-Control` a Call's audio is served with, given its enhancement
/// state.
fn audio_cache_control(enhancement: &str) -> &'static str {
    match is_pending(enhancement) {
        true => PENDING_AUDIO_CACHE_CONTROL,
        false => AUDIO_CACHE_CONTROL,
    }
}

fn is_pending(enhancement: &str) -> bool {
    enhancement == crate::db::entities::call::EnhancementState::PENDING
}

/// What serving one Call's audio decided — the whole of it, as a value.
///
/// Three ways of answering, and [`plan`] also has three ways of refusing
/// ([`Reason::CallHasNoAudio`], [`Reason::AudioNotFound`],
/// [`Reason::RangeNotSatisfiable`]) — six outcomes, every one of them decided by
/// a pure function over facts a test can state, with no socket, no store and no
/// database. Before #92 the only way to assert that a pending Call's redirect is
/// capped at thirty seconds was to stand up an S3 backend and read a header off
/// a real 307.
#[derive(Debug, PartialEq, Eq)]
pub enum Serve {
    /// The store signs its own URLs: send the listener straight there.
    Redirect { url: String, max_age: u64 },
    /// Proxy the whole object.
    Whole {
        mime: String,
        cache_control: &'static str,
    },
    /// Proxy `[start, end]` of it, inclusive.
    Range {
        start: u64,
        end: u64,
        size: u64,
        mime: String,
        cache_control: &'static str,
    },
}

impl IntoResponse for Serve {
    fn into_response(self) -> Response {
        match self {
            Serve::Redirect { url, max_age } => (
                StatusCode::TEMPORARY_REDIRECT,
                [
                    (header::LOCATION, url),
                    (header::CACHE_CONTROL, format!("private, max-age={max_age}")),
                ],
            )
                .into_response(),
            // The bytes are attached by [`audio`], which is the only thing that
            // can fetch them; this renders everything else about the answer.
            Serve::Whole {
                mime,
                cache_control,
            } => (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (header::CACHE_CONTROL, cache_control.to_string()),
                ],
            )
                .into_response(),
            Serve::Range {
                start,
                end,
                size,
                mime,
                cache_control,
            } => (
                StatusCode::PARTIAL_CONTENT,
                [
                    (header::CONTENT_TYPE, mime),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}")),
                    (header::CACHE_CONTROL, cache_control.to_string()),
                ],
            )
                .into_response(),
        }
    }
}

/// Everything the world said about a Call's audio, gathered before anything is
/// decided about it.
///
/// The **whole** input to [`plan`]: what the row holds, what the store answered,
/// and what the client asked for. A struct rather than six parameters because
/// what a test wants to state is a *situation* — "the store signed it and the
/// Call is still queued for enhancement" — and because the fetch order is
/// [`audio`]'s business, not the decision's.
pub struct Facts<'a> {
    /// Where the audio lives, or empty for an **Encrypted Call** (#42, US 9).
    pub object_key: &'a str,
    /// The Call's enhancement state, which decides how long the answer may be
    /// cached (#20, #31).
    pub enhancement: &'a str,
    pub mime: Option<&'a str>,
    /// `Some` when the store signs its own URLs *and* signed this one. The store
    /// is never asked for a `size` in that case, so `size` is `None` alongside
    /// it and means nothing.
    pub signed: Option<crate::blob::PresignedUrl>,
    /// `Some(n)` when the store has the object, `None` when it does not — the
    /// object pruned between the row being read and the store being asked.
    pub size: Option<u64>,
    /// The client's `Range` header, if it sent one.
    pub range: Option<&'a HeaderValue>,
}

/// **The audio-serving decision**, whole and pure.
///
/// The three refusals are [`Reason`]s rather than `Serve` arms because that is
/// what they are: a caller told to stop asking, in the one vocabulary every
/// refusal in the app shares (#92). What separates `CallHasNoAudio` from
/// `AudioNotFound` is worth the two arms — the first is a Call that will never
/// have audio and the second is one whose audio has just been pruned, and only
/// the log can tell an operator which happened.
pub fn plan(facts: Facts<'_>) -> Result<Serve, Reason> {
    // An encrypted Call is a row with no object behind it (#42, spec US 9).
    // Answered here rather than left to the store, which would be asked for an
    // object named by the empty string and fail as a *500* — the wire already
    // omits `audioUrl` for these, so anything arriving here is a hand-built URL
    // or a stale link, and both deserve the truth.
    if facts.object_key.is_empty() {
        return Err(Reason::CallHasNoAudio);
    }
    if let Some(signed) = facts.signed {
        return Ok(Serve::Redirect {
            max_age: redirect_max_age(facts.enhancement, signed.max_age_secs),
            url: signed.url,
        });
    }

    let size = facts.size.ok_or(Reason::AudioNotFound)?;
    let mime = facts.mime.unwrap_or("application/octet-stream").to_string();
    let cache_control = audio_cache_control(facts.enhancement);

    match parse_range_header(facts.range, size) {
        RangeOutcome::None => Ok(Serve::Whole {
            mime,
            cache_control,
        }),
        RangeOutcome::Range { start, end } => Ok(Serve::Range {
            start,
            end,
            size,
            mime,
            cache_control,
        }),
        RangeOutcome::Unsatisfiable => Err(Reason::RangeNotSatisfiable { size }),
    }
}

/// A Call's audio, ready to send: what was decided, and the bytes it needs.
pub struct Audio {
    serve: Serve,
    /// Empty for a [`Serve::Redirect`], which is a `307` and has no body — the
    /// bytes are the object store's to hand over, which is the whole point of
    /// the redirect (ADR-0002).
    bytes: bytes::Bytes,
}

impl IntoResponse for Audio {
    fn into_response(self) -> Response {
        let mut response = self.serve.into_response();
        *response.body_mut() = axum::body::Body::from(self.bytes);
        response
    }
}

/// `GET /api/call/{id}/audio` — serve a stored Call's audio (ADR-0002).
///
/// **Gather, decide, fetch.** Everything with an opinion is in [`plan`]; what is
/// left here is the I/O, in the one order that is allowed: a store that signs is
/// never also asked for a size, and bytes are only ever read for a plan that
/// says which bytes.
pub async fn audio(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
    headers: HeaderMap,
) -> Result<Audio, Failure> {
    let call = repo::get_call_audio(&state.db, id)
        .await
        .map_err(Stage::LookUpCall.failed())?
        .ok_or(Reason::CallNotFound)?;

    // Nothing is asked of the store about an object that isn't there — an
    // Encrypted Call's key is the empty string, and `plan` is what turns that
    // into an answer.
    let stored = (!call.object_key.is_empty()).then_some(call.object_key.as_str());
    let signed = match stored.filter(|_| state.audio.is_presigning()) {
        Some(key) => state
            .audio
            .presigned_get_url(key)
            .await
            .transpose()
            .map_err(Stage::SignAudioUrl.failed())?,
        None => None,
    };
    let size = match stored.filter(|_| signed.is_none()) {
        Some(key) => state
            .audio
            .size(key)
            .await
            .map_err(Stage::StatAudio.failed())?,
        None => None,
    };

    let serve = plan(Facts {
        object_key: &call.object_key,
        enhancement: &call.enhancement,
        mime: call.mime.as_deref(),
        signed,
        size,
        range: headers.get(header::RANGE),
    })?;

    // Only now, and only for a plan that says which bytes.
    let bytes = match &serve {
        Serve::Redirect { .. } => bytes::Bytes::new(),
        Serve::Whole { .. } => state
            .audio
            .get(&call.object_key)
            .await
            .map_err(Stage::ReadAudio.failed())?
            .ok_or(Reason::AudioNotFound)?,
        Serve::Range { start, end, .. } => state
            .audio
            .get_range(&call.object_key, *start, *end + 1)
            .await
            .map_err(Stage::ReadAudioRange.failed())?,
    };
    Ok(Audio { serve, bytes })
}

/// The parsed outcome of a `Range` request header.
#[derive(Debug, PartialEq, Eq)]
enum RangeOutcome {
    /// No (usable) range header — serve the whole object.
    None,
    /// A satisfiable single byte range, inclusive `[start, end]`.
    Range { start: u64, end: u64 },
    /// A malformed or unsatisfiable range.
    Unsatisfiable,
}

/// Parse a single-range `Range: bytes=...` header against an object of `size`
/// bytes. Multi-range requests are treated as unsatisfiable (we don't emit
/// multipart/byteranges).
fn parse_range_header(value: Option<&HeaderValue>, size: u64) -> RangeOutcome {
    let Some(value) = value else {
        return RangeOutcome::None;
    };
    let Ok(text) = value.to_str() else {
        return RangeOutcome::Unsatisfiable;
    };
    let Some(spec) = text.trim().strip_prefix("bytes=") else {
        return RangeOutcome::Unsatisfiable;
    };
    let spec = spec.trim();
    // Multi-range (`a-b,c-d`) and empty specs need no explicit guard: a comma
    // makes one side fail to parse below, and an empty spec has no `-` to split.
    // (Keeping the guard would be an equivalent mutant — dead by construction.)
    let Some((raw_start, raw_end)) = spec.split_once('-') else {
        return RangeOutcome::Unsatisfiable;
    };
    if size == 0 {
        return RangeOutcome::Unsatisfiable;
    }

    let (start, end) = match (raw_start.trim(), raw_end.trim()) {
        ("", "") => return RangeOutcome::Unsatisfiable,
        // Suffix range: the last N bytes.
        ("", suffix) => {
            let Ok(n) = suffix.parse::<u64>() else {
                return RangeOutcome::Unsatisfiable;
            };
            if n == 0 {
                return RangeOutcome::Unsatisfiable;
            }
            let n = n.min(size);
            (size - n, size - 1)
        }
        // Open-ended: from `start` to the end.
        (start, "") => {
            let Ok(start) = start.parse::<u64>() else {
                return RangeOutcome::Unsatisfiable;
            };
            (start, size - 1)
        }
        // Closed range.
        (start, end) => {
            let (Ok(start), Ok(end)) = (start.parse::<u64>(), end.parse::<u64>()) else {
                return RangeOutcome::Unsatisfiable;
            };
            (start, end.min(size - 1))
        }
    };

    if start > end || start >= size {
        return RangeOutcome::Unsatisfiable;
    }
    RangeOutcome::Range { start, end }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::call::EnhancementState;
    use proptest::prelude::*;
    use rstest::rstest;

    /// The pending Call's ceiling is written twice — once as a header string
    /// for the proxied response, once as a number the S3 redirect takes the
    /// minimum against (#31) — so they are held to being the same number.
    /// Letting them drift would leave the two serving paths promising different
    /// things about the same Call, with nothing to notice.
    #[test]
    fn the_two_spellings_of_the_pending_ceiling_agree() {
        assert_eq!(
            PENDING_AUDIO_CACHE_CONTROL,
            format!("private, max-age={PENDING_AUDIO_MAX_AGE_SECS}")
        );
    }

    /// A Call the enhancement worker still owes is one whose bytes are about to
    /// be replaced, so neither serving path may promise they never change —
    /// and a settled Call's may be kept for a week (#20, #31).
    #[rstest]
    #[case(EnhancementState::PENDING, PENDING_AUDIO_CACHE_CONTROL)]
    #[case(EnhancementState::DONE, AUDIO_CACHE_CONTROL)]
    #[case(EnhancementState::SKIPPED, AUDIO_CACHE_CONTROL)]
    #[case(EnhancementState::NONE, AUDIO_CACHE_CONTROL)]
    fn only_a_settled_call_is_cached_as_immutable(
        #[case] enhancement: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(audio_cache_control(enhancement), expected);
        assert!(
            expected.contains("immutable") == (enhancement != EnhancementState::PENDING),
            "a Call about to be re-levelled must not be `immutable`"
        );
    }

    /// The redirect obeys **both** limits: the signature's own budget, and — for
    /// a Call still queued — the point at which the key it names may change.
    #[rstest]
    // Settled: only the signature binds, however long it is.
    #[case(EnhancementState::DONE, 900, 900)]
    #[case(EnhancementState::DONE, 5, 5)]
    // Pending: capped, but never *extended* past what the signature can honour —
    // a redirect cached past its signature is a 403 the listener cannot explain.
    #[case(EnhancementState::PENDING, 900, PENDING_AUDIO_MAX_AGE_SECS)]
    #[case(EnhancementState::PENDING, 5, 5)]
    fn a_redirect_is_capped_by_the_sooner_of_its_two_limits(
        #[case] enhancement: &str,
        #[case] budget: u64,
        #[case] expected: u64,
    ) {
        assert_eq!(redirect_max_age(enhancement, budget), expected);
    }

    /// The facts of an ordinary settled Call whose object the store has, with
    /// nothing signed and no range asked for. Every case below states only what
    /// it changes.
    fn facts<'a>(object_key: &'a str, range: Option<&'a HeaderValue>) -> Facts<'a> {
        Facts {
            object_key,
            enhancement: EnhancementState::DONE,
            mime: Some("audio/mp4"),
            signed: None,
            size: Some(10),
            range,
        }
    }

    fn signature(max_age_secs: u64) -> crate::blob::PresignedUrl {
        crate::blob::PresignedUrl {
            url: "https://s3.example/ab/c.m4a?sig=x".to_string(),
            max_age_secs,
        }
    }

    /// **The whole audio-serving decision, without a socket** (#92).
    ///
    /// Six outcomes over one table: the three ways of answering and the three
    /// ways of refusing. Every one of these used to need a running app — the
    /// redirect an S3 backend, the two 404s a store made to lie, the 416 a real
    /// object of a known size — which is why "gone" and "broken" were so easy to
    /// get the wrong way round here.
    #[rstest]
    // An Encrypted Call: a row that will never have audio (#42, US 9). Not the
    // same as a Call whose audio is gone, and only the log can say which.
    #[case::encrypted(facts("", None), Err(Reason::CallHasNoAudio))]
    // The object was pruned between the row being read and the store asked.
    #[case::pruned(Facts { size: None, ..facts("ab/c.m4a", None) }, Err(Reason::AudioNotFound))]
    // A range naming bytes this object does not have.
    #[case::unsatisfiable(
        facts("ab/c.m4a", Some(&BAD_RANGE)),
        Err(Reason::RangeNotSatisfiable { size: 10 })
    )]
    // Whole object, whole body.
    #[case::whole(
        facts("ab/c.m4a", None),
        Ok(Serve::Whole { mime: "audio/mp4".into(), cache_control: AUDIO_CACHE_CONTROL })
    )]
    // ...and one iOS `<audio>` asks for by range.
    #[case::ranged(
        facts("ab/c.m4a", Some(&GOOD_RANGE)),
        Ok(Serve::Range {
            start: 4, end: 9, size: 10,
            mime: "audio/mp4".into(),
            cache_control: AUDIO_CACHE_CONTROL,
        })
    )]
    // A store that signs answers for itself, and the size is never asked for —
    // which is why `size` says nothing here.
    #[case::redirect(
        Facts { signed: Some(signature(900)), size: None, ..facts("ab/c.m4a", None) },
        Ok(Serve::Redirect { url: "https://s3.example/ab/c.m4a?sig=x".into(), max_age: 900 })
    )]
    // A signed URL wins over a `Range` header: the object store honours the
    // range itself, and the request never reaches us again (#31).
    #[case::redirect_beats_a_range(
        Facts { signed: Some(signature(900)), size: None, ..facts("ab/c.m4a", Some(&GOOD_RANGE)) },
        Ok(Serve::Redirect { url: "https://s3.example/ab/c.m4a?sig=x".into(), max_age: 900 })
    )]
    // A Call still queued for enhancement caps *both* ways of answering, because
    // its bytes are about to be replaced (#20).
    #[case::pending_whole(
        Facts { enhancement: EnhancementState::PENDING, ..facts("ab/c.m4a", None) },
        Ok(Serve::Whole { mime: "audio/mp4".into(), cache_control: PENDING_AUDIO_CACHE_CONTROL })
    )]
    #[case::pending_redirect(
        Facts {
            enhancement: EnhancementState::PENDING,
            signed: Some(signature(900)),
            size: None,
            ..facts("ab/c.m4a", None)
        },
        Ok(Serve::Redirect {
            url: "https://s3.example/ab/c.m4a?sig=x".into(),
            max_age: PENDING_AUDIO_MAX_AGE_SECS,
        })
    )]
    // A recorder that named no MIME type still gets played, not refused.
    #[case::no_mime(
        Facts { mime: None, ..facts("ab/c.m4a", None) },
        Ok(Serve::Whole {
            mime: "application/octet-stream".into(),
            cache_control: AUDIO_CACHE_CONTROL,
        })
    )]
    fn the_whole_serving_decision(
        #[case] facts: Facts<'_>,
        #[case] expected: Result<Serve, Reason>,
    ) {
        assert_eq!(plan(facts), expected);
    }

    // `static`, not `const`: a reference to a `const` is a fresh temporary at
    // every use, and an rstest case is an expression whose temporaries die at
    // the end of the statement.
    static GOOD_RANGE: HeaderValue = HeaderValue::from_static("bytes=4-9");
    static BAD_RANGE: HeaderValue = HeaderValue::from_static("bytes=99-200");

    fn parse(header: &str, size: u64) -> RangeOutcome {
        parse_range_header(Some(&HeaderValue::from_str(header).unwrap()), size)
    }

    #[rstest]
    // Satisfiable ranges (size = 10 unless noted).
    #[case("bytes=0-9", 10, RangeOutcome::Range { start: 0, end: 9 })]
    #[case("bytes=4-9", 10, RangeOutcome::Range { start: 4, end: 9 })]
    #[case("bytes=0-0", 10, RangeOutcome::Range { start: 0, end: 0 })]
    // Closed end past EOF clamps to size-1.
    #[case("bytes=4-100", 10, RangeOutcome::Range { start: 4, end: 9 })]
    // Open-ended runs to EOF.
    #[case("bytes=5-", 10, RangeOutcome::Range { start: 5, end: 9 })]
    // Suffix = last N bytes; over-long suffix clamps to the whole object.
    #[case("bytes=-4", 10, RangeOutcome::Range { start: 6, end: 9 })]
    #[case("bytes=-100", 10, RangeOutcome::Range { start: 0, end: 9 })]
    // Whitespace around the numbers is tolerated.
    #[case("bytes= 4 - 9 ", 10, RangeOutcome::Range { start: 4, end: 9 })]
    // Unsatisfiable / malformed.
    #[case("bytes=9-4", 10, RangeOutcome::Unsatisfiable)] // start > end
    #[case("bytes=10-20", 10, RangeOutcome::Unsatisfiable)] // start >= size
    #[case("bytes=a-9", 10, RangeOutcome::Unsatisfiable)] // non-numeric start
    #[case("bytes=4-b", 10, RangeOutcome::Unsatisfiable)] // non-numeric end
    #[case("bytes=x-", 10, RangeOutcome::Unsatisfiable)] // non-numeric open-ended
    #[case("bytes=-x", 10, RangeOutcome::Unsatisfiable)] // non-numeric suffix
    #[case("bytes=5", 10, RangeOutcome::Unsatisfiable)] // missing '-'
    #[case("bytes=", 10, RangeOutcome::Unsatisfiable)] // empty spec
    #[case("bytes=-", 10, RangeOutcome::Unsatisfiable)] // both empty
    #[case("bytes=-0", 10, RangeOutcome::Unsatisfiable)] // zero-length suffix
    #[case("bytes=0-1,2-3", 10, RangeOutcome::Unsatisfiable)] // multi-range not emitted
    #[case("items=0-9", 10, RangeOutcome::Unsatisfiable)] // wrong unit prefix
    #[case("bytes=0-9", 0, RangeOutcome::Unsatisfiable)] // zero-length object
    fn range_header_cases(#[case] header: &str, #[case] size: u64, #[case] expected: RangeOutcome) {
        assert_eq!(
            parse(header, size),
            expected,
            "header={header:?} size={size}"
        );
    }

    #[test]
    fn no_range_header_serves_the_whole_object() {
        assert_eq!(parse_range_header(None, 10), RangeOutcome::None);
    }

    #[test]
    fn non_ascii_range_header_is_unsatisfiable() {
        // A header value that isn't valid UTF-8 -> `to_str()` fails.
        let value = HeaderValue::from_bytes(&[0xFF, 0xFE]).unwrap();
        assert_eq!(
            parse_range_header(Some(&value), 10),
            RangeOutcome::Unsatisfiable
        );
    }

    /// Each way of answering renders the headers a media element needs, without
    /// a socket, a store or a Call — a redirect says where and for how long, a
    /// whole object advertises that it takes ranges, and a partial one says
    /// which bytes of what.
    #[rstest]
    #[case::redirect(
        Serve::Redirect { url: "https://s3.example/ab/c.m4a?sig=x".into(), max_age: 30 },
        307,
        &[("location", "https://s3.example/ab/c.m4a?sig=x"),
          ("cache-control", "private, max-age=30")]
    )]
    #[case::whole(
        Serve::Whole { mime: "audio/mp4".into(), cache_control: AUDIO_CACHE_CONTROL },
        200,
        &[("content-type", "audio/mp4"),
          ("accept-ranges", "bytes"),
          ("cache-control", AUDIO_CACHE_CONTROL)]
    )]
    #[case::partial(
        Serve::Range {
            start: 4, end: 9, size: 10,
            mime: "audio/x-wav".into(),
            cache_control: PENDING_AUDIO_CACHE_CONTROL,
        },
        206,
        &[("content-type", "audio/x-wav"),
          ("accept-ranges", "bytes"),
          ("content-range", "bytes 4-9/10"),
          ("cache-control", PENDING_AUDIO_CACHE_CONTROL)]
    )]
    fn each_way_of_answering_renders_itself(
        #[case] serve: Serve,
        #[case] status: u16,
        #[case] expected: &[(&str, &str)],
    ) {
        let response = serve.into_response();

        assert_eq!(response.status().as_u16(), status);
        for (name, value) in expected {
            assert_eq!(
                response
                    .headers()
                    .get(*name)
                    .unwrap_or_else(|| panic!("a {name} header"))
                    .to_str()
                    .expect("an ascii header"),
                *value,
                "{name}"
            );
        }
    }

    proptest! {
        /// A well-formed closed range fully inside the object round-trips exactly.
        #[test]
        fn closed_range_within_bounds_round_trips(
            size in 1u64..10_000,
            start in 0u64..10_000,
            len in 0u64..10_000,
        ) {
            prop_assume!(start < size);
            let end = (start + len).min(size - 1);
            let header = format!("bytes={start}-{end}");
            prop_assert_eq!(
                parse(&header, size),
                RangeOutcome::Range { start, end }
            );
        }

        /// Whatever the input, a `Range` outcome is always a valid slice: no
        /// panic, `start <= end`, and `end` inside the object.
        #[test]
        fn parsed_range_is_always_a_valid_slice(
            size in 1u64..10_000,
            body in "[ -~]{0,24}",
        ) {
            let header = format!("bytes={body}");
            if let Ok(value) = HeaderValue::from_str(&header)
                && let RangeOutcome::Range { start, end } = parse_range_header(Some(&value), size)
            {
                prop_assert!(start <= end, "start {start} > end {end}");
                prop_assert!(end < size, "end {end} >= size {size}");
            }
        }
    }
}
