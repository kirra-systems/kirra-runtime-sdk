//! **kirra-sidecars** — the shipped doer-side services of the governed loop.
//!
//! The loop this crate serves (typed text → enforced motion, no speech):
//!
//! ```text
//!   text ─▶ mick_service (LLM → fail-closed typed intent, READ-ONLY publish)
//!               │  GET /intent/last            (never a command)
//!               ▼
//!          occy_doer (the DOER bridge, ros2_ws) ─▶ planner_service POST /plan
//!               │        Occy grounds the intent; the KIRRA slow-loop checker
//!               │        bounds it and NARRATES a refusal (#893 reason)
//!               ▼
//!          /cmd_vel_raw ─▶ interceptor + governor ─▶ verifying consumer ─▶ wheels
//! ```
//!
//! Everything here PROPOSES or NARRATES. Enforcement lives elsewhere (the
//! verifier service, the fast-loop governor, the ADR-0033 verifying motor
//! consumer) — and this crate is FENCED by `ci/check_mick_actuation_fence.py`:
//! no dependency route to the release-token mint, the serial seam, or any
//! ROS/DDS transport can compile into these binaries.

#![forbid(unsafe_code)]

pub mod http;
pub mod mick;
pub mod narrator;
pub mod net;
pub mod planner;
pub mod taj;
