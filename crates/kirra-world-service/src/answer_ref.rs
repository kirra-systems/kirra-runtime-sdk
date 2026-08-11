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
//! Since box 3h it re-executes against `read_composed_at_generation`, which
//! reconstructs the claims AND the identity graph at that one coordinate — so a
//! resolved ref resolves its objects through the graph as it stood then, never
//! through today's.
//!
//! # What a ref carries, and why each field
//!
//! | Field | Why |
//! |---|---|
//! | query kind | which query this describes; the ref is meaningless without it |
//! | parameters | subject, clock, staleness budget — change one, change the answer |
//! | pinned generation | the snapshot coordinate to re-execute AT |
//! | semantic versions | the rules the answer was produced under — a SET, one entry per rule the query family depends on |
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

use kirra_world_store::snapshot::{Irreproducible, PinnedComposedRead};
use kirra_world_store::WorldStore;

use crate::read_view::{AskError, ObjectIdentity, WorldAnswer};
use crate::semantics::{SemanticVersions, VersionDifference};

/// **The semantics a `CurrentSubject` answer is produced under, right now.**
///
/// A convenience over [`SemanticVersions::for_query`], which is where the set is
/// actually derived. THREE rules bear on a resolved ref since box 3h — the
/// claim fold (`supersedes` / `fold_step`, which decides which claim wins a
/// key), the **identity fold** (which builds the graph a resolved object is
/// looked up in), and the boundary's admissibility test (which decides whether
/// a claim is servable at all). All three come from their crates' live
/// declarations rather than being restated here.
///
/// # This replaced a single opaque constant, and the difference is box 3b
///
/// The first version of this file carried `RULE_VERSION: u32 = 1`: one number,
/// hand-bumped, pinned by one corpus. It refused on a mismatch, which was real
/// — but it could not say *which* rule moved, and it covered two of the four
/// versioned rules in the system while the identity and subject-summary folds
/// had no declared version at all. Both gaps are what 3b names.
///
/// The set is not decorative in the other direction either: box 3h ADDED
/// `entity_fold` to it, and did so because a red test said the old membership
/// claim had stopped being true — not because someone edited a list to match
/// the code.
#[must_use]
pub fn current_semantics() -> SemanticVersions {
    SemanticVersions::for_query(QueryKind::CurrentSubject)
}

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
    semantics: SemanticVersions,
}

impl AnswerRef {
    /// Describe a `CurrentSubject` query at a pinned generation.
    ///
    /// The semantic versions are stamped from the live declarations rather than
    /// accepted from the caller: a ref records the semantics its answer was
    /// produced under, and letting a caller name them would let it claim any.
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
            semantics: SemanticVersions::for_query(QueryKind::CurrentSubject),
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
    pub fn semantics(&self) -> &SemanticVersions {
        &self.semantics
    }

    /// Rebuild a ref that was recorded under a different semantic version set.
    ///
    /// For tests and for a reader decoding a persisted ref. Deliberately
    /// explicit — there is no way to reach it by accident, and a ref built this
    /// way says so by carrying versions that may not be this build's.
    #[must_use]
    pub fn recorded_under(mut self, semantics: SemanticVersions) -> Self {
        self.semantics = semantics;
        self
    }

    /// Rebuild a ref with ONE rule's recorded version overridden.
    ///
    /// The common shape in a test — *"what if only the fold had moved?"* — and
    /// writing it out longhand each time invites restating the whole set, which
    /// would make the test pass for a reason it did not intend. An unknown rule
    /// name is added rather than rejected, so a ref carrying a dependency this
    /// build no longer has is representable.
    #[must_use]
    pub fn recorded_with(self, rule: &str, version: u32) -> Self {
        let mut entries: Vec<_> = self
            .semantics
            .entries()
            .iter()
            .filter(|e| e.rule != rule)
            .cloned()
            .collect();
        entries.push(crate::semantics::RuleVersion {
            rule: rule.to_string(),
            version,
        });
        self.recorded_under(SemanticVersions::new(entries))
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
        let differences = self
            .semantics
            .differences(&SemanticVersions::for_query(self.kind));
        if !differences.is_empty() {
            return Ok(RefResolution::VersionMismatch { differences });
        }

        // COMPOSED, not two reads. Box 3h: an historical answer resolves
        // objects against the identity graph as it stood at this coordinate.
        // Reading the two halves separately would put a live `identity_view`
        // one autocomplete away, and today's merges would silently rewrite what
        // a recorded reference means.
        let composed = match store.read_composed_at_generation(self.generation)? {
            PinnedComposedRead::Reproduced(c) => c,
            PinnedComposedRead::Irreproducible(reason) => {
                return Ok(RefResolution::Irreproducible(reason))
            }
        };

        let clock = self.now_ms.max(0).unsigned_abs();
        let mut answers = Vec::new();
        for claim in composed.claims().current(&self.subject, self.now_ms) {
            if !crate::read_view::is_admissible_for_ref(&claim, clock, self.staleness_budget_ms) {
                continue;
            }
            answers.push(crate::read_view::bind_composed(
                &claim,
                clock,
                self.staleness_budget_ms,
                composed.identity(),
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
    ///
    /// Carries every rule that moved, by name. A refusal that said only *"the
    /// rules changed"* would leave an operator holding a reference they cannot
    /// act on — the versions exist precisely so the answer to *"changed how?"*
    /// is in the refusal rather than in a changelog.
    VersionMismatch {
        /// Every rule whose version differs, recorded versus current.
        differences: Vec<VersionDifference>,
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
