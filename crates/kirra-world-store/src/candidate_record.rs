//! Persisting a `same_as` **candidate** as evidence — `WM_SCOPE.md` §5 box 2a.
//!
//! `KIRRA-WM-PROMOTION-001`: *clustering may PROPOSE co-reference; it may never
//! CONFIRM identity.* This module is the propose half's storage door.
//!
//! # The durable observation is the artifact
//!
//! §2a: *"a promotion cites candidates by `ObservationId` through
//! `Justification`, and `KIRRA-WM-CANDIDATE-ID-001` keeps a candidate's
//! identifier out of the hashed record — so a cluster **cannot** be cited, only
//! a candidate observation can. 2a therefore emits candidates as observations,
//! not as in-memory cluster objects passed to an adjudicator."*
//!
//! So [`SameAsCandidate`] is the *input* to this module and never the thing
//! that travels. What an adjudicator later judges is the ROW — reachable by
//! `ObservationId`, chained, and readable through the ordinary
//! [`crate::WorldStore::candidates`] path like any other candidate claim.
//!
//! # Two fields the caller does not get to supply
//!
//! [`CandidateRow`] has no `writer_class` and no `claim_status`, unlike
//! [`crate::adjudication_record::AdjudicationRow`], which takes the former. That
//! asymmetry is the whole point of this door: both are pinned here, to
//! [`WriterClass::Derivation`] and [`ClaimStatus::Candidate`], so a caller
//! cannot ask this path to write a confirmed claim **or** to write one under a
//! more privileged class. The rule is not enforced by review of the call site;
//! there is no call site that could express the violation.
//!
//! That is the producer-side half. The store-side half is schema v8's trigger,
//! which holds for a producer written later against the same store — see
//! [`crate::schema::SCHEMA_V8_MIGRATION`]. Two independent mechanisms, because
//! the type system does not reach raw SQL and SQL does not reach a caller's
//! intent.
//!
//! # Retention: `"raw"`, and what that costs
//!
//! Deliberate, and NOT the choice
//! [`crate::adjudication_record::ADJUDICATION_RETENTION_CLASS`] makes.
//! `crate::compaction::is_protected` holds for every class except `"raw"`, so
//! this is the one spelling that leaves a candidate compactable.
//!
//! It has to be. A matcher proposes continuously and most proposals are never
//! promoted; protecting every one would make retention a function of matcher
//! throughput, which is exactly the unbounded growth ADR-0041 §11.3 exists to
//! prevent.
//!
//! **The consequence, stated rather than buried:** a candidate that WAS promoted
//! can still be compacted, and the promotion's citation of it then dangles.
//! That is reported honestly — box 4b resolves such a citation to `Dangling`
//! rather than inventing evidence — but "an adjudication may outlive the
//! evidence it cites" is a real property, not an accident of this constant.
//!
//! Whether a promotion should PROTECT what it cites is a retention ruling, and
//! it is deliberately not made here: 2a is the propose door, and inventing a
//! protection rule inside it would be deciding a retention question in the one
//! place nobody would look for it. Recorded as an open item in `WM_SCOPE.md`
//! §5 2a.

use kirra_world::observation::{CalibrationRef, Confidence, ConfidenceBasis};
use kirra_world::reference::ObservationId;
use kirra_world::same_as_candidate::{CandidatePair, MatcherIdentity, SameAsCandidate};

/// The `world_events.kind` token every candidate row carries.
///
/// `"observation"` rather than a bespoke `"same_as_candidate"` kind, so a
/// candidate reads back through [`crate::WorldStore::candidates`] like every
/// other candidate claim instead of needing its own query path. The predicate
/// column already says *which* claim this is; a second discriminant in `kind`
/// would be two places to look and two places to disagree — the reasoning
/// [`crate::adjudication_record::ADJUDICATION_KIND`] records for the opposite
/// case, applied to a claim that genuinely IS an observation.
///
/// It also keeps the semantic class `(observation, "same_as")` — the pair the
/// freshness-coverage gate tracks and `ci/freshness_unruled_baseline.json`
/// records as knowingly unruled.
pub const CANDIDATE_KIND: &str = "observation";

/// The `world_events.predicate` token every candidate row carries.
///
/// Pinned as a constant rather than read from
/// `kirra_world::same_as_candidate::CANDIDATE_PREDICATE` at each call, because
/// this is the STORED spelling: the domain enum could be renamed without
/// breaking a build, and every row already written would then be unreadable.
/// A test holds the two in lock-step.
pub const CANDIDATE_PREDICATE_TOKEN: &str = "same_as";

/// The `world_events.retention_class` every candidate row carries.
///
/// See the module docs — `"raw"` is the one spelling that leaves a row
/// compactable, and that is the intended behaviour for matcher output.
pub const CANDIDATE_RETENTION_CLASS: &str = "raw";

/// The stored `world_events.writer_class` a candidate must carry.
///
/// A matcher's proposal, and only a matcher's proposal. Kept as the STORED
/// spelling for the same reason as [`CANDIDATE_PREDICATE_TOKEN`].
pub const DERIVATION_TOKEN: &str = "derivation";

/// The stored `world_events.claim_status` a candidate must carry.
pub const CANDIDATE_STATUS_TOKEN: &str = "candidate";

/// Version of the payload encoding below.
pub const CANDIDATE_PAYLOAD_SCHEMA: i64 = 1;

/// Why a stored candidate could not be read back.
///
/// **Every variant is a refusal, never a repair** — the contract
/// [`crate::adjudication_record::AdjudicationDecodeError`] sets, for the same
/// reason: a candidate is the record of what a matcher actually proposed, and a
/// guessed one is worse than a missing one. Box 2b will judge these rows, and an
/// adjudicator must never be handed a repaired approximation of the evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateDecodeError {
    /// The payload is not JSON, or not an object.
    Malformed {
        /// What went wrong, without echoing the payload back.
        detail: String,
    },
    /// The payload's schema version is not one this build can read.
    ///
    /// Fail-closed on a newer payload for the same reason
    /// `assert_schema_not_future` refuses a newer database — and equally on an
    /// OLDER or nonsensical one, which is the half a `>` comparison misses. A
    /// row stamped `0` is not a v1 payload, and reading it as one would be
    /// interpreting bytes under a contract they were never written to.
    UnsupportedSchema {
        /// The version the row carries.
        found: i64,
        /// The newest version this build understands.
        supported: i64,
    },
    /// A required field was absent or the wrong shape.
    Field {
        /// Which field.
        key: &'static str,
        /// What was wrong with it.
        detail: String,
    },
    /// A confidence-basis token this build does not know.
    ///
    /// Refused rather than mapped to `Unspecified`: silently widening an unknown
    /// basis to "no basis stated" would turn a forward-compatibility problem
    /// into a quiet loss of provenance.
    UnknownConfidenceBasis {
        /// The token found.
        token: String,
    },
    /// The row's columns disagree with what a candidate must be — a wrong
    /// `kind`, a wrong `predicate`, a missing `object`, or a writer class or
    /// claim status that is not a matcher's proposal.
    ///
    /// The last two are the ones box 2b leans on: an adjudicator must judge
    /// evidence that IS a derivation-class `same_as` candidate, not a row that
    /// merely looks like one. A confirmed row, or one written under a more
    /// privileged class, is refused here rather than judged.
    NotACandidateRow {
        /// What disagreed.
        detail: String,
    },
    /// The decoded parts were rejected by the domain's own constructor.
    ///
    /// The important variant: decoding ends in
    /// [`SameAsCandidate::propose`] rather than in a struct literal, so a row
    /// that would build a candidate the domain refuses (a self-pair, duplicated
    /// support, an empty matcher version) is refused at read time exactly as it
    /// would have been at write time.
    Domain {
        /// The domain's own message.
        detail: String,
    },
}

impl std::fmt::Display for CandidateDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed { detail } => write!(f, "candidate payload is malformed: {detail}"),
            Self::UnsupportedSchema { found, supported } => write!(
                f,
                "candidate payload schema {found} is newer than this build supports ({supported})"
            ),
            Self::Field { key, detail } => write!(f, "candidate payload field `{key}`: {detail}"),
            Self::UnknownConfidenceBasis { token } => {
                write!(f, "unknown confidence basis token `{token}`")
            }
            Self::NotACandidateRow { detail } => write!(f, "not a same_as candidate row: {detail}"),
            Self::Domain { detail } => write!(f, "candidate refused by the domain: {detail}"),
        }
    }
}

impl std::error::Error for CandidateDecodeError {}

// ---------------------------------------------------------------------------
// Confidence basis <-> stored token
// ---------------------------------------------------------------------------

/// The stored spelling of a confidence basis.
///
/// An exhaustive `match` with no wildcard arm, deliberately: adding a
/// [`ConfidenceBasis`] variant must be a COMPILE error here, not a row that
/// silently stores a basis this encoder never considered. The same discipline
/// the explain-wire's semantics table uses.
#[must_use]
pub fn confidence_basis_token(basis: ConfidenceBasis) -> &'static str {
    match basis {
        ConfidenceBasis::ModelScore => "model_score",
        ConfidenceBasis::GeometricResidual => "geometric_residual",
        ConfidenceBasis::OperatorCertainty => "operator_certainty",
        ConfidenceBasis::Corroboration => "corroboration",
        ConfidenceBasis::Assumed => "assumed",
        ConfidenceBasis::Unspecified => "unspecified",
    }
}

/// The inverse of [`confidence_basis_token`], refusing an unknown token.
///
/// # Errors
///
/// [`CandidateDecodeError::UnknownConfidenceBasis`] for a token this build does
/// not know — see that variant on why this does not fall back to
/// `Unspecified`.
pub fn confidence_basis_from_token(token: &str) -> Result<ConfidenceBasis, CandidateDecodeError> {
    match token {
        "model_score" => Ok(ConfidenceBasis::ModelScore),
        "geometric_residual" => Ok(ConfidenceBasis::GeometricResidual),
        "operator_certainty" => Ok(ConfidenceBasis::OperatorCertainty),
        "corroboration" => Ok(ConfidenceBasis::Corroboration),
        "assumed" => Ok(ConfidenceBasis::Assumed),
        "unspecified" => Ok(ConfidenceBasis::Unspecified),
        other => Err(CandidateDecodeError::UnknownConfidenceBasis {
            token: other.to_owned(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// The payload every candidate row carries.
///
/// Holds the matcher identity and the confidence provenance — the parts with
/// nowhere else to live. The PAIR is not in here: it travels in the `subject`
/// and `object` columns, where the projection and any predicate-shaped query can
/// see it. Duplicating it into the payload would create a second copy that could
/// disagree with the columns the store actually indexes.
///
/// The support observations are likewise not in the payload: they are the row's
/// `provenance`, which is what box 4a's citation index is built from. A
/// candidate whose support lived only in its payload would be invisible to the
/// provenance walk.
#[must_use]
pub fn encode_candidate(c: &SameAsCandidate) -> String {
    let confidence = c.confidence();
    serde_json::json!({
        "matcher": {
            "producer": c.matcher().producer(),
            "model_or_rule": c.matcher().model_or_rule(),
            "version": c.matcher().version(),
        },
        "confidence": {
            "score": confidence.score(),
            "basis": confidence_basis_token(confidence.basis()),
            "calibration": confidence.calibration().map(CalibrationRef::as_str),
        },
    })
    .to_string()
}

/// The observation ids a candidate row records as its `provenance`.
///
/// Exactly [`SameAsCandidate::support`] — what the matcher computed the proposal
/// FROM. `SameAsCandidate::propose` already refuses an empty or duplicated
/// support list, so this cannot produce a candidate citing nothing.
#[must_use]
pub fn candidate_provenance(c: &SameAsCandidate) -> Vec<String> {
    c.support().iter().map(|o| o.as_str().to_owned()).collect()
}

// ---------------------------------------------------------------------------
// Decoding — the path box 2b will judge through
// ---------------------------------------------------------------------------

fn object(
    v: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, CandidateDecodeError> {
    v.as_object()
        .ok_or_else(|| CandidateDecodeError::Malformed {
            detail: "payload is not a JSON object".to_owned(),
        })
}

fn str_field(
    o: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<String, CandidateDecodeError> {
    o.get(key)
        .ok_or(CandidateDecodeError::Field {
            key,
            detail: "absent".to_owned(),
        })?
        .as_str()
        .map(str::to_owned)
        .ok_or(CandidateDecodeError::Field {
            key,
            detail: "not a string".to_owned(),
        })
}

/// The stored columns a candidate decode reads.
///
/// A struct rather than eight positional parameters, and not only for the arity:
/// `writer_class`, `claim_status`, `kind`, `subject` and `payload` are all
/// `&str`, so a positional call is five same-typed arguments in a row and a
/// transposition compiles cleanly. Naming them makes the wrong order a
/// syntax-visible mistake instead of a silent one.
#[derive(Debug, Clone, Copy)]
pub struct StoredCandidateRow<'a> {
    /// `world_events.writer_class` — must be [`DERIVATION_TOKEN`].
    pub writer_class: &'a str,
    /// `world_events.claim_status` — must be [`CANDIDATE_STATUS_TOKEN`].
    pub claim_status: &'a str,
    /// `world_events.kind` — must be [`CANDIDATE_KIND`].
    pub kind: &'a str,
    /// `world_events.predicate` — must be [`CANDIDATE_PREDICATE_TOKEN`].
    pub predicate: Option<&'a str>,
    /// The pair's low side.
    pub subject: &'a str,
    /// The pair's high side.
    pub object_id: Option<&'a str>,
    /// The encoded matcher identity and confidence.
    pub payload: &'a str,
    /// The payload encoding version.
    pub payload_schema: i64,
}

/// Rebuild a [`SameAsCandidate`] from a stored row.
///
/// The columns are passed separately from the payload because that is where the
/// pair and the support actually live — see [`encode_candidate`]. Passing the
/// whole row would invite a decoder that trusted the payload's copy.
///
/// # Errors
///
/// [`CandidateDecodeError`] — every variant a refusal. Note the last step:
/// this ends in [`SameAsCandidate::propose`], so the domain's own rules
/// (no self-pair, no duplicate support, non-empty matcher version) are enforced
/// on READ as well as on write.
pub fn decode_candidate(
    row: &StoredCandidateRow<'_>,
    support: &[String],
) -> Result<SameAsCandidate, CandidateDecodeError> {
    let StoredCandidateRow {
        writer_class,
        claim_status,
        kind,
        predicate,
        subject,
        object_id,
        payload,
        payload_schema,
    } = *row;
    // The two the write door pins, checked on the way back out. Not redundant
    // with pinning them: the door governs what THIS crate writes, and a decoder
    // that trusted that would be trusting the one thing a row from anywhere else
    // could differ in. `KIRRA-WM-PROMOTION-001` is about who may confirm, so a
    // reader must be able to tell a matcher's proposal from everything else.
    if writer_class != DERIVATION_TOKEN {
        return Err(CandidateDecodeError::NotACandidateRow {
            detail: format!(
                "writer_class is `{writer_class}`, expected `{DERIVATION_TOKEN}` \
                 (a same_as candidate is a matcher's proposal)"
            ),
        });
    }
    if claim_status != CANDIDATE_STATUS_TOKEN {
        return Err(CandidateDecodeError::NotACandidateRow {
            detail: format!(
                "claim_status is `{claim_status}`, expected `{CANDIDATE_STATUS_TOKEN}`"
            ),
        });
    }
    // ANY version other than the supported one, not merely a newer one. An
    // older or nonsensical `payload_schema` (0, -1, a v0 draft) is not a v1
    // payload either, and decoding it AS v1 would be interpreting bytes under a
    // contract they were never written to -- the same fail-open a newer version
    // would be, minus the excuse that the future is unknowable.
    //
    // When a v2 encoding lands this becomes membership in a supported SET, so
    // v1 rows keep decoding. It is `!=` today because that set has one member.
    if payload_schema != CANDIDATE_PAYLOAD_SCHEMA {
        return Err(CandidateDecodeError::UnsupportedSchema {
            found: payload_schema,
            supported: CANDIDATE_PAYLOAD_SCHEMA,
        });
    }
    if kind != CANDIDATE_KIND {
        return Err(CandidateDecodeError::NotACandidateRow {
            detail: format!("kind is `{kind}`, expected `{CANDIDATE_KIND}`"),
        });
    }
    match predicate {
        Some(p) if p == CANDIDATE_PREDICATE_TOKEN => {}
        Some(p) => {
            return Err(CandidateDecodeError::NotACandidateRow {
                detail: format!("predicate is `{p}`, expected `{CANDIDATE_PREDICATE_TOKEN}`"),
            })
        }
        None => {
            return Err(CandidateDecodeError::NotACandidateRow {
                detail: "no predicate".to_owned(),
            })
        }
    }
    let Some(other) = object_id else {
        return Err(CandidateDecodeError::NotACandidateRow {
            detail: "no object: a same_as candidate names two entities".to_owned(),
        });
    };

    let parsed: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| CandidateDecodeError::Malformed {
            detail: e.to_string(),
        })?;
    let root = object(&parsed)?;

    let matcher_obj = object(root.get("matcher").ok_or(CandidateDecodeError::Field {
        key: "matcher",
        detail: "absent".to_owned(),
    })?)?;
    let matcher = MatcherIdentity::new(
        str_field(matcher_obj, "producer")?,
        str_field(matcher_obj, "model_or_rule")?,
        str_field(matcher_obj, "version")?,
    )
    .map_err(|e| CandidateDecodeError::Domain {
        detail: e.to_string(),
    })?;

    let conf_obj = object(root.get("confidence").ok_or(CandidateDecodeError::Field {
        key: "confidence",
        detail: "absent".to_owned(),
    })?)?;
    // `null` and absent both mean "no score" — §7.3's "must not force producers
    // to invent precision they do not have", surviving a round trip.
    let score = match conf_obj.get("score") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => Some(
            v.as_f64()
                .ok_or(CandidateDecodeError::Field {
                    key: "score",
                    detail: "not a number".to_owned(),
                })?
                .to_owned() as f32,
        ),
    };
    let basis = confidence_basis_from_token(&str_field(conf_obj, "basis")?)?;
    let calibration = match conf_obj.get("calibration") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => Some(
            CalibrationRef::new(v.as_str().ok_or(CandidateDecodeError::Field {
                key: "calibration",
                detail: "not a string".to_owned(),
            })?)
            // `{:?}`, not `{}`: `ConfidenceError` has no `Display`. Neither do
            // `observation.rs`'s other three error enums, while `reference.rs`
            // and `same_as_candidate.rs` both do -- a gap in that one module
            // rather than a decision about this type. Noted here instead of
            // widened from this PR, which is a storage door, not a domain
            // change. The variants are fieldless, so `Debug` reads acceptably.
            .map_err(|e| CandidateDecodeError::Domain {
                detail: format!("{e:?}"),
            })?,
        ),
    };
    // Same `ConfidenceError` Display gap as above.
    let confidence =
        Confidence::new(score, basis, calibration).map_err(|e| CandidateDecodeError::Domain {
            detail: format!("{e:?}"),
        })?;

    let pair = CandidatePair::new(
        kirra_world::reference::EntityId::new(subject).map_err(|e| {
            CandidateDecodeError::Domain {
                detail: e.to_string(),
            }
        })?,
        kirra_world::reference::EntityId::new(other).map_err(|e| CandidateDecodeError::Domain {
            detail: e.to_string(),
        })?,
    )
    .map_err(|e| CandidateDecodeError::Domain {
        detail: e.to_string(),
    })?;

    let support: Result<Vec<ObservationId>, CandidateDecodeError> = support
        .iter()
        .map(|s| {
            ObservationId::new(s.as_str()).map_err(|e| CandidateDecodeError::Domain {
                detail: e.to_string(),
            })
        })
        .collect();

    SameAsCandidate::propose(pair, matcher, confidence, support?).map_err(|e| {
        CandidateDecodeError::Domain {
            detail: e.to_string(),
        }
    })
}

/// The row fields a candidate append needs from its caller.
///
/// **No `writer_class` and no `claim_status`** — see the module docs. Both are
/// pinned by the door, which is what makes "a matcher cannot self-confirm" a
/// property of the signature rather than of a convention.
#[derive(Debug, Clone, Copy)]
pub struct CandidateRow<'a> {
    /// This record's identity in the log.
    pub event_id: &'a kirra_world::reference::EventId,
    /// The candidate's own observation identity — **the thing a promotion
    /// cites**. `KIRRA-WM-PROMOTION-001` makes this the citable handle, so it is
    /// the caller's to mint and to keep.
    pub observation_id: &'a ObservationId,
    /// When the store learned this.
    pub txn_time_ms: i64,
    /// When the proposal begins to hold.
    pub valid_from_ms: i64,
    /// The producing system.
    pub source: &'a str,
    /// Its version.
    pub source_version: &'a str,
}
