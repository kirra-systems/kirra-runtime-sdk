//! **Tier 3 box 3c — one coherent point across several projections.**
//!
//! An answer that composes projections must read them at ONE coherent point, or
//! report each coordinate explicitly. This module provides the first arm, and
//! carries the second alongside it.
//!
//! # Why this exists as a type rather than as discipline
//!
//! Identity, claims and subject summaries are three independently-folded
//! projections. Each advances its own checkpoint row when its own fold commits,
//! and nothing coordinates them. A composed read that calls
//! [`WorldStore::current`] and then [`WorldStore::identity_view`] can therefore
//! observe a fold landing between the two calls and answer from two different
//! states of the world — the exact failure [`IdentityView`]'s own docs rule out
//! *within* a walk, reappearing *between* walks.
//!
//! [`IdentityView`]: crate::entity_projection::IdentityView
//!
//! # The mechanism, and why it is a guarantee rather than a detector
//!
//! Every projection is a table in ONE SQLite database, and every fold commits
//! atomically. So a single read transaction — which in WAL mode holds one
//! snapshot of the whole database for its lifetime — sees each projection at a
//! committed fold boundary, and sees *the same set of commits* for all of them.
//! That is coherence by construction, not drift detection after the fact.
//!
//! This matters for what the box is allowed to claim. Detecting drift and
//! refusing would also satisfy 3c's fallback arm, but it is strictly weaker: it
//! turns a concurrent fold into a refused answer, where a snapshot turns it into
//! a correct one.
//!
//! # What this is NOT
//!
//! It is **not** the generation-pinned read that `KIRRA-WM-ANSWER-IDENTITY-001`
//! needs, and [`SnapshotCoordinate`] is **not** an `AnswerRef`. A snapshot is
//! coherent for as long as it is held and cannot be re-entered once dropped;
//! re-executing a query against a *recorded* coordinate needs a way to read
//! `world_current` as of a generation, which does not exist. That gap has its
//! own open box in `docs/design/WM_SCOPE.md`, and
//! `KIRRA-WM-ANSWERREF-NAMING-001` reserves the name `AnswerRef` for the day it
//! closes. Naming this type `AnswerRef` would put the ruled guarantee's name on
//! a mechanism that cannot honour it.

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension};

use crate::compaction::Citation;
use crate::entity_projection::{self, IdentityView, ProjectedEntity};
use crate::projection::{self, ProjectedClaim};
use crate::subject_projection;
use crate::{claim_from_row, StoreError, CLAIM_COLUMNS};

/// Where ONE projection stood when a snapshot observed it.
///
/// Carries the `state_digest` as well as the generation because a generation
/// alone cannot distinguish a fold that advanced from a rebuild that landed on
/// the same head — the pair is what the fold itself commits, so it is what an
/// observer should record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionCoordinate {
    name: &'static str,
    generation: i64,
    state_digest: String,
}

impl ProjectionCoordinate {
    /// The projection's checkpoint name, as the fold writes it.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// How far this projection's fold has consumed.
    ///
    /// **Not comparable to another projection's generation.** See
    /// [`SnapshotCoordinate`] for why that comparison is meaningless rather than
    /// merely unreliable.
    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// The digest of the state the fold left, or `""` if never folded.
    pub fn state_digest(&self) -> &str {
        &self.state_digest
    }

    /// Whether this projection has ever been folded.
    ///
    /// A projection whose table has not been installed reports generation 0,
    /// matching [`WorldStore::projection_generation`]'s existing convention —
    /// so "absent" and "folded nothing" are deliberately the same observation.
    /// They have the same consequence for a reader: there is nothing there.
    pub fn is_unfolded(&self) -> bool {
        self.generation == 0
    }
}

/// Where EVERY projection stood, observed at one coherent point.
///
/// # These numbers are not comparable to each other
///
/// The obvious check — assert two projections sit at the same generation — is
/// wrong, and wrong in the direction that looks like rigour. `world_current`
/// and `subject_summary` advance their checkpoint past every event *considered*,
/// including events they adopt nothing from; the entity fold advances only to
/// the generation of the last *adjudication* it folded. So appending any
/// non-adjudication event leaves the entity checkpoint legitimately behind the
/// other two, with all three fully folded and nothing wrong.
///
/// An equality check would therefore report constant false drift, and the fix
/// someone reaches for under deadline is to delete the check. Coherence here
/// comes from the snapshot, not from comparing these numbers; they are recorded
/// so an answer can say *which* state it came from, which is 3c's second arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCoordinate {
    world_current: ProjectionCoordinate,
    entities: ProjectionCoordinate,
    subject_summary: ProjectionCoordinate,
}

impl SnapshotCoordinate {
    /// The claims projection's coordinate.
    pub fn world_current(&self) -> &ProjectionCoordinate {
        &self.world_current
    }

    /// The identity projection's coordinate.
    pub fn entities(&self) -> &ProjectionCoordinate {
        &self.entities
    }

    /// The subject-summary projection's coordinate.
    pub fn subject_summary(&self) -> &ProjectionCoordinate {
        &self.subject_summary
    }

    /// Every coordinate, in a stable order.
    ///
    /// All three are recorded even when a read touched one, deliberately: a
    /// coordinate that listed only the projections a particular read happened to
    /// consult would be a record that changes shape with the query, and a reader
    /// comparing two of them could not tell "this projection was not read" from
    /// "this projection did not exist".
    pub fn all(&self) -> [&ProjectionCoordinate; 3] {
        [&self.world_current, &self.entities, &self.subject_summary]
    }
}

/// **A coherent multi-projection read.**
///
/// Holds one SQLite read transaction for its lifetime, so every read taken
/// through it observes one state of the database — and therefore one state of
/// every projection in it.
///
/// # The snapshot begins at the first read, not at construction
///
/// The transaction is DEFERRED, so SQLite establishes the snapshot when the
/// first statement runs. This does not weaken coherence — every read still
/// agrees with every other — but it does mean a snapshot held open and unused
/// is not "as of" the moment it was created. Stated because the alternative
/// reading is the one that produces a stale-data bug report years later.
///
/// # Holding one open is not free
///
/// A read transaction holds a WAL read-mark, which prevents checkpointing from
/// reclaiming WAL frames beyond it. Take one for a composed read and drop it;
/// do not park one across a request loop.
pub struct ReadSnapshot<'a> {
    tx: rusqlite::Transaction<'a>,
}

impl<'a> ReadSnapshot<'a> {
    pub(crate) fn new(tx: rusqlite::Transaction<'a>) -> Self {
        Self { tx }
    }

    /// The current claims for `subject`, holding at `now_ms`.
    ///
    /// Identical in result to [`WorldStore::current`] — the same code answers
    /// both — except that it is bound to this snapshot.
    ///
    /// [`WorldStore::current`]: crate::WorldStore::current
    pub fn current(&self, subject: &str, now_ms: i64) -> Result<Vec<ProjectedClaim>, StoreError> {
        current_on(&self.tx, subject, now_ms)
    }

    /// The identity graph, loaded from this snapshot.
    ///
    /// Unlike [`WorldStore::identity_view`] before 3c, the rows and the
    /// generation label are read from ONE state: the view cannot be stamped
    /// with a generation newer than the rows it holds.
    ///
    /// [`WorldStore::identity_view`]: crate::WorldStore::identity_view
    pub fn identity_view(&self) -> Result<IdentityView, StoreError> {
        Ok(IdentityView::new(
            load_entity_projection_on(&self.tx)?,
            checkpoint_on(&self.tx, entity_projection::ENTITY_PROJECTION)?.0,
        ))
    }

    /// Where every projection stood in this snapshot.
    pub fn coordinate(&self) -> Result<SnapshotCoordinate, StoreError> {
        coordinate_on(&self.tx)
    }

    /// **Reconstruct `world_current` as it stood at projection generation
    /// `generation`** — the generation-pinned read.
    ///
    /// `KIRRA-WM-ANSWER-IDENTITY-001` rules that resolving an `AnswerRef` means
    /// *"re-execute this exact deterministic query against the same snapshot"*.
    /// Until this existed the ruling had no mechanism behind it:
    /// `projection_generation()` could report the coordinate, and nothing could
    /// read AT it.
    ///
    /// # It fails closed, and never falls forward
    ///
    /// Two things make a generation unreconstructible, and both refuse:
    /// [`Irreproducible::NotYetReached`] and [`Irreproducible::Compacted`].
    /// Neither returns current state. That is the whole point — a caller asking
    /// what was true at generation 40, handed what is true at generation 90
    /// because 40 was compacted, has been answered a different question with no
    /// way to tell.
    ///
    /// # Generation, not transaction time
    ///
    /// The store already cuts on transaction time ([`WorldStore::as_of`]), and
    /// that axis cannot be made exact after compaction: the removed rows are the
    /// only record of their own `txn_time_ms`, so a span can never be shown
    /// irrelevant to a past instant. `identity_degradation`'s comment works
    /// through why the obvious filter there was fail-open.
    ///
    /// Generation does not have that problem. A [`Citation`] records the exact
    /// `lo_generation..=hi_generation` it removed, which is the same axis being
    /// pinned, so "did compaction take anything at or below `generation`" is an
    /// EXACT test rather than a necessary condition. This is the one place the
    /// two axes genuinely differ, and it is why the pin is on this one.
    ///
    /// [`WorldStore::as_of`]: crate::WorldStore::as_of
    ///
    /// # Errors
    ///
    /// [`StoreError::InvalidGeneration`] for a negative generation — a malformed
    /// query, which rule 3 puts in the error channel. Generation `0` is legal
    /// and reconstructs the empty projection that preceded every event.
    pub fn read_at_generation(&self, generation: i64) -> Result<PinnedRead, StoreError> {
        if let Some(reason) = self.coordinate_reached(generation)? {
            return Ok(PinnedRead::Irreproducible(reason));
        }

        // Compaction check BEFORE the replay, deliberately. Replaying first and
        // checking after would produce a plausible projection built from a log
        // with holes in it, and the temptation to return it "since we have it"
        // is exactly what fails closed here.
        let spans = compacted_at_or_below(&self.tx, generation)?;
        if !spans.is_empty() {
            return Ok(PinnedRead::Irreproducible(Irreproducible::Compacted {
                spans,
            }));
        }

        Ok(PinnedRead::Reproduced(PinnedProjection {
            generation,
            rows: replay_to(&self.tx, generation)?,
        }))
    }

    /// **Does this coordinate exist yet?** — the half of reproducibility that
    /// every pinned read answers the same way.
    ///
    /// Shared rather than repeated. The compaction half is deliberately NOT
    /// here, because the two pinned families answer it differently and one
    /// helper returning both would hide that: a projection replay must REFUSE a
    /// compacted coordinate (a fold over a log with holes is silently wrong),
    /// while a lineage read DEGRADES (the removed spans are themselves a fact,
    /// and `Resolution::Degraded` is the type for it). Merging them would force
    /// one family to take the other's answer.
    ///
    /// # Errors
    ///
    /// [`StoreError::InvalidGeneration`] for a negative generation — a malformed
    /// query rather than an unreachable one, so it goes in the error channel.
    fn coordinate_reached(&self, generation: i64) -> Result<Option<Irreproducible>, StoreError> {
        if generation < 0 {
            return Err(StoreError::InvalidGeneration {
                requested: generation,
            });
        }
        let head = checkpoint_on(&self.tx, projection::CURRENT_PROJECTION)?.0;
        Ok((generation > head).then_some(Irreproducible::NotYetReached { head }))
    }

    /// **One page of a subject's lineage, as it stood at `generation`** —
    /// box 3f.
    ///
    /// `KIRRA-WM-EXPLAIN-TIER-001` asks Tier 3 for a lineage contract that is
    /// *bounded and paginated, with truncation visible* and *historically
    /// correct*. The selection rule — which events, in what order, where the
    /// page ends — is [`crate::lineage::select_lineage`], versioned and
    /// corpus-pinned; this supplies it with candidates and the compaction
    /// verdict.
    ///
    /// # Compaction DEGRADES here rather than refusing
    ///
    /// [`Self::read_at_generation`] refuses a compacted coordinate, and must:
    /// a projection folded from a log with holes in it is silently wrong, and
    /// looks exactly like a correct one. Lineage is not folded — it is the
    /// evidence itself — so a page missing a compacted span is *incomplete*
    /// rather than *wrong*, and the citations say exactly which generations were
    /// removed and under which digest. Refusing would discard a usable answer;
    /// `Resolution::Degraded` is 3g's type for precisely this.
    ///
    /// The split mirrors one this store already makes: `read_composed_at_generation`
    /// refuses while `as_of_composed` degrades.
    ///
    /// **No summaries ride along, and that is not an omission.** A
    /// [`crate::DegradedSummary`] summarises folded *claims* for a key; it is
    /// not a stand-in for removed evidence *rows*, so offering one here would
    /// answer a question lineage did not ask. The spans are what name the loss.
    ///
    /// # Errors
    ///
    /// As [`Self::coordinate_reached`].
    pub fn lineage_at_generation(
        &self,
        subject: &str,
        generation: i64,
        page: crate::lineage::LineagePage,
    ) -> Result<PinnedLineage, StoreError> {
        if let Some(reason) = self.coordinate_reached(generation)? {
            return Ok(PinnedLineage::Irreproducible(reason));
        }

        let spans = compacted_at_or_below(&self.tx, generation)?;
        let completeness = if spans.is_empty() {
            crate::Resolution::Full
        } else {
            crate::Resolution::Degraded {
                spans,
                summaries: Vec::new(),
            }
        };

        Ok(PinnedLineage::Reproduced {
            // The fetch is narrowed to the most the rule can consume (see
            // `lineage_candidates`); the rule still decides everything. The two
            // are held in agreement by an explicit test rather than by reading.
            selection: crate::lineage::select_lineage(
                lineage_candidates(&self.tx, subject, generation, page)?,
                subject,
                generation,
                page,
            ),
            completeness,
        })
    }

    /// **The identity graph reachable from `seeds`, and nothing else** — box 3d.
    ///
    /// The bounded counterpart to [`Self::identity_view`], which loads every
    /// entity in the store. An answer only ever resolves the objects its own
    /// claims name, so loading the whole graph to resolve a handful of ids was
    /// `O(entities)` work for an `O(predicates)` question.
    ///
    /// Snapshot-scoped deliberately: it reads through this snapshot's
    /// transaction, so the identity half of a composed answer is observed at the
    /// SAME point as the claims half. A `WorldStore`-level bounded resolve would
    /// be a second connection and would reintroduce exactly the between-walks
    /// incoherence this type exists to close.
    ///
    /// All seeds are loaded into ONE view rather than one view per object, so
    /// every object in an answer resolves against the same graph.
    ///
    /// # Errors
    ///
    /// As [`Self::identity_view`], plus
    /// [`StoreError::IdentityClosureTooLarge`] — refused, never truncated.
    pub fn identity_view_for(
        &self,
        seeds: &[kirra_world::reference::EntityId],
    ) -> Result<IdentityView, StoreError> {
        let rows = load_reachable_entity_projection_on(&self.tx, seeds)?;
        // Generation 0: a REACHABLE SUBSET is not a snapshot of the projection,
        // and stamping it with the projection's head would label a partial graph
        // as a complete state of the world.
        Ok(IdentityView::new(rows, 0))
    }

    /// **Reconstruct claims AND identity at one generation — box 3h.**
    ///
    /// 3h's rule is *"historical queries use historical identity and historical
    /// evidence — never today's entity graph applied to old evidence."* This is
    /// the primitive that makes that possible rather than merely intended.
    ///
    /// # Why this is one call and not two
    ///
    /// A caller who fetched the two halves separately would be one refactor away
    /// from pinning claims at `g` and resolving identity against the live graph
    /// — which is exactly the failure 3h names, and it would look like ordinary
    /// code. Composing here makes the shared coordinate structural: one
    /// generation, one compaction check, one refusal covering both halves.
    ///
    /// The reproducibility rules are **delegated** to
    /// [`Self::read_at_generation`] rather than restated, so that guarantee
    /// rests on there being one implementation rather than on two copies
    /// agreeing.
    ///
    /// # The bound is the LOG's progress, not the entity checkpoint
    ///
    /// The obvious head for the identity half — `entities_projection`'s own
    /// checkpoint — is **wrong**, and wrong in the direction that looks careful.
    /// [`SnapshotCoordinate`] records at length why the two checkpoints are not
    /// comparable: `world_current` advances past every event *considered*, while
    /// the entity fold advances only to the last *adjudication* it folded, so
    /// appending one ordinary claim leaves the entity checkpoint legitimately
    /// behind with both folds complete. Gating the identity half on it would
    /// refuse reproducible generations on a perfectly healthy store — the same
    /// false drift that finding was recorded to prevent.
    ///
    /// Both halves replay from the **log**, so both are bounded by how far the
    /// log has been folded, which is one number.
    ///
    /// # Staleness is not a question here
    ///
    /// [`Self::identity_is_current`] exists because a LIVE read consults a
    /// projection that may lag the log. A pinned read does not: it folds the
    /// adjudications itself, up to `generation`, so the reconstruction is
    /// complete at that coordinate by construction. The gate is unnecessary
    /// rather than skipped.
    ///
    /// # Errors
    ///
    /// As [`Self::read_at_generation`].
    pub fn read_composed_at_generation(
        &self,
        generation: i64,
    ) -> Result<PinnedComposedRead, StoreError> {
        // DELEGATED, not re-derived. Every reproducibility rule — the negative
        // guard, the head bound, the compaction refusal — lives in
        // `read_at_generation` and is reached from here, so "one refusal covers
        // both halves" is true because there is only one implementation of it,
        // not because two copies currently agree.
        //
        // The first draft duplicated the three checks. They were identical, and
        // that is exactly the problem: a later edit to one path would leave the
        // other silently reproducing a generation its sibling refuses, and the
        // half that drifted would be the one nothing tested directly. Caught in
        // review on #1437.
        //
        // Identity replays only on the reproduced path, so a refused
        // composition does no work and cannot half-succeed.
        let projection = match self.read_at_generation(generation)? {
            PinnedRead::Reproduced(p) => p,
            PinnedRead::Irreproducible(reason) => {
                return Ok(PinnedComposedRead::Irreproducible(reason))
            }
        };

        Ok(PinnedComposedRead::Reproduced(PinnedComposition {
            projection,
            identity: replay_identity_to(&self.tx, generation)?,
        }))
    }

    /// **Has the identity graph consumed every adjudication the log holds?**
    ///
    /// The question a composed read must ask before trusting a resolution, and
    /// it is deliberately *not* "has the entity projection been folded".
    ///
    /// Those differ in both directions, which is why the cheap check is the
    /// wrong one:
    ///
    /// - A store that has **never** folded the entity projection because it has
    ///   **no adjudications at all** is perfectly current. There are no merges
    ///   to miss, so an object stands for itself. Treating that as unresolvable
    ///   would refuse every object-bearing claim on the overwhelmingly common
    ///   store — an availability failure bought for no safety.
    /// - A store folded once, with merges recorded **since**, has a projection
    ///   that exists and is *stale*. A folded-or-not check calls that fine and
    ///   resolves against data known to be out of date — the dangerous case,
    ///   waved through.
    ///
    /// Answered from the same snapshot as everything else, so a fold committing
    /// concurrently cannot make this answer disagree with the view it describes.
    pub fn identity_is_current(&self) -> Result<bool, StoreError> {
        if !table_exists(&self.tx, "world_events")? {
            return Ok(true);
        }
        let checkpoint = checkpoint_on(&self.tx, entity_projection::ENTITY_PROJECTION)?.0;
        let pending: Option<i64> = self
            .tx
            .query_row(
                "SELECT 1 FROM world_events
                 WHERE kind = ?1 AND claim_status = 'confirmed' AND generation > ?2
                 LIMIT 1",
                params![crate::adjudication_record::ADJUDICATION_KIND, checkpoint],
                |r| r.get(0),
            )
            .optional()?;
        Ok(pending.is_none())
    }
}

/// **Why a pinned read can stop being possible, and what ends it.**
///
/// Returned instead of a reconstruction, never alongside one — the caller cannot
/// hold a `PinnedProjection` that quietly means "current state".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Irreproducible {
    /// Evidence at or below the requested generation was compacted away.
    ///
    /// **Compaction ends a pinned read's life for every generation at or above
    /// the compacted span, not merely inside it.** Reconstructing at `g` folds
    /// every confirmed event `<= g`; if any of them is gone, the fold cannot be
    /// reproduced, whatever its result would have been.
    ///
    /// That is deliberately stricter than necessary, on the asymmetry
    /// [`Resolution`] already documents: a removed event may well have been
    /// superseded and made no difference to the answer, but the removed rows are
    /// the only record of themselves, so it cannot be *shown* to have made none.
    /// Over-refusing costs availability; under-refusing returns a silently wrong
    /// reconstruction wearing the word "pinned".
    ///
    /// [`Resolution`]: crate::compaction::Resolution
    ///
    /// Worth stating plainly to whoever sets a retention horizon: **the
    /// compaction floor is also the floor on how far back answers stay
    /// reproducible.**
    Compacted {
        /// The compacted spans that bear on the request, lowest first.
        spans: Vec<Citation>,
    },
    /// The requested generation is ahead of everything the store has recorded.
    NotYetReached {
        /// How far the claims projection has actually consumed.
        head: i64,
    },
}

/// `world_current` as it stood at one projection generation.
///
/// Reconstructed by replaying the log, not by reading the live table — the live
/// table holds one point (latest known, latest valid) and every other point has
/// to be replayed. The reconstruction is exact because the fold is deterministic
/// over its input, which is the same property `rebuild_from_zero_equals_incremental`
/// already pins.
#[derive(Debug, Clone)]
pub struct PinnedProjection {
    generation: i64,
    rows: BTreeMap<(String, String), ProjectedClaim>,
}

impl PinnedProjection {
    /// The generation this was reconstructed at.
    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// The claims for `subject` holding at `now_ms`, as of the pinned
    /// generation.
    ///
    /// Same shape and same `holds_at` filter as [`WorldStore::current`], so a
    /// pinned read and a live read differ in WHEN they are answered and in
    /// nothing else.
    ///
    /// [`WorldStore::current`]: crate::WorldStore::current
    pub fn current(&self, subject: &str, now_ms: i64) -> Vec<ProjectedClaim> {
        let mut out: Vec<ProjectedClaim> = self
            .rows
            .iter()
            .filter(|((s, _), _)| s == subject)
            .map(|(_, c)| c.clone())
            .filter(|c| c.holds_at(now_ms))
            .collect();
        // `world_current` returns a subject's rows ordered by `predicate_key`;
        // the BTreeMap is already in that order, so this only re-states it.
        out.sort_by(|a, b| {
            a.predicate
                .as_deref()
                .unwrap_or("")
                .cmp(b.predicate.as_deref().unwrap_or(""))
        });
        out
    }

    /// How many keys the reconstructed projection holds.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the reconstruction is empty.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// **Claims and identity, reconstructed at ONE generation — box 3h.**
///
/// The two halves are private and reachable only together, so there is no way
/// to hold a pinned projection beside a live identity view and call the pair an
/// historical answer. That is the whole point of the type: 3h's failure mode is
/// not exotic, it is the natural result of resolving an object while a live
/// `WorldStore` is in scope.
#[derive(Debug, Clone)]
pub struct PinnedComposition {
    projection: PinnedProjection,
    identity: entity_projection::IdentityView,
}

impl PinnedComposition {
    /// The claims half.
    pub fn claims(&self) -> &PinnedProjection {
        &self.projection
    }

    /// **The identity graph as it stood at the pinned generation.**
    ///
    /// Adjudications recorded *after* that coordinate are absent, which is the
    /// property 3h is about: a merge performed today must not silently rewrite
    /// what an answer from before it meant.
    pub fn identity(&self) -> &entity_projection::IdentityView {
        &self.identity
    }

    /// The coordinate both halves were reconstructed at.
    pub fn generation(&self) -> i64 {
        self.projection.generation()
    }
}

/// The outcome of a pinned lineage read — box 3f.
///
/// Note the asymmetry with [`PinnedComposedRead`], which is deliberate and
/// documented on [`ReadSnapshot::lineage_at_generation`]: only *"that coordinate
/// does not exist yet"* refuses here. Compaction rides on the reproduced arm as
/// a [`crate::Resolution`], because removed evidence is a fact a lineage answer
/// can report rather than a reason it cannot be given.
#[derive(Debug, Clone)]
pub enum PinnedLineage {
    /// The page, and whether compaction bore on it.
    Reproduced {
        /// The selected events and the page boundary.
        selection: crate::lineage::SelectedLineage,
        /// Whether compaction removed evidence at or below this coordinate.
        completeness: crate::Resolution,
    },
    /// The coordinate has not been reached.
    Irreproducible(Irreproducible),
}

/// The outcome of a composed generation-pinned read.
///
/// One refusal for both halves — see [`ReadSnapshot::read_composed_at_generation`].
#[derive(Debug, Clone)]
pub enum PinnedComposedRead {
    /// Both halves reconstructed at the requested generation.
    Reproduced(PinnedComposition),
    /// Neither half can be reconstructed, and why.
    Irreproducible(Irreproducible),
}

/// **Claims and identity at ONE transaction-time cut** — the `as_of` twin.
///
/// The same composition [`PinnedComposition`] provides on the generation axis,
/// for the axis `as_of` actually asks about: *what did this store know at time
/// T*. Both halves come from one snapshot, so a fold landing mid-read cannot
/// pair claims from one commit with an identity graph from another.
///
/// # Why this one has no refusal variant, and the pinned one does
///
/// The asymmetry is real and worth stating, because two composed reads with
/// different failure shapes look like an oversight:
///
/// * A **generation pin** promises exact reconstruction of a recorded
///   coordinate. Evidence removed at or below it makes that impossible, so it
///   [`Irreproducible`]-refuses — anything else would be a reconstruction with
///   holes wearing the word "pinned".
/// * An **`as_of`** promises *what was known then, from what remains*.
///   Compaction does not make that impossible, it makes it **incomplete** — and
///   incompleteness already has a carrier, [`crate::Resolution`], on the answer.
///   Refusing here would throw away an answer the caller can legitimately use
///   while being told exactly what is missing.
///
/// So one refuses and one degrades, and both are honest about which they did.
#[derive(Debug, Clone)]
pub struct TemporalComposition {
    answer: crate::compaction::TemporalAnswer,
    identity: IdentityView,
}

impl TemporalComposition {
    /// Build from halves read in one snapshot.
    ///
    /// `pub(crate)` so the only way a caller obtains one is
    /// [`crate::WorldStore::as_of_composed`], which is what opens that snapshot.
    /// A public constructor would let the two halves be assembled from separate
    /// reads — the pairing this type exists to prevent.
    pub(crate) fn new(answer: crate::compaction::TemporalAnswer, identity: IdentityView) -> Self {
        Self { answer, identity }
    }

    /// The claims half, carrying its own completeness.
    pub fn answer(&self) -> &crate::compaction::TemporalAnswer {
        &self.answer
    }

    /// **The identity graph as it stood at the same `as_known_at` cut.**
    ///
    /// Adjudications recorded after that instant are absent — the transaction-
    /// time form of the property box 3h established on the generation axis.
    pub fn identity(&self) -> &IdentityView {
        &self.identity
    }

    /// Consume into the two halves, for a caller that must own the answer.
    pub fn into_parts(self) -> (crate::compaction::TemporalAnswer, IdentityView) {
        (self.answer, self.identity)
    }
}

/// The outcome of a generation-pinned read.
///
/// A two-variant result rather than `Option` or a fallback, because the one
/// thing this must never do is answer with the CURRENT state when the requested
/// generation cannot be rebuilt. Falling forward is not merely wrong, it is
/// wrong in the way that looks right: the caller asked what was true then and
/// receives what is true now, with nothing in the value to say so.
#[derive(Debug, Clone)]
pub enum PinnedRead {
    /// Reconstructed exactly at the requested generation.
    Reproduced(PinnedProjection),
    /// The generation cannot be reconstructed, and why.
    Irreproducible(Irreproducible),
}

impl PinnedRead {
    /// The reconstruction, or `None` if the generation is irreproducible.
    pub fn reproduced(&self) -> Option<&PinnedProjection> {
        match self {
            Self::Reproduced(p) => Some(p),
            Self::Irreproducible(_) => None,
        }
    }

    /// Why the read could not be reproduced, if it could not.
    pub fn irreproducible(&self) -> Option<&Irreproducible> {
        match self {
            Self::Reproduced(_) => None,
            Self::Irreproducible(r) => Some(r),
        }
    }
}

/// Read one projection's checkpoint row: `(generation, state_digest)`.
///
/// A missing checkpoint table or a missing row is `(0, "")` rather than an
/// error, matching the existing generation readers: a projection that has never
/// folded has consumed nothing, which is a fact and not a fault.
pub(crate) fn checkpoint_on(conn: &Connection, name: &str) -> Result<(i64, String), StoreError> {
    let installed: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='projection_checkpoint'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if installed.is_none() {
        return Ok((0, String::new()));
    }
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT generation, state_digest FROM projection_checkpoint WHERE name = ?1",
            params![name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.unwrap_or((0, String::new())))
}

pub(crate) fn coordinate_on(conn: &Connection) -> Result<SnapshotCoordinate, StoreError> {
    let one = |name: &'static str| -> Result<ProjectionCoordinate, StoreError> {
        let (generation, state_digest) = checkpoint_on(conn, name)?;
        Ok(ProjectionCoordinate {
            name,
            generation,
            state_digest,
        })
    };
    Ok(SnapshotCoordinate {
        world_current: one(projection::CURRENT_PROJECTION)?,
        entities: one(entity_projection::ENTITY_PROJECTION)?,
        subject_summary: one(subject_projection::SUBJECT_SUMMARY_PROJECTION)?,
    })
}

/// The body of `WorldStore::current`, over any connection or transaction.
///
/// Extracted rather than duplicated so a snapshot read and a direct read cannot
/// drift apart: two copies of a `holds_at` filter is exactly the kind of
/// divergence that shows up as "the composed answer disagrees with the simple
/// one" long after the copy was made.
pub(crate) fn current_on(
    conn: &Connection,
    subject: &str,
    now_ms: i64,
) -> Result<Vec<ProjectedClaim>, StoreError> {
    if !table_exists(conn, "world_current")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {CLAIM_COLUMNS} FROM world_current WHERE subject = ?1 ORDER BY predicate_key"
    ))?;
    let rows = stmt.query_map(params![subject], claim_from_row)?;
    let mut out = Vec::new();
    for c in rows {
        let c = c?;
        if c.holds_at(now_ms) {
            out.push(c);
        }
    }
    Ok(out)
}

/// The body of `WorldStore::load_entity_projection`, over any connection.
pub(crate) fn load_entity_projection_on(
    conn: &Connection,
) -> Result<BTreeMap<String, ProjectedEntity>, StoreError> {
    if !table_exists(conn, "entities_projection")? {
        return Ok(BTreeMap::new());
    }
    let mut stmt = conn.prepare(
        "SELECT entity_id, lifecycle, redirect, origin, contradicted, contradiction
         FROM entities_projection ORDER BY entity_id ASC",
    )?;
    let mut out = BTreeMap::new();
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let (id, entity) = decode_entity_row(r)?;
        out.insert(id, entity);
    }
    Ok(out)
}

/// The largest reachable identity closure this will load before refusing.
///
/// Far above `MAX_REDIRECT_EDGES` on purpose: this is not the traversal bound,
/// it is the backstop that keeps a pathological graph from making a "bounded"
/// read unbounded again. Hitting it is a refusal, never a truncation.
pub const MAX_IDENTITY_CLOSURE: usize = 4_096;

/// **The entity rows reachable from `from`, and nothing else** — box 3d.
///
/// The bounded counterpart to [`load_entity_projection_on`], which loads the
/// WHOLE graph. `kirra_world::resolution::resolve` already caps its walk at
/// `MAX_REDIRECT_EDGES`, so a resolution touches at most that many entities
/// however large the graph is — the unboundedness was never in the traversal,
/// only in materialising everything before it.
///
/// # Why a preloader and not a lazy `AdjudicationGraph`
///
/// The obvious fix — implement the trait over per-id SQL reads — is ruled out by
/// the trait itself. `lifecycle_of` returns `Option<Lifecycle>` with no error
/// channel, and its documentation is explicit that an implementation backed by
/// storage must NOT turn a read failure into `None`, because that reports an
/// existing id as no-such-entity; it prescribes doing the fallible work BEFORE
/// resolving. This keeps that order: every row is read and decoded here, failing
/// closed, and the walk then runs over what was loaded.
///
/// # Why not a recursive CTE
///
/// `WITH RECURSIVE` over `json_each(redirect)` would also bound the read, and
/// would put edge-following logic in SQL — a SECOND implementation of the
/// traversal rule that `resolve` owns. This codebase has repeatedly paid for
/// duplicated semantics drifting; the loader deliberately follows edges with the
/// same decoded `Lifecycle` the fold wrote, and decides nothing about them.
///
/// # The superset argument, and why truncation is refused
///
/// This loads every entity within `MAX_REDIRECT_EDGES` hops of `from`, which is
/// a SUPERSET of anything the walk can reach before exhausting its budget.
/// Over-fetching is harmless — a superset yields the identical walk — while
/// under-fetching changes truth, so the bias is deliberate.
///
/// A closure larger than [`MAX_IDENTITY_CLOSURE`] is REFUSED rather than
/// truncated. Handing the walk a truncated graph would turn a
/// `TraversalBudgetExceeded` refusal into a `DanglingRedirect` one — a wrong
/// answer about WHY the graph could not be resolved, which is precisely the
/// confusion the trait's no-error-channel contract exists to prevent.
pub(crate) fn load_reachable_entity_projection_on(
    conn: &Connection,
    seeds: &[kirra_world::reference::EntityId],
) -> Result<BTreeMap<String, ProjectedEntity>, StoreError> {
    let mut out = BTreeMap::new();
    if !table_exists(conn, "entities_projection")? {
        return Ok(out);
    }
    let mut stmt = conn.prepare(
        "SELECT entity_id, lifecycle, redirect, origin, contradicted, contradiction
         FROM entities_projection WHERE entity_id = ?1",
    )?;

    // Breadth-FIRST and depth-capped, not node-capped: the walk's budget counts
    // HOPS, so "everything within MAX_REDIRECT_EDGES hops" is the set that
    // provably contains whatever it can reach. A node cap would truncate by
    // discovery order instead, which a deep chain slips through.
    // Several seeds at once for a whole answer, not one call per object: the
    // closures are unioned into ONE view so `resolve` sees the same graph for
    // every object in an answer. Resolving each object against its own view
    // would be a different composition, and a shared entity reached from two
    // objects would be loaded twice.
    let mut frontier: Vec<String> = seeds.iter().map(|e| e.as_str().to_string()).collect();
    for _ in 0..=kirra_world::resolution::MAX_REDIRECT_EDGES {
        if frontier.is_empty() {
            break;
        }
        let mut next: Vec<String> = Vec::new();
        for id in frontier.drain(..) {
            if out.contains_key(&id) {
                continue;
            }
            if out.len() >= MAX_IDENTITY_CLOSURE {
                return Err(StoreError::IdentityClosureTooLarge {
                    from: seeds
                        .iter()
                        .map(|e| e.as_str().to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    limit: MAX_IDENTITY_CLOSURE,
                });
            }
            // Two-step rather than `query_row(.., decode_entity_row)`: the
            // decoder returns `StoreError`, and rusqlite's row mapper may only
            // return `rusqlite::Error`. Collapsing them would mean decoding
            // inside the mapper and losing the fail-closed corrupt-row detail.
            let raw: Option<EntityRowColumns> = stmt
                .query_row(params![&id], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })
                .optional()?;
            let Some(raw) = raw else {
                // Absent is a legitimate answer the walk must see for itself —
                // it is how `DanglingRedirect` and `Unknown` are distinguished.
                continue;
            };
            let (key, entity) = decode_entity_columns(raw.0, raw.1, raw.2, raw.3, raw.4, raw.5)?;
            next.extend(neighbours(&entity));
            out.insert(key, entity);
        }
        frontier = next;
    }
    Ok(out)
}

/// The ids one entity's lifecycle points at.
///
/// Reads the DECODED lifecycle rather than the stored JSON, so the loader
/// follows exactly the edges the fold recorded and invents no notion of its own.
fn neighbours(entity: &ProjectedEntity) -> Vec<String> {
    use kirra_world::entity::Lifecycle;
    match &entity.lifecycle {
        Lifecycle::Merged { into } => vec![into.as_str().to_string()],
        Lifecycle::Superseded { by } => by.iter().map(|e| e.as_str().to_string()).collect(),
        Lifecycle::Split { from } => vec![from.as_str().to_string()],
        _ => Vec::new(),
    }
}

/// Decode ONE `entities_projection` row, failing closed.
///
/// Extracted so the whole-graph loader and the bounded one decode by calling
/// the same code rather than by two implementations agreeing. A drifted decoder
/// would be worse than a drifted query: it could make the two loaders disagree
/// about an entity's LIFECYCLE, which changes what `resolve` answers.
fn decode_entity_row(r: &rusqlite::Row<'_>) -> Result<(String, ProjectedEntity), StoreError> {
    decode_entity_columns(
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
    )
}

/// The six columns of an `entities_projection` row, as read.
///
/// Named so the bounded loader can hold one without a six-deep tuple type at
/// the call site; the decode below still takes them positionally, because they
/// are read positionally.
type EntityRowColumns = (
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
);

/// The decode itself, over already-read columns.
///
/// Separate from [`decode_entity_row`] because rusqlite's row mapper may only
/// return `rusqlite::Error`, while this fails closed with
/// [`StoreError::CorruptEntityProjectionRow`] carrying which row and why. The
/// bounded loader reads the columns first and decodes here for that reason.
#[allow(clippy::too_many_arguments)]
fn decode_entity_columns(
    id: String,
    token: String,
    redirect: Option<String>,
    origin: Option<String>,
    contradicted: i64,
    detail: Option<String>,
) -> Result<(String, ProjectedEntity), StoreError> {
    {
        let contradiction = match (contradicted != 0, detail.as_deref()) {
            (false, _) => None,
            (true, Some(raw)) => Some(entity_projection::contradiction_from_json(raw).map_err(
                |e| StoreError::CorruptEntityProjectionRow {
                    detail: format!("{id}: {e}"),
                },
            )?),
            // Flagged contradicted with nothing recorded: the fold always
            // writes both, so this is a row edited underneath the store.
            (true, None) => {
                return Err(StoreError::CorruptEntityProjectionRow {
                    detail: format!("{id}: contradicted with no contradiction recorded"),
                })
            }
        };
        let lifecycle = entity_projection::lifecycle_from_columns(
            &token,
            redirect.as_deref(),
            origin.as_deref(),
        )
        .map_err(|e| StoreError::CorruptEntityProjectionRow {
            detail: format!("{id}: {e}"),
        })?;
        let entity = kirra_world::reference::EntityId::new(&id).map_err(|e| {
            StoreError::CorruptEntityProjectionRow {
                detail: format!("{id}: inadmissible entity id: {e:?}"),
            }
        })?;
        Ok((
            id,
            ProjectedEntity {
                entity,
                lifecycle,
                contradiction,
            },
        ))
    }
}

/// Compacted spans that removed anything at or below `generation`.
///
/// The test is `lo_generation <= generation`, not span containment: a citation
/// covering 5..=50 removed generations 5..=40 too, so it bears on a request to
/// rebuild at 40 exactly as much as on one at 50.
pub(crate) fn compacted_at_or_below(
    conn: &Connection,
    generation: i64,
) -> Result<Vec<Citation>, StoreError> {
    if !table_exists(conn, "compaction_citations")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT lo_generation, hi_generation, event_count, range_digest,
                chain_before, chain_after, compacted_at_ms
         FROM compaction_citations
         WHERE lo_generation <= ?1
         ORDER BY lo_generation ASC",
    )?;
    let rows = stmt.query_map(params![generation], |r| {
        Ok(Citation {
            lo_generation: r.get(0)?,
            hi_generation: r.get(1)?,
            event_count: r.get(2)?,
            range_digest: r.get(3)?,
            chain_before: r.get(4)?,
            chain_after: r.get(5)?,
            compacted_at_ms: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Fold every confirmed event up to `generation` into the projection it produced.
///
/// The same reducer the live fold uses, over the same confirmed-only filter, in
/// the same generation order — so this is not a second implementation of the
/// projection that could drift from the first. It is the one implementation,
/// given a bounded input.
///
/// Applied INCREMENTALLY via `projection::fold_step` rather than collecting into
/// a `Vec` for `fold_all`. `fold_all` is exactly `fold_step` in a loop, so this
/// is the identical reduction — it simply does not hold the whole history in
/// memory at once on the way there, which for a pinned read near the head means
/// the entire confirmed log. The accumulator is bounded by the KEY count; the
/// buffer would have been bounded by the EVENT count, and those diverge without
/// limit as a store ages.
pub(crate) fn replay_to(
    conn: &Connection,
    generation: i64,
) -> Result<BTreeMap<(String, String), ProjectedClaim>, StoreError> {
    if !table_exists(conn, "world_events")? {
        return Ok(BTreeMap::new());
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {CLAIM_COLUMNS} FROM world_events
         WHERE generation <= ?1 AND claim_status = 'confirmed'
         ORDER BY generation ASC"
    ))?;
    let mut acc = BTreeMap::new();
    let mut rows = stmt.query(params![generation])?;
    while let Some(row) = rows.next()? {
        projection::fold_step(&mut acc, claim_from_row(row)?);
    }
    Ok(acc)
}

/// Replay the identity graph from the log up to `generation` — box 3h.
///
/// The generation-cut twin of [`crate::WorldStore::identity_view_at`], which
/// cuts on transaction time. Both exist because the two coordinates answer
/// different questions and must not be mixed: transaction time asks *"what did
/// we know then"*, generation asks *"what does this exact recorded position
/// reconstruct to"*. Box 3c's whole subject is that pairing one with the other
/// produces an answer whose halves come from different cuts.
///
/// Folds with the shipped [`entity_projection::fold_adjudication`] rather than a
/// re-derivation, so a pinned identity and a live one differ in WHERE they stop
/// and in nothing else — the same relationship [`replay_to`] has with the claims
/// fold.
///
/// The view's own generation is the last adjudication folded, NOT the requested
/// `generation`: that is what `IdentityView` means by its coordinate, and
/// stamping the request onto it would claim the graph had consumed events it
/// never saw.
pub(crate) fn replay_identity_to(
    conn: &Connection,
    generation: i64,
) -> Result<entity_projection::IdentityView, StoreError> {
    let mut acc = BTreeMap::new();
    let mut head: i64 = 0;
    if !table_exists(conn, "world_events")? {
        return Ok(entity_projection::IdentityView::new(acc, head));
    }

    let mut stmt = conn.prepare(
        "SELECT generation, payload, payload_schema, provenance
         FROM world_events
         WHERE claim_status = 'confirmed' AND kind = ?1 AND generation <= ?2
         ORDER BY generation ASC",
    )?;
    let mut rows = stmt.query(params![
        crate::adjudication_record::ADJUDICATION_KIND,
        generation
    ])?;
    while let Some(r) = rows.next()? {
        let at: i64 = r.get(0)?;
        let payload: String = r.get(1)?;
        let payload_schema: i64 = r.get(2)?;
        let provenance: String = r.get(3)?;
        let cited: Vec<String> = serde_json::from_str(&provenance).map_err(|e| {
            StoreError::CorruptEntityProjectionRow {
                detail: format!("provenance is not a JSON array: {e}"),
            }
        })?;
        let adjudication =
            crate::adjudication_record::decode_adjudication(&payload, payload_schema, &cited)
                .map_err(|e| StoreError::CorruptEntityProjectionRow {
                    detail: format!("generation {at}: {e}"),
                })?;
        entity_projection::fold_adjudication(&mut acc, &adjudication, at);
        head = at;
    }
    Ok(entity_projection::IdentityView::new(acc, head))
}

/// The candidate events [`crate::lineage::select_lineage`] rules over, narrowed
/// to the most the rule can possibly consume for this request.
///
/// # The narrowing is a fetch bound, NOT a second implementation of the rule
///
/// An earlier draft of this fetched every event recorded under `subject` and let
/// the rule bound it, reasoning that a query which pre-applied the generation
/// bound, the ordering and the page would be a second implementation of a
/// versioned rule — the drift `read_composed_at_generation` was corrected for on
/// #1437, *"in a place where nothing would notice."*
///
/// That reasoning was right about the hazard and wrong about the remedy. Leaving
/// the fetch unbounded made a two-event page over a long-lived subject load that
/// subject's entire history into memory, so the page bound governed the answer
/// but nothing governed the work — a resource ceiling that grows with how long
/// the system has been running, on an auditor-reachable read.
///
/// The remedy the #1437 lesson actually points at is not *"never pre-narrow"* —
/// it is *"never pre-narrow anywhere that nothing would notice."* So the
/// narrowing is supplied together with the thing that notices:
/// `narrowing_never_removes_what_the_rule_would_keep` runs the rule over the
/// narrowed candidates AND over the unnarrowed ones and asserts the two
/// selections are identical, across the corpus and every page position. Tighten
/// this query past the rule and that test goes red.
///
/// # Why `limit + 1`
///
/// The rule distinguishes a page that is exactly full and complete from one with
/// a successor by looking for an event beyond the page — its own comment says
/// *"one over the limit is fetched conceptually here."* Fetching exactly
/// `limit + 1` makes that actual: one probe row, so `More` stays detectable, and
/// never the whole tail.
///
/// The rule still applies every filter, the ordering and the truncation itself.
/// It is idempotent over this narrowing — re-filtering an already-filtered set
/// is a no-op — which is what makes the agreement test a tautology when the two
/// agree and a failure the moment they do not.
pub(crate) fn lineage_candidates(
    conn: &Connection,
    subject: &str,
    at_generation: i64,
    page: crate::lineage::LineagePage,
) -> Result<Vec<crate::lineage::LineageEvent>, StoreError> {
    if !table_exists(conn, "world_events")? {
        return Ok(Vec::new());
    }
    // `after_generation` is exclusive, matching the rule's `e.generation > a`.
    // `i64::MIN` admits everything, so an absent cursor needs no second query.
    let after = page.after_generation().unwrap_or(i64::MIN);
    // `limit` is validated into `1..=MAX_LINEAGE_PAGE` at construction, so this
    // cannot overflow and the cast is lossless.
    let probe = i64::try_from(page.limit())
        .unwrap_or(i64::MAX)
        .saturating_add(1);
    let mut stmt = conn.prepare(
        "SELECT generation, event_id, observation_id, txn_time_ms, valid_from_ms,
                valid_to_ms, source, source_version, writer_class, claim_status,
                provenance, kind, subject, predicate, object, chain_digest
         FROM world_events
         WHERE subject = ?1 AND generation <= ?2 AND generation > ?3
         ORDER BY generation ASC
         LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![subject, at_generation, after, probe], |r| {
        Ok(crate::lineage::LineageEvent {
            generation: r.get(0)?,
            event_id: r.get(1)?,
            observation_id: r.get(2)?,
            txn_time_ms: r.get(3)?,
            valid_from_ms: r.get(4)?,
            valid_to_ms: r.get(5)?,
            source: r.get(6)?,
            source_version: r.get(7)?,
            writer_class: crate::WriterClass::from_stored(&r.get::<_, String>(8)?),
            claim_status: crate::ClaimStatus::from_stored(&r.get::<_, String>(9)?),
            provenance: r.get(10)?,
            kind: r.get(11)?,
            subject: r.get(12)?,
            predicate: r.get(13)?,
            object: r.get(14)?,
            chain_digest: r.get(15)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Every event recorded under `subject`, narrowed by nothing but the subject.
///
/// The reference set the narrowed [`lineage_candidates`] is held against. Exists
/// ONLY for `narrowing_never_removes_what_the_rule_would_keep` — the production
/// path must never call it, or the resource bound it was written to enforce is
/// gone.
#[cfg(test)]
fn lineage_candidates_unnarrowed(
    conn: &Connection,
    subject: &str,
) -> Result<Vec<crate::lineage::LineageEvent>, StoreError> {
    if !table_exists(conn, "world_events")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT generation, event_id, observation_id, txn_time_ms, valid_from_ms,
                valid_to_ms, source, source_version, writer_class, claim_status,
                provenance, kind, subject, predicate, object, chain_digest
         FROM world_events WHERE subject = ?1",
    )?;
    let rows = stmt.query_map(params![subject], |r| {
        Ok(crate::lineage::LineageEvent {
            generation: r.get(0)?,
            event_id: r.get(1)?,
            observation_id: r.get(2)?,
            txn_time_ms: r.get(3)?,
            valid_from_ms: r.get(4)?,
            valid_to_ms: r.get(5)?,
            source: r.get(6)?,
            source_version: r.get(7)?,
            writer_class: crate::WriterClass::from_stored(&r.get::<_, String>(8)?),
            claim_status: crate::ClaimStatus::from_stored(&r.get::<_, String>(9)?),
            provenance: r.get(10)?,
            kind: r.get(11)?,
            subject: r.get(12)?,
            predicate: r.get(13)?,
            object: r.get(14)?,
            chain_digest: r.get(15)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn table_exists(conn: &Connection, name: &str) -> Result<bool, StoreError> {
    let n: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?1",
            params![name],
            |r| r.get(0),
        )
        .optional()?;
    Ok(n.is_some())
}

#[cfg(test)]
mod lineage_fetch_tests {
    use super::*;
    use crate::lineage::{select_lineage, LineagePage};
    use crate::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "kirra-world-lineage-fetch-{}-{}.sqlite",
            name,
            std::process::id()
        ));
        for s in ["", "-wal", "-shm"] {
            let mut q = p.as_os_str().to_os_string();
            q.push(s);
            let _ = std::fs::remove_file(std::path::PathBuf::from(q));
        }
        p
    }

    /// The log's head, read straight from the event table.
    ///
    /// Deliberately NOT the `world_current` checkpoint `lineage_at_generation`
    /// bounds against: this fixture never folds, and lineage reads evidence
    /// rather than a projection, so the events exist regardless.
    fn max_generation(conn: &Connection) -> i64 {
        conn.query_row("SELECT MAX(generation) FROM world_events", [], |r| r.get(0))
            .expect("a written log has a head")
    }

    fn append(s: &mut WorldStore, id: &str, subject: &str) {
        let event_id = EventId::new(id).expect("admissible event id");
        let observation_id = ObservationId::new(id).expect("admissible observation id");
        s.append(&NewEvent {
            event_id: &event_id,
            observation_id: &observation_id,
            txn_time_ms: 1_000,
            valid_from_ms: 1_000,
            valid_to_ms: None,
            source: "sensor-a",
            source_version: "0.1.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "observation",
            subject,
            subject_ref: None,
            predicate: Some("at"),
            object: Some("bench"),
            payload: r#"{"n":1}"#,
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append");
    }

    /// **The tripwire that lets `lineage_candidates` narrow at all.**
    ///
    /// The narrowed fetch and the rule are two expressions of the same bounds,
    /// and #1437 is the record of what happens when two such expressions drift
    /// somewhere nothing is watching. This is the watching.
    ///
    /// It asserts the property that makes the narrowing sound: the rule's answer
    /// over the narrowed candidates is IDENTICAL to its answer over every event
    /// the subject has. Not merely "the page looks right" — identical, including
    /// the boundary, so a narrowing that ate the `More` probe fails here even
    /// though every returned event was correct.
    ///
    /// Swept across page positions rather than checked once, because the two
    /// bounds that can disagree are the cursor and the probe, and both are
    /// invisible on a first page that fits.
    #[test]
    fn narrowing_never_removes_what_the_rule_would_keep() {
        let mut s = WorldStore::open(&tmp("agreement")).unwrap();
        for i in 1..=9 {
            append(&mut s, &format!("ev-{i}"), "subject-a");
            // A second subject interleaved throughout, so a narrowing that lost
            // the subject filter would also be caught here.
            append(&mut s, &format!("other-{i}"), "subject-b");
        }

        let snap = s.read_snapshot().unwrap();
        let head = max_generation(&snap.tx);

        let mut checked = 0;
        for limit in [1_usize, 2, 3, 9, 18] {
            for cursor in [None, Some(0_i64), Some(3), Some(9), Some(head)] {
                for at in [head, head / 2, 1] {
                    let page = LineagePage::new(limit, cursor).expect("valid page");

                    let narrowed = select_lineage(
                        lineage_candidates(&snap.tx, "subject-a", at, page).unwrap(),
                        "subject-a",
                        at,
                        page,
                    );
                    let full = select_lineage(
                        lineage_candidates_unnarrowed(&snap.tx, "subject-a").unwrap(),
                        "subject-a",
                        at,
                        page,
                    );

                    assert_eq!(
                        narrowed, full,
                        "narrowed and unnarrowed disagree at limit {limit}, \
                         cursor {cursor:?}, generation {at} — the fetch has been \
                         tightened past the rule"
                    );
                    checked += 1;
                }
            }
        }
        // The sweep is only evidence if it ran; a bound that silently produced
        // no cases would pass every assertion above.
        assert_eq!(checked, 75, "the sweep did not cover what it claims to");
    }

    /// The narrowing's REASON, asserted rather than described: a small page over
    /// a long history fetches a small number of rows.
    ///
    /// Without this, a later refactor could restore the unbounded fetch and only
    /// the agreement test would run — which the unbounded fetch also passes, it
    /// being the thing the narrowed one is compared against.
    #[test]
    fn a_small_page_over_a_long_history_fetches_a_small_number_of_rows() {
        let mut s = WorldStore::open(&tmp("bounded")).unwrap();
        for i in 1..=200 {
            append(&mut s, &format!("ev-{i}"), "subject-a");
        }

        let snap = s.read_snapshot().unwrap();
        let head = max_generation(&snap.tx);
        let page = LineagePage::new(2, None).expect("valid page");

        let fetched = lineage_candidates(&snap.tx, "subject-a", head, page).unwrap();
        assert_eq!(
            fetched.len(),
            3,
            "a 2-event page must fetch the page plus ONE probe row, not the history"
        );

        let all = lineage_candidates_unnarrowed(&snap.tx, "subject-a").unwrap();
        assert_eq!(
            all.len(),
            200,
            "the history really is long — the bound is not vacuous"
        );
    }
}
