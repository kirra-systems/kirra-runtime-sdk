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
//! **Real** — four modules, pure functions over pure data, still zero
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
//! **Still absent:** storage, API and queries — plus the parts of §6/§7 that
//! need a dependency (ULID identity, content hashing, frames, maps, typed
//! payloads), which belong to the store. The remaining types below are
//! **unconstructible placeholders**, each with a private unit field, so no logic
//! can accrete around a name before the model that gives it meaning exists.
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
pub mod observation;
pub mod relationship;
pub mod retention;
pub mod trust;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Stable identity of a thing the world contains.
///
/// PLACEHOLDER. ADR-0040 fixes that identity adjudication is **revisable** —
/// merge and split are recorded events, not destructive edits — so whatever
/// this becomes cannot be a bare opaque key that loses its own history.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntityId(());

/// Identity of a single recorded observation.
///
/// PLACEHOLDER. Distinct from [`EntityId`] on purpose: the blueprint's model is
/// evidence-first, so an observation outlives whatever entity it was later
/// attributed to, and re-attribution must not rewrite it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObservationId(());

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
/// PLACEHOLDER, and the one to be most careful with. ADR-0042 Decision 2 draws
/// the line between a *semantic* map and the checker's *authoritative* corridor:
/// a frame or map reference held here may help an untrusted doer choose a goal,
/// and must never become the checker's geometry by virtue of existing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FrameId(());

/// Which map a spatial claim is relative to.
///
/// PLACEHOLDER. See [`FrameId`] — same boundary, same prohibition. If Kirra
/// World and the safety path ever read the same map artifact, that is a
/// reviewed [[shared_source_artifact]] entry, not an implicit permission.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MapId(());

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
