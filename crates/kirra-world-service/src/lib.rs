//! **Kirra World service adapter — PROTOTYPE: no service, but no longer empty.**
//!
//! No HTTP, no routes, no server, no runtime, no transport dependency — all
//! still true, and all still load-bearing. This crate does not run.
//!
//! **What changed 2026-08-07:** it now carries [`read_view`], the **answer
//! boundary** — the one place Tier 3's "no bare values" rule can be made
//! structural, since `ProjectedClaim` cannot carry it (see that module). It
//! landed here because `WM_SCOPE.md` §9's consumer was refused every doer-side
//! host by Fence B, and this is the only crate that could legally hold the shape
//! that consumer produced.
//!
//! So the header below is amended rather than deleted: the crate is no longer
//! *shape only*, but the emptiness that mattered — no transport, no runtime, no
//! route to an actuator — is exactly as it was, and is what the section below is
//! actually about.
//!
//! # Why the emptiness is the point
//!
//! Fence A says Kirra World must be structurally unable to reach an actuator or
//! an authorization. A *service* crate is where that would most plausibly erode
//! — a transport dependency added "just to publish status", a ROS handle
//! threaded through "temporarily". `ci/check_kirra_world_bidirectional_fence.py`
//! now walks this crate's dependency closure for exactly those edges, and it
//! does so from today rather than from whenever the service is first written.
//!
//! A fence that arrives with the code is a fence argued with. This one is
//! already here.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod answer_ref;
pub mod read_view;

// Both edges exercised, so the proposed three-node graph is genuinely built and
// not merely described in a manifest. The edge is no longer only exercised --
// `read_view` consumes the core and the store in earnest.
pub use kirra_world::ResolutionOutcome;
pub use kirra_world_store::EntityId;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_re_exported_entity_id_is_the_domain_type_not_a_placeholder() {
        // Regression guard. `kirra_world` briefly exported TWO types named
        // `EntityId` — the real `entity::EntityId(String)` and a dead
        // crate-root `EntityId(())` — and the placeholder was what travelled
        // core → store → service. This crate is the furthest hop, so it is
        // where the substitution was hardest to see and is worth pinning.
        //
        // Before the fix this did not compile: the placeholder has a private
        // unit field and no constructor.
        let id = EntityId::new("e-1").expect("the real type constructs");
        assert_eq!(id.as_str(), "e-1");

        // And it is the SAME type the domain model uses, not a look-alike.
        //
        // The right-hand side is the DECLARATION SITE (`reference`, since
        // 2026-08-08 — the type moved there so `observation::SubjectRef` could
        // carry it without an entity <-> observation module cycle), and that is
        // deliberate rather than incidental.
        //
        // `EntityId` here travels `kirra_world` (crate root) -> `kirra_world_store`
        // -> this crate: the store re-exports the ROOT name, not the module
        // path. So the left side already carries whatever the root resolves to,
        // and comparing it against the declaration is what proves the root has
        // not been shadowed by a placeholder again. Asserting against
        // `kirra_world::EntityId` instead would put the root on BOTH sides and
        // make the check tautological — a root regression would compare a
        // placeholder to itself.
        assert_eq!(id, kirra_world::reference::EntityId::new("e-1").unwrap());
    }
}
