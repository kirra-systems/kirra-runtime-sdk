//! **Persisting an identity adjudication as evidence** — §6.3, §14.3, ADR-0041.
//!
//! `kirra_world::adjudication` has had the four verbs since 2026-08-07 with
//! nowhere to put them. This is the door: an [`IdentityAdjudication`] becomes a
//! `world_events` row, and comes back out through the domain's own
//! constructors.
//!
//! # Why there is no `identity_adjudications` table
//!
//! ADR-0041's provisional table list names one — *"`identity_adjudications`,
//! merge/split events, resolvable forever"* — sitting unannotated among the
//! entries marked *derived*. Read as a second **writable** table it contradicts
//! the parenthetical three lines above it, which fixes `world_events` as *"the
//! only writable table"*. Stating the reading rather than letting it pass as
//! obvious, because the two readings build different systems:
//!
//! **Adjudications are `world_events` rows, and `identity_adjudications` is a
//! projection over them.** Three things agree, and nothing argues the other way:
//!
//! 1. **The blueprint settles it directly.** §6.3: *"A query at a past instant
//!    resolves identity as it was adjudicated then — because identity is a
//!    projection like everything else."*
//! 2. **The ratified v1 schema already anticipated it.**
//!    `world_events.retention_class` has carried `'adjudication'` in its closed
//!    vocabulary since the baseline, and [`crate::compaction::is_protected`]
//!    holds for it — so an adjudication row is never compacted. That is
//!    precisely what §6.3's *"resolvable forever"* requires, built before there
//!    was anything to put in it.
//!
//!    Naming the *predicate* rather than [`crate::compaction::PROTECTED_CLASSES`],
//!    because the two are not the same thing and the difference is easy to
//!    state wrongly: `is_protected` is `retention_class != "raw"`, so
//!    **everything except raw is protected** and the constant is the
//!    enumeration OQ2 ruled, kept as documentation. Editing that array would
//!    not change what compacts. This module's guarantee rests on the predicate,
//!    and the test that proves it asks the compaction *planner* rather than
//!    comparing a token.
//! 3. **The chain.** The hash chain lives on `world_events`. A second evidence
//!    table would need its own chain or would be unchained, and an unchained
//!    adjudication is exactly the editable-history failure §6.3 exists to
//!    prevent.
//!
//! # The decoder goes back through the constructors
//!
//! [`decode_adjudication`] never assembles a verb field by field. It calls
//! `MergeEntities::new`, `SplitEntity::partition`, `SplitEntity::subtract`,
//! `ForgetEntity::new`, `AssertIdentity::new` — so **every refusal those
//! constructors make is a refusal at the storage boundary**, structurally rather
//! than by a matching list of checks that could drift.
//!
//! That is what carries the cardinality check `kirra_world::resolution`
//! deliberately declined. The resolver refuses an *empty* supersession because
//! it is unanswerable, but answers a one-element one rather than discarding a
//! correct answer — recording that rejecting a malformed row *"belongs at the
//! decoding boundary, fail-closed, where the store can refuse it before it is
//! ever walked."* This is that boundary: a stored partition naming fewer than
//! two destinations is refused here by `SplitEntity::partition` itself, so the
//! row never becomes a `Lifecycle::Superseded` for the resolver to meet.

use kirra_world::adjudication::{
    AssertIdentity, ForgetEntity, IdentityAdjudication, Justification, MergeEntities,
    RetirementReason, SplitEntity, SplitShape,
};
use kirra_world::observation::{ClockDomain, DomainInstant};
use kirra_world::reference::{EntityId, ObservationId};

use crate::WriterClass;

/// The `world_events.kind` token every adjudication row carries.
///
/// One kind for all four verbs, with the verb inside the payload, rather than
/// four kinds. The `kind` column answers *"what sort of claim is this"* for
/// indexing and for the SD-4 spatial constraint; which judgement was made is the
/// payload's business, and splitting it across both would give two places to
/// look and two places to disagree.
pub const ADJUDICATION_KIND: &str = "identity_adjudication";

/// The `world_events.retention_class` every adjudication row carries.
///
/// Not a choice this module gets to make. §6.3 requires a merged id to stay
/// resolvable forever, and [`crate::compaction::is_protected`] is what delivers
/// that — it holds for every class except `"raw"`, so writing a row as `"raw"`
/// is the one spelling that makes it compactable and deletes the redirect.
///
/// Note the predicate is the *mechanism*, not
/// [`crate::compaction::PROTECTED_CLASSES`]: that constant enumerates what OQ2
/// ruled and is not consulted by the compaction path, so editing it changes
/// nothing.
pub const ADJUDICATION_RETENTION_CLASS: &str = "adjudication";

/// Version of the payload encoding below.
pub const ADJUDICATION_PAYLOAD_SCHEMA: i64 = 1;

/// Why a stored adjudication could not be read back.
///
/// **Every variant is a refusal, never a repair.** A row that cannot be decoded
/// faithfully is not turned into a best-effort verb: an adjudication is the
/// record of a judgement, and a guessed one is worse than a missing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdjudicationDecodeError {
    /// The payload is not JSON, or not an object.
    Malformed {
        /// What went wrong, without echoing the payload back.
        detail: String,
    },
    /// The payload's schema version is not one this build can read.
    ///
    /// Fail-closed on a **newer** payload for the same reason
    /// `assert_schema_not_future` refuses a newer database: an older binary
    /// cannot know what a later encoding means, and reading it optimistically
    /// is how a field silently goes missing.
    UnsupportedSchema {
        /// The version found.
        found: i64,
        /// The version this build writes and reads.
        supported: i64,
    },
    /// A required key is absent, or holds the wrong JSON type.
    Field {
        /// Which key.
        key: &'static str,
        /// What was expected of it.
        detail: String,
    },
    /// The verb token is not one of the four.
    UnknownVerb {
        /// The token found.
        token: String,
    },
    /// The split-shape token is neither `partition` nor `subtraction`.
    UnknownSplitShape {
        /// The token found.
        token: String,
    },
    /// The clock-domain token is neither `boundary` nor `system`.
    UnknownClockDomain {
        /// The token found.
        token: String,
    },
    /// **The decoded values are not one the domain admits.**
    ///
    /// The whole reason decoding runs through the constructors. A partition
    /// naming one destination, a merge into one of its own sources, a duplicate
    /// citation, an empty justification — each is refused here by the same code
    /// that refuses it at the point of judgement, so the storage boundary cannot
    /// be more permissive than the model.
    Inadmissible {
        /// The domain's own refusal, rendered.
        detail: String,
    },
}

impl core::fmt::Display for AdjudicationDecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Malformed { detail } => write!(f, "malformed adjudication payload: {detail}"),
            Self::UnsupportedSchema { found, supported } => write!(
                f,
                "adjudication payload schema {found} is not readable by this build (supports {supported})"
            ),
            Self::Field { key, detail } => write!(f, "adjudication payload field `{key}`: {detail}"),
            Self::UnknownVerb { token } => write!(f, "unknown adjudication verb `{token}`"),
            Self::UnknownSplitShape { token } => write!(f, "unknown split shape `{token}`"),
            Self::UnknownClockDomain { token } => write!(f, "unknown clock domain `{token}`"),
            Self::Inadmissible { detail } => {
                write!(f, "stored adjudication is inadmissible: {detail}")
            }
        }
    }
}

impl std::error::Error for AdjudicationDecodeError {}

/// **Which entity a row's `subject` column names.**
///
/// One column, and an adjudication can touch several entities, so this is a
/// choice rather than a derivation. It names the entity the judgement is *about*:
/// the survivor of a merge, the source of a split, the entity asserted or
/// retired.
///
/// # What this column is NOT
///
/// **Not sufficient to find every entity an adjudication affects.** A merge's
/// sources are the entities whose lifecycle changes, and they are in the
/// payload, not here. The projection folds every adjudication row and reads
/// payloads; it does not query by subject. Saying so because a subject index
/// that looks like it answers "what happened to entity X" — and silently misses
/// the merged-away ids, which are exactly the ones a caller is asking about — is
/// worse than no index at all.
#[must_use]
pub fn adjudication_subject(a: &IdentityAdjudication) -> &EntityId {
    match a {
        IdentityAdjudication::Assert(x) => x.entity(),
        IdentityAdjudication::Merge(m) => m.into_entity(),
        IdentityAdjudication::Split(s) => s.source(),
        IdentityAdjudication::Forget(f) => f.entity(),
    }
}

/// The observation ids a row's `provenance` column should carry.
///
/// [`Justification`] *is* provenance in the store's sense — §14.1's `Evidence`
/// was ruled to mean "the observations that justify the judgement", and
/// `NewEvent::provenance` is documented as "observation ids this claim rests on
/// (SD-3)". The same set, so it is carried in the column that already means it
/// rather than duplicated into the payload's own list.
#[must_use]
pub fn adjudication_provenance(a: &IdentityAdjudication) -> Vec<String> {
    a.justification()
        .observations()
        .iter()
        .map(|o| o.as_str().to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

fn instant_json(at: DomainInstant) -> serde_json::Value {
    serde_json::json!({
        "ms": at.ms,
        // The domain is stored, never defaulted. ADR-0006's non-mixing rule is
        // only enforceable if a stored instant still knows which clock it came
        // from; a row that lost it could be compared with one that did not.
        "domain": match at.domain {
            ClockDomain::Boundary => "boundary",
            ClockDomain::System => "system",
        },
    })
}

fn ids_json(ids: &[EntityId]) -> serde_json::Value {
    serde_json::Value::Array(
        ids.iter()
            .map(|e| serde_json::Value::String(e.as_str().to_owned()))
            .collect(),
    )
}

/// Encode an adjudication as the row's `payload`.
///
/// The justification is **not** in here — it rides `provenance`, the column that
/// already means it. See [`adjudication_provenance`].
#[must_use]
pub fn encode_adjudication(a: &IdentityAdjudication) -> String {
    let body = match a {
        IdentityAdjudication::Assert(x) => serde_json::json!({
            "verb": "assert",
            "entity": x.entity().as_str(),
            "at": instant_json(x.at()),
        }),
        IdentityAdjudication::Merge(m) => serde_json::json!({
            "verb": "merge",
            "sources": ids_json(m.sources()),
            "into": m.into_entity().as_str(),
            "at": instant_json(m.at()),
        }),
        IdentityAdjudication::Split(s) => serde_json::json!({
            "verb": "split",
            "shape": match s.shape() {
                SplitShape::Partition => "partition",
                SplitShape::Subtraction => "subtraction",
            },
            "source": s.source().as_str(),
            "into": ids_json(s.destinations()),
            "at": instant_json(s.at()),
        }),
        IdentityAdjudication::Forget(f) => serde_json::json!({
            "verb": "forget",
            "entity": f.entity().as_str(),
            "reason": f.reason().as_str(),
            "at": instant_json(f.at()),
        }),
    };
    body.to_string()
}

// ---------------------------------------------------------------------------
// Decoding — every path ends in a domain constructor
// ---------------------------------------------------------------------------

fn field<'j>(
    o: &'j serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<&'j serde_json::Value, AdjudicationDecodeError> {
    o.get(key).ok_or(AdjudicationDecodeError::Field {
        key,
        detail: "absent".to_owned(),
    })
}

fn str_field(
    o: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<String, AdjudicationDecodeError> {
    field(o, key)?
        .as_str()
        .map(str::to_owned)
        .ok_or(AdjudicationDecodeError::Field {
            key,
            detail: "expected a string".to_owned(),
        })
}

fn entity_field(
    o: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<EntityId, AdjudicationDecodeError> {
    EntityId::new(&str_field(o, key)?).map_err(|e| AdjudicationDecodeError::Field {
        key,
        detail: format!("not an admissible entity id: {e:?}"),
    })
}

fn entity_list_field(
    o: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<Vec<EntityId>, AdjudicationDecodeError> {
    let arr = field(o, key)?
        .as_array()
        .ok_or(AdjudicationDecodeError::Field {
            key,
            detail: "expected an array".to_owned(),
        })?;
    arr.iter()
        .map(|v| {
            let s = v.as_str().ok_or(AdjudicationDecodeError::Field {
                key,
                detail: "expected an array of strings".to_owned(),
            })?;
            EntityId::new(s).map_err(|e| AdjudicationDecodeError::Field {
                key,
                detail: format!("not an admissible entity id: {e:?}"),
            })
        })
        .collect()
}

fn instant_field(
    o: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<DomainInstant, AdjudicationDecodeError> {
    let obj = field(o, key)?
        .as_object()
        .ok_or(AdjudicationDecodeError::Field {
            key,
            detail: "expected an object".to_owned(),
        })?;
    let ms = field(obj, "ms")?
        .as_u64()
        .ok_or(AdjudicationDecodeError::Field {
            key: "ms",
            detail: "expected a non-negative integer".to_owned(),
        })?;
    let domain = match str_field(obj, "domain")?.as_str() {
        "boundary" => ClockDomain::Boundary,
        "system" => ClockDomain::System,
        other => {
            return Err(AdjudicationDecodeError::UnknownClockDomain {
                token: other.to_owned(),
            })
        }
    };
    Ok(DomainInstant { ms, domain })
}

fn inadmissible<E: core::fmt::Debug>(e: E) -> AdjudicationDecodeError {
    AdjudicationDecodeError::Inadmissible {
        detail: format!("{e:?}"),
    }
}

/// **Read a stored adjudication back, or refuse.**
///
/// `justification` comes from the row's `provenance` column, not the payload.
///
/// # Errors
///
/// Every [`AdjudicationDecodeError`]. Note the last one: the verb is rebuilt by
/// the domain's own constructor, so a stored partition into one destination, a
/// merge into its own source, or a duplicate citation is refused **here** — the
/// storage boundary cannot admit a judgement the model would not.
pub fn decode_adjudication(
    payload: &str,
    payload_schema: i64,
    justification: &[String],
) -> Result<IdentityAdjudication, AdjudicationDecodeError> {
    if payload_schema != ADJUDICATION_PAYLOAD_SCHEMA {
        return Err(AdjudicationDecodeError::UnsupportedSchema {
            found: payload_schema,
            supported: ADJUDICATION_PAYLOAD_SCHEMA,
        });
    }

    let value: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| AdjudicationDecodeError::Malformed {
            detail: format!("not JSON: {e}"),
        })?;
    let o = value
        .as_object()
        .ok_or_else(|| AdjudicationDecodeError::Malformed {
            detail: "top level is not an object".to_owned(),
        })?;

    let cited: Vec<ObservationId> = justification
        .iter()
        .map(|s| {
            ObservationId::new(s).map_err(|e| AdjudicationDecodeError::Field {
                key: "provenance",
                detail: format!("not an admissible observation id: {e:?}"),
            })
        })
        .collect::<Result<_, _>>()?;
    let justification = Justification::new(cited).map_err(inadmissible)?;
    let at = instant_field(o, "at")?;

    match str_field(o, "verb")?.as_str() {
        "assert" => Ok(IdentityAdjudication::Assert(AssertIdentity::new(
            entity_field(o, "entity")?,
            justification,
            at,
        ))),
        "merge" => MergeEntities::new(
            entity_list_field(o, "sources")?,
            entity_field(o, "into")?,
            justification,
            at,
        )
        .map(IdentityAdjudication::Merge)
        .map_err(inadmissible),
        "split" => {
            let source = entity_field(o, "source")?;
            let into = entity_list_field(o, "into")?;
            let built = match str_field(o, "shape")?.as_str() {
                "partition" => SplitEntity::partition(source, into, justification, at),
                "subtraction" => SplitEntity::subtract(source, into, justification, at),
                other => {
                    return Err(AdjudicationDecodeError::UnknownSplitShape {
                        token: other.to_owned(),
                    })
                }
            };
            built.map(IdentityAdjudication::Split).map_err(inadmissible)
        }
        "forget" => {
            let reason = RetirementReason::new(&str_field(o, "reason")?).map_err(inadmissible)?;
            Ok(IdentityAdjudication::Forget(ForgetEntity::new(
                entity_field(o, "entity")?,
                reason,
                justification,
                at,
            )))
        }
        other => Err(AdjudicationDecodeError::UnknownVerb {
            token: other.to_owned(),
        }),
    }
}

// ---------------------------------------------------------------------------
// The door
// ---------------------------------------------------------------------------

/// The row-level facts a caller supplies when recording an adjudication.
///
/// Everything an adjudication *determines* is absent: `kind`, `subject`,
/// `payload`, `payload_schema`, `provenance` and `retention_class` are derived
/// by [`WorldStore::append_adjudication`], not passed in. That is the point of
/// the type — assembling a `NewEvent` by hand means twenty fields to get right,
/// and getting `retention_class` wrong silently makes the row compactable,
/// which deletes the redirect §6.3 promises to keep forever.
#[derive(Debug, Clone, Copy)]
pub struct AdjudicationRow<'a> {
    /// Identity of this record.
    pub event_id: &'a kirra_world::reference::EventId,
    /// Identity of the observation this record is of.
    pub observation_id: &'a kirra_world::reference::ObservationId,
    /// When the store learned the judgement.
    pub txn_time_ms: i64,
    /// When the judgement began holding.
    pub valid_from_ms: i64,
    /// Who recorded it.
    pub source: &'a str,
    /// Their version.
    pub source_version: &'a str,
    /// **Not free choice between all four classes in practice.**
    ///
    /// An adjudication is written [`ClaimStatus::Confirmed`] (see
    /// [`WorldStore::append_adjudication`]), and `append`'s existing SD-2 check
    /// refuses an [`WriterClass::LlmCandidate`] writing `Confirmed`. So **an
    /// LLM cannot author an adjudication at all**, enforced by the rule that
    /// already exists rather than by a new one bolted on here.
    pub writer_class: WriterClass,
}
