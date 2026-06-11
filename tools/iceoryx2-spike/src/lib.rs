// iceoryx2-spike — host-side feature-subset + fault-matrix spike (#273).
//
// A STANDALONE crate (its own workspace + lockfile) that proves the iceoryx2
// feature subset KIRRA's governor edge needs and runs the QNX-harness fault
// matrix over a real Rust zero-copy pub/sub channel — no C++ shim, no FFI, no
// unsafe on the command path. Informs #274 (QNX 8.0 feature check) and #275
// (the transport ADR). It is a SPIKE, not an adoption: iceoryx2 must never enter
// any SDK/parko crate's dependency tree.
//
// The certified kinematic envelope lives in the untouched talisman
// `src/gateway/kinematics_contract.rs`; this crate imports NOTHING from it and
// labels its own envelope numbers as PROXIES.

pub mod harness;
pub mod judge;
pub mod wire;
