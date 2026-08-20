//! Persisting a `same_as` **adjudication** — `WM_SCOPE.md` §5 box 2b.
//!
//! `KIRRA-WM-PROMOTION-001`: confirmed identity arrives only through an
//! **explicitly authorized adjudicator**, and v1 is `SourceClass::Operator`
//! only.
//!
//! # Where the authority rule is actually enforced
//!
//! In the TYPE, not here. `AdjudicationAuthority::new` refuses every class but
//! `Operator`, and the struct does not even store one — `class()` returns the
//! constant. An unauthorized adjudicator is therefore unrepresentable, and this
//! module deliberately does NOT re-check it: a comparison that can only ever be
//! `false` reads as the enforcement and stops a reader looking for the real one.
//! (An earlier draft of this module carried exactly that dead check, and its own
//! test found it.)
//!
//! # What box 2b actually closed
//!
//! Adjudication used to accept a `&SameAsCandidate` — a value the caller
//! constructed. That let an adjudicator judge something that merely LOOKED like
//! evidence, which is the opposite of resting confirmed identity on recorded
//! evidence. The API now takes the persisted candidate's `ObservationId` and the
//! store does the loading, so an in-memory candidate cannot enter at all.
//!
//! # Why the predicate is not `same_as`
//!
//! A row saying `(low, same_as, high)` at `claim_status = confirmed` would read
//! as *"these are the same"*. That is not what this row is. This row says *"an
//! authorized adjudicator reached a decision about that pair"* — and the
//! decision may be `Rejected`. Storing a rejection under the `same_as`
//! predicate would make the log assert the very thing the operator refused.
//!
//! So the predicate is [`ADJUDICATION_PREDICATE_TOKEN`], and the outcome lives
//! in the payload. Both entities stay in indexed columns, which is what lets a
//! reader find every decision about a pair without parsing payloads.

use kirra_world::observation::SourceClass;
use kirra_world::reference::{EventId, ObservationId};
use kirra_world::same_as_adjudication::{AdjudicationAuthority, Outcome, SameAsAdjudication};
use kirra_world::same_as_candidate::CandidatePair;

/// The `world_events.kind` token every `same_as` adjudication row carries.
pub const SAME_AS_ADJUDICATION_KIND: &str = "same_as_adjudication";

/// The `world_events.predicate` token every adjudication row carries.
///
/// **Not `same_as`** — see the module docs. This names the RELATION between the
/// row and the pair (*a decision was reached about it*), not the relation
/// between the two entities.
pub const ADJUDICATION_PREDICATE_TOKEN: &str = "same_as_adjudged";

/// The `world_events.retention_class` every adjudication row carries.
///
/// `"adjudication"`, and NOT the `"raw"` its candidate carries.
/// `crate::compaction::is_protected` holds for every class except `"raw"`, so
/// this is what keeps a decision from being compacted away.
///
/// The asymmetry with [`crate::candidate_record::CANDIDATE_RETENTION_CLASS`] is
/// the point rather than an inconsistency. A matcher proposes continuously and
/// most proposals are never judged; an *authorized adjudicator's decision* is
/// rare, deliberate, and the thing §6.3's "a merged id stays resolvable forever"
/// ultimately rests on.
pub const SAME_AS_ADJUDICATION_RETENTION_CLASS: &str = "adjudication";

/// Version of the payload encoding below.
pub const SAME_AS_ADJUDICATION_PAYLOAD_SCHEMA: i64 = 1;

/// The stored spelling of an outcome.
///
/// An exhaustive `match` with no wildcard: a new [`Outcome`] variant must be a
/// COMPILE error here rather than a row storing a decision this encoder never
/// considered.
#[must_use]
pub fn outcome_token(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Promoted => "promoted",
        Outcome::Rejected => "rejected",
        Outcome::Unresolved => "unresolved",
    }
}

/// The inverse of [`outcome_token`].
///
/// # Errors
///
/// Returns `None` for a token this build does not know — refused by the caller
/// rather than defaulted, because guessing which decision an operator made is
/// worse than admitting the row cannot be read.
#[must_use]
pub fn outcome_from_token(token: &str) -> Option<Outcome> {
    match token {
        "promoted" => Some(Outcome::Promoted),
        "rejected" => Some(Outcome::Rejected),
        "unresolved" => Some(Outcome::Unresolved),
        _ => None,
    }
}

/// The stored spelling of an authority's source class.
#[must_use]
pub fn source_class_token(class: SourceClass) -> &'static str {
    match class {
        SourceClass::Sensor => "sensor",
        SourceClass::Operator => "operator",
        SourceClass::Configuration => "configuration",
        SourceClass::Import => "import",
        SourceClass::Derivation => "derivation",
        SourceClass::Network => "network",
    }
}

/// Encode an adjudication's payload.
///
/// The pair is NOT in here — it travels in `subject`/`object`, where an index
/// can see it. The evidence is not here either: the judged candidate and the
/// cited observations are the row's `provenance`, because box 4a's citation
/// index is built from that column and a decision whose evidence lived only in
/// its payload would be invisible to the provenance walk.
///
/// `candidate_observation_id` DOES appear here as well, and the duplication is
/// deliberate: `provenance` is a flat list of ids, so it records *that* the
/// candidate was cited but not that it was the SUBJECT of the decision rather
/// than one input among several. The payload keeps that distinction; the
/// provenance column keeps the walk.
#[must_use]
pub fn encode_same_as_adjudication(a: &SameAsAdjudication) -> String {
    serde_json::json!({
        "outcome": outcome_token(a.outcome()),
        "candidate_observation_id": a.candidate_observation_id().as_str(),
        "authority": {
            "source_class": source_class_token(a.authority().class()),
            "identity": a.authority().adjudicator(),
        },
        "decided_at": {
            "ms": a.decided_at().ms,
            "domain": match a.decided_at().domain {
                kirra_world::observation::ClockDomain::Boundary => "boundary",
                kirra_world::observation::ClockDomain::System => "system",
            },
        },
    })
    .to_string()
}

/// The observation ids an adjudication row records as its `provenance`.
///
/// **The judged candidate comes first, then the cited observations.**
///
/// An earlier version returned only `cited()`, which left the
/// `candidate_observation_id` — the single most important citation, the thing
/// the decision is ABOUT — reachable only by parsing the payload. That
/// contradicted this module's own stated reason for putting citations in
/// `provenance`: box 4a's index is built from that column, so a provenance walk
/// from a decision could not reach the proposal it judged.
///
/// Deduplicated, because a caller may legitimately also name the candidate in
/// `cited` and the citation index keys on `(generation, ordinal)` — one
/// observation appearing twice would make one piece of evidence look like two,
/// the same defect `SameAsCandidate::propose` refuses for support lists.
#[must_use]
pub fn same_as_adjudication_provenance(a: &SameAsAdjudication) -> Vec<String> {
    let mut out = vec![a.candidate_observation_id().as_str().to_owned()];
    for o in a.cited() {
        let id = o.as_str().to_owned();
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// What a caller must supply to adjudicate a persisted candidate.
///
/// Note what is **absent**: there is no candidate value and no pair. Both are
/// derived by the store from the row `candidate_observation_id` names, which is
/// what makes "adjudication acts on persisted evidence" a property of this
/// type rather than of the caller's discipline.
#[derive(Debug, Clone)]
pub struct SameAsAdjudicationRequest<'a> {
    /// This decision's identity in the log.
    pub event_id: &'a EventId,
    /// This decision's own observation identity.
    pub observation_id: &'a ObservationId,
    /// **The persisted candidate being judged.** The store loads it.
    pub candidate_observation_id: &'a str,
    /// The observations this decision rests on.
    pub cited: Vec<ObservationId>,
    /// Who decided.
    pub authority: AdjudicationAuthority,
    /// What they decided.
    pub outcome: Outcome,
    /// When they decided, on a named clock.
    pub decided_at: kirra_world::observation::DomainInstant,
    /// When the store learned it.
    pub txn_time_ms: i64,
    /// The operator console or tool that submitted it.
    pub source: &'a str,
    /// Its version.
    pub source_version: &'a str,
}

/// Why an adjudication could not be recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdjudicateError {
    /// No row in the log carries that observation id.
    ///
    /// Distinct from [`Self::NotACandidate`]: "there is no such evidence" and
    /// "that is not evidence of this kind" are different findings, and an
    /// adjudicator told the wrong one would look in the wrong place.
    NoSuchCandidate {
        /// The id that named nothing.
        observation_id: String,
    },
    /// The row exists but is not a derivation-class `same_as` candidate.
    NotACandidate {
        /// What disagreed, from the decoder.
        detail: String,
    },
    /// The domain refused the decision.
    Domain {
        /// The domain's own message.
        detail: String,
    },
}

impl std::fmt::Display for AdjudicateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchCandidate { observation_id } => write!(
                f,
                "no candidate observation `{observation_id}` in the log — \
                 an adjudication must judge evidence that exists"
            ),
            Self::NotACandidate { detail } => {
                write!(f, "that observation is not a same_as candidate: {detail}")
            }
            Self::Domain { detail } => write!(f, "decision refused: {detail}"),
        }
    }
}

impl std::error::Error for AdjudicateError {}

/// The pair a loaded candidate names, for the record's `subject`/`object`.
#[must_use]
pub fn adjudication_columns(pair: &CandidatePair) -> (String, String) {
    (
        pair.low().as_str().to_owned(),
        pair.high().as_str().to_owned(),
    )
}

// ---------------------------------------------------------------------------
// Reading a stored adjudication back — Tier 5 box 5a.
// ---------------------------------------------------------------------------

/// A stored adjudication row's columns, as the decoder needs them.
#[derive(Debug, Clone, Copy)]
pub struct StoredAdjudicationRow<'a> {
    /// `world_events.writer_class`.
    pub writer_class: &'a str,
    /// `world_events.claim_status`.
    pub claim_status: &'a str,
    /// `world_events.kind`.
    pub kind: &'a str,
    /// `world_events.predicate`.
    pub predicate: Option<&'a str>,
    /// `world_events.subject` — the pair's low entity.
    pub subject: &'a str,
    /// `world_events.object` — the pair's high entity.
    pub object: Option<&'a str>,
    /// `world_events.payload`.
    pub payload: &'a str,
    /// `world_events.payload_schema`.
    pub payload_schema: i64,
}

/// What a stored `same_as` adjudication says, read back from its row.
///
/// # Why this is not a [`SameAsAdjudication`]
///
/// It would be one field short of honest. [`same_as_adjudication_provenance`]
/// writes the judged candidate first and then `cited`, **deduplicated**, into
/// one flat column — so a caller who also named the candidate in `cited` and one
/// who did not produce the identical row. Reconstructing `SameAsAdjudication`
/// would have to invent a `cited` list, and the only available guess (the whole
/// provenance column) attributes a citation the writer may never have made.
///
/// So this carries exactly the fields the row records unambiguously, and the
/// projection that consumes it needs none of the rest. A decoder that returned
/// a richer type than the row supports would be manufacturing the difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAdjudication {
    /// The pair decided about, in canonical order.
    pub pair: CandidatePair,
    /// What was decided.
    pub outcome: Outcome,
    /// The persisted candidate this decision judged.
    pub candidate_observation_id: String,
    /// Who decided.
    pub adjudicator: String,
    /// When they decided, on a named clock.
    pub decided_at: kirra_world::observation::DomainInstant,
}

/// Why a stored adjudication row could not be read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdjudicationDecodeError {
    /// The row's columns do not describe an authorized `same_as` decision.
    NotAnAdjudicationRow {
        /// What disagreed.
        detail: String,
    },
    /// The payload was written under an encoding this build does not have.
    UnsupportedSchema {
        /// The row's `payload_schema`.
        found: i64,
        /// The one this build reads.
        supported: i64,
    },
    /// The payload is not the JSON object this encoding writes.
    Malformed {
        /// The parse failure.
        detail: String,
    },
    /// A payload field is absent or the wrong shape.
    Field {
        /// Which field.
        key: &'static str,
        /// How it disagreed.
        detail: String,
    },
    /// The domain refused a value the row carried.
    Domain {
        /// The domain's own message.
        detail: String,
    },
}

impl std::fmt::Display for AdjudicationDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnAdjudicationRow { detail } => {
                write!(f, "not a same_as adjudication row: {detail}")
            }
            Self::UnsupportedSchema { found, supported } => {
                write!(f, "payload_schema {found} is not the supported {supported}")
            }
            Self::Malformed { detail } => write!(f, "malformed adjudication payload: {detail}"),
            Self::Field { key, detail } => write!(f, "field `{key}`: {detail}"),
            Self::Domain { detail } => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for AdjudicationDecodeError {}

fn payload_object(
    v: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, AdjudicationDecodeError> {
    v.as_object()
        .ok_or_else(|| AdjudicationDecodeError::Malformed {
            detail: "payload is not a JSON object".to_owned(),
        })
}

fn string_field<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<&'a str, AdjudicationDecodeError> {
    match obj.get(key) {
        None => Err(AdjudicationDecodeError::Field {
            key,
            detail: "absent".to_owned(),
        }),
        Some(v) => v.as_str().ok_or(AdjudicationDecodeError::Field {
            key,
            detail: "not a string".to_owned(),
        }),
    }
}

/// **Read a stored adjudication row back, fail-closed.**
///
/// Every column the write door pins is checked on the way out, for the reason
/// [`crate::candidate_record::decode_candidate`] records: the door governs what
/// *this crate* writes, and a decoder that trusted the door would be trusting
/// the one thing a row from anywhere else could differ in.
///
/// # The pair's order is checked, not repaired
///
/// [`CandidatePair::new`] canonicalizes, so handing it a row whose `subject` and
/// `object` were stored the wrong way round would return a correct-looking pair
/// and silently hide that the row disagrees with the writer's own convention.
/// The order is therefore asserted after construction: `subject` must be the
/// low entity. A projection keyed on the pair cannot afford a repair it never
/// reported, because the repaired and unrepaired rows would collide on one key
/// while claiming different provenance.
///
/// # Errors
///
/// [`AdjudicationDecodeError`], per variant.
pub fn decode_same_as_adjudication(
    row: &StoredAdjudicationRow<'_>,
) -> Result<StoredAdjudication, AdjudicationDecodeError> {
    let StoredAdjudicationRow {
        writer_class,
        claim_status,
        kind,
        predicate,
        subject,
        object,
        payload,
        payload_schema,
    } = *row;

    let authorized_class =
        source_class_token(kirra_world::same_as_adjudication::AUTHORIZED_ADJUDICATOR_CLASS);
    if writer_class != authorized_class {
        return Err(AdjudicationDecodeError::NotAnAdjudicationRow {
            detail: format!(
                "writer_class is `{writer_class}`, expected `{authorized_class}` \
                 (KIRRA-WM-PROMOTION-001 admits one adjudicating class)"
            ),
        });
    }
    if claim_status != crate::ClaimStatus::Confirmed.as_str() {
        return Err(AdjudicationDecodeError::NotAnAdjudicationRow {
            detail: format!("claim_status is `{claim_status}`, expected `confirmed`"),
        });
    }
    if kind != SAME_AS_ADJUDICATION_KIND {
        return Err(AdjudicationDecodeError::NotAnAdjudicationRow {
            detail: format!("kind is `{kind}`, expected `{SAME_AS_ADJUDICATION_KIND}`"),
        });
    }
    match predicate {
        Some(p) if p == ADJUDICATION_PREDICATE_TOKEN => {}
        Some(p) => {
            return Err(AdjudicationDecodeError::NotAnAdjudicationRow {
                detail: format!("predicate is `{p}`, expected `{ADJUDICATION_PREDICATE_TOKEN}`"),
            })
        }
        None => {
            return Err(AdjudicationDecodeError::NotAnAdjudicationRow {
                detail: "no predicate".to_owned(),
            })
        }
    }
    // ANY other version, not merely a newer one -- `decode_candidate`'s reason.
    if payload_schema != SAME_AS_ADJUDICATION_PAYLOAD_SCHEMA {
        return Err(AdjudicationDecodeError::UnsupportedSchema {
            found: payload_schema,
            supported: SAME_AS_ADJUDICATION_PAYLOAD_SCHEMA,
        });
    }
    let Some(high) = object else {
        return Err(AdjudicationDecodeError::NotAnAdjudicationRow {
            detail: "no object: a same_as decision names two entities".to_owned(),
        });
    };

    let low_id = kirra_world::reference::EntityId::new(subject).map_err(|e| {
        AdjudicationDecodeError::Domain {
            detail: format!("subject: {e}"),
        }
    })?;
    let high_id = kirra_world::reference::EntityId::new(high).map_err(|e| {
        AdjudicationDecodeError::Domain {
            detail: format!("object: {e}"),
        }
    })?;
    let pair =
        CandidatePair::new(low_id, high_id).map_err(|e| AdjudicationDecodeError::Domain {
            detail: e.to_string(),
        })?;
    if pair.low().as_str() != subject {
        return Err(AdjudicationDecodeError::NotAnAdjudicationRow {
            detail: format!(
                "the stored pair is not in canonical order: subject `{subject}` is not the low \
                 entity of ({}, {})",
                pair.low().as_str(),
                pair.high().as_str()
            ),
        });
    }

    let parsed: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| AdjudicationDecodeError::Malformed {
            detail: e.to_string(),
        })?;
    let root = payload_object(&parsed)?;

    let outcome_str = string_field(root, "outcome")?;
    let outcome =
        outcome_from_token(outcome_str).ok_or_else(|| AdjudicationDecodeError::Field {
            key: "outcome",
            detail: format!("`{outcome_str}` is not a decision this build knows"),
        })?;

    let candidate_observation_id = string_field(root, "candidate_observation_id")?.to_owned();
    // Parsed, not merely copied: this id is the citation a reader follows back
    // to the judged proposal, and one that is not an admissible ObservationId
    // resolves to nothing while looking like evidence.
    ObservationId::new(candidate_observation_id.as_str()).map_err(|e| {
        AdjudicationDecodeError::Field {
            key: "candidate_observation_id",
            detail: e.to_string(),
        }
    })?;

    let authority = root
        .get("authority")
        .ok_or(AdjudicationDecodeError::Field {
            key: "authority",
            detail: "absent".to_owned(),
        })?;
    let authority = payload_object(authority)?;
    let payload_class = string_field(authority, "source_class")?;
    // The payload's own record of the class, checked against the column. They
    // are written from one value, so a row where they disagree was not written
    // by this door -- and taking either one as the truth would pick a winner.
    if payload_class != authorized_class {
        return Err(AdjudicationDecodeError::NotAnAdjudicationRow {
            detail: format!(
                "payload authority.source_class is `{payload_class}`, expected \
                 `{authorized_class}`"
            ),
        });
    }
    let adjudicator = string_field(authority, "identity")?.to_owned();
    // Through the domain constructor rather than trusted: an empty adjudicator
    // is a decision nobody signed, which `AdjudicationAuthority::new` refuses at
    // write time and a reader must refuse at read time for the same reason.
    AdjudicationAuthority::new(
        kirra_world::same_as_adjudication::AUTHORIZED_ADJUDICATOR_CLASS,
        adjudicator.clone(),
    )
    .map_err(|e| AdjudicationDecodeError::Domain {
        detail: e.to_string(),
    })?;

    let decided = root
        .get("decided_at")
        .ok_or(AdjudicationDecodeError::Field {
            key: "decided_at",
            detail: "absent".to_owned(),
        })?;
    let decided = payload_object(decided)?;
    // `as_u64`, matching `DomainInstant::ms`. A negative JSON number is not a
    // reading on any clock, and coercing one would put an instant before the
    // epoch into a field nothing else in the store can express.
    let ms = decided
        .get("ms")
        .and_then(serde_json::Value::as_u64)
        .ok_or(AdjudicationDecodeError::Field {
            key: "decided_at.ms",
            detail: "absent, negative, or not an integer".to_owned(),
        })?;
    let domain = match string_field(decided, "domain")? {
        "boundary" => kirra_world::observation::ClockDomain::Boundary,
        "system" => kirra_world::observation::ClockDomain::System,
        other => {
            return Err(AdjudicationDecodeError::Field {
                key: "decided_at.domain",
                detail: format!(
                    "`{other}` is not a clock this build knows — \
                     AOU-TIMESYNC-001 forbids guessing a domain"
                ),
            })
        }
    };

    Ok(StoredAdjudication {
        pair,
        outcome,
        candidate_observation_id,
        adjudicator,
        decided_at: kirra_world::observation::DomainInstant { ms, domain },
    })
}
