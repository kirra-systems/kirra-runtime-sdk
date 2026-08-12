//! **Declared reducer semantics — Tier 3 box 3b.**
//!
//! Every projection in this store is produced by a pure reducer. This module
//! gives each one a **declared version**, and gives that version two independent
//! things to be accountable to, because neither alone is enough:
//!
//! * a **conformance corpus** — a fixed input with a pinned rendered output, so
//!   a change in *behaviour* is visible as a diff; and
//! * a **source pin** — a digest over the reducer's own source span, so a silent
//!   edit reds CI even when the corpus happens not to exercise it.
//!
//! The corpus proves meaning; the pin proves nobody edited quietly. A version
//! checked by neither is what `WM_SCOPE.md` calls **decorative metadata** — a
//! constant nobody is obliged to move, so a consumer that refuses on a version
//! mismatch never fires when it matters.
//!
//! # `KIRRA-WM-REDUCER-VERSION-001`
//!
//! > **A reducer version changes whenever the reducer's behaviour changes in a
//! > way that can alter a derived answer. A pinned answer reference refuses
//! > replay under a different semantic version rather than replaying under the
//! > new one.**
//!
//! The second sentence is the half that gives the first one teeth. A version
//! that nothing consumes is a comment; the consumer is
//! `kirra_world_service::answer_ref`, whose `resolve` compares the versions a
//! reference was recorded under against the versions this build implements and
//! refuses on any difference.
//!
//! # What forces the bump — and the honest limit
//!
//! Three checks stack, and it is worth being precise about which one does what,
//! because the interesting failure is a behaviour change that keeps its version:
//!
//! | Check | Where | Catches |
//! |---|---|---|
//! | corpus digest == declared | `tests/semantics_corpus.rs` | behaviour moved and the declaration did not |
//! | source pin == span digest | `ci/check_world_semantics.py` | the reducer was edited at all |
//! | **corpus digest may not move at a fixed version** | `ci/check_world_semantics.py` | behaviour moved *and* the declaration was updated, but the version was not |
//!
//! The third is the one that makes a bump unavoidable in practice. The baseline
//! records what each version's corpus digest *was*; re-declaring a different
//! digest for a version already on record reds, and the only clean way forward
//! is a new version with a new row.
//!
//! **The residual, stated rather than papered over:** an author who edits the
//! Rust declaration *and* the recorded baseline row in one commit still passes.
//! No gate can force a human to increment an integer. What these remove is doing
//! it silently, doing it by accident, and doing it without a reviewer seeing a
//! diff that says — in a file whose only purpose is to be that record — that a
//! historical fact was rewritten.
//!
//! # Why the corpora are ordinary code rather than test fixtures
//!
//! They are pure functions over in-memory values: no store is opened, no schema
//! is installed, nothing is written. That matters for one specific reason beyond
//! tidiness — ADR-0041's **D-20** measurement compares the on-disk size of a
//! log-only store against one with projections, and `projection.rs` documents at
//! length that installing a projection table early would silently move that
//! figure. A corpus that touched a database would be the same mistake wearing a
//! different hat. These cannot: they have no `WorldStore` in scope.
//!
//! Being ordinary code also lets the *consumer* crate's tests reach them, which
//! `#[cfg(test)]` items cannot do across a crate boundary.
//!
//! # ADR-0042 condition (1)
//!
//! Nothing here may become a required safety input. These are pure renderings of
//! fold outputs for comparison purposes; they hold no corridor, no actuator
//! handle and no release token, and gate test t24 checks this file by contents.

use std::collections::BTreeMap;

use kirra_world::adjudication::{
    AssertIdentity, ForgetEntity, IdentityAdjudication, Justification, MergeEntities,
    RetirementReason, SplitEntity,
};
use kirra_world::observation::{ClockDomain, DomainInstant};
use kirra_world::reference::{EntityId, ObservationId};

use crate::entity_projection::{
    self, contradiction_json, lifecycle_token, origin_of, redirect_json, ProjectedEntity,
};
use crate::projection::{self, ProjectedClaim};
use crate::subject_projection::{self, ProjectedSubject, SubjectKey, SubjectObservation};

/// A versioned reducer.
///
/// A closed enum rather than a string, for the same reason
/// `kirra_world_service::answer_ref::QueryKind` is one: a rule identity a caller
/// could invent is a rule identity nothing can be pinned against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuleId {
    /// [`crate::projection::fold_step`] / [`crate::projection::supersedes`] —
    /// the `world_current` claim fold.
    WorldCurrentFold,
    /// [`crate::entity_projection::fold_adjudication`] — the identity fold.
    EntityFold,
    /// [`crate::subject_projection::subject_fold_step`] — the subject summary
    /// fold.
    SubjectSummaryFold,
    /// [`crate::lineage::select_lineage`] — the lineage selection rule.
    ///
    /// Not a *reducer*, and included anyway. The membership test this module
    /// applies is *"can changing this alter a derived answer"*, not *"is this a
    /// fold"* — and lineage selection can alter one four ways at once: the
    /// events chosen, the generation bound, the order, and where a page ends.
    /// A recorded lineage reference whose page-2 cursor was minted under a
    /// different ordering describes a different set of evidence while looking
    /// identical.
    LineageSelection,
}

impl RuleId {
    /// The stable name used in declarations, baselines and mismatch reports.
    ///
    /// Stable is the operative word: this string appears in
    /// `ci/world_semantics_baseline.json`, so renaming it orphans that rule's
    /// recorded history. It is deliberately not derived from the variant name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorldCurrentFold => "world_current_fold",
            Self::EntityFold => "entity_fold",
            Self::SubjectSummaryFold => "subject_summary_fold",
            Self::LineageSelection => "lineage_selection",
        }
    }

    /// Every versioned reducer in this store.
    ///
    /// Used by the conformance test to walk the whole set, so a rule added to
    /// [`RuleId`] without a [`RuleSpec`] fails rather than going unversioned.
    #[must_use]
    pub fn all() -> &'static [RuleId] {
        &[
            RuleId::WorldCurrentFold,
            RuleId::EntityFold,
            RuleId::SubjectSummaryFold,
            RuleId::LineageSelection,
        ]
    }
}

/// One reducer's declaration.
///
/// `source_file` and `span` are here so the gate does not have to guess where a
/// reducer lives: the pin is computed over the region between
/// `SEMANTICS-PIN-BEGIN: <span>` and `SEMANTICS-PIN-END: <span>` markers in that
/// file. Marker comments rather than function-name extraction, because a rule
/// whose boundary is inferred moves whenever someone renames or reorders, and a
/// pin that moves for cosmetic reasons trains exactly the reflexive re-pinning
/// that destroys its signal.
#[derive(Debug, Clone, Copy)]
pub struct RuleSpec {
    /// Which reducer.
    pub rule: RuleId,
    /// The declared semantic version.
    pub version: u32,
    /// SHA-256 of [`corpus_rendering`] for this rule, at this version.
    pub corpus_digest: &'static str,
    /// SHA-256 of the comment-stripped source span named by `span`.
    ///
    /// Declared here and computed by `ci/check_world_semantics.py`, not by this
    /// crate. A pin a program computes over itself at runtime proves only that
    /// the running code agrees with the running code.
    pub source_pin: &'static str,
    /// The file holding the reducer.
    pub source_file: &'static str,
    /// The marker name delimiting the reducer's span in that file.
    pub span: &'static str,
}

/// **The declared semantics of every reducer in this store.**
///
/// Formatted one field per line and parsed by `ci/check_world_semantics.py`. The
/// gate refuses to pass on an empty parse, so a formatting change that defeats
/// the parser reds rather than silently checking nothing.
pub const SEMANTICS: &[RuleSpec] = &[
    RuleSpec {
        rule: RuleId::WorldCurrentFold,
        version: 1,
        corpus_digest: "c38fc802ea1d7eca0afbf2dc5a11b8f5d55da0f4008dd43f9fcf22dae1d26da7",
        source_pin: "8725833999142be53519dc5029ab37afe03c782d57ab5d90f55dc67aed440c82",
        source_file: "crates/kirra-world-store/src/projection.rs",
        span: "world_current_fold",
    },
    RuleSpec {
        rule: RuleId::EntityFold,
        version: 1,
        corpus_digest: "966bc0bf440e824f5c3ebc916f2f753eee74dad914c015dab16dc58296b0f39e",
        source_pin: "b605bc92b7ae460293ab7f48db814fd3576c68977e1cb0c642ed19e66df9324b",
        source_file: "crates/kirra-world-store/src/entity_projection.rs",
        span: "entity_fold",
    },
    RuleSpec {
        rule: RuleId::SubjectSummaryFold,
        version: 1,
        corpus_digest: "b1008c8775e491639258f64560bcfe0abebab288022c5f03c9a7f682d79a34c7",
        source_pin: "2faa627ee865dd00229b52960b35d250caadc42acf4b10aef7bb48f97ecebb85",
        source_file: "crates/kirra-world-store/src/subject_projection.rs",
        span: "subject_summary_fold",
    },
    RuleSpec {
        rule: RuleId::LineageSelection,
        version: 1,
        corpus_digest: "750c6e83b752ea7743032696b2a57b40134e97a91492961513a61e5a1fb8bc55",
        source_pin: "e25f2863158f146facab6c2a135c0e6b6efb0fdaa50ee139329bf4b340baa964",
        source_file: "crates/kirra-world-store/src/lineage.rs",
        span: "lineage_selection",
    },
];

/// The declaration for one rule.
///
/// # Panics
///
/// If [`SEMANTICS`] has no row for `rule`. Deliberately a panic rather than an
/// `Option`: a rule with no declaration is an unversioned reducer, which is the
/// state this module exists to make unrepresentable, and handing callers a
/// `None` to ignore would let it persist.
#[must_use]
pub fn spec(rule: RuleId) -> &'static RuleSpec {
    SEMANTICS
        .iter()
        .find(|s| s.rule == rule)
        .unwrap_or_else(|| panic!("reducer {} has no declared semantics", rule.as_str()))
}

/// The declared version of one rule.
#[must_use]
pub fn version_of(rule: RuleId) -> u32 {
    spec(rule).version
}

// ---------------------------------------------------------------------------
// Corpora
// ---------------------------------------------------------------------------
//
// Each corpus is split into an INPUT and a RENDERING, rather than one function
// returning a digest. That split is what lets `tests/semantics_corpus.rs` run a
// deliberately-divergent reducer over the *same* input and assert the rendering
// moves — which is how the corpus's sensitivity to each behavioural axis is
// proved rather than assumed. A corpus that only exercises the happy path
// produces a stable digest under a mutated rule, and pins nothing.

/// The field separator inside one rendered row.
const FIELD: char = '\u{1f}';
/// The separator between rendered rows.
const ROW: char = '\u{1e}';

/// The `world_current` fold's corpus input.
///
/// Every claim here earns its place by discriminating a behaviour:
///
/// * `dock_first` then `dock_second` — an **accepted** supersession, later valid
///   time arriving later.
/// * `dock_tie` — equal valid time, higher generation: the **tiebreak**, which
///   is what makes the rule total and rebuild-equals-incremental true.
/// * `dock_backdated` — a **refused** supersession, earlier valid time arriving
///   later.
/// * a second predicate and a predicate-less claim on the same subject — the
///   **key**, so a fold that keyed on subject alone collides visibly.
///
/// # The generation ORDER here is load-bearing, and a first draft got it wrong
///
/// `dock_backdated` must carry the **highest** generation for its key. An
/// earlier draft placed it in the middle, and the corpus went blind to the axis
/// it exists to cover: under a fold ordering on generation the backdated claim
/// won *temporarily* and was then displaced by `dock_tie` anyway, so both the
/// real rule and the mutated one ended on the same row. A fold is only
/// observable through its FINAL state; an intermediate divergence that later
/// converges is invisible to any corpus.
///
/// Recorded here rather than fixed silently because it is the failure mode a
/// conformance corpus has by default, and it was found by
/// `the_claim_corpus_catches_generation_leading_valid_time` — the control doing
/// precisely the job it was added for, on its first run.
#[must_use]
pub fn world_current_corpus() -> Vec<ProjectedClaim> {
    let base = |predicate: Option<&str>, object: &str, valid_from_ms: i64, generation: i64| {
        ProjectedClaim {
            subject: "package_17".to_string(),
            predicate: predicate.map(str::to_string),
            object: Some(object.to_string()),
            kind: "mission".to_string(),
            payload: "{}".to_string(),
            frame_id: None,
            map_id: None,
            source: "sensor".to_string(),
            valid_from_ms,
            valid_to_ms: None,
            txn_time_ms: valid_from_ms,
            generation,
            event_id: format!("ev-{generation}"),
            chain_digest: format!("chain-{generation}"),
            trust: None,
        }
    };
    vec![
        base(Some("last_seen_at"), "dock_first", 1_000, 1),
        base(Some("last_seen_at"), "dock_second", 1_010, 2),
        base(Some("last_seen_at"), "dock_tie", 1_010, 3),
        base(Some("last_seen_at"), "dock_backdated", 1_005, 4),
        base(Some("carried_by"), "robot_a", 1_000, 5),
        base(None, "unpredicated", 1_000, 6),
    ]
}

/// Render a folded `world_current` accumulator.
///
/// Rendered rather than hashed at the assertion site so a failure shows **what**
/// changed. The digest is taken over this text; the text is what a human reads.
#[must_use]
pub fn render_world_current(rows: &BTreeMap<(String, String), ProjectedClaim>) -> String {
    let mut out = String::new();
    for ((subject, predicate_key), claim) in rows {
        out.push_str(subject);
        out.push(FIELD);
        out.push_str(predicate_key);
        out.push(FIELD);
        out.push_str(claim.object.as_deref().unwrap_or(""));
        out.push(FIELD);
        out.push_str(&claim.valid_from_ms.to_string());
        out.push(FIELD);
        out.push_str(&claim.generation.to_string());
        out.push(ROW);
    }
    out
}

/// The identity fold's corpus input, as `(generation, adjudication)`.
///
/// Covers creation, a merge that redirects, a split, a retirement, and — the
/// axis a happy-path corpus would miss — a **contradiction**: re-asserting an
/// id that already exists. Contradiction handling is sticky and refuses to
/// advance the entity further, so a fold that dropped either half renders
/// differently.
#[must_use]
pub fn entity_corpus() -> Vec<(i64, IdentityAdjudication)> {
    let eid = |s: &str| EntityId::new(s).expect("corpus entity id");
    let just = || {
        Justification::new([ObservationId::new("obs-corpus").expect("corpus obs")])
            .expect("corpus justification")
    };
    let at = || DomainInstant {
        ms: 1,
        domain: ClockDomain::System,
    };

    vec![
        (
            1,
            IdentityAdjudication::Assert(AssertIdentity::new(eid("a"), just(), at())),
        ),
        (
            2,
            IdentityAdjudication::Assert(AssertIdentity::new(eid("b"), just(), at())),
        ),
        (
            3,
            IdentityAdjudication::Merge(
                MergeEntities::new(vec![eid("a")], eid("b"), just(), at()).expect("corpus merge"),
            ),
        ),
        (
            4,
            IdentityAdjudication::Split(
                SplitEntity::partition(eid("b"), [eid("b1"), eid("b2")], just(), at())
                    .expect("corpus split"),
            ),
        ),
        (
            5,
            IdentityAdjudication::Forget(ForgetEntity::new(
                eid("b1"),
                RetirementReason::new("decommissioned").expect("corpus reason"),
                just(),
                at(),
            )),
        ),
        // The contradiction axis: `a` was already asserted at generation 1.
        (
            6,
            IdentityAdjudication::Assert(AssertIdentity::new(eid("a"), just(), at())),
        ),
    ]
}

/// Render a folded identity accumulator.
///
/// The contradiction **payload** is rendered, not merely a flag, for the reason
/// [`crate::entity_projection::state_digest_of`] gives: two projections that
/// refuse the same entities while naming different generations would otherwise
/// compare equal, and the generation is the field an operator uses to find the
/// disagreeing event.
#[must_use]
pub fn render_entities(rows: &BTreeMap<String, ProjectedEntity>) -> String {
    let mut out = String::new();
    for (id, e) in rows {
        out.push_str(id);
        out.push(FIELD);
        out.push_str(lifecycle_token(&e.lifecycle));
        out.push(FIELD);
        out.push_str(redirect_json(&e.lifecycle).as_deref().unwrap_or(""));
        out.push(FIELD);
        out.push_str(origin_of(&e.lifecycle).map_or("", EntityId::as_str));
        out.push(FIELD);
        out.push_str(
            &e.contradiction
                .as_ref()
                .map(contradiction_json)
                .unwrap_or_default(),
        );
        out.push(ROW);
    }
    out
}

/// The subject summary fold's corpus input.
///
/// The discriminating entries are the **out-of-order** ones. A summary fold that
/// tied `last_observed_ms` to the head — the bug `subject_fold_step`'s own docs
/// record — regresses the maximum when a later generation carries an earlier
/// transaction time, so the corpus supplies exactly that pair. Two kinds and a
/// `NULL` discriminant cover the key.
#[must_use]
pub fn subject_corpus() -> Vec<SubjectObservation> {
    let obs = |subject: &str,
               subject_kind: Option<&str>,
               txn_time_ms: i64,
               generation: i64|
     -> SubjectObservation {
        SubjectObservation {
            subject: subject.to_string(),
            subject_kind: subject_kind.map(str::to_string),
            txn_time_ms,
            generation,
            event_id: format!("ev-{generation}"),
            chain_digest: format!("chain-{generation}"),
        }
    };
    vec![
        obs("package_17", Some("entity"), 1_000, 1),
        obs("package_17", Some("entity"), 1_020, 2),
        // Later generation, EARLIER transaction time: the head must advance
        // while `last_observed_ms` must not regress.
        obs("package_17", Some("entity"), 1_010, 3),
        // Same subject name under a different kind — a distinct summary.
        obs("package_17", Some("frame"), 1_030, 4),
        // No discriminant: the sentinel, and a third distinct summary.
        obs("package_17", None, 1_040, 5),
    ]
}

/// Render a folded subject summary accumulator.
#[must_use]
pub fn render_subjects(rows: &BTreeMap<SubjectKey, ProjectedSubject>) -> String {
    let mut out = String::new();
    for ((kind, subject), s) in rows {
        out.push_str(kind.as_str());
        out.push(FIELD);
        out.push_str(subject);
        out.push(FIELD);
        out.push_str(&s.first_observed_ms.to_string());
        out.push(FIELD);
        out.push_str(&s.last_observed_ms.to_string());
        out.push(FIELD);
        out.push_str(&s.observation_count.to_string());
        out.push(FIELD);
        out.push_str(&s.last_generation.to_string());
        out.push(FIELD);
        out.push_str(&s.provenance_head);
        out.push(ROW);
    }
    out
}

/// The lineage selection rule's corpus input — the **evidence**.
///
/// Supplied deliberately **out of generation order**, so a rule that dropped the
/// sort produces a different rendering rather than accidentally the same one.
///
/// | Event | What it holds open |
/// |---|---|
/// | gen 5, `package_17` | supplied FIRST, so ordering is discriminated |
/// | gen 1, `package_17` | the oldest, and the first page's head |
/// | gen 3, **`other_subject`** | the **subject filter** — a rule ignoring it interleaves this at position 3 |
/// | gen 2, `package_17`, **candidate** | candidates are lineage; a rule that filtered on `claim_status` like the claim fold does would drop it |
/// | gen 4, `package_17` | an ordinary interior event |
/// | gen 9, `package_17` | **above the pin** in the queries that bound at 5 — the historical-correctness axis |
#[must_use]
pub fn lineage_corpus() -> Vec<crate::lineage::LineageEvent> {
    let ev = |generation: i64,
              subject: &str,
              claim_status: crate::ClaimStatus,
              writer_class: crate::WriterClass| {
        crate::lineage::LineageEvent {
            generation,
            event_id: format!("ev-{generation}"),
            observation_id: format!("obs-{generation}"),
            txn_time_ms: 1_000 + generation,
            valid_from_ms: 1_000 + generation,
            valid_to_ms: None,
            source: "warehouse-scanner".to_string(),
            source_version: "1.0.0".to_string(),
            writer_class,
            claim_status,
            provenance: format!("[\"obs-{generation}\"]"),
            kind: "mission".to_string(),
            subject: subject.to_string(),
            predicate: Some("last_seen_at".to_string()),
            object: Some(format!("dock-{generation}")),
            chain_digest: format!("chain-{generation}"),
        }
    };
    use crate::{ClaimStatus::*, WriterClass::*};
    vec![
        ev(5, "package_17", Confirmed, Sensor),
        ev(1, "package_17", Confirmed, Sensor),
        ev(3, "other_subject", Confirmed, Sensor),
        ev(2, "package_17", Candidate, LlmCandidate),
        ev(4, "package_17", Confirmed, Sensor),
        ev(9, "package_17", Confirmed, Sensor),
    ]
}

/// The queries the lineage corpus is rendered under.
///
/// A selection rule is a function of the query as well as the data, so the
/// corpus has to be a set of *queries* — one input rendered once would pin the
/// ordering and leave the bound, the cursor and the page boundary unexercised.
///
/// | Label | Holds open |
/// |---|---|
/// | `bounded` | the generation bound (gen 9 excluded) and the subject filter (gen 3 excluded), in one query |
/// | `page_1` / `page_2` / `page_3` | the cursor walk: fills, resumes, and ends — a rule that mis-set the cursor by one moves `page_2` |
/// | `exactly_full` | a page whose length equals the limit and which IS complete — the off-by-one that reports a successor that does not exist |
/// | `other_subject` | the subject filter's **positive** control: a rule that dropped every non-matching row rather than filtering by the queried value would empty this |
#[must_use]
pub fn lineage_queries() -> Vec<(&'static str, &'static str, i64, crate::lineage::LineagePage)> {
    let page = |limit: usize, after: Option<i64>| {
        crate::lineage::LineagePage::new(limit, after)
            .expect("the corpus queries must be valid, or they pin an error")
    };
    vec![
        ("bounded", "package_17", 5, page(10, None)),
        ("page_1", "package_17", 9, page(2, None)),
        ("page_2", "package_17", 9, page(2, Some(2))),
        ("page_3", "package_17", 9, page(2, Some(5))),
        ("exactly_full", "package_17", 5, page(4, None)),
        ("other_subject", "other_subject", 9, page(10, None)),
    ]
}

/// Render the lineage selection rule's verdict over its corpus.
///
/// Renders the selected **generations** and the boundary, not whole events: the
/// rule decides *which* events and *in what order*, and it cannot change an
/// event's own fields. Rendering the fields would make the digest move when an
/// unrelated column changed, which is the reflexive re-pinning these digests
/// exist to avoid.
#[must_use]
pub fn render_lineage(selection: &crate::lineage::SelectedLineage) -> String {
    use crate::lineage::PageBoundary;
    let mut out = String::new();
    for e in &selection.events {
        out.push_str(&e.generation.to_string());
        out.push(FIELD);
    }
    match &selection.boundary {
        PageBoundary::Complete => out.push_str("complete"),
        PageBoundary::More {
            next_after_generation,
        } => {
            out.push_str("more:");
            out.push_str(&next_after_generation.to_string());
        }
    }
    out
}

/// **Run one rule's corpus through the real reducer and render the result.**
///
/// This is the value the declared [`RuleSpec::corpus_digest`] is a digest of. It
/// calls the shipped reducers directly — not a copy, not a re-derivation — so a
/// change to the rule moves this string by construction.
#[must_use]
pub fn corpus_rendering(rule: RuleId) -> String {
    match rule {
        RuleId::WorldCurrentFold => {
            render_world_current(&projection::fold_all(world_current_corpus()))
        }
        RuleId::EntityFold => {
            let corpus = entity_corpus();
            render_entities(&entity_projection::fold_all(
                corpus.iter().map(|(g, a)| (*g, a)),
            ))
        }
        RuleId::SubjectSummaryFold => {
            let corpus = subject_corpus();
            render_subjects(
                &subject_projection::subject_fold_all(corpus.iter())
                    .expect("the subject corpus must fold, or it is pinning an error"),
            )
        }
        RuleId::LineageSelection => {
            let mut out = String::new();
            for (label, subject, at_generation, page) in lineage_queries() {
                out.push_str(label);
                out.push(FIELD);
                out.push_str(&render_lineage(&crate::lineage::select_lineage(
                    lineage_corpus(),
                    subject,
                    at_generation,
                    page,
                )));
                out.push(ROW);
            }
            out
        }
    }
}

/// The digest of [`corpus_rendering`], in the form [`RuleSpec::corpus_digest`]
/// declares.
#[must_use]
pub fn corpus_digest(rule: RuleId) -> String {
    digest(&corpus_rendering(rule))
}

/// Digest a corpus rendering in the form a declaration carries.
///
/// Public so the **answer boundary** — which owns a versioned rule of its own,
/// in another crate — declares its digest in the same form rather than choosing
/// a second hash and making the two tables incomparable.
#[must_use]
pub fn digest(rendering: &str) -> String {
    crate::sha256_hex(rendering.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every rule in the enum has a declaration, and every declaration names a
    /// rule in the enum. Without this a reducer added later is silently
    /// unversioned — which is the exact state 3b exists to end.
    #[test]
    fn every_rule_is_declared_exactly_once() {
        for rule in RuleId::all() {
            let found: Vec<_> = SEMANTICS.iter().filter(|s| s.rule == *rule).collect();
            assert_eq!(
                found.len(),
                1,
                "{} must have exactly one declaration",
                rule.as_str()
            );
        }
        assert_eq!(SEMANTICS.len(), RuleId::all().len());
    }

    /// A corpus that folds to nothing pins nothing, and would keep pinning
    /// nothing while the reducer was rewritten underneath it.
    #[test]
    fn no_corpus_renders_empty() {
        for rule in RuleId::all() {
            let rendered = corpus_rendering(*rule);
            assert!(
                !rendered.is_empty(),
                "{} renders an empty corpus — it pins nothing",
                rule.as_str()
            );
        }
    }

    /// Rendering must be deterministic within a process, or the digest compares
    /// unequal to itself.
    #[test]
    fn rendering_is_stable() {
        for rule in RuleId::all() {
            assert_eq!(corpus_rendering(*rule), corpus_rendering(*rule));
        }
    }

    /// Two rules rendering identically would let one stand in for the other.
    #[test]
    fn the_corpora_are_distinguishable() {
        let mut seen: Vec<String> = Vec::new();
        for rule in RuleId::all() {
            let d = corpus_digest(*rule);
            assert!(!seen.contains(&d), "{} shares a digest", rule.as_str());
            seen.push(d);
        }
    }
}
