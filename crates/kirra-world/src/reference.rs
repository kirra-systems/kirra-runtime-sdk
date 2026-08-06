//! **Opaque references** — identity and spatial-reference newtypes.
//!
//! Five types that name something outside this crate's type system: two
//! identities ([`EventId`], [`ObservationId`]) and two spatial references
//! ([`FrameId`], [`MapId`]). They are grouped here rather than split across
//! §7's observation module and §6's entity module because they share one
//! shape and one set of rules, and reading those rules once is better than
//! reading them four times.
//!
//! # What these replace
//!
//! Four crate-root placeholders — `ObservationId(())`, `FrameId(())`,
//! `MapId(())` — that were unconstructible by anything importing them. The
//! storage layer meanwhile carried all four as bare `&str`. See
//! [`WM_Q1_SEAM_BASELINE.md`](../../../docs/design/WM_Q1_SEAM_BASELINE.md) for
//! the measurement that found them.
//!
//! # The rule these make structural
//!
//! Distinct types for values that are all "a short string" is not ceremony
//! here; it is the only thing standing between two adjacent parameters and a
//! silent swap. Concretely, the storage layer's append path took
//!
//! ```text
//! event_id: &str,  observation_id: &str,      // adjacent, same type
//! frame_id: Option<&str>,  map_id: Option<&str>,   // adjacent, same type
//! ```
//!
//! A caller passing those two pairs in the wrong order compiled, wrote, and
//! hashed. The `event_id`/`observation_id` swap is the more damaging of the
//! two — re-attribution is defined by those being *different* identities, so
//! swapping them silently claims an observation is its own event record. The
//! `frame_id`/`map_id` swap is the more evasive: it also satisfies the SD-4
//! check that a spatial claim carries a frame, because a frame is *present* —
//! just the wrong one, in the position ADR-0042 Decision 2 draws its boundary
//! around. Neither is representable now.
//!
//! This is the same defect class as the `EntityId` collision fixed in #1375,
//! one layer down: there, two types shared a name; here, four values shared a
//! type.
//!
//! ## Asserted, not just described
//!
//! The two doctests below are the negative controls for that claim, and they
//! are written as a **pair on purpose**. A lone `compile_fail` test is weak
//! evidence: it passes when the code fails to compile for *any* reason, so a
//! typo in it looks exactly like the property holding. Each pair differs by a
//! single token — one type name — so the positive half failing would be the
//! thing that catches a mistake in the negative half.
//!
//! An [`EventId`] is usable where an `EventId` is wanted:
//!
//! ```
//! use kirra_world::reference::{EventId, ObservationId};
//! let event = EventId::new("evt-1").unwrap();
//! let _: &EventId = &event;
//! ```
//!
//! …and nowhere else. There is no `From`, no `Deref` and no common trait that
//! would let this through:
//!
//! ```compile_fail
//! use kirra_world::reference::{EventId, ObservationId};
//! let event = EventId::new("evt-1").unwrap();
//! let _: &ObservationId = &event;
//! ```
//!
//! The same for the spatial pair — a [`FrameId`] is not a [`MapId`]:
//!
//! ```
//! use kirra_world::reference::{FrameId, MapId};
//! let frame = FrameId::new("base_link").unwrap();
//! let _: &FrameId = &frame;
//! ```
//!
//! ```compile_fail
//! use kirra_world::reference::{FrameId, MapId};
//! let frame = FrameId::new("base_link").unwrap();
//! let _: &MapId = &frame;
//! ```
//!
//! Because the storage layer's `NewEvent` declares those exact field types,
//! rejecting the assignment here is what rejects the swap there.
//!
//! # Verbatim, never normalized — and why that is load-bearing
//!
//! Every constructor here either **accepts a value unchanged or refuses it**.
//! None trims, lowercases, or rewrites. That is not a stylistic preference:
//! the storage layer's chain verifier rebuilds each record from its *stored*
//! strings and rehashes it, so a constructor that normalized on the way back
//! in would produce bytes the original write never produced, and every
//! affected row would report as tampered. A validating constructor may reject;
//! it may not edit.
//!
//! Rejection on the read path is a different and honest outcome: it means a
//! stored value is not one this type admits, which is a corrupt row rather
//! than a broken chain — a distinction the storage layer already draws.
//!
//! # Why generation is not here
//!
//! `WM_SCOPE.md` lists `observation_id` as needing ULID. ULID generation needs
//! a dependency and a clock; this crate has **zero dependencies** by
//! ratification criterion, checked in CI. So the split is: this crate owns the
//! *type* and what makes a value admissible, and whichever layer has a clock
//! and a random source **mints** one. Nothing is lost — an id that came from
//! somewhere else (a replayed log, an operator import, another fleet) has to
//! be admissible anyway, so validation could never have been folded into
//! generation.

use core::fmt;

/// The longest reference this crate admits, in bytes.
///
/// A ULID is 26 characters and a UUID is 36, so this is roomy for a namespaced
/// or prefixed identifier while still being a bound. It **is** a bound rather
/// than a formality: ADR-0041 OQ2's retention horizons are derived from a
/// bytes-per-event figure, and a column with no upper bound makes that figure
/// — and therefore the horizons computed from it — underivable rather than
/// merely imprecise.
pub const MAX_REFERENCE_LEN: usize = 128;

/// Why a reference was refused.
///
/// Carries no offending value. These are constructed from data that may have
/// arrived from outside, and an error type that echoes its input invites that
/// input into a log line, where a control character is exactly the thing that
/// makes the log ambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceError {
    /// Empty, or nothing but whitespace.
    ///
    /// An empty identity is not a missing one: `Some("")` reads as present at
    /// every layer that checks for presence — including the SD-4 spatial-frame
    /// check — while naming nothing. `None` is how absence is said.
    Empty,
    /// Longer than [`MAX_REFERENCE_LEN`] bytes.
    TooLong {
        /// What was offered, in bytes.
        len: usize,
    },
    /// Contained an ASCII control character.
    ///
    /// These values are written into a canonically-hashed JSON form, exported
    /// in audit trails and printed in operator diagnostics. A newline or a NUL
    /// inside an identifier makes every line-oriented reader of those outputs
    /// ambiguous, and the ambiguity appears far from the write that caused it.
    ControlCharacter,
}

impl fmt::Display for ReferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "reference is empty or only whitespace"),
            Self::TooLong { len } => write!(
                f,
                "reference is {len} bytes, over the {MAX_REFERENCE_LEN}-byte limit"
            ),
            Self::ControlCharacter => {
                write!(f, "reference contains an ASCII control character")
            }
        }
    }
}

/// Shared admissibility. Returns the value **unchanged** on success.
fn admit(v: String) -> Result<String, ReferenceError> {
    if v.trim().is_empty() {
        return Err(ReferenceError::Empty);
    }
    if v.len() > MAX_REFERENCE_LEN {
        return Err(ReferenceError::TooLong { len: v.len() });
    }
    if v.chars().any(char::is_control) {
        return Err(ReferenceError::ControlCharacter);
    }
    Ok(v)
}

/// Declares one opaque reference newtype. The types are deliberately *not*
/// convertible into one another — that is the whole point — so there is no
/// shared trait and no `From` between them.
macro_rules! reference_newtype {
    ($(#[$meta:meta])* $name:ident, $what:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Admit a ", $what, ".")]
            ///
            /// The value is stored **verbatim** — see the module docs on why
            /// normalizing here would corrupt chain verification.
            ///
            /// # Errors
            ///
            /// [`ReferenceError`] if the value is empty, over
            /// [`MAX_REFERENCE_LEN`] bytes, or contains a control character.
            pub fn new(v: impl Into<String>) -> Result<Self, ReferenceError> {
                admit(v.into()).map(Self)
            }

            #[doc = concat!("The ", $what, ", exactly as admitted.")]
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[doc = concat!("Consume this ", $what, ", returning the inner string.")]
            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

reference_newtype! {
    /// Identity of one **record** in the evidence log.
    ///
    /// Distinct from [`ObservationId`] and not interchangeable with it. An
    /// observation can be re-attributed — recorded again against a different
    /// entity, with different confidence, on different authority — and each
    /// such record is its own event. That is what makes re-attribution
    /// non-destructive: the observation keeps its identity while a new event
    /// carries the new claim.
    ///
    /// Collapse the two and the log can no longer answer *"how many times has
    /// this been reconsidered, and by whom"*, which is the question §9's
    /// provenance model exists to answer.
    EventId, "record identity"
}

reference_newtype! {
    /// Identity of a single recorded **observation**.
    ///
    /// The blueprint's model is evidence-first: an observation outlives
    /// whatever entity it was later attributed to, so this identity must
    /// survive re-attribution unchanged. See [`EventId`] for the other half of
    /// that pairing.
    ///
    /// This is also the identity SD-3's provenance lists are made of — a claim
    /// records the observation ids it rests on, which is what lets a claim be
    /// re-judged when the evidence under it changes.
    ObservationId, "observation identity"
}

reference_newtype! {
    /// The coordinate frame a spatial claim is expressed in.
    ///
    /// # The boundary this type sits on
    ///
    /// ADR-0042 Decision 2 separates a *semantic* map from the checker's
    /// *authoritative* corridor. A frame named here may help an untrusted doer
    /// choose a goal; it must never become the checker's geometry by virtue of
    /// existing. Nothing in the safety closure reads this type, and the
    /// bidirectional fence plus gate test t24 are what keep that true — this
    /// doc is the reason, not the enforcement.
    ///
    /// SD-4 requires a spatial claim to carry one. That requirement is a schema
    /// `CHECK` in the storage layer, not a property of this type: whether a
    /// given claim is spatial is not knowable from the frame alone.
    FrameId, "coordinate frame reference"
}

reference_newtype! {
    /// Which map a spatial claim is relative to.
    ///
    /// See [`FrameId`] — same boundary, same prohibition. If Kirra World and
    /// the safety path ever read the same map artifact, that is a reviewed
    /// `[[shared_source_artifact]]` entry, not an implicit permission acquired
    /// by both sides happening to name the same string.
    MapId, "map reference"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_an_ordinary_identifier_verbatim() {
        let id = ObservationId::new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("admissible");
        assert_eq!(id.as_str(), "01ARZ3NDEKTSV4RRFFQ69G5FAV");
    }

    /// The load-bearing one. A constructor that trimmed would rewrite stored
    /// bytes on the read path and report untampered rows as broken chains.
    #[test]
    fn surrounding_whitespace_is_preserved_not_trimmed() {
        let id = ObservationId::new(" obs-1 ").expect("admissible");
        assert_eq!(id.as_str(), " obs-1 ", "constructor must not normalize");
    }

    #[test]
    fn case_is_preserved() {
        assert_eq!(FrameId::new("Base_Link").unwrap().as_str(), "Base_Link");
    }

    #[test]
    fn empty_and_whitespace_only_are_refused() {
        assert_eq!(ObservationId::new(""), Err(ReferenceError::Empty));
        assert_eq!(ObservationId::new("   "), Err(ReferenceError::Empty));
        assert_eq!(ObservationId::new("\t\n"), Err(ReferenceError::Empty));
    }

    #[test]
    fn control_characters_are_refused() {
        assert_eq!(
            ObservationId::new("obs\n1"),
            Err(ReferenceError::ControlCharacter)
        );
        assert_eq!(
            ObservationId::new("obs\u{0}1"),
            Err(ReferenceError::ControlCharacter)
        );
    }

    #[test]
    fn the_length_bound_is_inclusive_at_the_limit() {
        let at = "a".repeat(MAX_REFERENCE_LEN);
        assert!(
            ObservationId::new(at).is_ok(),
            "the limit itself is admissible"
        );

        let over = "a".repeat(MAX_REFERENCE_LEN + 1);
        assert_eq!(
            ObservationId::new(over),
            Err(ReferenceError::TooLong {
                len: MAX_REFERENCE_LEN + 1
            })
        );
    }

    /// Length is counted in **bytes**, matching what the storage row costs and
    /// what the retention horizons are derived from. A char count would let a
    /// multi-byte identifier exceed the byte bound the limit exists to impose.
    #[test]
    fn length_is_bytes_not_characters() {
        let multibyte = "é".repeat(MAX_REFERENCE_LEN / 2 + 1); // 2 bytes each
        assert!(multibyte.chars().count() <= MAX_REFERENCE_LEN);
        assert!(multibyte.len() > MAX_REFERENCE_LEN);
        assert!(matches!(
            ObservationId::new(multibyte),
            Err(ReferenceError::TooLong { .. })
        ));
    }

    /// Non-control, non-ASCII content is ordinary and admissible — an operator
    /// naming a room in their own language is not an error.
    #[test]
    fn non_ascii_is_admissible() {
        assert_eq!(MapId::new("étage-2").unwrap().as_str(), "étage-2");
    }

    /// Two references with the same text are equal *within* a type. Across
    /// types they cannot be compared at all, which is the point — the
    /// compile-fail half is asserted in the storage layer's tests, where the
    /// swap it prevents was actually possible.
    #[test]
    fn equality_is_within_a_type() {
        assert_eq!(
            ObservationId::new("x").unwrap(),
            ObservationId::new("x").unwrap()
        );
        assert_ne!(
            ObservationId::new("x").unwrap(),
            ObservationId::new("y").unwrap()
        );
    }

    #[test]
    fn display_and_into_inner_round_trip() {
        let e = EventId::new("evt-1").unwrap();
        assert_eq!(e.to_string(), "evt-1");
        assert_eq!(e.into_inner(), "evt-1");
    }

    #[test]
    fn errors_describe_themselves_without_echoing_input() {
        assert_eq!(
            ReferenceError::Empty.to_string(),
            "reference is empty or only whitespace"
        );
        assert_eq!(
            ReferenceError::TooLong { len: 200 }.to_string(),
            "reference is 200 bytes, over the 128-byte limit"
        );
        assert_eq!(
            ReferenceError::ControlCharacter.to_string(),
            "reference contains an ASCII control character"
        );
    }
}
