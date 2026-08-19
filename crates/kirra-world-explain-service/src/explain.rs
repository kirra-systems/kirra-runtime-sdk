//! **`explain_current_subject` — the World-side explanation core.**
//!
//! Tier 4 box 3a. Turns a subject name into a bounded, presentation-safe
//! [`ExplanationArtifact`]. No HTTP, no Mick, and no `AnswerRef` leaves this
//! module: the transport (3b) wraps this, it does not reach past it.
//!
//! # The two steps, named for what they are
//!
//! This is NOT `Ask → Explain`, and calling it that would misdescribe the code.
//! [`WorldAnswer`] exposes `event_id` and a provenance digest but **no
//! generation**, so an `Ask` cannot yield the coordinate a provenance walk roots
//! at. The public route from a subject to a generation is the LINEAGE family:
//!
//! ```text
//! subject
//!   ↓  bounded Lineage query, server-chosen bounds     ← step 1
//! the newest retained generation for that subject
//!   ↓  bounded provenance walk at that pin             ← step 2
//! ProvenanceTree
//!   ↓  project + label
//! ExplanationArtifact
//! ```
//!
//! # Every bound is chosen HERE
//!
//! The caller supplies a subject and nothing else. Page size, graph depth, node
//! ceiling and the pin are all fixed below, which is what makes this an
//! explanation operation rather than a query surface: there is no argument a
//! caller could set to make the work larger, so there is nothing to abuse.
//!
//! [`GraphSpec`] has no `Default` on purpose — *"a default bound is the shape of
//! bound people stop noticing"* — so the bounds here are named constants with
//! reasons, not `GraphSpec::widest()`.
//!
//! [`WorldAnswer`]: kirra_world_service::read_view::WorldAnswer

use kirra_explain_types::ExplanationArtifact;
use kirra_world_service::explain::project_explanation;
use kirra_world_service::freshness::FreshnessSource;
use kirra_world_service::lineage::LineageRef;
use kirra_world_service::query::{Lineage, QueryEngine};
use kirra_world_service::read_view::AskError;
use kirra_world_store::provenance_graph::{GraphSpec, GraphSpecError};
use kirra_world_store::{StoreError, WorldStore};

use crate::labels::StoreLabels;

/// How many lineage entries the root-selection step reads.
///
/// One would do to find the newest, but a page of one cannot tell "this subject
/// has exactly one claim" from "there are more and you saw the first" — and the
/// artifact's completeness flags are built out of exactly that kind of
/// distinction. Small, because this step only picks a root.
pub const ROOT_LINEAGE_PAGE: usize = 8;

/// Provenance depth this operation walks.
///
/// Well under `MAX_PROVENANCE_DEPTH` (32). An explanation is read by a human,
/// and a thirty-deep citation chain is not an explanation — it is a dump with a
/// narrator. Truncation is reported by the artifact, so a deeper graph degrades
/// visibly rather than silently.
pub const EXPLAIN_DEPTH: usize = 4;

/// Provenance nodes this operation walks.
///
/// Well under `MAX_PROVENANCE_NODES` (256), for the same reason.
pub const EXPLAIN_NODES: usize = 32;

/// Why an explanation could not be produced.
///
/// Distinct variants because the operator-facing answers differ, and collapsing
/// them would be this tier's own defect: *"nothing is recorded about that
/// subject"* and *"the store could not be read"* are not the same statement, and
/// an unavailable explanation must never be reported as an empty one.
#[derive(Debug)]
pub enum ExplainError {
    /// The store could not be read.
    Store(StoreError),
    /// The lineage query could not be answered.
    Ask(AskError),
    /// The lineage query was refused rather than answered — an irreproducible
    /// coordinate or a changed selection rule. Carried separately because a
    /// REFUSAL is a fact about reproducibility, not an absence of evidence.
    LineageRefused,
    /// Nothing is retained about this subject, so there is nothing to explain.
    ///
    /// An honest empty, distinct from every failure above.
    NothingRecorded,
    /// The configured bounds are not valid ones.
    ///
    /// Only reachable if the constants above are edited to something the store
    /// refuses; surfaced rather than unwrapped so that edit fails loudly.
    Bounds(GraphSpecError),
}

impl From<StoreError> for ExplainError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<AskError> for ExplainError {
    fn from(e: AskError) -> Self {
        Self::Ask(e)
    }
}

impl std::fmt::Display for ExplainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(e) => write!(f, "the store could not be read: {e:?}"),
            Self::Ask(e) => write!(f, "the lineage query failed: {e:?}"),
            Self::LineageRefused => f.write_str(
                "the lineage query was refused — the coordinate is irreproducible \
                 or the selection rule changed",
            ),
            Self::NothingRecorded => f.write_str("nothing is recorded about that subject"),
            Self::Bounds(e) => write!(f, "the configured explanation bounds are invalid: {e:?}"),
        }
    }
}

impl std::error::Error for ExplainError {}

/// **Explain the current answer about `subject`.**
///
/// The one operation this crate offers.
///
/// # There is deliberately no clock parameter
///
/// An earlier draft took `now_ms`, by analogy with the read path. It was never
/// used, and that is the interesting part: the pin here is the log HEAD — a
/// generation — and the lineage family is addressed by generation, not by wall
/// time. So the artifact is a pure function of the store's state.
///
/// Carrying an unused `now_ms` would have been a lie about what the result
/// depends on, and silencing it as `_now_ms` would have been the same lie with
/// the compiler talked out of mentioning it. Removed instead, which also buys
/// something real: two calls against an unchanged store produce the same
/// artifact, so a divergence is evidence rather than noise.
///
/// # Errors
///
/// [`ExplainError`], whose variants keep *unavailable* apart from *empty*.
pub fn explain_current_subject(
    store: &WorldStore,
    subject: &str,
) -> Result<ExplanationArtifact, ExplainError> {
    // The pin is the FOLDED projection coordinate, not the raw log head.
    //
    // Found by the tests refusing: the lineage family is addressed by the
    // projection coordinate, so a pin past the fold is `Irreproducible` — the
    // store correctly declining to reproduce a coordinate it has not projected
    // to. Using `cursor_anchor().head` asked for exactly that whenever an
    // append had not yet been folded.
    //
    // Pinning to the fold is also the honest reading of "the current answer":
    // it describes what the store has actually projected, rather than raw
    // events the answer path cannot yet see. And it is a READ — this operation
    // never advances the fold, because an explanation must not change what it
    // is explaining.
    //
    // Taken FIRST so every step below reads the same coordinate: taking it
    // later would let a concurrent fold produce an artifact describing two
    // different instants.
    let head = store.projection_generation()?;

    // --- step 1: subject -> root generation, through the typed engine --------
    let engine = QueryEngine::new(store, FreshnessSource::Ruled);
    let resolution = engine.execute(Lineage {
        reference: LineageRef::subject_lineage(subject, head, ROOT_LINEAGE_PAGE),
    })?;

    let page = match resolution.resolved() {
        Some(p) => p,
        // A refusal is NOT an absence. Reporting it as "nothing recorded" would
        // tell an operator the subject has no history when the truth is that
        // the store declined to reproduce it.
        None => return Err(ExplainError::LineageRefused),
    };

    let root = match page.entries().iter().map(|e| e.generation()).max() {
        Some(g) => g,
        None => return Err(ExplainError::NothingRecorded),
    };

    // --- step 2: bounded provenance walk at that pin -------------------------
    let spec = GraphSpec::new(EXPLAIN_DEPTH, EXPLAIN_NODES).map_err(ExplainError::Bounds)?;
    let tree = store.provenance_tree(root, head, spec)?;

    // --- project, with labels that can tell absent from deleted --------------
    let labels = StoreLabels::new(store);
    Ok(project_explanation(&tree, &labels, head)?)
}
