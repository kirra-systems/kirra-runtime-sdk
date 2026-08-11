//! **The ruled `AnswerRef`** — a reproducible descriptor, not a stored answer.
//!
//! `KIRRA-WM-ANSWER-IDENTITY-001`:
//!
//! > Tier 3 answers have no durable stored identity. An `AnswerRef` is a
//! > reproducible DESCRIPTOR, not a persisted answer row. […] Resolving a ref
//! > means **re-execute this exact deterministic query against the same
//! > snapshot** and return its lineage, not *fetch the stored answer*.
//!
//! `KIRRA-WM-ANSWERREF-NAMING-001` reserved this name for exactly that contract
//! and forbade putting it on a drift *detector*. The name is taken here because
//! the mechanism now exists: `ReadSnapshot::read_at_generation` re-executes
//! against the same snapshot, and fails closed when it cannot.
//!
//! # What a ref carries, and why each field
//!
//! | Field | Why |
//! |---|---|
//! | query kind | which query this describes; the ref is meaningless without it |
//! | parameters | subject, clock, staleness budget — change one, change the answer |
//! | pinned generation | the snapshot coordinate to re-execute AT |
//! | rule version | the semantics the answer was produced under |
//!
//! Everything needed to re-execute, and nothing that is the answer itself. A ref
//! holds no claims, no digests of claims, and no summary — because a durable
//! answer row would need its own retention horizon, its own compaction story and
//! its own provenance, recursively, which is the second store §10 puts out of
//! scope.
//!
//! # `KIRRA-WM-REPRODUCIBILITY-HORIZON-001`
//!
//! > **Retention policy sets the historical reproducibility horizon for durable
//! > answer references.** An `AnswerRef` is only as durable as the oldest
//! > generation still reproducible from retained evidence and citations.
//! > *"Durable reference"* must never be read as *"forever replayable"*.
//!
//! Stated on the contract rather than in a footnote, because the failure it
//! prevents is someone keeping refs as an audit artifact and discovering years
//! later that the retention horizon swallowed them. A ref that has aged past the
//! compaction floor resolves to [`RefResolution::Irreproducible`] — which is an
//! honest answer, and the reason resolution is not infallible.

use kirra_world_store::snapshot::{Irreproducible, PinnedRead};
use kirra_world_store::WorldStore;

use crate::read_view::{AskError, ObjectIdentity, WorldAnswer};

/// **The semantics an answer was produced under.**
///
/// Bumped when a rule that can change an answer changes. Two rules bear on a
/// resolved ref today: the projection fold (`supersedes` / `fold_step`, which
/// decides which claim wins a key) and the boundary's admissibility test (which
/// decides whether a claim is servable at all).
///
/// # This is not decorative, and that took a mechanism
///
/// A hand-bumped constant with nothing behind it is exactly what Tier 3 box 3b
/// calls *"decorative metadata"* — it would read the same across a semantics
/// change, so [`RefResolution::VersionMismatch`] would never fire when it
/// mattered and the ref would replay under new rules while claiming the old
/// ones. `answer_ref.rs`'s corpus test pins the fold's observable output against
/// this constant: change the rule without changing the version and the test
/// fails, naming the obligation.
///
/// # What it is NOT
///
/// It is not box 3b. 3b asks for declared, behaviour-changing, corpus-and-source
/// pinned versioning across *rules and projections generally*; this covers the
/// two rules THIS ref's resolution depends on, which is the honest subset a ref
/// can carry today. Widening it is 3b's job.
pub const RULE_VERSION: u32 = 1;

/// Which query a ref describes.
///
/// An enum with one variant today, and deliberately an enum: the ruling requires
/// every public Tier 3 query to be fully serializable and deterministic, so the
/// query's *identity* has to be a closed set rather than a free-form string a
/// caller could invent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryKind {
    /// [`crate::read_view::WorldView::ask`] — what is currently known about one
    /// subject.
    CurrentSubject,
}

/// **A reproducible descriptor for one answer.**
///
/// Equality and hashing are structural over every field, which is what makes
/// *"same query + same generation + same version produces the same ref"* a
/// property of the type rather than a convention. There is deliberately no
/// interior mutability, no clock read, and no random component — a ref that
/// varied run to run could not be recorded.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnswerRef {
    kind: QueryKind,
    subject: String,
    now_ms: i64,
    staleness_budget_ms: Option<u64>,
    generation: i64,
    rule_version: u32,
}

impl AnswerRef {
    /// Describe a `CurrentSubject` query at a pinned generation.
    ///
    /// The rule version is stamped from [`RULE_VERSION`] rather than accepted
    /// from the caller: a ref records the semantics its answer was produced
    /// under, and letting a caller name them would let it claim any.
    #[must_use]
    pub fn current_subject(
        subject: impl Into<String>,
        now_ms: i64,
        staleness_budget_ms: Option<u64>,
        generation: i64,
    ) -> Self {
        Self {
            kind: QueryKind::CurrentSubject,
            subject: subject.into(),
            now_ms,
            staleness_budget_ms,
            generation,
            rule_version: RULE_VERSION,
        }
    }

    /// The query this describes.
    #[must_use]
    pub fn kind(&self) -> QueryKind {
        self.kind
    }

    /// The subject asked about.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// The query instant.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.now_ms
    }

    /// The caller's staleness budget, as supplied.
    #[must_use]
    pub fn staleness_budget_ms(&self) -> Option<u64> {
        self.staleness_budget_ms
    }

    /// The snapshot coordinate to re-execute at.
    #[must_use]
    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// The semantics this ref's answer was produced under.
    #[must_use]
    pub fn rule_version(&self) -> u32 {
        self.rule_version
    }

    /// Rebuild a ref that was recorded under a different rule version.
    ///
    /// For tests and for a reader decoding a persisted ref. Deliberately
    /// explicit — there is no way to reach it by accident, and a ref built this
    /// way says so by carrying a version that may not be [`RULE_VERSION`].
    #[must_use]
    pub fn recorded_under(mut self, rule_version: u32) -> Self {
        self.rule_version = rule_version;
        self
    }

    /// **Re-execute this exact query against the same snapshot.**
    ///
    /// # Order of refusals, which is not arbitrary
    ///
    /// The version check runs FIRST, before the store is touched. Re-executing
    /// under new semantics and then noticing is not a check — the answer would
    /// already have been computed under rules the ref never described, and
    /// whether it got returned would depend on a later branch.
    ///
    /// # Errors
    ///
    /// Only on a storage fault, or `StoreError::InvalidGeneration` for a ref
    /// carrying a negative generation. Irreproducibility is an OUTCOME, not an
    /// error: *"we deleted the evidence"* is a fact about the data.
    pub fn resolve(&self, store: &WorldStore) -> Result<RefResolution, AskError> {
        if self.rule_version != RULE_VERSION {
            return Ok(RefResolution::VersionMismatch {
                recorded: self.rule_version,
                current: RULE_VERSION,
            });
        }

        let pinned = match store.read_at_generation(self.generation)? {
            PinnedRead::Reproduced(p) => p,
            PinnedRead::Irreproducible(reason) => return Ok(RefResolution::Irreproducible(reason)),
        };

        let clock = self.now_ms.max(0).unsigned_abs();
        let mut answers = Vec::new();
        for claim in pinned.current(&self.subject, self.now_ms) {
            if !crate::read_view::is_admissible_for_ref(&claim, clock, self.staleness_budget_ms) {
                continue;
            }
            answers.push(crate::read_view::bind_pinned(
                &claim,
                clock,
                self.staleness_budget_ms,
            )?);
        }
        Ok(RefResolution::Resolved(answers))
    }
}

/// What resolving a ref produced.
///
/// Three outcomes, and **no silent fallback to current state**. That absence is
/// the contract: a ref that cannot be re-executed says so, rather than handing
/// back today's answer under yesterday's coordinate.
#[derive(Debug, Clone)]
pub enum RefResolution {
    /// Re-executed against the same snapshot.
    Resolved(Vec<WorldAnswer>),
    /// The referenced snapshot is no longer reproducible — see
    /// `KIRRA-WM-REPRODUCIBILITY-HORIZON-001`.
    Irreproducible(Irreproducible),
    /// The rules changed since the ref was recorded.
    ///
    /// Refused rather than replayed, because replaying would answer a question
    /// the ref does not describe: the coordinate would be honoured and the
    /// SEMANTICS silently swapped, which is the subtler half of falling forward.
    VersionMismatch {
        /// The version the ref was recorded under.
        recorded: u32,
        /// The version this build implements.
        current: u32,
    },
}

impl RefResolution {
    /// The answers, if it resolved.
    #[must_use]
    pub fn resolved(&self) -> Option<&[WorldAnswer]> {
        match self {
            Self::Resolved(a) => Some(a),
            _ => None,
        }
    }

    /// Whether this is any kind of refusal.
    #[must_use]
    pub fn is_refused(&self) -> bool {
        !matches!(self, Self::Resolved(_))
    }
}

/// The object-identity an answer reached through a ref carries.
///
/// Always [`ObjectIdentity::NotResolvedInReplay`]. Stated as a named function
/// rather than left implicit because it is a real limitation with a real reason:
/// identity is a SECOND projection with its own coordinate, and the pinned read
/// exists only for `world_current`. `identity_view_at` cuts on transaction time,
/// so resolving identity here would pair a generation-pinned claim with a
/// transaction-time-pinned identity — mixing the two axes, which is exactly what
/// box 3c closed.
///
/// A generation-pinned identity read is the natural next prerequisite if refs
/// ever need to carry resolved objects. Until then a resolved ref reports the
/// stored object and says plainly that it was not resolved.
#[must_use]
pub fn pinned_object_identity() -> ObjectIdentity {
    ObjectIdentity::NotResolvedInReplay
}
