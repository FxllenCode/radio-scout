//! The **Selection** (CONTEXT.md): which Systems and Talkgroups a listener
//! hears, and the one rule that decides whether a Call is one of them.
//!
//! It lives here rather than inside the live feed because two surfaces now ask
//! the same question of it — the live-feed socket ([`crate::live`], #9/#11) and
//! Web Push ([`crate::push`], #16) — and a listener who is notified about a
//! Talkgroup the feed would not have played them is a bug that only shows up on
//! someone's phone. The client holds the same algebra in
//! `client/src/lib/selection.ts`, deliberately in the same shape.
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
const TALKGROUP_WILDCARD: &str = "*";

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
    /// (ADR-0008), which Web Push has no equivalent of and passes open.
    pub fn reaches(&self, call: &StoredCall, permits: impl Fn(i64, i64) -> bool) -> bool {
        if self.is_all_off() {
            return false;
        }
        let system_ref = call.system_ref;
        call.talkgroups().any(|talkgroup_ref| {
            self.selects(system_ref, talkgroup_ref) && permits(system_ref, talkgroup_ref)
        })
    }

    /// [`Selection::reaches`] with nothing further to satisfy.
    pub fn admits(&self, call: &StoredCall) -> bool {
        self.reaches(call, |_, _| true)
    }
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
}
