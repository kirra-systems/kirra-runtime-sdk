//! **Kirra World — domain core. PROTOTYPE: shape only, no domain logic.**
//!
//! This crate exists to prove ONE thing that is expensive to get wrong later:
//! the dependency *shape*. ADR-0040 (WM-1) proposes `kirra-world` as a pure
//! domain core with `-store` and `-service` as adapters, and its ratification
//! criteria ask for a "prototype crate graph — `kirra-world` compiling as a leaf
//! with no ROS, no actuation, and no checker edge". That is what this is.
//!
//! # What this crate deliberately does NOT contain
//!
//! No fields. No invariants. No constructors. No storage. No API. No queries.
//! The ten types below are **unconstructible placeholders** — each has a private
//! unit field, so nothing outside this crate can build one and no logic can
//! quietly accrete around them while the decision that governs them is still
//! open.
//!
//! That decision is the safety-assurance scope ruling
//! ([ADR-0042](../../../docs/adr/0042-world-model-terminology-and-safety-boundary-scope.md)
//! Decision 5), which is **PENDING and unassigned**. ADR-0039, ADR-0040,
//! ADR-0041 and ADR-0042 are all **Proposed**, none Accepted. Nothing here
//! ratifies any of them, and the first real domain-types work is gated behind
//! that ruling — not behind this crate existing.
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
//! It is intended. ADR-0042 Decision 5 released the *storage* gate specifically,
//! on the argument that persisting evidence carries no authority; it did not
//! release the domain-logic gate, which is what would let real types, fields and
//! invariants land here. So the store could proceed and this could not.
//!
//! The private unit fields below are what keep that honest. Nothing outside this
//! crate can construct these types, so no logic can quietly accrete around a
//! name while the ruling that governs it is still open — which is exactly the
//! failure the split is protecting against.
//!
//! # Naming — this is NOT "the world model"
//!
//! Canonical name: **Kirra World**. Accurate prose gloss: **evidence ledger**.
//!
//! "World model" is ruled out (ADR-0042 Decision 1) because it already means
//! two other things here — *redundant perception channels* in
//! `kirra-trajectory`'s `perception_redundancy.rs` and the ros2 adapter, **both
//! inside the safety closure**, and a TTL'd operator-facing read projection in
//! `robot/world_model.py`. The reason is safety communication, not taste:
//! *"the world model was wrong"* must not be able to mean a perception fault
//! and a knowledge fault at once. Externally it invites a second wrong reading —
//! a learned predictive model — which this is not, in any part.
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

/// Trust decomposed into its four orthogonal axes.
///
/// PLACEHOLDER. The blueprint is explicit that trust is **not a scalar and not a
/// single enum**: it decomposes into *origin*, *corroboration*, *adjudication*
/// and *temporal validity*, stored separately and collapsed to a grade only at
/// the query boundary, for consumers that ask for one.
///
/// Collapsing early is the failure this type exists to prevent — a single number
/// cannot distinguish "one trusted sensor said so once" from "three sources
/// agree but the claim is stale".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrustAxes(());

// ---------------------------------------------------------------------------
// Query results
// ---------------------------------------------------------------------------

/// The outcome of asking the world model a question.
///
/// PLACEHOLDER, and deliberately **not** `Option<T>`. ADR-0040 records why:
/// `Option::None` collapses "we looked and it is not there", "we could not
/// look", and "we looked and are not sure" into one value — three answers a
/// caller must treat differently, and the distinction is unrecoverable once
/// lost.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolutionOutcome(());
