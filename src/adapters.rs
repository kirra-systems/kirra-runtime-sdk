// src/adapters.rs
//
// ADR-0035 Stage 1: the per-fieldbus adapter layer (`adapters/{mod,canopen,dnp3,
// ethernet_ip}`) was moved VERBATIM into the lean `kirra-industrial` leaf crate.
// This module is now a re-export shim so every existing
// `kirra_verifier::adapters::{canopen,dnp3,ethernet_ip}::*` path and the trait /
// bound types (`IndustrialAdapter`, `AdapterVerdict`, `ScalarType`, `BoundSpec`)
// resolve unchanged. The glob re-exports the child modules too, so the submodule
// paths (`kirra_verifier::adapters::canopen::init_node_map_from_env`, …) hold.
pub use kirra_industrial::adapters::*;
