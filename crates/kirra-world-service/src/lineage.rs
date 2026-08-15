//! **Lineage retrieval — Tier 3 box 3f, the boundary half.**
//!
//! `KIRRA-WM-EXPLAIN-TIER-001`:
//!
//! > **`Explain` stays at Tier 4. Tier 3 builds only the deterministic lineage
//! > CONTRACT that Tier 4 consumes.**
//!
//! with two constraints the ruling attaches to it regardless of where the
//! rendering ends up living:
//!
//! > * **Bounded and paginated, with truncation visible.** […] A lineage
//! >   response that silently stops is worse than one that says it stopped.
//! > * **Historically correct.** Lineage for an answer true at *T* traverses the
//! >   evidence visible at *T*, not today's graph.
//!
//! The selection itself — which events, in what order, where the page ends — is
//! [`kirra_world_store::lineage::select_lineage`], where it is versioned and
//! corpus-pinned. This module is the *query family*: a reproducible reference,
//! its dependency set, and the provenance handles that make each entry citable.
//!
//! # A reference of its own, rather than a variant of [`AnswerRef`]
//!
//! `KIRRA-WM-ANSWER-IDENTITY-001` says a reference serializes *"query kind,
//! query parameters, `as_known_at`, the requested valid instant, the
//! projection/rule version set, the snapshot coordinate, **and the pagination
//! bound**"*. [`AnswerRef`] carries no pagination bound, because the family it
//! describes has no pages; this one carries no `now_ms` and no staleness budget,
//! because lineage is evidence and evidence does not go stale — it is either in
//! the log at that coordinate or it is not.
//!
//! One struct holding the union would let a caller build a lineage reference
//! carrying a staleness budget, which hashes into the reference's identity while
//! meaning nothing — so two references describing the *same* query would compare
//! unequal on a field neither query reads. That would falsify the one property
//! [`AnswerRef`] exists to have. The two types share what is genuinely shared:
//! [`QueryKind`], [`SemanticVersions`], the refusal ordering, and the
//! irreproducibility horizon.
//!
//! [`AnswerRef`]: crate::answer_ref::AnswerRef

use kirra_world::evidence::{DigestError, EvidenceDigest};
use kirra_world_store::compaction::Resolution;
use kirra_world_store::lineage::{LineageEvent, LineagePage};

use crate::cursor::{resolve_cursor, Continuation, CursorFamily, PageCursor};
use crate::read_view::to_continuation;
use kirra_world_store::snapshot::{Irreproducible, PinnedLineage};
use kirra_world_store::{ClaimStatus, WorldStore, WriterClass};

use crate::answer_ref::QueryKind;
use crate::read_view::AskError;
use crate::semantics::{SemanticVersions, VersionDifference};

/// **The semantics a lineage answer is produced under, right now.**
///
/// Exactly ONE rule, and the three exclusions are the interesting part — each
/// is a claim about what a lineage answer is derived from, and each is asserted
/// in `the_lineage_query_depends_on_exactly_one_rule` rather than merely
/// written down here.
///
/// | Rule | In? | Why |
/// |---|---|---|
/// | `lineage_selection` | yes | it decides which events, in what order, and where the page ends |
/// | `world_current_fold` | **no** | lineage returns evidence, not folded claims. Nothing here asks which claim won a key |
/// | `entity_fold` | **no** | the subject is matched as written; lineage follows no identity edges |
/// | `answer_admissibility` | **no** | an inadmissible claim is still evidence — refusing to *serve* it is a different question from whether it *happened*, and hiding it would defeat the purpose |
///
/// The `answer_admissibility` exclusion is the one worth pausing on, because
/// including it would look conservative and would be wrong. Lineage exists to
/// answer *"why does this answer say what it says"*; an event that was rejected,
/// or that expired, or that an LLM proposed and nobody confirmed, is frequently
/// the whole explanation. A lineage that showed only servable claims would be
/// silent in exactly the cases somebody is investigating.
///
/// Each exclusion is also a **tripwire**. If lineage ever follows identity
/// edges, `entity_fold` starts being able to change what a lineage answer says
/// and must join this set — and the assertion that it is absent will go red
/// first, which is how `entity_fold` entered `CurrentSubject`'s set in box 3h.
#[must_use]
pub fn lineage_semantics() -> SemanticVersions {
    SemanticVersions::for_query(QueryKind::SubjectLineage)
}

/// **A reproducible descriptor for one page of one subject's lineage.**
///
/// Structural equality and hashing over every field, including the page bound —
/// so *"the same query at the same coordinate for the same page"* is one
/// reference, and a different page is a different reference. A reference whose
/// identity ignored the page would name a whole lineage while resolving to a
/// slice of it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LineageRef {
    subject: String,
    generation: i64,
    limit: usize,
    /// Where to continue from — an OPAQUE cursor, never a log position.
    ///
    /// The reference used to carry a `LineagePage`, whose `after_generation` was
    /// the raw SQLite coordinate. That made a recorded reference a value a
    /// holder could do arithmetic on, and made a reference from a different
    /// family or a superseded rule set indistinguishable from a valid one.
    after: Option<PageCursor>,
    semantics: SemanticVersions,
}

impl LineageRef {
    /// Describe a lineage page at a pinned generation.
    ///
    /// The versions are stamped from the live declarations rather than taken
    /// from the caller, for the reason [`AnswerRef`] states: a reference records
    /// the semantics its answer was produced under, and a caller that could name
    /// them could claim any.
    ///
    /// [`AnswerRef`]: crate::answer_ref::AnswerRef
    #[must_use]
    pub fn subject_lineage(subject: impl Into<String>, generation: i64, limit: usize) -> Self {
        Self {
            subject: subject.into(),
            generation,
            limit,
            after: None,
            semantics: SemanticVersions::for_query(QueryKind::SubjectLineage),
        }
    }

    /// The query this describes. Always [`QueryKind::SubjectLineage`].
    #[must_use]
    pub fn kind(&self) -> QueryKind {
        QueryKind::SubjectLineage
    }

    /// The subject whose evidence this names.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// The coordinate the lineage is pinned at.
    #[must_use]
    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// How many entries this reference's page may hold.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Where this reference continues from, if it is not the first page.
    #[must_use]
    pub fn after(&self) -> Option<&PageCursor> {
        self.after.as_ref()
    }

    /// The semantics this reference's answer was produced under.
    #[must_use]
    pub fn semantics(&self) -> &SemanticVersions {
        &self.semantics
    }

    /// Rebuild a reference recorded under a different semantic version set.
    ///
    /// For tests and for a reader decoding a persisted reference — the same
    /// deliberately-explicit escape hatch [`AnswerRef::recorded_under`] provides.
    ///
    /// [`AnswerRef::recorded_under`]: crate::answer_ref::AnswerRef::recorded_under
    #[must_use]
    pub fn recorded_under(mut self, semantics: SemanticVersions) -> Self {
        self.semantics = semantics;
        self
    }

    /// **Rebuild a continuing reference from a cursor held across a restart.**
    ///
    /// [`Self::next_page`] covers the in-flight case, where the previous
    /// resolution is still to hand. This covers the other one: a caller that
    /// persisted a cursor, came back later, and wants the page after it.
    ///
    /// Accepting a cursor is not a hole in the opacity. The cursor still cannot
    /// be constructed or edited by a caller, and it is still validated on
    /// presentation — family, semantics and reproducibility — so a reference
    /// built here from the wrong cursor refuses at resolution rather than
    /// serving someone else's page.
    #[must_use]
    pub fn continuing_from(mut self, cursor: PageCursor) -> Self {
        self.after = Some(cursor);
        self
    }

    /// **The reference for the page that follows this one**, if any follows.
    ///
    /// Minted from a *resolution* rather than from a page number, and that is
    /// the point: the cursor is the last generation actually returned, which
    /// only the resolution knows. A caller who computed the next page by adding
    /// the limit to an offset would skip or repeat at every boundary where the
    /// selection rule and the arithmetic disagreed — and they disagree whenever
    /// generations are not contiguous, which is always.
    ///
    /// Returns `None` when the page is [`PageBoundary::Complete`]: there is no
    /// next reference, so none can be constructed. A caller cannot accidentally
    /// paginate past the end.
    #[must_use]
    pub fn next_page(&self, resolved: &LineagePageAnswer) -> Option<Self> {
        let cursor = resolved.continuation().cursor()?.clone();
        Some(Self {
            subject: self.subject.clone(),
            generation: self.generation,
            limit: self.limit,
            after: Some(cursor),
            semantics: SemanticVersions::for_query(QueryKind::SubjectLineage),
        })
    }

    /// **Re-execute this exact lineage query against the same coordinate.**
    ///
    /// # Order of refusals
    ///
    /// The version check runs FIRST, before the store is touched — the ordering
    /// [`AnswerRef::resolve`] establishes and for the same reason: selecting
    /// under new rules and noticing afterwards is not a check, because the page
    /// would already have been built under rules the reference never described.
    ///
    /// [`AnswerRef::resolve`]: crate::answer_ref::AnswerRef::resolve
    ///
    /// # Errors
    ///
    /// On a storage fault, on a negative generation
    /// (`StoreError::InvalidGeneration`), or when an entry's stored chain digest
    /// is not a digest — see [`AskError::CorruptProvenance`], which this family
    /// raises for the same reason the answer family does: an entry that cannot
    /// be cited is not lineage.
    pub(crate) fn resolve(&self, store: &WorldStore) -> Result<LineageResolution, AskError> {
        let differences = self
            .semantics
            .differences(&SemanticVersions::for_query(QueryKind::SubjectLineage));
        if !differences.is_empty() {
            return Ok(LineageResolution::VersionMismatch { differences });
        }

        // The cursor is validated BEFORE the store page is built, and after the
        // version check above — a reference that cannot be continued says so
        // without a read, and says WHY in the caller's own terms.
        let after = match &self.after {
            None => None,
            Some(cursor) => Some(resolve_cursor(store, cursor, CursorFamily::Lineage)?),
        };
        let page = LineagePage::new(self.limit, after)?;

        let (selection, completeness) =
            match store.lineage_at_generation(&self.subject, self.generation, page)? {
                PinnedLineage::Reproduced {
                    selection,
                    completeness,
                } => (selection, completeness),
                PinnedLineage::Irreproducible(reason) => {
                    return Ok(LineageResolution::Irreproducible(reason))
                }
            };

        let mut entries = Vec::with_capacity(selection.events.len());
        for event in selection.events {
            entries.push(LineageEntry::bind(event)?);
        }
        Ok(LineageResolution::Resolved(LineagePageAnswer {
            entries,
            continuation: to_continuation(&selection.boundary, CursorFamily::Lineage),
            completeness,
            semantics: SemanticVersions::for_query(QueryKind::SubjectLineage),
        }))
    }
}

/// **One citable piece of evidence.**
///
/// The store's [`LineageEvent`] with its chain digest turned into a real
/// [`EvidenceDigest`]. That conversion is the whole difference between the two
/// types, and it is not cosmetic: rule 1 says every answer carries a provenance
/// handle, and an entry whose handle will not parse cannot be verified against
/// the log by whoever reads it.
#[derive(Debug, Clone)]
pub struct LineageEntry {
    event: LineageEvent,
    provenance: EvidenceDigest,
}

impl LineageEntry {
    /// Bind an event, refusing an unreadable handle.
    fn bind(event: LineageEvent) -> Result<Self, AskError> {
        let provenance =
            EvidenceDigest::new(event.chain_digest.clone()).map_err(|cause: DigestError| {
                AskError::CorruptProvenance {
                    subject: event.subject.clone(),
                    predicate: event.predicate.clone(),
                    cause,
                }
            })?;
        Ok(Self { event, provenance })
    }

    /// The log position — also this entry's page cursor.
    #[must_use]
    pub fn generation(&self) -> i64 {
        self.event.generation
    }

    /// The event's identity.
    #[must_use]
    pub fn event_id(&self) -> &str {
        &self.event.event_id
    }

    /// The observation this event was written from.
    #[must_use]
    pub fn observation_id(&self) -> &str {
        &self.event.observation_id
    }

    /// When the store learned it.
    #[must_use]
    pub fn txn_time_ms(&self) -> i64 {
        self.event.txn_time_ms
    }

    /// When the fact it asserts became true.
    #[must_use]
    pub fn valid_from_ms(&self) -> i64 {
        self.event.valid_from_ms
    }

    /// When that fact stopped being true, if it has.
    #[must_use]
    pub fn valid_to_ms(&self) -> Option<i64> {
        self.event.valid_to_ms
    }

    /// Who wrote it.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.event.source
    }

    /// Which version of that writer.
    #[must_use]
    pub fn source_version(&self) -> &str {
        &self.event.source_version
    }

    /// The writer's class.
    #[must_use]
    pub fn writer_class(&self) -> WriterClass {
        self.event.writer_class
    }

    /// Whether this was a confirmed fact or an unconfirmed proposal.
    #[must_use]
    pub fn claim_status(&self) -> ClaimStatus {
        self.event.claim_status
    }

    /// The recorded provenance array, **verbatim and uninterpreted**.
    ///
    /// Passed through as stored rather than parsed. `WM_SCOPE.md` §7 records
    /// `Explain` as depending on *"derivation edges being real structure rather
    /// than a JSON array of identifiers"*, and this is that array — walking it
    /// here would be Tier 3 inventing the structure whose absence is the reason
    /// `Explain` is Tier 4.
    #[must_use]
    pub fn provenance_ids_json(&self) -> &str {
        &self.event.provenance
    }

    /// The claim kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.event.kind
    }

    /// The subject asserted about.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.event.subject
    }

    /// The predicate, or `None` for a payload-only claim.
    #[must_use]
    pub fn predicate(&self) -> Option<&str> {
        self.event.predicate.as_deref()
    }

    /// The object, or `None` for a claim carrying no relationship.
    #[must_use]
    pub fn object(&self) -> Option<&str> {
        self.event.object.as_deref()
    }

    /// The provenance handle: this event's position in the tamper-evident chain.
    #[must_use]
    pub fn provenance(&self) -> &EvidenceDigest {
        &self.provenance
    }
}

/// **One page of lineage, with everything needed to judge it.**
///
/// Three things ride alongside the entries, and none is optional:
/// the page boundary (is this all of it), the compaction verdict (was any of it
/// deleted), and the semantics (under which rule was it selected). An answer
/// missing any one of them can be misread as complete when it is not.
#[derive(Debug, Clone)]
pub struct LineagePageAnswer {
    entries: Vec<LineageEntry>,
    continuation: Continuation,
    completeness: Resolution,
    semantics: SemanticVersions,
}

impl LineagePageAnswer {
    /// The evidence, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[LineageEntry] {
        &self.entries
    }

    /// **Whether this page is the whole lineage, and where to continue.**
    ///
    /// The ruling's *"truncation visible"*, as a value that must be looked at to
    /// learn where the next page starts — so a caller that paginates cannot do
    /// it without seeing that the last page said `Complete`.
    #[must_use]
    pub fn continuation(&self) -> &Continuation {
        &self.continuation
    }

    /// Whether this page stopped short of the whole lineage.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.continuation.is_truncated()
    }

    /// **Whether compaction removed evidence at or below this coordinate.**
    ///
    /// Box 3g's obligation, which lineage inherits by being an answer family:
    /// completeness is carried *independently of the payload outcome*. An empty
    /// page that is `Degraded` says *"the evidence was deleted"*, which is a
    /// different fact from *"there was none"* — and on a lineage query, which
    /// exists to reconstruct what happened, it is the more important one.
    ///
    /// Truncation and degradation are **separate** and both can be true: a page
    /// can be cut short by its own limit *and* be missing a compacted span. One
    /// is a bound the caller chose; the other is evidence that no longer exists.
    #[must_use]
    pub fn completeness(&self) -> &Resolution {
        &self.completeness
    }

    /// Whether compaction bore on this answer.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.completeness.is_degraded()
    }

    /// The rules this page was selected under.
    #[must_use]
    pub fn semantics(&self) -> &SemanticVersions {
        &self.semantics
    }
}

/// What resolving a lineage reference produced.
///
/// The same three outcomes as [`RefResolution`], deliberately: a lineage
/// reference is subject to the same reproducibility horizon and the same
/// version discipline, and giving it a different vocabulary would suggest
/// otherwise.
///
/// [`RefResolution`]: crate::answer_ref::RefResolution
#[derive(Debug, Clone)]
pub enum LineageResolution {
    /// Re-executed at the pinned coordinate.
    Resolved(LineagePageAnswer),
    /// The coordinate has not been reached — see
    /// `KIRRA-WM-REPRODUCIBILITY-HORIZON-001`.
    ///
    /// Note what is **not** here: a compacted coordinate. Compaction degrades a
    /// lineage answer rather than refusing it, because the removed spans are
    /// themselves reportable — see
    /// [`ReadSnapshot::lineage_at_generation`][l] for why this family splits
    /// from the pinned projection read on exactly this point.
    ///
    /// [l]: kirra_world_store::snapshot::ReadSnapshot::lineage_at_generation
    Irreproducible(Irreproducible),
    /// The selection rule changed since the reference was recorded.
    ///
    /// Refused rather than replayed. A lineage reference is the case where
    /// replaying would be most quietly wrong: the cursor in a recorded page-2
    /// reference was minted by the *old* ordering, so re-running it under a new
    /// one returns a set that is neither the old page 2 nor the new one, and
    /// looks entirely ordinary.
    VersionMismatch {
        /// Every rule whose version differs, recorded versus current.
        differences: Vec<VersionDifference>,
    },
}

impl LineageResolution {
    /// The page, if it resolved.
    #[must_use]
    pub fn resolved(&self) -> Option<&LineagePageAnswer> {
        match self {
            Self::Resolved(p) => Some(p),
            _ => None,
        }
    }

    /// Whether this is any kind of refusal.
    #[must_use]
    pub fn is_refused(&self) -> bool {
        !matches!(self, Self::Resolved(_))
    }
}
