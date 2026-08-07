//! **The answer boundary** — where Tier 3's "no bare values" rule can actually live.
//!
//! # What this is, and what it is not
//!
//! `WM_SCOPE.md` §9 asks for a small consumer wired **before** Tier 3, on the
//! argument that a real caller falsifies what a design document cannot. That
//! consumer was built, and **Fence B refused every host §9 nominates** — the
//! planner, perception and LLM crates all implement `CorridorSource`, which is
//! authoritative to the checker, so none of them may depend on Kirra World. §9
//! records the refusal.
//!
//! So this is **not** the external doer caller §9 asked for, and it should not
//! be read as one. What it is: the **shape that caller produced**, kept
//! somewhere durable instead of being lost, in the one crate that can legally
//! hold it — this one already depends on both `kirra-world` and
//! `kirra-world-store`, implements no `CorridorSource`, and sits outside the
//! safety closure.
//!
//! The falsification is already banked. What was still at risk was the *type*,
//! and a type that has met a caller is worth more to Tier 3 than one derived
//! from the rule alone.
//!
//! # The rule this exists to make structural
//!
//! Blueprint §14.2, rule 1:
//!
//! > **No API returns a bare value.** Every answer carries the value, the trust
//! > axes, the validity at the supplied clock, and a `ProvenanceHandle`. …
//! > *"a deliberate ergonomic cost: it makes 'I got a number and lost where it
//! > came from' impossible to write."*
//!
//! **It cannot be met by [`ProjectedClaim`]**, and not for want of care. That
//! type's fields are public, so
//!
//! ```ignore
//! let payload = &store.current("robot-01", now)?[0].payload;
//! ```
//!
//! compiles — no validity, no trust, no handle. `validity_at` and `grade_at` are
//! methods a caller must *remember* to call, and forgetting is the default.
//!
//! That is not a defect. `ProjectedClaim` is the projection **row**, and a row
//! is honestly a bag of columns. The rule belongs one layer out, at the boundary
//! where a row becomes an *answer* — which is here.
//!
//! # What [`WorldAnswer`] buys, and the bound on it
//!
//! It has no constructor that omits validity, trust or provenance, and
//! [`WorldView::ask`] is the only way to obtain one. So an answer in hand always
//! carries them.
//!
//! **"Trust" here means the axes, not a summary of them.** Rule 1 says *the
//! trust axes*, and an earlier draft of this type carried only the collapsed
//! [`TrustGrade`] while quoting the rule verbatim — which would have been the
//! same overclaim this module exists to catch. Both are carried now:
//! [`WorldAnswer::axes`] is what the rule requires, and [`WorldAnswer::grade`]
//! is a convenience over it. Collapsing is fine; collapsing *and discarding the
//! reason* is not, because `Weak` can mean uncorroborated, stale, or awaiting
//! adjudication and a caller who needs to know which would be left with the raw
//! log.
//!
//! **The honest bound, because overclaiming here would be the same failure the
//! rule is about:** this closes the hole at *retrieval*. A caller cannot obtain
//! the value without being handed the rest. It does **not** stop that caller
//! destructuring and passing the value onward alone — Rust cannot prevent that
//! without infecting every downstream signature, and a rule advertised as
//! airtight when it is not is worse than one whose limit is written down.
//!
//! # Three rules, honoured deliberately
//!
//! 1. **No bare values** — [`WorldAnswer`], above.
//! 2. **Queries are bounded** — [`WorldView::ask`] asks for *one subject*, never
//!    the whole projection. D-9 measured 10.5 s p99 temporal queries at 100 000
//!    entities, and ADR-0041 D-12 bars graph and temporal queries from any
//!    control or safety deadline path. The bounded call is the habit to
//!    establish before eight verbs exist.
//! 3. **`Unknown` is a success** — [`WorldLookup::Unknown`] is a variant of the
//!    `Ok` value. The error channel is for storage faults only. Conflating them
//!    is how *"I don't know"* becomes an exception somebody catches and turns
//!    into a default value.
//!
//! # Fence A still holds, and this is where it would erode
//!
//! This crate is inside Fence A's walk. A read view has no route to an actuator
//! or an authorization: it reads a projection and returns owned data. That is
//! the whole point of putting the boundary in a crate the fence already watches
//! rather than in one it does not.

use kirra_world::evidence::{DigestError, EvidenceDigest};
use kirra_world_store::{ProjectedClaim, StoreError, TrustAxes, TrustGrade, Validity, WorldStore};

/// Why an ask could not be answered at all.
///
/// Distinct from [`WorldLookup::Unknown`], and the split is rule 3: *absence of
/// knowledge is a success*, and only genuine faults use this channel.
#[derive(Debug)]
pub enum AskError {
    /// The store could not be read.
    Store(StoreError),
    /// A stored chain digest is not a digest.
    ///
    /// **Refused rather than served**, and the reasoning is this module's own:
    /// rule 1 says every answer carries a `ProvenanceHandle`, so an answer whose
    /// handle cannot be parsed is one that cannot be *cited* — serving it would
    /// break the rule this boundary exists to enforce, while looking like an
    /// ordinary answer.
    ///
    /// It belongs in the **error** channel rather than in `Unknown` because it
    /// is a storage fault: the stored bytes are not what the schema promises.
    /// `Unknown` means *"nothing is known"*, which would be a false statement
    /// here — something is known, and it is unreadable.
    CorruptProvenance {
        /// The claim's subject, so the bad row can be found.
        subject: String,
        /// What was wrong with the digest.
        cause: DigestError,
    },
}

impl fmt::Display for AskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(e) => write!(f, "store: {e:?}"),
            Self::CorruptProvenance { subject, cause } => {
                write!(f, "unreadable provenance handle for {subject:?}: {cause}")
            }
        }
    }
}

impl std::error::Error for AskError {}

impl From<StoreError> for AskError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

use core::fmt;

/// Why the world had nothing to say. **Not an error** — see rule 3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownReason {
    /// No current claim for this subject at this clock.
    ///
    /// Note what this currently conflates: a subject never heard of, and one
    /// whose claim has **expired**, arrive here identically, because
    /// [`WorldStore::current`] filters on `holds_at` before this boundary sees
    /// anything. An operator whose coverage merely lapsed is therefore told
    /// "nothing known" and sent to a coverage question rather than a freshness
    /// one. A boundary cannot reconstruct the difference; only the store can
    /// report it, which makes it Tier 3's to fix.
    NoClaim,
    /// Claims exist, but none is admissible at this clock and budget.
    ///
    /// **Unreachable today, and deliberately kept.** Two filters — the
    /// projection's `claim_status = 'confirmed'` fold and `current()`'s
    /// `holds_at` — mean an inadmissible claim never reaches this boundary. That
    /// guarantee is pinned in
    /// `kirra-world-store/tests/inadmissible_never_read.rs`.
    ///
    /// Kept because neither filter is a *stated contract*. A boundary that
    /// silently begins serving rejected or expired facts if one of them changes
    /// is a worse outcome than a variant that never occurs.
    NoneAdmissible,
}

/// One answer, which cannot exist without the things that make it citable.
#[derive(Debug, Clone)]
pub struct WorldAnswer {
    subject: String,
    predicate: Option<String>,
    value: String,
    validity: Validity,
    axes: Option<TrustAxes>,
    grade: Option<TrustGrade>,
    provenance: EvidenceDigest,
    event_id: String,
}

impl WorldAnswer {
    /// The claim's subject.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// The claim's predicate, `None` for predicate-less claims.
    #[must_use]
    pub fn predicate(&self) -> Option<&str> {
        self.predicate.as_deref()
    }

    /// The value itself.
    ///
    /// Reaching this requires already holding a `WorldAnswer`, which cannot be
    /// built without validity, grade and provenance beside it. The value never
    /// *arrives* alone — though a caller may still choose to carry it onward
    /// alone, which is the bound named in the module docs.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Validity resolved at the clock and budget the caller supplied.
    ///
    /// Resolved at construction rather than offered as a method, so it cannot be
    /// the step that gets skipped. That is the entire difference between this
    /// and [`ProjectedClaim`].
    #[must_use]
    pub fn validity(&self) -> Validity {
        self.validity
    }

    /// The three stored trust axes, or `None` when the claim is unlabelled.
    ///
    /// **This is what rule 1 actually requires** — *"the trust axes"*, not a
    /// summary of them. It is carried beside [`Self::grade`] rather than instead
    /// of it, and the distinction is not cosmetic.
    ///
    /// A grade is a **collapse**. `Weak` can mean uncorroborated, or stale, or
    /// awaiting adjudication, and a boundary that returned only the grade would
    /// have performed that collapse *and thrown away the reason* — leaving a
    /// caller who needs to know **why** with nowhere to look but the raw log.
    /// The store's own `grade_at` says the collapse should be "something a
    /// caller *does*, never something they receive by default"; returning only
    /// the result of it would invert exactly that.
    ///
    /// `None` is *"unlabelled"*, never *"assume the default"*.
    #[must_use]
    pub fn axes(&self) -> Option<TrustAxes> {
        self.axes
    }

    /// The collapsed trust grade, or `None` when the claim carries no axes.
    ///
    /// A convenience over [`Self::axes`] and [`Self::validity`], not a
    /// replacement for them: every answer carries the axes it was collapsed
    /// from, so taking the grade never costs the reason behind it.
    ///
    /// `None` means *"this claim is unlabelled"*, never *"assume the default"*.
    /// An unlabelled claim has no trust to grade, and manufacturing one is the
    /// failure separate axes exist to prevent.
    #[must_use]
    pub fn grade(&self) -> Option<TrustGrade> {
        self.grade
    }

    /// The provenance handle: the claim's chain digest.
    ///
    /// What makes the answer *citable* rather than merely believed — it locates
    /// the claim in the tamper-evident log, so a reader can verify it instead of
    /// trusting the API that served it.
    #[must_use]
    pub fn provenance(&self) -> &EvidenceDigest {
        &self.provenance
    }

    /// Identity of the event this answer came from.
    #[must_use]
    pub fn event_id(&self) -> &str {
        &self.event_id
    }
}

/// The outcome of asking the world about a subject.
#[derive(Debug, Clone)]
pub enum WorldLookup {
    /// One or more admissible answers, in the store's key order.
    Answered(Vec<WorldAnswer>),
    /// Nothing usable, and why.
    Unknown(UnknownReason),
}

/// A read-only view onto the world.
///
/// Read-only is structural, not a convention: this type holds a `&WorldStore`
/// and offers no mutation. Kirra World is evidence; an answer boundary serves it
/// and never adjudicates it.
pub struct WorldView<'a> {
    store: &'a WorldStore,
    staleness_budget_ms: Option<u64>,
}

impl<'a> WorldView<'a> {
    /// Bind a view with the **caller's** staleness policy.
    ///
    /// The budget is the caller's deliberately: how long a claim stays fresh is
    /// a policy of the asking consumer, and a floor plan and a person's location
    /// go stale at wildly different rates. Baking one budget into the row would
    /// answer for every reader at once. `None` means this caller treats claims
    /// as not going stale.
    #[must_use]
    pub fn new(store: &'a WorldStore, staleness_budget_ms: Option<u64>) -> Self {
        Self {
            store,
            staleness_budget_ms,
        }
    }

    /// Ask what is currently known about one subject.
    ///
    /// Bounded by construction — one subject, never the whole projection.
    ///
    /// # Errors
    ///
    /// Only on a storage fault. An empty or wholly-inadmissible result is
    /// [`WorldLookup::Unknown`], which is a **success**.
    pub fn ask(&self, subject: &str, now_ms: i64) -> Result<WorldLookup, AskError> {
        let claims = self.store.current(subject, now_ms)?;
        if claims.is_empty() {
            return Ok(WorldLookup::Unknown(UnknownReason::NoClaim));
        }

        let clock = now_ms.max(0).unsigned_abs();
        let mut answers: Vec<WorldAnswer> = Vec::new();
        for c in claims
            .iter()
            .filter(|c| Self::is_admissible(c, clock, self.staleness_budget_ms))
        {
            answers.push(self.bind(c, clock)?);
        }

        if answers.is_empty() {
            return Ok(WorldLookup::Unknown(UnknownReason::NoneAdmissible));
        }
        Ok(WorldLookup::Answered(answers))
    }

    /// Expired, or graded `Inadmissible`, is not servable.
    ///
    /// An **unlabelled** claim is admitted: it has no axes to grade, and
    /// refusing it would invent a trust judgement from the absence of one — the
    /// mirror of manufacturing a default grade.
    fn is_admissible(claim: &ProjectedClaim, clock: u64, budget: Option<u64>) -> bool {
        if claim.validity_at(clock, budget) == Validity::Expired {
            return false;
        }
        !matches!(
            claim.grade_at(clock, budget),
            Some(TrustGrade::Inadmissible)
        )
    }

    /// The one place a `WorldAnswer` is built — every field populated together.
    ///
    /// Fallible only on the provenance handle, which is the one field that
    /// carries an invariant the row cannot enforce.
    fn bind(&self, claim: &ProjectedClaim, clock: u64) -> Result<WorldAnswer, AskError> {
        let provenance = EvidenceDigest::new(claim.chain_digest.clone()).map_err(|cause| {
            AskError::CorruptProvenance {
                subject: claim.subject.clone(),
                cause,
            }
        })?;
        Ok(WorldAnswer {
            subject: claim.subject.clone(),
            predicate: claim.predicate.clone(),
            value: claim.payload.clone(),
            validity: claim.validity_at(clock, self.staleness_budget_ms),
            axes: claim.trust,
            grade: claim.grade_at(clock, self.staleness_budget_ms),
            provenance,
            event_id: claim.event_id.clone(),
        })
    }
}
