//! **Kirra World — domain core. Tier 1 in progress: the domain model is real.**
//!
//! This crate exists to prove ONE thing that is expensive to get wrong later:
//! the dependency *shape*. ADR-0040 (WM-1) proposes `kirra-world` as a pure
//! domain core with `-store` and `-service` as adapters, and its ratification
//! criteria ask for a "prototype crate graph — `kirra-world` compiling as a leaf
//! with no ROS, no actuation, and no checker edge". That is what this is.
//!
//! # What this crate contains, and what it still does not
//!
//! **Real** — seven modules, pure functions over pure data, still zero
//! dependencies:
//!
//! * [`mod@trust`] (§9) — the four orthogonal trust axes and the transition
//!   rules, with the anti-laundering rule (5) and read-time validity (6) as the
//!   load-bearing parts.
//! * [`mod@observation`] (§7, pure half) — structured `Confidence`, source
//!   classes, clock domains that cannot be mixed, and payload provenance that
//!   an operator correction cannot launder (P10).
//! * [`mod@entity`] (§6, structure and kinds) — the root-closed taxonomy,
//!   lifecycle, and kind as adjudicated evidence rather than a stored field.
//! * [`mod@relationship`] (§8) — directed, typed, time-bounded relations;
//!   supersession instead of update; inferences that cannot omit their
//!   derivation.
//! * [`mod@retention`] — ADR-0040's Tier 1 exit criterion, deciding half. The
//!   store has known *how* to compact since WM-2; nothing has ever decided
//!   *when*, which is why the horizons OQ2 ruled have gone unenforced.
//!
//! * [`mod@reference`] — the opaque identity and spatial-reference newtypes
//!   (`EventId`, `ObservationId`, `FrameId`, `MapId`), which make two adjacent
//!   pairs of same-typed strings unswappable. Validated, never normalized.
//! * [`mod@kind`] (§7.1) — `ObservationKind` and the per-kind versioned body
//!   contract. Ruled rather than invented (`KIRRA-WM-OBSKIND-001`, 2026-08-07):
//!   this was a *specification* gap, not an implementation one.
//!
//! **Still absent:** storage, API and queries — plus the parts of §6/§7 that
//! genuinely need a dependency (ULID *generation*, content hashing, frames,
//! maps), which belong to the store. Note the split that settled: the core owns
//! the **type** and what makes a value admissible; the layer with a clock and a
//! hash **mints** the value. Five types below are still **unconstructible
//! placeholders**, each with a private unit field, so no logic can accrete
//! around a name before the model that gives it meaning exists.
//!
//! The governing decision is the safety-assurance scope ruling
//! ([ADR-0042](../../../docs/adr/0042-world-model-terminology-and-safety-boundary-scope.md)
//! Decision 5) — **PENDING and unassigned when this crate was written, RECORDED
//! on 2026-08-05** as *safety-related, non-authoritative*. Statuses as they now
//! stand: **all four World Model ADRs are Accepted** — 0041 on 2026-08-04, and
//! 0039, 0040 and 0042 on 2026-08-06. Each was an owner self-assessment; none
//! authorizes implementation by its own terms.
//!
//! # The names
//!
//! Taken from the blueprint's own vocabulary (`KIRRA-WM-ARCH-001`), not invented
//! here: bitemporal time (P7), four orthogonal trust axes (P6), entities and
//! observations with provenance (§9). Names are placeholders too — a name is a
//! decision, and these have not been ratified either.
//!
//! # Why the ADAPTER is ahead of this core — read this before assuming neglect
//!
//! An unusual shape, and the one most likely to read as sloppiness to someone
//! scanning the crate list: **`kirra-world-store` is a working implementation**
//! — schema, write path, hash chain, current-state projection, bitemporal
//! queries, compaction — **while this crate, the domain core it adapts, is
//! still unconstructible placeholders.** Adapters normally trail their core.
//!
//! **It is not because a gate holds this crate closed.** The domain-logic gate
//! (`ci/check_world_domain_logic_gate.py`) is deliberately **self-releasing**:
//! recording the Decision 5 ruling relaxes it automatically, and the ruling was
//! recorded on 2026-08-05. `kirra-world*` is no longer held to
//! declaration-only. What still constrains this crate is the ruling's own
//! *Conditions that reopen the decision* — not the gate.
//!
//! So the honest reason the core was empty was simpler and less flattering than
//! a gate: **WM-2's scoped work was the storage slice, and nobody had done the
//! domain-types work.** That was a decision about sequencing, recorded as one
//! (ADR-0041, *WM-2 implementation milestone*) rather than dressed up as an
//! external hold.
//!
//! **Tier 1 has now started**, and the gap is closing from this end: the four
//! modules above are domain logic the store does not have. Note which direction
//! that runs — the store's `WriterClass` + two-valued `ClaimStatus` is, in the
//! scope doc's words, *"an adjudication proxy and nothing more"*. The four axes
//! are what it is a proxy **for**, so the core is now ahead of the adapter on
//! this one concept, and the store will need to grow toward it rather than the
//! reverse.
//!
//! # Naming — this is NOT "the world model"
//!
//! Canonical name: **Kirra World**. Accurate prose gloss: **evidence ledger**.
//!
//! "World model" is ruled out by ADR-0042 Decision 1, off a collision that was
//! **live in the safety closure when the ruling was made**: `kirra-trajectory`'s
//! `perception_redundancy.rs` and the ros2 adapter used it for *redundant
//! perception channels*. Those have since been renamed to *independent
//! perception channel*, so that half of the collision is resolved in code — the
//! rule is what keeps it resolved.
//!
//! One live collision remains: `robot/world_model.py`, a TTL'd operator-facing
//! read projection. ADR-0042 puts its rename behind safety review, because the
//! module is imported by `rabbit_converse.py`, staged by the installer, and
//! gated by `KIRRA_WORLD_MODEL_ENABLED` — renaming it changes robot deployment,
//! not prose.
//!
//! The reason is safety communication, not taste: *"the world model was wrong"*
//! must not be able to mean a perception fault and a knowledge fault at once.
//! Externally the term invites a second wrong reading — a learned predictive
//! model — which this is not, in any part.
//!
//! # Fence position
//!
//! This crate is inside **Fence A**: Kirra World must be structurally unable to
//! reach an actuator or an authorization. It has **zero dependencies**, so that
//! holds trivially today — and `ci/check_kirra_world_bidirectional_fence.py`
//! now checks it for real rather than reporting "not present yet".
//!
//! It must never appear in the safety closure (**Fence B**). The same gate
//! enforces that from the other direction.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod entity;
pub mod kind;
pub mod observation;
pub mod reference;
pub mod relationship;
pub mod retention;
pub mod trust;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Stable identity of a thing the world contains.
///
/// **No longer a placeholder** — the real type is [`entity::EntityId`], and this
/// is a re-export of it rather than a second type sharing its name.
///
/// # Why this is a re-export and not a struct
///
/// It *was* a struct — `EntityId(())`, unconstructible — and the §6 taxonomy
/// slice added the real `entity::EntityId(String)` beside it without retiring
/// it. That left the crate exporting **two distinct types with the same name**:
/// inside the crate nothing was confused, because every module names
/// `crate::entity::EntityId` explicitly, but `kirra_world::EntityId` resolved to
/// the dead one — and `kirra-world-store` and `kirra-world-service` both
/// re-exported that, putting an unconstructible placeholder three names from the
/// domain type at the layer an integrator imports from.
///
/// Measured and recorded in
/// [`WM_Q1_SEAM_BASELINE.md`](../../../docs/design/WM_Q1_SEAM_BASELINE.md)
/// before being fixed here.
///
/// ADR-0040's constraint on whatever this became still holds and is now the
/// real type's to keep: identity adjudication is **revisable** — merge and split
/// are recorded events, not destructive edits — so it cannot be a bare opaque
/// key that loses its own history. `entity::EntityId` is opaque by construction;
/// the *history* half is entity resolution, `WM_SCOPE.md` Tier 2.
pub use entity::EntityId;

/// Identity of a single recorded observation.
///
/// **No longer a placeholder** — the real type is [`reference::ObservationId`],
/// re-exported rather than redeclared, following the shape [`EntityId`] and
/// [`TrustAxes`] already use. Redeclaring is what produced the two-types-one-name
/// collision fixed in #1375; there is now exactly one way to name each of these.
///
/// Distinct from [`EntityId`] on purpose: the blueprint's model is evidence-first,
/// so an observation outlives whatever entity it was later attributed to, and
/// re-attribution must not rewrite it. It is equally distinct from [`EventId`],
/// which identifies the *record* — see that type for why collapsing the two costs
/// the log its re-attribution history.
pub use reference::ObservationId;

/// Identity of one record in the evidence log.
///
/// New in the reference slice, and not one of the original placeholders: the
/// storage layer had carried this concept as a bare `&str` alongside
/// `observation_id` since it was written, with nothing but argument order
/// keeping them apart. See [`reference::EventId`].
pub use reference::EventId;

/// Why a reference was refused. See [`mod@reference`].
pub use reference::ReferenceError;

/// What a claim asserts about its subject, and the per-kind versioned body
/// contract — §7.1's `TypedPayload`.
///
/// **No longer absent.** The variant list was a *specification* gap rather than
/// an implementation one: §7.1 named the field and the blueprint never
/// enumerated it, so [`mod@observation`] refused to invent a taxonomy. Ruled
/// 2026-08-07 (`KIRRA-WM-OBSKIND-001`, option 2) — three attested variants plus
/// a degrade target, with `Existence` deferred.
pub use kind::{BodyError, ObservationKind, TypedBody};

// ---------------------------------------------------------------------------
// Where a claim came from
// ---------------------------------------------------------------------------

/// Who or what produced an observation.
///
/// PLACEHOLDER. ADR-0040 fixes the writer classes: an LLM may create only a
/// suggestion, a candidate label, a candidate relationship or a candidate query
/// — **never a confirmed fact**. Whatever this becomes has to make that
/// distinction unforgeable rather than conventional.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Source(());

/// The full chain behind a claim: source, derivation, and what it rests on.
///
/// PLACEHOLDER. Provenance is the blueprint's most-cited concept (§9) and the
/// reason the store is evidence-not-truth: a claim without its chain cannot be
/// re-judged when the evidence under it changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Provenance(());

// ---------------------------------------------------------------------------
// Spatial reference
// ---------------------------------------------------------------------------

/// The coordinate frame a spatial claim is expressed in.
///
/// **No longer a placeholder** — [`reference::FrameId`], re-exported. Still the
/// one to be most careful with: ADR-0042 Decision 2 draws the line between a
/// *semantic* map and the checker's *authoritative* corridor, and a frame or map
/// reference held here may help an untrusted doer choose a goal but must never
/// become the checker's geometry by virtue of existing.
pub use reference::FrameId;

/// Which map a spatial claim is relative to.
///
/// **No longer a placeholder** — [`reference::MapId`], re-exported. See
/// [`FrameId`] — same boundary, same prohibition. If Kirra World and the safety
/// path ever read the same map artifact, that is a reviewed
/// `[[shared_source_artifact]]` entry, not an implicit permission.
///
/// Being a *separate type* from [`FrameId`] rather than another `Option<&str>` is
/// what makes the two unswappable; the storage layer held them adjacent, and a
/// swap there satisfied SD-4's "a spatial claim carries a frame" check while
/// carrying the wrong one.
pub use reference::MapId;

// ---------------------------------------------------------------------------
// Bitemporality (blueprint P7)
// ---------------------------------------------------------------------------

/// When the fact held **in the world**.
///
/// PLACEHOLDER. Kept distinct from [`TransactionTime`] because without both,
/// "what did you believe at 14:03?" is unanswerable and incident reconstruction
/// degrades to guesswork.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ValidTime(());

/// When the **system learned** the fact.
///
/// PLACEHOLDER. The other half of P7. A store that keeps only this one can say
/// what it was told and when, but not what was true.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TransactionTime(());

// ---------------------------------------------------------------------------
// Trust (blueprint P6)
// ---------------------------------------------------------------------------

/// Trust decomposed into its orthogonal axes.
///
/// **No longer a placeholder** — this is the first Tier 1 slice, implemented in
/// [`mod@trust`]. The blueprint is explicit that trust is *not a scalar and not a
/// single enum*: it decomposes into *origin*, *corroboration*, *adjudication*
/// and *temporal validity*, stored separately and collapsed to a grade only at
/// the query boundary, for consumers that ask for one.
///
/// Collapsing early is the failure this type exists to prevent — a single number
/// cannot distinguish "one trusted sensor said so once" from "three sources
/// agree but the claim is stale".
///
/// Note the shape the implementation took: **three stored axes, not four.**
/// Validity is computed by [`trust::validity_at`] and has nowhere to be written,
/// which makes transition rule 6 unbreakable rather than merely documented.
pub use trust::TrustAxes;

// ---------------------------------------------------------------------------
// Query results
// ---------------------------------------------------------------------------

/// The outcome of asking Kirra World a question.
///
/// PLACEHOLDER, and deliberately **not** `Option<T>`. ADR-0040 records why:
/// `Option::None` collapses "we looked and it is not there", "we could not
/// look", and "we looked and are not sure" into one value — three answers a
/// caller must treat differently, and the distinction is unrecoverable once
/// lost.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolutionOutcome(());
