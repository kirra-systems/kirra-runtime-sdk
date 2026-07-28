//! Deterministic non-motion fence for Mick's text→intent boundary.
//!
//! **Why this exists.** The deployed `gemma3:4b` returns valid-looking MOTION
//! JSON for plainly conversational text. Observed live against
//! `POST /intent`:
//!
//! | text | model reply |
//! |---|---|
//! | `hello rabbit` | `{"intent":"cruise","target_speed_mps":5}` |
//! | `hello parker` | `{"intent":"cruise","target_speed_mps":10}` |
//! | `what do you see` | `{"intent":"route_to","x_m":120,"y_m":40}` |
//!
//! Each is well-formed, in-schema, and finite, so the fail-closed parse admits
//! it — correctly, because the parse checks SHAPE, not whether the utterance
//! asked for motion at all. The gap is upstream of the parse.
//!
//! **Defense in depth, not a safety boundary.** Occy grounds an intent, KIRRA
//! bounds it, and the verifying consumer enforces it — all unchanged, all still
//! authoritative. A greeting that slipped through would still be clamped by the
//! governor. This just stops it becoming an intent in the first place, which is
//! cheaper and far easier to explain to an operator than a governor clamp.
//!
//! **Exact whole-utterance matching, deliberately.** The allowlist is compared
//! against the WHOLE normalized utterance, never as a substring. That single
//! decision buys both properties the fence needs:
//!
//!   - `drive to the hello sign` contains "hello" and is NOT fenced;
//!   - `hello rabbit, drive forward one meter` carries an explicit command, so
//!     it is not an exact match either and continues to the model.
//!
//! Anything not on the list goes to the model exactly as before. The fence can
//! only ever REMOVE motion, never create or alter it.

/// Why an utterance was fenced. Telemetry only — every kind has the same
/// effect (no intent, no latch change).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NonMotionKind {
    /// A bare wake phrase: the operator got the listener's attention and said
    /// nothing else yet.
    WakePhrase,
    /// A greeting or conversational acknowledgement.
    Greeting,
    /// A read-only perception/status question — answering it is Rabbit's job,
    /// and it must never move the platform.
    StatusQuestion,
}

impl NonMotionKind {
    /// Stable token for logs and counters.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WakePhrase => "wake_phrase",
            Self::Greeting => "greeting",
            Self::StatusQuestion => "status_question",
        }
    }
}

/// Bare wake phrases. The listener's attention word plus a name is not a
/// request; `hello rabbit, drive forward` is, and is not an exact match here.
const WAKE_PHRASES: &[&str] = &[
    "hello rabbit",
    "hey rabbit",
    "yo rabbit",
    "hello parker",
    "hey parker",
    "yo parker",
    // The bare names, for a listener that already consumed the wake word.
    "rabbit",
    "parker",
];

/// Greetings and acknowledgements.
const GREETINGS: &[&str] = &[
    "hello",
    "hi",
    "hey",
    "good morning",
    "good afternoon",
    "good evening",
    "thanks",
    "thank you",
    "how are you",
    "are you there",
];

/// Read-only perception / status questions.
///
/// Written in NORMALIZED form — apostrophes are dropped by [`normalize`], so
/// `what's around us` and `whats around us` both arrive here as
/// `whats around us` and one entry covers both.
const STATUS_QUESTIONS: &[&str] = &[
    "what do you see",
    "what can you see",
    "what is around us",
    "whats around us",
    "are we okay",
    "are we ok",
    "why did we stop",
    "what stopped us",
    "run diagnostics",
    "how are your systems",
    "what is your status",
];

/// Normalize an utterance to lowercase space-separated word tokens.
///
/// - case folded;
/// - apostrophes (ASCII `'` and the typographic `’` a phone keyboard emits)
///   are DROPPED, not spaced, so `what's` → `whats` and one allowlist entry
///   covers both spellings;
/// - every other non-alphanumeric character becomes a separator, so
///   punctuation and surrounding whitespace cannot defeat a match;
/// - runs of separators collapse.
///
/// Token-based rather than substring-based: the caller compares the WHOLE
/// result, so a normalized utterance either is a known conversational form or
/// is not.
#[must_use]
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch == '\'' || ch == '\u{2019}' {
            continue; // contraction — join the halves
        }
        if ch.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
        } else {
            pending_space = true;
        }
    }
    out
}

/// Classify an utterance as deterministically non-motion, or `None` to let the
/// model decide.
///
/// Pure: no model call, no network, no I/O, no clock. `None` is the default for
/// everything unrecognised — the fence narrows what reaches the model, it never
/// widens what is accepted.
#[must_use]
pub fn classify_non_motion(text: &str) -> Option<NonMotionKind> {
    let normalized = normalize(text);
    if normalized.is_empty() {
        // An empty request is already MICK_EMPTY_REQUEST downstream; that is a
        // client error worth surfacing, not a conversational turn to absorb.
        return None;
    }
    if WAKE_PHRASES.contains(&normalized.as_str()) {
        return Some(NonMotionKind::WakePhrase);
    }
    if GREETINGS.contains(&normalized.as_str()) {
        return Some(NonMotionKind::Greeting);
    }
    if STATUS_QUESTIONS.contains(&normalized.as_str()) {
        return Some(NonMotionKind::StatusQuestion);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- the observed live regressions --------------------------------------

    #[test]
    fn the_four_observed_live_inputs_classify_as_intended() {
        // Three that produced motion JSON from gemma3:4b and must not reach it.
        assert_eq!(
            classify_non_motion("hello rabbit"),
            Some(NonMotionKind::WakePhrase)
        );
        assert_eq!(
            classify_non_motion("hello parker"),
            Some(NonMotionKind::WakePhrase)
        );
        assert_eq!(
            classify_non_motion("what do you see"),
            Some(NonMotionKind::StatusQuestion)
        );
        // The one that IS a motion request must still reach the model.
        assert_eq!(classify_non_motion("drive forward one meter"), None);
    }

    // --- normalization ------------------------------------------------------

    #[test]
    fn case_punctuation_and_whitespace_do_not_defeat_a_match() {
        for text in [
            "Hello, Rabbit!",
            "  hello rabbit  ",
            "HELLO RABBIT",
            "Yo Rabbit?",
            "hey parker",
            "\thello   rabbit\n",
        ] {
            assert!(
                classify_non_motion(text).is_some(),
                "must be fenced: {text:?}"
            );
        }
    }

    #[test]
    fn both_apostrophe_spellings_normalize_together() {
        // A phone keyboard emits U+2019; a shell emits U+0027; a transcriber
        // may emit neither. One allowlist entry has to cover all three.
        assert_eq!(normalize("what's around us"), "whats around us");
        assert_eq!(normalize("what\u{2019}s around us"), "whats around us");
        assert_eq!(normalize("whats around us"), "whats around us");
        for text in [
            "what's around us",
            "what\u{2019}s around us",
            "whats around us",
        ] {
            assert_eq!(
                classify_non_motion(text),
                Some(NonMotionKind::StatusQuestion),
                "{text:?}"
            );
        }
    }

    #[test]
    fn normalize_collapses_separators_and_never_leads_or_trails() {
        assert_eq!(normalize("  a,,,  b  "), "a b");
        assert_eq!(normalize("!!!"), "");
        assert_eq!(normalize(""), "");
    }

    // --- wake prefix PLUS a command must still reach the model --------------

    #[test]
    fn a_wake_prefix_followed_by_a_command_is_not_fenced() {
        // The load-bearing case. Exact whole-utterance matching is what makes
        // this fall through: "hello rabbit drive forward one meter" is not the
        // entry "hello rabbit".
        for text in [
            "hello rabbit, drive forward one meter",
            "hey parker turn left",
            "rabbit, stop",
            "hello rabbit take me to the dock",
            "hey rabbit pull over",
        ] {
            assert_eq!(
                classify_non_motion(text),
                None,
                "must reach the model: {text:?}"
            );
        }
    }

    // --- false-positive resistance ------------------------------------------

    #[test]
    fn conversational_words_inside_a_command_do_not_fence_it() {
        // None of these may be caught merely for containing hello/see/status/stop.
        for text in [
            "drive to the hello sign",
            "go see the loading dock",
            "stop at the status board",
            "turn toward Parker Street",
            "how are you going to reach the dock",
            "are we okay to proceed to the ramp",
            "what do you see on the left, then go there",
        ] {
            assert_eq!(
                classify_non_motion(text),
                None,
                "must reach the model: {text:?}"
            );
        }
    }

    #[test]
    fn every_allowlist_entry_is_already_normalized() {
        // A non-normalized entry would be dead: nothing could ever equal it.
        for entry in WAKE_PHRASES.iter().chain(GREETINGS).chain(STATUS_QUESTIONS) {
            assert_eq!(
                &normalize(entry),
                entry,
                "allowlist entry is not in normalized form and can never match"
            );
        }
    }

    #[test]
    fn the_allowlists_do_not_overlap() {
        // Overlap would make the reported kind depend on match order, which
        // would quietly mislabel telemetry.
        let mut seen: Vec<&str> = Vec::new();
        for entry in WAKE_PHRASES.iter().chain(GREETINGS).chain(STATUS_QUESTIONS) {
            assert!(
                !seen.contains(entry),
                "duplicate allowlist entry: {entry:?}"
            );
            seen.push(entry);
        }
    }

    #[test]
    fn empty_and_blank_text_is_left_to_the_existing_empty_request_error() {
        assert_eq!(classify_non_motion(""), None);
        assert_eq!(classify_non_motion("   \t\n "), None);
        assert_eq!(classify_non_motion("?!."), None);
    }

    #[test]
    fn classification_is_pure_and_repeatable() {
        for text in ["hello rabbit", "drive forward one meter", "what do you see"] {
            let first = classify_non_motion(text);
            for _ in 0..5 {
                assert_eq!(classify_non_motion(text), first);
            }
        }
    }
}
