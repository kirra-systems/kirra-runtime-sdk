//! **Kirra World service adapter — PROTOTYPE: shape only, no service.**
//!
//! No HTTP, no routes, no server, no runtime, no transport dependency. This
//! crate exists so the third node of ADR-0040's proposed graph is present and
//! checkable; it does not run.
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

// Both edges exercised, so the proposed three-node graph is genuinely built and
// not merely described in a manifest.
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
        assert_eq!(id, kirra_world::entity::EntityId::new("e-1").unwrap());
    }
}
