//! The **Selection** (CONTEXT.md): which Systems and Talkgroups a listener
//! hears, and the one rule that decides whether a Call is one of them.
//!
//! It lives here rather than inside the live feed because two surfaces now ask
//! the same question of it — the live-feed socket ([`crate::live`], #9/#11) and
//! a **Downstream** peer's scope ([`crate::downstream`], #52) — and a peer sent
//! traffic the feed would not have played is the same rule applied twice and
//! getting two answers. (A third consumer, Web Push, was why this module was
//! extracted in the first place; it went with #107, and the extraction outlived
//! it.) The client holds the same algebra in `client/src/lib/selection.ts`,
//! deliberately in the same shape.
//!
//! The wire form is the stored form (ADR-0004): `all` plus exceptions under
//! `sel[system][talkgroup | "*"]`, so nothing is rebuilt between the socket, the
//! database and the browser's local storage.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::call::StoredCall;

/// The Talkgroup key meaning "every Talkgroup in this System" (#11). A client
/// holding a System can't enumerate its Talkgroups — it only knows the ones it
/// has heard — and rdio only avoids the problem by shipping its whole config to
/// the client, so the matrix gets a wildcard instead.
pub(crate) const TALKGROUP_WILDCARD: &str = "*";

/// The listener's subscription matrix: `systemRef -> talkgroupRef -> enabled`.
/// JSON object keys are strings, so refs are compared as strings. `all` is the
/// spec's global all-on (story 21) — a "monitor everything" client.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Selection {
    pub sel: HashMap<String, HashMap<String, bool>>,
    pub all: bool,
}

impl Selection {
    /// Is `(system_ref, talkgroup_ref)` selected?
    ///
    /// Most specific wins: an explicit entry for the Talkgroup, then the
    /// System's wildcard, then global-all. That ordering is what lets a listener
    /// **avoid** one Talkgroup out of an all-on selection (spec US 14) or out of
    /// a held System (US 11) — an exception to a default, rather than something
    /// the default overrules.
    pub fn selects(&self, system_ref: i64, talkgroup_ref: i64) -> bool {
        let system = self.sel.get(&system_ref.to_string());
        if let Some(explicit) = system.and_then(|tgs| tgs.get(&talkgroup_ref.to_string())) {
            return *explicit;
        }
        if let Some(wildcard) = system.and_then(|tgs| tgs.get(TALKGROUP_WILDCARD)) {
            return *wildcard;
        }
        self.all
    }

    /// Nothing is selected at all (rdio's `IsAllOff`): no global-all and no
    /// enabled entry — exclusions alone select nothing. Lets [`Selection::reaches`]
    /// skip patch resolution for an idle listener.
    pub fn is_all_off(&self) -> bool {
        !self.all
            && self
                .sel
                .values()
                .all(|talkgroups| talkgroups.values().all(|&on| !on))
    }

    /// Does `call` reach this Selection, given a further per-Talkgroup gate?
    ///
    /// Yes when some Talkgroup the Call reaches — its own, or any patched one
    /// within the Call's System — is both selected and permitted. Mirrors rdio's
    /// `IsEnabled` (primary OR patch); the gate is the live feed's access scope
    /// (ADR-0008), which a **Downstream** scope has no equivalent of and passes
    /// open.
    pub fn reaches(&self, call: &StoredCall, permits: impl Fn(i64, i64) -> bool) -> bool {
        self.reaches_channels(call.system_ref, call.talkgroups(), permits)
    }

    /// [`Selection::reaches`] over the Refs alone.
    ///
    /// The same rule, reachable before a [`StoredCall`] exists — which is where
    /// a **Downstream**'s scope is asked (#52): forwarding is decided inside the
    /// transaction that stores the Call, with the Refs in hand and the
    /// denormalized view not yet built. Written as the one implementation and
    /// delegated to, rather than a second copy that only *looks* the same:
    /// rdio's own forwarder is exactly that second copy, and it checks the
    /// Call's own Talkgroup and **not its patches** (`downstream.go:99`), so a
    /// patched transmission never reaches a peer subscribed to the channel it
    /// was patched onto.
    pub fn reaches_channels(
        &self,
        system_ref: i64,
        talkgroups: impl Iterator<Item = i64>,
        permits: impl Fn(i64, i64) -> bool,
    ) -> bool {
        if self.is_all_off() {
            return false;
        }
        talkgroups.into_iter().any(|talkgroup_ref| {
            self.selects(system_ref, talkgroup_ref) && permits(system_ref, talkgroup_ref)
        })
    }

    /// [`Selection::reaches`] with nothing further to satisfy.
    pub fn admits(&self, call: &StoredCall) -> bool {
        self.reaches(call, |_, _| true)
    }

    /// The Selection a link carries, or `None` if it does not carry one.
    ///
    /// The spelling is the client's (`client/src/lib/selectionUrl.ts`):
    /// `<default>`, then a `_`-separated System per exception, each
    /// `<systemRef>` followed by `.`-separated entries — `*` for the System's
    /// wildcard, a Ref otherwise, prefixed `-` when it is off. Only
    /// `[0-9*._-]`, which is what a query string carries unescaped.
    ///
    /// It is read here because a **DVR**'s scope is a Selection the browser
    /// encodes and the Archive filters on (#63), where before it only ever
    /// travelled between two browsers (#61). Neither side's own round trip can
    /// notice the two drifting apart, so
    /// `client/src/lib/selectionEncoding.json` is a table **both** are held to
    /// — the same reason #62 runs its bucket SQL against its bucket Rust.
    ///
    /// **All or nothing**, like the client's reader and for its reason: a link
    /// is one statement made by somebody else, and applying half of it would
    /// answer with a scanner nobody asked for. Refused here means the request
    /// is refused, naming the parameter.
    pub fn decode(encoded: &str) -> Option<Selection> {
        let mut parts = encoded.split('_');
        let all = match parts.next()? {
            "1" => true,
            "0" => false,
            _ => return None,
        };

        let mut sel: HashMap<String, HashMap<String, bool>> = HashMap::new();
        for system in parts {
            let mut fields = system.split('.');
            let system_ref = canonical_ref(fields.next()?)?;

            let mut talkgroups = HashMap::new();
            for entry in fields {
                let (on, key) = match entry.strip_prefix('-') {
                    Some(rest) => (false, rest),
                    None => (true, entry),
                };
                let key = if key == TALKGROUP_WILDCARD {
                    TALKGROUP_WILDCARD.to_string()
                } else {
                    canonical_ref(key)?
                };
                talkgroups.insert(key, on);
            }
            // A System named with nothing to say about it is not an exception
            // to anything — and is how a trailing `_` reads.
            if talkgroups.is_empty() {
                return None;
            }
            sel.insert(system_ref, talkgroups);
        }

        Some(Selection { sel, all })
    }
}

/// A Ref as the matrix keys it: digits only, and **normalized through the
/// number it names**, because the client's reader does the same (`Number()`) —
/// so a hand-typed `0100` addresses System 100 at both ends rather than a
/// System whose key nothing will ever match.
fn canonical_ref(value: &str) -> Option<String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(value.parse::<i64>().ok()?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn selection(pairs: &[(&str, &str)], all: bool) -> Selection {
        let mut sel: HashMap<String, HashMap<String, bool>> = HashMap::new();
        for (system, talkgroup) in pairs {
            sel.entry((*system).to_string())
                .or_default()
                .insert((*talkgroup).to_string(), true);
        }
        Selection { sel, all }
    }

    #[rstest]
    #[case(selection(&[], false), true)] // truly empty
    #[case(selection(&[("11", "100")], false), false)] // one enabled
    #[case(selection(&[], true), false)] // global all-on
    fn is_all_off_cases(#[case] selection: Selection, #[case] expected: bool) {
        assert_eq!(selection.is_all_off(), expected);
    }

    #[test]
    fn is_all_off_with_only_disabled_entries() {
        let mut selection = selection(&[], false);
        selection
            .sel
            .entry("11".to_string())
            .or_default()
            .insert("100".to_string(), false);
        assert!(
            selection.is_all_off(),
            "an explicit-false entry is still all-off"
        );
    }

    /// The stored form and the wire form are the same JSON, and a Selection
    /// nobody has touched is "nothing selected" rather than "everything".
    #[test]
    fn a_selection_round_trips_through_its_json() {
        let selection = selection(&[("11", "100"), ("11", "*")], true);

        let json = serde_json::to_string(&selection).expect("serialize");
        assert_eq!(
            serde_json::from_str::<Selection>(&json).expect("deserialize"),
            selection
        );
        assert_eq!(
            serde_json::from_str::<Selection>("{}").expect("an empty object"),
            Selection::default()
        );
        assert!(Selection::default().is_all_off());
    }

    /// The link spelling, held to the table the client is held to.
    ///
    /// This file and `client/src/lib/selectionUrl.ts` are one format written
    /// twice in two languages, and a drift between them is not a crash: the
    /// server answers a **DVR** with a perfectly ordinary page of somebody
    /// else's channels. Each side's own round trip is happily green through
    /// that, which is why the cases live in a file neither owns.
    #[derive(serde::Deserialize)]
    struct EncodingCases {
        readable: Vec<ReadableCase>,
        unreadable: Vec<String>,
    }

    #[derive(serde::Deserialize)]
    struct ReadableCase {
        encoded: String,
        all: bool,
        sel: HashMap<String, HashMap<String, bool>>,
    }

    fn encoding_cases() -> EncodingCases {
        serde_json::from_str(include_str!("../client/src/lib/selectionEncoding.json"))
            .expect("the shared encoding table parses")
    }

    #[test]
    fn every_link_the_client_writes_reads_back_as_the_selection_it_meant() {
        let cases = encoding_cases();
        assert!(
            cases.readable.len() >= 8,
            "the shared table is the only thing holding the two spellings together"
        );
        for case in cases.readable {
            assert_eq!(
                Selection::decode(&case.encoded),
                Some(Selection {
                    sel: case.sel,
                    all: case.all,
                }),
                "decoding {:?}",
                case.encoded
            );
        }
    }

    #[test]
    fn a_link_that_is_not_one_is_refused_whole() {
        for encoded in encoding_cases().unreadable {
            assert_eq!(
                Selection::decode(&encoded),
                None,
                "{encoded:?} is not a Selection"
            );
        }
    }
}
