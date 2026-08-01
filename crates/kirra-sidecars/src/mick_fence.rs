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
//! **Compositional prefixes, by bounded grammar — not substrings.** Observed
//! live: `Hello Mr. Parker, how are you?` normalizes to
//! `hello mr parker how are you`, which is no exact entry, fell through to the
//! motion-only schema, and came back as `{"intent":"cruise","target_speed_mps":10}`.
//! The fix is a second, equally deterministic pass with a tiny fixed grammar:
//!
//! ```text
//! utterance := [greeting-head] [address] remainder
//!              (at most ONE of each, in that order, at least one present)
//! ```
//!
//! where `greeting-head` and `address` come from closed vocabularies below and
//! the REMAINDER must be, in its entirety, an existing `GREETINGS` /
//! `STATUS_QUESTIONS` entry (or empty → a bare wake). Every comparison is a
//! whole-token-sequence match at the start of the utterance — never a
//! substring, never repeated stripping. That is why the false-positive
//! resistance survives:
//!
//!   - `hello mr parker, pull over` strips its prefix but the remainder
//!     `pull over` is on no non-motion list → the model sees it;
//!   - `turn toward parker street` starts with no head/address token → the
//!     grammar never engages;
//!   - `how are you going to reach the dock` engages no prefix and is not,
//!     as a whole, a known clause → the model sees it;
//!   - the grammar consumes at most one head and one address, so it can never
//!     erase arbitrary leading text in front of a command.
//!
//! Anything not recognised goes to the model exactly as before. The fence can
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

/// Conversational GREETING-HEADS the bounded grammar may strip — the words an
/// operator opens with before addressing the robot. Closed vocabulary, matched
/// as a whole leading token sequence, consumed AT MOST ONCE.
const GREETING_HEADS: &[&str] = &[
    "hello",
    "hey",
    "hi",
    "yo",
    "good morning",
    "good afternoon",
    "good evening",
];

/// ADDRESS forms the bounded grammar may strip — the robot's names and their
/// honorific variants (`Mr.` normalizes to `mr`). Closed vocabulary, matched
/// as a whole leading token sequence, consumed AT MOST ONCE, longest first so
/// `mister parker` is never half-consumed as a bare name.
const ADDRESS_FORMS: &[&str] = &[
    "mister parker",
    "mister rabbit",
    "mr parker",
    "mr rabbit",
    "rabbit",
    "parker",
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

/// Strip `phrase` off the front of `s` at a TOKEN boundary: either `s` is
/// exactly `phrase`, or `s` continues with a space after it. Normalized input
/// guarantees single-space separation, so this is a whole-token-sequence
/// comparison — `parker` never matches the front of `parkersburg`.
fn strip_leading_phrase<'a>(s: &'a str, phrase: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(phrase)?;
    if rest.is_empty() {
        Some(rest)
    } else {
        rest.strip_prefix(' ')
    }
}

/// Strip ONE conversational prefix — `[greeting-head] [address]`, each from
/// its closed vocabulary, each consumed at most once, in that order — and
/// return the remainder (possibly empty). `None` when no prefix unit matched
/// at all.
///
/// Bounded by construction: two lookups against fixed lists, no repetition,
/// no arbitrary-token skipping — so the grammar can never erase meaningful
/// command text. What it CANNOT do is as load-bearing as what it can:
/// `drive`, `turn`, `stop`, `go` are in neither vocabulary, so a command-led
/// utterance never engages this path, and a command REMAINDER survives intact
/// for the caller to (fail to) match against the non-motion clause lists.
#[must_use]
pub fn strip_conversational_prefix(normalized: &str) -> Option<&str> {
    let mut rest = normalized;
    let mut consumed = false;
    if let Some(r) = GREETING_HEADS
        .iter()
        .find_map(|head| strip_leading_phrase(rest, head))
    {
        rest = r;
        consumed = true;
    }
    // ADDRESS_FORMS is ordered longest-first, so `mr parker` wins over any
    // shorter overlap before a bare name is tried.
    if let Some(r) = ADDRESS_FORMS
        .iter()
        .find_map(|addr| strip_leading_phrase(rest, addr))
    {
        rest = r;
        consumed = true;
    }
    consumed.then_some(rest)
}

/// Classify an utterance as deterministically non-motion, or `None` to let the
/// model decide.
///
/// Pure: no model call, no network, no I/O, no clock. `None` is the default for
/// everything unrecognised — the fence narrows what reaches the model, it never
/// widens what is accepted.
///
/// Two passes, both deterministic:
///  1. the original EXACT whole-utterance match against the three lists;
///  2. the bounded-grammar pass: strip one conversational prefix
///     ([`strip_conversational_prefix`]) and require the ENTIRE remainder to
///     be a known `GREETINGS` / `STATUS_QUESTIONS` clause. An empty remainder
///     is a bare wake (`good morning parker`, `mr parker`). Any other
///     remainder — `pull over`, `drive forward one meter`, anything — falls
///     through to the model.
///
/// The reported [`NonMotionKind`] follows the semantic REMAINDER when one
/// exists (`hello mr parker how are you` → `Greeting`;
/// `hey rabbit what do you see` → `StatusQuestion`); a bare prefix reports
/// `WakePhrase`, matching the existing telemetry for `hello rabbit`.
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
    // Bounded-grammar pass: one optional greeting-head, one optional address,
    // then the WHOLE remainder must already be a known non-motion clause.
    if let Some(rest) = strip_conversational_prefix(&normalized) {
        if rest.is_empty() {
            return Some(NonMotionKind::WakePhrase);
        }
        if GREETINGS.contains(&rest) {
            return Some(NonMotionKind::Greeting);
        }
        if STATUS_QUESTIONS.contains(&rest) {
            return Some(NonMotionKind::StatusQuestion);
        }
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

    // --- the compositional live regression (2026-08, observed on the R2) ----

    #[test]
    fn the_live_compositional_greeting_is_fenced_verbatim() {
        // This exact utterance fell through the exact-match fence and gemma
        // answered {"intent":"cruise","target_speed_mps":10}.
        assert_eq!(
            classify_non_motion("Hello Mr. Parker, how are you?"),
            Some(NonMotionKind::Greeting)
        );
    }

    #[test]
    fn required_compositional_non_motion_utterances_classify_by_remainder() {
        for (text, kind) in [
            ("Hello Mr. Parker, how are you?", NonMotionKind::Greeting),
            (
                "Hello Parker, how are your systems?",
                NonMotionKind::StatusQuestion,
            ),
            (
                "Hey Rabbit, what do you see?",
                NonMotionKind::StatusQuestion,
            ),
            ("Mr. Parker, are you there?", NonMotionKind::Greeting),
            (
                "Rabbit, what is your status?",
                NonMotionKind::StatusQuestion,
            ),
            ("Good morning Parker, how are you?", NonMotionKind::Greeting),
        ] {
            assert_eq!(classify_non_motion(text), Some(kind), "{text:?}");
        }
    }

    #[test]
    fn bare_prefix_combinations_report_wake_phrase() {
        // A head + address (or address alone) with NOTHING after it is the
        // operator getting the robot's attention — same telemetry kind as the
        // existing exact entries ("hello rabbit" → WakePhrase).
        for text in [
            "Hello Mr. Parker",
            "Good morning, Parker!",
            "Mr. Parker",
            "Mister Rabbit?",
            "hey mister parker",
        ] {
            assert_eq!(
                classify_non_motion(text),
                Some(NonMotionKind::WakePhrase),
                "{text:?}"
            );
        }
    }

    #[test]
    fn punctuation_and_casing_variants_of_the_live_phrase_all_fence() {
        for text in [
            "HELLO, MR. PARKER \u{2014} HOW ARE YOU?",
            "hello mr parker how are you",
            "Hi, Parker! What is your status?",
        ] {
            assert!(classify_non_motion(text).is_some(), "{text:?}");
        }
    }

    #[test]
    fn required_motion_utterances_still_reach_the_model() {
        // The spec's negative set: every one must return None — a prefix plus
        // a COMMAND remainder, a command containing conversational words, or a
        // question that is actually about motion.
        for text in [
            "Hello Parker, drive forward one meter",
            "Hey Rabbit, turn left",
            "Rabbit, stop",
            "Mr. Parker, stop",
            "Drive to the hello sign",
            "Turn toward Parker Street",
            "How are you going to reach the dock",
            "Are we okay to proceed to the ramp",
            "What do you see on the left, then go there",
            "Good morning, drive to the loading dock",
            "Hello Mr. Parker, pull over",
        ] {
            assert_eq!(classify_non_motion(text), None, "{text:?}");
        }
    }

    // --- systematic product coverage ----------------------------------------

    /// prefix (head, address, both) × every known non-motion clause → fenced,
    /// with the kind following the clause's own list.
    #[test]
    fn every_prefix_times_every_known_clause_is_fenced_with_the_clause_kind() {
        let mut checked = 0usize;
        for head in GREETING_HEADS.iter().map(Some).chain([None]) {
            for addr in ADDRESS_FORMS.iter().map(Some).chain([None]) {
                if head.is_none() && addr.is_none() {
                    continue; // no prefix at all — pass-1 territory, not ours
                }
                let prefix = [head, addr]
                    .iter()
                    .flatten()
                    .cloned()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ");
                for (clause, want) in GREETINGS
                    .iter()
                    .map(|c| (*c, NonMotionKind::Greeting))
                    .chain(
                        STATUS_QUESTIONS
                            .iter()
                            .map(|c| (*c, NonMotionKind::StatusQuestion)),
                    )
                {
                    let utterance = format!("{prefix} {clause}");
                    assert_eq!(classify_non_motion(&utterance), Some(want), "{utterance:?}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 300, "product coverage collapsed: {checked}");
    }

    /// prefix (head, address, both) × explicit MOTION clauses → never fenced.
    #[test]
    fn every_prefix_times_every_motion_clause_reaches_the_model() {
        const MOTION_CLAUSES: &[&str] = &[
            "drive forward one meter",
            "turn left",
            "stop",
            "pull over",
            "go to the dock",
            "proceed to the ramp",
            "take me to the loading dock",
            "reverse half a meter",
            "cruise at two meters per second",
            "follow the corridor",
        ];
        for head in GREETING_HEADS.iter().map(Some).chain([None]) {
            for addr in ADDRESS_FORMS.iter().map(Some).chain([None]) {
                let prefix = [head, addr]
                    .iter()
                    .flatten()
                    .cloned()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ");
                for clause in MOTION_CLAUSES {
                    let utterance = if prefix.is_empty() {
                        (*clause).to_string()
                    } else {
                        format!("{prefix} {clause}")
                    };
                    assert_eq!(
                        classify_non_motion(&utterance),
                        None,
                        "{utterance:?} must reach the model"
                    );
                }
            }
        }
    }

    // --- the grammar's bounds -----------------------------------------------

    #[test]
    fn the_prefix_grammar_is_bounded_and_token_anchored() {
        // At most one head + one address, in that order — a doubled head or a
        // reversed order does not parse, so nothing unexpected is consumed.
        assert_eq!(classify_non_motion("hello hello rabbit"), None);
        assert_eq!(classify_non_motion("parker good morning how are you"), None);
        // Token-anchored: names embedded in longer words never match.
        assert_eq!(classify_non_motion("parkersburg how are you"), None);
        assert_eq!(strip_leading_phrase("parkersburg ahead", "parker"), None);
        // The stripper itself: both units, one unit, no unit.
        assert_eq!(
            strip_conversational_prefix("hello mr parker how are you"),
            Some("how are you")
        );
        assert_eq!(strip_conversational_prefix("rabbit stop"), Some("stop"));
        assert_eq!(strip_conversational_prefix("good morning"), Some(""));
        assert_eq!(strip_conversational_prefix("drive forward"), None);
        assert_eq!(strip_conversational_prefix(""), None);
    }

    #[test]
    fn prefix_vocabularies_are_normalized_and_free_of_command_words() {
        for entry in GREETING_HEADS.iter().chain(ADDRESS_FORMS) {
            assert_eq!(&normalize(entry), entry, "non-normalized vocab entry");
            for tok in entry.split(' ') {
                assert!(
                    ![
                        "drive", "go", "turn", "stop", "cruise", "reverse", "pull", "proceed",
                        "follow", "take"
                    ]
                    .contains(&tok),
                    "command word in the prefix vocabulary: {entry:?}"
                );
            }
        }
        // Longest-first ordering of ADDRESS_FORMS is load-bearing (the doc on
        // the const): a shorter entry must never precede a longer one it
        // token-prefixes, or the longer form could be half-consumed.
        for (i, a) in ADDRESS_FORMS.iter().enumerate() {
            for b in &ADDRESS_FORMS[i + 1..] {
                assert!(
                    !b.starts_with(&format!("{a} ")),
                    "{a:?} precedes longer {b:?} it prefixes — ordering broken"
                );
            }
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
