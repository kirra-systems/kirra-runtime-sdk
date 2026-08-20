//! **Promoted `same_as` identity as a projection** — `WM_SCOPE.md` §5 box 5a.
//!
//! Box 2a built the production door that writes a `same_as` *candidate*; box 2b
//! built the door that lets an authorized adjudicator judge a **persisted**
//! candidate and record the decision. This folds those decisions into the
//! relationships that currently hold.
//!
//! # Scope, stated before the code
//!
//! > The relationship projection is authoritative over promoted identity
//! > decisions, not over continued retention of the candidate evidence that
//! > motivated them. If cited candidate evidence is compacted, the relationship
//! > remains valid as adjudicated, while its explanatory provenance may degrade.
//!
//! That separation is what `KIRRA-WM-IDENTITY-FRESHNESS-001` already ruled at
//! the freshness layer — a promoted `same_as` is `Timeless` *as an adjudicated
//! identity fact* — carried down into storage. The projection row holds the
//! decision; the candidate it cites is `retention_class = "raw"` and may be
//! compacted out from under it. See *Provenance is a citation, not a flag*.
//!
//! # Operator-authored promotions are the only production input today
//!
//! Said plainly rather than left to be inferred from the fold's filter.
//! `KIRRA-WM-PROMOTION-001` v1 authorizes `SourceClass::Operator` and nothing
//! else, `AdjudicationAuthority::new` refuses every other class, and
//! `WorldStore::adjudicate_same_as` is the only writer of the rows this folds.
//! So every row in this table traces to a human decision. An automated
//! adjudicator is not merely unimplemented — it is unruled.
//!
//! # What it consumes, and what it will not invent
//!
//! * `kind = same_as_adjudication` at `claim_status = 'confirmed'` **only**. A
//!   candidate is `claim_status = 'candidate'` and is therefore not selected —
//!   the same predicate `WorldStore::fold_entity_range` inherits, and the reason
//!   a matcher cannot reach this table by proposing harder.
//! * The pair comes from the row's indexed `subject`/`object`, in the canonical
//!   order [`CandidatePair`] fixes. [`decode_same_as_adjudication`] refuses a row
//!   stored the other way round rather than repairing it.
//! * **No transitive closure.** `KIRRA-WM-TRANSITIVITY-001` permits *resolution*
//!   to traverse accepted merges and forbids this layer from emitting the
//!   traversed relation as evidence. `A=B` promoted and `B=C` promoted yield two
//!   rows here, never three. There is no closure step to disable — the fold
//!   touches exactly the one pair its input names, which is why
//!   `promotion_never_synthesises_a_transitive_relation` can assert the absence
//!   rather than the fold having declined to add it.
//!
//! [`CandidatePair`]: kirra_world::same_as_candidate::CandidatePair
//! [`decode_same_as_adjudication`]:
//!     crate::same_as_adjudication_record::decode_same_as_adjudication
//!
//! # A later decision supersedes an earlier one, and a non-promotion WITHDRAWS
//!
//! `KIRRA-WM-IDENTITY-FRESHNESS-001`: the identity decision *"remains valid
//! until changed by later adjudication"*. So the fold consumes **every**
//! confirmed decision about a pair, not only the promotions, and the newest one
//! wins. The table then holds only the pairs whose newest decision is
//! `Promoted`.
//!
//! [`Outcome::Rejected`] withdrawing a prior promotion is uncontroversial.
//! [`Outcome::Unresolved`] withdrawing one is a **choice this box makes**, and
//! the alternative is real: an operator recording *"I can no longer tell"* might
//! mean *leave the earlier decision standing*. It is not taken, for one reason —
//! the log does not record which they meant, so abstaining would make the
//! table's contents depend on a distinction nothing wrote down, and it would err
//! toward continuing to assert an identity the most recent authorized decision
//! declined to affirm. Withdrawal is the direction that fails closed.
//!
//! If operators want abstain semantics, that is a ruling and a new
//! [`Outcome`] variant, not a quiet change here — the exhaustive match in
//! [`fold_same_as_adjudication`] makes adding one a compile error.
//!
//! # Provenance is a citation, not a flag
//!
//! The row stores `candidate_observation_id` — the id of the judged proposal —
//! and **no** "fully evidenced" boolean. That is the structural half of the
//! scope statement above: there is no field that could be left `true` after the
//! evidence was compacted, because there is no field. Whether that citation
//! still resolves is answered by asking, through
//! `WorldStore::relationship_provenance`, which returns box 4b's
//! [`CitationResolution`] — a type whose `Resolved` variant cannot be produced
//! without a visible carrier. After compaction the honest answer degrades to
//! `Dangling { PossiblyCompacted }` while the relationship itself is untouched,
//! which is exactly what
//! `a_promoted_relationship_survives_compaction_of_its_candidate` proves.
//!
//! [`CitationResolution`]: crate::provenance_graph::CitationResolution
//!
//! # No DDL in the ratified surface, and no schema bump
//!
//! `WM2_EVENT_SCHEMA.md` §7 names `relationships_projection` under *what this
//! ruling does not decide* — a rebuildable view that follows from the fold.
//! `SCHEMA_VERSION` is untouched, and the DDL is installed **by the first fold,
//! never at `open`**, for [`crate::entity_projection`]'s reason: ADR-0041 D-20's
//! `log_only_bytes` is the size of a store holding only the log, and adding root
//! pages at `open` would move that figure for a store that never projects.

use std::collections::BTreeMap;

use kirra_world::observation::DomainInstant;
use kirra_world::same_as_adjudication::Outcome;
use kirra_world::same_as_candidate::CandidatePair;

use crate::same_as_adjudication_record::StoredAdjudication;

/// The lazily-installed projection table. See the module docs for why this is
/// not schema DDL and why it is not installed at `open`.
pub const RELATIONSHIP_PROJECTION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS relationships_projection (
    low                      TEXT    NOT NULL,
    high                     TEXT    NOT NULL,
    -- The confirmed adjudication event that put this row here. An operator
    -- reads it to find the decision, and the fold records it so a superseding
    -- decision is visibly newer rather than merely different.
    decided_generation       INTEGER NOT NULL,
    -- The persisted candidate the decision judged. A CITATION, not a flag:
    -- there is deliberately no "fully evidenced" column, because a compacted
    -- candidate would leave one reading true. See the module docs.
    candidate_observation_id TEXT    NOT NULL,
    adjudicator              TEXT    NOT NULL,
    decided_at_ms            INTEGER NOT NULL,
    decided_at_domain        TEXT    NOT NULL,
    PRIMARY KEY (low, high)
);
"#;

/// This projection's checkpoint name.
///
/// Its own, not the entity projection's: the two folds consume different row
/// kinds and advance independently, and sharing a cursor would make one fold's
/// progress silently skip the other's input.
pub const RELATIONSHIP_PROJECTION: &str = "relationships_projection";

/// The accumulator key — the canonical pair, as two ids.
///
/// A tuple rather than a joined string: two entity ids that concatenate to the
/// same bytes under some separator would collide on one row, and the ids are
/// caller-supplied.
pub type PairKey = (String, String);

/// The key a pair folds under.
#[must_use]
pub fn pair_key(pair: &CandidatePair) -> PairKey {
    (
        pair.low().as_str().to_owned(),
        pair.high().as_str().to_owned(),
    )
}

/// One relationship that currently holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedRelationship {
    /// The pair, canonically ordered.
    pub pair: CandidatePair,
    /// The generation of the confirmed adjudication that promoted it.
    pub decided_generation: i64,
    /// The persisted candidate that decision judged. May no longer resolve —
    /// see the module docs.
    pub candidate_observation_id: String,
    /// Who decided.
    pub adjudicator: String,
    /// When they decided, on a named clock.
    pub decided_at: DomainInstant,
}

// SEMANTICS-PIN-BEGIN: relationship_fold
//
// Digested by `ci/check_world_semantics.py` and pinned in
// `semantics::SEMANTICS` — see `crate::semantics` for the two-check rationale.

/// **The pure fold step.** Apply one confirmed decision to the accumulator.
///
/// `BTreeMap` rather than `HashMap` for [`crate::projection::fold_all`]'s
/// reason: the state digest is taken over iteration order, and a digest that
/// depended on hash seeding would compare unequal to itself.
///
/// # Ordering is a precondition, not a guard
///
/// Callers apply this in generation order — `WorldStore::fold_relationship_range`
/// does, by `ORDER BY generation ASC`. There is deliberately no
/// `if generation < existing.decided_generation { return }` check: on the one
/// caller that exists it can never be true, and a comparison that is always
/// false reads as the enforcement and stops a reader looking for the real one.
/// (The same defect this crate already removed once, from
/// `WorldStore::adjudicate_same_as`.)
///
/// # The accumulator holds only what currently holds
///
/// A withdrawn pair is REMOVED rather than kept with a rejected marker. That is
/// what lets an incremental fold seed itself from the stored rows — which carry
/// promotions only — and still reach the same state a rebuild from generation 0
/// reaches, without the table needing to store decisions it does not publish.
pub fn fold_same_as_adjudication(
    acc: &mut BTreeMap<PairKey, ProjectedRelationship>,
    decision: &StoredAdjudication,
    generation: i64,
) {
    let key = pair_key(&decision.pair);
    // Exhaustive, no wildcard: a new `Outcome` variant must be a COMPILE error
    // here rather than silently taking whichever arm a `_` fell into. Adding
    // one is a ruling about what it does to a standing promotion.
    match decision.outcome {
        Outcome::Promoted => {
            acc.insert(
                key,
                ProjectedRelationship {
                    pair: decision.pair.clone(),
                    decided_generation: generation,
                    candidate_observation_id: decision.candidate_observation_id.clone(),
                    adjudicator: decision.adjudicator.clone(),
                    decided_at: decision.decided_at,
                },
            );
        }
        // Both WITHDRAW a standing promotion, and on a pair that was never
        // promoted both are no-ops. See the module docs for why `Unresolved`
        // is a withdrawal rather than an abstention.
        Outcome::Rejected | Outcome::Unresolved => {
            acc.remove(&key);
        }
    }
}

/// Fold a whole sequence of `(generation, decision)` in order.
#[must_use]
pub fn fold_all<'a, I>(decisions: I) -> BTreeMap<PairKey, ProjectedRelationship>
where
    I: IntoIterator<Item = (i64, &'a StoredAdjudication)>,
{
    let mut acc = BTreeMap::new();
    for (generation, d) in decisions {
        fold_same_as_adjudication(&mut acc, d, generation);
    }
    acc
}

// SEMANTICS-PIN-END: relationship_fold

/// The stored spelling of a clock domain.
#[must_use]
pub fn clock_domain_token(domain: kirra_world::observation::ClockDomain) -> &'static str {
    match domain {
        kirra_world::observation::ClockDomain::Boundary => "boundary",
        kirra_world::observation::ClockDomain::System => "system",
    }
}

/// The inverse of [`clock_domain_token`].
///
/// `None` for a token this build does not know. Not defaulted: AOU-TIMESYNC-001
/// makes the clock a stated property of a timestamp, and guessing which one a
/// row meant is how a boundary instant gets compared against a system one.
#[must_use]
pub fn clock_domain_from_token(token: &str) -> Option<kirra_world::observation::ClockDomain> {
    match token {
        "boundary" => Some(kirra_world::observation::ClockDomain::Boundary),
        "system" => Some(kirra_world::observation::ClockDomain::System),
        _ => None,
    }
}

/// A digest over a relationship accumulator, in key order.
///
/// Takes the accumulator rather than reading the table, so the value written
/// into the checkpoint is the one the fold actually produced.
///
/// **`decided_generation` and the citation are inside it**, not just the pair.
/// Digesting membership alone would rate two projections equal when they agree
/// on *which* pairs hold but disagree on which decision put them there — and
/// that is the field an operator follows to the decision, so the
/// rebuild-equals-incremental check would pass while pointing two stores at
/// different history. Exactly the reason
/// [`crate::entity_projection::state_digest_of`] covers the contradiction
/// payload rather than the flag.
#[must_use]
pub fn state_digest_of(rows: &BTreeMap<PairKey, ProjectedRelationship>) -> String {
    let mut buf = String::new();
    for ((low, high), r) in rows {
        buf.push_str(low);
        buf.push('\u{1f}');
        buf.push_str(high);
        buf.push('\u{1f}');
        buf.push_str(&r.decided_generation.to_string());
        buf.push('\u{1f}');
        buf.push_str(&r.candidate_observation_id);
        buf.push('\u{1f}');
        buf.push_str(&r.adjudicator);
        buf.push('\u{1f}');
        buf.push_str(&r.decided_at.ms.to_string());
        buf.push('\u{1f}');
        buf.push_str(clock_domain_token(r.decided_at.domain));
        buf.push('\u{1e}');
    }
    crate::sha256_hex(buf.as_bytes())
}
