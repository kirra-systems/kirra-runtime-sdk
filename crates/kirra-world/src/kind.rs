//! **`ObservationKind`** — what a claim asserts, and the per-kind body contract.
//!
//! # Ruled, not invented
//!
//! §7.1 names this field and the blueprint never enumerates its variants, so
//! [`observation`](crate::observation) refused to invent a taxonomy. The list
//! below is not this module's guess: it was proposed in
//! `WM_OBSERVATION_KIND_PROPOSAL.md` (`KIRRA-WM-OBSKIND-001`) and **adopted
//! 2026-08-07, option 2** — the three variants attested by what the system
//! actually writes, plus a degrade target.
//!
//! `Existence` was proposed as a fourth and **deferred**, on an asymmetry
//! rather than a preference: adding a variant later costs an additive enum
//! change, while removing one already written into a hash-chained log would
//! mean rewriting rows inside the chain. Its revisit trigger is a perception
//! producer with a saw-something-claiming-nothing output.
//!
//! # `ObservationKind` is not `EntityKind` renamed
//!
//! Worth stating because the two are easy to conflate, and conflating them
//! makes the payload vocabulary grow as 19 × *k* instead of *k*:
//!
//! * [`EntityKind`](crate::entity::EntityKind) answers *what is this thing* —
//!   `Robot`, `Door`, `Room`.
//! * `ObservationKind` answers *what does this claim assert about it*.
//!
//! A `Door` can be the subject of a spatial claim, an attribute claim and a
//! relationship claim; a spatial claim can be about a `Door`, a `Robot` or a
//! `Package`. They are orthogonal axes, not two spellings of one.
//!
//! # The tokens are fixed, and that is a hash constraint
//!
//! The storage layer's `kind` column is inside the canonically-hashed bytes
//! **twice** — as a distinct argument to the record hash and as a field in the
//! canonical JSON. Re-spelling `observation` as `attribute` would not be a
//! rename; it would change the digest of every row that carried it and break
//! chain verification on every existing store. [`ObservationKind::as_str`]
//! returns the stored token, and those tokens are frozen.
//!
//! # What this module does NOT do
//!
//! It does not change the storage layer's `kind` field from `&str` to this
//! enum. The ruling is explicit that doing so in the same slice is not
//! authorized — it touches the hashed bytes and needs its own compatibility
//! argument, exactly as the trust axes did. This types the *boundary*: a writer
//! converts on the way in, a reader adjudicates on the way out, and the bytes
//! are untouched.

use core::fmt;

/// What a claim asserts about its subject.
///
/// See the module docs for the ruling this list comes from, and for why it is
/// orthogonal to [`EntityKind`](crate::entity::EntityKind).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObservationKind {
    /// A property of one subject. The default, and the most-written value in
    /// every store.
    Attribute,
    /// Where a subject is.
    ///
    /// **Carries SD-4's frame requirement.** A spatial claim with no frame is
    /// refused by the storage schema, not by a caller's diligence, because a
    /// frameless spatial claim cannot later be shown *not* to have become
    /// checker geometry (ADR-0042 Decision 2).
    Spatial,
    /// That two subjects are related. The only kind for which the storage
    /// layer's `predicate` and `object` columns carry meaning.
    Relationship,
    /// **A kind this build does not recognise.**
    ///
    /// The degrade target, shaped after
    /// [`EntityKind::Unknown`](crate::entity::EntityKind::Unknown). Reached only
    /// by [`ObservationKind::from_token`]; there is no way to *write* one,
    /// because [`ObservationKind::as_str`] has no token for it — see that
    /// method.
    ///
    /// A kind arriving from a newer writer is the ordinary case in a fleet
    /// mid-rollout, not an exception, so degrading is the common path rather
    /// than the error path.
    Unrecognised,
}

impl ObservationKind {
    /// The stored token, or `None` for [`ObservationKind::Unrecognised`].
    ///
    /// # Why this returns `Option` rather than a token
    ///
    /// There is no token for `Unrecognised` because writing one would be
    /// claiming a vocabulary entry that does not exist. Returning `Option`
    /// makes "this kind cannot be written" a thing the caller must handle,
    /// rather than a placeholder string that round-trips and silently becomes
    /// a real stored value.
    ///
    /// It is the same move as
    /// [`EntityKind::group`](crate::entity::EntityKind::group) returning `None`
    /// for `Unknown`: the degrade target has no reading to give, so the type
    /// does not offer one.
    ///
    /// **These tokens are frozen** — they are inside the chain hash. See the
    /// module docs.
    #[must_use]
    pub fn as_str(self) -> Option<&'static str> {
        Some(match self {
            Self::Attribute => "observation",
            Self::Spatial => "spatial",
            Self::Relationship => "relationship",
            Self::Unrecognised => return None,
        })
    }

    /// Parse a stored token, degrading rather than guessing.
    ///
    /// An unrecognised token is **not** an error and **not** silently mapped to
    /// `Attribute`. §6.2's rule — *degrade to unknown, not a guessed supertype*
    /// — applies with more force here than for entities, because reading a
    /// newer writer's kind is routine during a rollout.
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "observation" => Self::Attribute,
            "spatial" => Self::Spatial,
            "relationship" => Self::Relationship,
            _ => Self::Unrecognised,
        }
    }

    /// Whether this kind requires a coordinate frame (SD-4).
    ///
    /// `Unrecognised` answers **`false`**, and that is deliberate rather than
    /// convenient: the storage `CHECK` keys on the literal token `'spatial'`,
    /// so a kind this build cannot name is not spatial *to the schema* either.
    /// Answering `true` here would put this type and the schema into
    /// disagreement about the same row — and the schema is the guarantee.
    #[must_use]
    pub fn requires_frame(self) -> bool {
        matches!(self, Self::Spatial)
    }

    /// Whether `predicate` and `object` carry meaning for this kind.
    #[must_use]
    pub fn uses_predicate_object(self) -> bool {
        matches!(self, Self::Relationship)
    }
}

impl fmt::Display for ObservationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_str() {
            Some(t) => f.write_str(t),
            None => f.write_str("<unrecognised>"),
        }
    }
}

// ---------------------------------------------------------------------------
// The per-kind versioned body — §7.1's TypedPayload
// ---------------------------------------------------------------------------

/// Why a body was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyError {
    /// The body was offered under a kind its type does not serve.
    ///
    /// Carries both so the message names the mismatch rather than only
    /// reporting that one happened.
    KindMismatch {
        /// The kind the body type declares.
        expected: ObservationKind,
        /// The kind it was offered under.
        offered: ObservationKind,
    },
    /// The body was offered at a schema version its type does not know.
    ///
    /// **Fail-closed, and specifically closed against the FUTURE.** A body
    /// written by a newer producer may carry fields this build cannot see, so
    /// decoding it as the version this build knows would silently drop them —
    /// and in an evidence log a silently truncated record is worse than an
    /// unread one.
    UnsupportedVersion {
        /// The version this build writes.
        supported: u32,
        /// The version offered.
        offered: u32,
    },
    /// The body's own content was refused by its type.
    Invalid(String),
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KindMismatch { expected, offered } => write!(
                f,
                "body serves kind `{expected}` but was offered under `{offered}`"
            ),
            Self::UnsupportedVersion { supported, offered } => write!(
                f,
                "body is schema version {offered}, this build supports {supported}"
            ),
            Self::Invalid(detail) => write!(f, "body refused: {detail}"),
        }
    }
}

/// A payload body that knows **which kind it serves and at which version**.
///
/// # What this contract buys, and what it deliberately does not
///
/// §7.1 asks for a per-kind versioned `TypedPayload`. The two halves are the
/// point:
///
/// * **Per-kind** — a body declares [`TypedBody::KIND`], so a spatial body
///   cannot be attached to a relationship claim by mistake. That check is
///   [`TypedBody::admit`], and it is a *function of the type*, not a
///   convention a writer follows.
/// * **Versioned** — a body declares [`TypedBody::SCHEMA_VERSION`], which is
///   what the storage layer's `payload_schema` column has always held without
///   anything defining what it meant.
///
/// **The stored bytes are unchanged.** This types the boundary: a writer
/// encodes on the way in, a reader adjudicates on the way out. The ruling that
/// authorized `ObservationKind` explicitly did not authorize changing the
/// stored `kind` field in the same slice, and the same reasoning applies to the
/// payload — both are inside the canonically-hashed bytes, and moving them
/// needs its own compatibility argument.
///
/// # Why encoding is the implementor's job
///
/// This trait does not require `serde`, a wire format, or any particular
/// encoding. `kirra-world` has **zero dependencies** by ratification criterion,
/// so a trait demanding a serializer would either pull one in or fake one. The
/// body owns its own `encode`/`decode`, and the contract is only about *kind*
/// and *version* — the two things that decide whether decoding should be
/// attempted at all.
pub trait TypedBody: Sized {
    /// The kind this body serves.
    const KIND: ObservationKind;

    /// The schema version this build writes and can read.
    ///
    /// Corresponds to the storage layer's `payload_schema` column, which held
    /// a number with no stated meaning until now.
    const SCHEMA_VERSION: u32;

    /// Validate the body's own content.
    ///
    /// Default: nothing to check. A body with invariants overrides this; one
    /// without says so by not overriding, which is a smaller lie than an empty
    /// override that looks like a check.
    ///
    /// # Errors
    ///
    /// [`BodyError::Invalid`] with a reason the body's author chooses.
    fn validate(&self) -> Result<(), BodyError> {
        Ok(())
    }

    /// Admit this body under `kind` at `version`, or refuse.
    ///
    /// The order is deliberate: **kind, then version, then content.** A body
    /// offered under the wrong kind is not a version problem, and reporting the
    /// version mismatch of a body that should never have been considered would
    /// send a reader to the wrong question.
    ///
    /// # Errors
    ///
    /// [`BodyError::KindMismatch`], [`BodyError::UnsupportedVersion`], or
    /// whatever [`TypedBody::validate`] returns.
    fn admit(&self, kind: ObservationKind, version: u32) -> Result<(), BodyError> {
        if kind != Self::KIND {
            return Err(BodyError::KindMismatch {
                expected: Self::KIND,
                offered: kind,
            });
        }
        if version != Self::SCHEMA_VERSION {
            return Err(BodyError::UnsupportedVersion {
                supported: Self::SCHEMA_VERSION,
                offered: version,
            });
        }
        self.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A body for each attested kind, minimal on purpose — the contract is what
    // is under test, not any particular payload shape.
    #[derive(Debug, PartialEq)]
    struct Colour(&'static str);
    impl TypedBody for Colour {
        const KIND: ObservationKind = ObservationKind::Attribute;
        const SCHEMA_VERSION: u32 = 1;
        fn validate(&self) -> Result<(), BodyError> {
            if self.0.is_empty() {
                return Err(BodyError::Invalid("colour is empty".into()));
            }
            Ok(())
        }
    }

    #[derive(Debug)]
    struct Pose;
    impl TypedBody for Pose {
        const KIND: ObservationKind = ObservationKind::Spatial;
        const SCHEMA_VERSION: u32 = 2;
    }

    // -----------------------------------------------------------------------
    // The ruled vocabulary
    // -----------------------------------------------------------------------

    /// The three tokens are FROZEN — they are inside the chain hash twice, so
    /// re-spelling one breaks verification on every existing store.
    #[test]
    fn the_stored_tokens_are_exactly_what_the_log_already_contains() {
        assert_eq!(ObservationKind::Attribute.as_str(), Some("observation"));
        assert_eq!(ObservationKind::Spatial.as_str(), Some("spatial"));
        assert_eq!(ObservationKind::Relationship.as_str(), Some("relationship"));
    }

    /// The degrade target has no token, so it cannot be written.
    #[test]
    fn unrecognised_has_no_token_to_write() {
        assert_eq!(ObservationKind::Unrecognised.as_str(), None);
    }

    /// An unknown token degrades rather than guessing — and specifically does
    /// NOT become `Attribute`, which is the guess a "sensible default" would
    /// make.
    #[test]
    fn an_unknown_token_degrades_rather_than_guessing() {
        for token in ["existence", "prediction", "", "OBSERVATION", "spatial "] {
            assert_eq!(
                ObservationKind::from_token(token),
                ObservationKind::Unrecognised,
                "{token:?} must degrade"
            );
        }
    }

    /// Round trip for every writable variant.
    #[test]
    fn every_writable_kind_round_trips() {
        for k in [
            ObservationKind::Attribute,
            ObservationKind::Spatial,
            ObservationKind::Relationship,
        ] {
            let token = k.as_str().expect("writable");
            assert_eq!(ObservationKind::from_token(token), k);
        }
    }

    /// `Existence` was DEFERRED, not adopted. Its token must degrade — if
    /// someone adds the variant without the ruling, this is what fails.
    #[test]
    fn the_deferred_existence_kind_is_not_in_the_vocabulary() {
        assert_eq!(
            ObservationKind::from_token("existence"),
            ObservationKind::Unrecognised
        );
    }

    /// SD-4's frame requirement rides on `Spatial` alone — and `Unrecognised`
    /// answers `false` so this type and the schema agree about the same row.
    #[test]
    fn only_spatial_requires_a_frame() {
        assert!(ObservationKind::Spatial.requires_frame());
        assert!(!ObservationKind::Attribute.requires_frame());
        assert!(!ObservationKind::Relationship.requires_frame());
        assert!(
            !ObservationKind::Unrecognised.requires_frame(),
            "the schema CHECK keys on the literal token, so an unnameable kind \
             is not spatial to it either"
        );
    }

    #[test]
    fn only_relationships_use_predicate_and_object() {
        assert!(ObservationKind::Relationship.uses_predicate_object());
        assert!(!ObservationKind::Attribute.uses_predicate_object());
        assert!(!ObservationKind::Spatial.uses_predicate_object());
        assert!(!ObservationKind::Unrecognised.uses_predicate_object());
    }

    // -----------------------------------------------------------------------
    // The per-kind versioned body
    // -----------------------------------------------------------------------

    #[test]
    fn a_body_is_admitted_under_its_own_kind_and_version() {
        assert!(Colour("red").admit(ObservationKind::Attribute, 1).is_ok());
        assert!(Pose.admit(ObservationKind::Spatial, 2).is_ok());
    }

    /// **The per-kind half.** A spatial body cannot be attached to a
    /// relationship claim, and the refusal names both sides.
    #[test]
    fn a_body_offered_under_the_wrong_kind_is_refused() {
        let err = Pose
            .admit(ObservationKind::Relationship, 2)
            .expect_err("must refuse");
        assert_eq!(
            err,
            BodyError::KindMismatch {
                expected: ObservationKind::Spatial,
                offered: ObservationKind::Relationship,
            }
        );
    }

    /// **The versioned half, failing closed against the FUTURE.** A body from a
    /// newer producer may carry fields this build cannot see, so decoding it as
    /// the known version would silently drop them — and in an evidence log a
    /// silently truncated record is worse than an unread one.
    #[test]
    fn a_body_from_a_newer_producer_is_refused_not_truncated() {
        let err = Pose
            .admit(ObservationKind::Spatial, 3)
            .expect_err("must refuse");
        assert_eq!(
            err,
            BodyError::UnsupportedVersion {
                supported: 2,
                offered: 3
            }
        );
    }

    /// An older version is refused too. Reading v1 as v2 would invent whatever
    /// v2 added, which is the same defect pointed the other way.
    #[test]
    fn an_older_version_is_refused_as_well() {
        assert!(matches!(
            Pose.admit(ObservationKind::Spatial, 1),
            Err(BodyError::UnsupportedVersion { .. })
        ));
    }

    /// **Kind before version.** A body offered under the wrong kind should
    /// never have been considered, so reporting its version mismatch would send
    /// a reader to the wrong question.
    #[test]
    fn kind_is_checked_before_version() {
        let err = Pose
            .admit(ObservationKind::Attribute, 99)
            .expect_err("must refuse");
        assert!(
            matches!(err, BodyError::KindMismatch { .. }),
            "both are wrong; the kind is the one to report, got {err:?}"
        );
    }

    /// …and content last, after both structural checks pass.
    #[test]
    fn content_is_validated_only_once_kind_and_version_agree() {
        let err = Colour("")
            .admit(ObservationKind::Attribute, 1)
            .expect_err("must refuse");
        assert_eq!(err, BodyError::Invalid("colour is empty".into()));

        // Wrong kind AND empty content: the kind is reported, not the content.
        assert!(matches!(
            Colour("").admit(ObservationKind::Spatial, 1),
            Err(BodyError::KindMismatch { .. })
        ));
    }

    /// A body with no invariants says so by not overriding `validate`, rather
    /// than by an empty override that looks like a check.
    #[test]
    fn a_body_without_invariants_validates_trivially() {
        assert!(Pose.validate().is_ok());
    }

    #[test]
    fn errors_name_the_mismatch_rather_than_only_reporting_one() {
        assert_eq!(
            BodyError::KindMismatch {
                expected: ObservationKind::Spatial,
                offered: ObservationKind::Attribute,
            }
            .to_string(),
            "body serves kind `spatial` but was offered under `observation`"
        );
        assert_eq!(
            BodyError::UnsupportedVersion {
                supported: 2,
                offered: 7
            }
            .to_string(),
            "body is schema version 7, this build supports 2"
        );
    }

    /// `Unrecognised` displays as something a reader can act on rather than as
    /// a token that might be mistaken for a stored value.
    #[test]
    fn the_degrade_target_displays_as_unwritable() {
        assert_eq!(ObservationKind::Unrecognised.to_string(), "<unrecognised>");
        assert_eq!(ObservationKind::Attribute.to_string(), "observation");
    }
}
