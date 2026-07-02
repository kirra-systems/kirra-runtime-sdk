//! # Precision-aware backend selection (Q-2; `parko/QUANTIZATION_DESIGN.md` §5/§9)
//!
//! Q-0/Q-1 built the measuring stick and produced the first per-chip evidence
//! (`parko/QUANTIZATION_Q1_SCOPE.md`; measured results in
//! `parko/crates/parko-tensorrt/Q1B_ORIN.md`). This module turns that evidence
//! into a runtime choice: a **precision ladder** — the operator's ORDERED list of
//! the `(chip, model)` precision rows that **passed the performance contract** —
//! walked at startup to pick the artifact actually run.
//!
//! ## Semantics (design note §5, exactly)
//!
//! - **The ladder is evidence, not preference.** Its entries are the rows that
//!   passed the §4 contract on THIS chip, ordered by measured merit. The Q-1b Orin
//!   result is the canonical example of why order is measured, not assumed: INT8
//!   was *slower* than FP32 on the planner scorer, so that deployment's ladder is
//!   just `fp32`.
//! - **A rung is "supported" by OPERATIONAL PROOF, not a static flag.**
//!   `BackendCapabilities` are deliberately conservative (`supports_int8` stays
//!   `false` until hardware-measured), so selection would be inert if it gated on
//!   them. Instead [`select_by_ladder`] attempts each rung via the caller's
//!   `try_build` (construct + fail-closed `warm_up` — the same proof the Q-1b
//!   probe uses) and takes the first that actually works.
//! - **Failure degrades, visibly — it does not fail closed.** The checker, not
//!   the selector, is the fail-closed safety authority; the doer is
//!   availability-preserving. A rung that will not build is recorded in
//!   [`LadderSelection::rejected`] (the no-silent-caps rule) and the walk
//!   continues. To guarantee the doer keeps running, [`PrecisionLadder::parse`]
//!   **anchors the ladder with FP32** if the operator omitted it (flagged in
//!   [`PrecisionLadder::fp32_anchored`], never silent) — the always-available
//!   reference artifact, the selection analogue of `PlanOutput::safe_stop`.
//! - **Nothing outside the ladder is ever chosen.** Exhausting every rung is an
//!   `Err` — the existing `BackendSelector` fail-closed posture; no silent
//!   substitution of an unlisted precision.
//!
//! Selection composes with (does not replace) [`crate::BackendSelector`]: the
//! descriptor→backend axis stays PARK-022's; this module adds the precision axis
//! via a caller-supplied constructor closure, because only the integrator knows
//! the per-precision artifact paths and backend config (e.g. the TensorRT
//! `Int8Qdq` posture + per-precision engine cache).
//!
//! Safety framing: selection tunes the UNTRUSTED doer's inference only. Whatever
//! rung wins, the KIRRA checker bounds its proposals exactly as it bounds FP32.

use crate::backend::{BackendError, PrecisionMode};

/// Env var carrying the deployment's precision ladder — a comma-separated,
/// highest-merit-first list of contract-passing precisions, e.g. `fp16,fp32`.
/// Unset/blank → the `fp32`-only default. Unknown tokens are an `Err`
/// (fail-closed parse — never a silently different precision), mirroring
/// [`crate::KIRRA_BACKEND_ENV`].
pub const KIRRA_PRECISION_LADDER_ENV: &str = "KIRRA_PRECISION_LADDER";

/// The operator's ordered precision allow-list for this deployment (see the
/// module docs for the semantics — entries must come from contract evidence).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecisionLadder {
    rungs: Vec<PrecisionMode>,
    fp32_anchored: bool,
}

impl PrecisionLadder {
    /// The unconfigured default: FP32 only (the reference artifact). Absent an
    /// explicit operator choice, no reduced-precision artifact is engaged —
    /// engaging one must be a deliberate, evidence-backed act.
    #[must_use]
    pub fn default_fp32() -> Self {
        Self { rungs: vec![PrecisionMode::FP32], fp32_anchored: false }
    }

    /// Parse a comma-separated ladder (`"int8,fp16,fp32"`; case-insensitive,
    /// whitespace-tolerant). Fail-closed: an unknown or duplicate token, or an
    /// empty list, is an `Err` — a config typo must never select something else.
    /// If `fp32` is absent it is APPENDED as the availability anchor and
    /// [`Self::fp32_anchored`] reports it (visible, per the no-silent-caps rule).
    pub fn parse(s: &str) -> Result<Self, BackendError> {
        let mut rungs = Vec::new();
        for raw in s.split(',') {
            let tok = raw.trim();
            if tok.is_empty() {
                return Err(BackendError::InitializationError(format!(
                    "empty precision token in ladder {s:?} (fail-closed)"
                )));
            }
            let mode = match tok.to_ascii_lowercase().as_str() {
                "fp32" => PrecisionMode::FP32,
                "fp16" => PrecisionMode::FP16,
                "int8" => PrecisionMode::INT8,
                other => {
                    return Err(BackendError::InitializationError(format!(
                        "unknown precision {other:?} in ladder {s:?} (fail-closed; \
                         expected fp32|fp16|int8)"
                    )));
                }
            };
            if rungs.contains(&mode) {
                return Err(BackendError::InitializationError(format!(
                    "duplicate precision {tok:?} in ladder {s:?} (fail-closed; \
                     a repeated rung is a config error)"
                )));
            }
            rungs.push(mode);
        }
        if rungs.is_empty() {
            return Err(BackendError::InitializationError(
                "empty precision ladder (fail-closed)".to_string(),
            ));
        }
        let fp32_anchored = !rungs.contains(&PrecisionMode::FP32);
        if fp32_anchored {
            rungs.push(PrecisionMode::FP32);
        }
        Ok(Self { rungs, fp32_anchored })
    }

    /// The env-routing logic, separated from the env read for testability
    /// (the workspace invariant: no `set_var` in tests). `None`/blank → the
    /// FP32-only default; otherwise [`Self::parse`].
    pub fn from_env_value(value: Option<&str>) -> Result<Self, BackendError> {
        match value {
            None => Ok(Self::default_fp32()),
            Some(s) if s.trim().is_empty() => Ok(Self::default_fp32()),
            Some(s) => Self::parse(s),
        }
    }

    /// Build the ladder from [`KIRRA_PRECISION_LADDER_ENV`].
    pub fn from_env() -> Result<Self, BackendError> {
        let raw = std::env::var(KIRRA_PRECISION_LADDER_ENV).ok();
        Self::from_env_value(raw.as_deref())
    }

    /// The rungs, in walk order (including the appended FP32 anchor, if any).
    #[must_use]
    pub fn rungs(&self) -> &[PrecisionMode] {
        &self.rungs
    }

    /// `true` iff FP32 was absent from the operator's list and appended as the
    /// availability anchor. Callers should log this — the anchor is deliberate
    /// and visible, never silent.
    #[must_use]
    pub fn fp32_anchored(&self) -> bool {
        self.fp32_anchored
    }
}

/// The outcome of a ladder walk: the backend that won, at which precision, and
/// every higher rung that was rejected on the way down (with its error) — the
/// caller logs these so a degradation is never silent.
#[derive(Debug)]
pub struct LadderSelection<B> {
    pub backend: B,
    pub precision: PrecisionMode,
    /// Higher-preference rungs that failed to build, in walk order.
    pub rejected: Vec<(PrecisionMode, BackendError)>,
    /// Mirrors [`PrecisionLadder::fp32_anchored`] for the caller's log line.
    pub fp32_anchored: bool,
}

impl<B> LadderSelection<B> {
    /// `true` iff the winning rung was not the operator's first choice.
    #[must_use]
    pub fn degraded(&self) -> bool {
        !self.rejected.is_empty()
    }
}

/// Walk `ladder` in order, attempting `try_build` per rung; the first rung that
/// builds wins. `try_build` should carry the FULL operational proof for a rung —
/// construct the backend at that precision AND run its fail-closed `warm_up`
/// (engine build) — so "supported" means demonstrated, not assumed.
///
/// Every rejected rung is returned in the selection (visibility); exhausting the
/// ladder is an `Err` naming each rung's failure (fail-closed — nothing outside
/// the ladder is substituted).
pub fn select_by_ladder<B>(
    ladder: &PrecisionLadder,
    mut try_build: impl FnMut(PrecisionMode) -> Result<B, BackendError>,
) -> Result<LadderSelection<B>, BackendError> {
    let mut rejected: Vec<(PrecisionMode, BackendError)> = Vec::new();
    for &rung in ladder.rungs() {
        match try_build(rung) {
            Ok(backend) => {
                return Ok(LadderSelection {
                    backend,
                    precision: rung,
                    rejected,
                    fp32_anchored: ladder.fp32_anchored(),
                });
            }
            Err(e) => rejected.push((rung, e)),
        }
    }
    let detail: Vec<String> =
        rejected.iter().map(|(p, e)| format!("{p:?}: {e}")).collect();
    Err(BackendError::InitializationError(format!(
        "no rung of the precision ladder could be built (fail-closed; nothing \
         outside the ladder is substituted): [{}]",
        detail.join("; ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fails(msg: &str) -> BackendError {
        BackendError::InitializationError(msg.to_string())
    }

    #[test]
    fn parse_preserves_order_and_is_case_insensitive() {
        let l = PrecisionLadder::parse("INT8, fp16 ,Fp32").unwrap();
        assert_eq!(
            l.rungs(),
            &[PrecisionMode::INT8, PrecisionMode::FP16, PrecisionMode::FP32]
        );
        assert!(!l.fp32_anchored(), "fp32 was explicit — no anchor appended");
    }

    #[test]
    fn parse_appends_fp32_anchor_visibly_when_absent() {
        let l = PrecisionLadder::parse("int8,fp16").unwrap();
        assert_eq!(
            l.rungs(),
            &[PrecisionMode::INT8, PrecisionMode::FP16, PrecisionMode::FP32],
            "fp32 availability anchor appended last"
        );
        assert!(l.fp32_anchored(), "the append is flagged, never silent");
    }

    #[test]
    fn parse_fails_closed_on_garbage() {
        assert!(PrecisionLadder::parse("").is_err(), "empty ladder");
        assert!(PrecisionLadder::parse("int4").is_err(), "unknown precision");
        assert!(PrecisionLadder::parse("fp32,,int8").is_err(), "empty token");
        assert!(PrecisionLadder::parse("int8,int8").is_err(), "duplicate rung");
    }

    #[test]
    fn env_default_is_fp32_only() {
        for v in [None, Some(""), Some("   ")] {
            let l = PrecisionLadder::from_env_value(v).unwrap();
            assert_eq!(l.rungs(), &[PrecisionMode::FP32]);
            assert!(!l.fp32_anchored());
        }
    }

    #[test]
    fn first_buildable_rung_wins_without_degradation() {
        let ladder = PrecisionLadder::parse("int8,fp32").unwrap();
        let sel = select_by_ladder(&ladder, Ok::<_, BackendError>).unwrap();
        assert_eq!(sel.precision, PrecisionMode::INT8);
        assert!(!sel.degraded());
        assert!(sel.rejected.is_empty());
    }

    #[test]
    fn failed_rung_degrades_visibly_to_the_next() {
        let ladder = PrecisionLadder::parse("int8,fp16,fp32").unwrap();
        let sel = select_by_ladder(&ladder, |p| match p {
            PrecisionMode::INT8 => Err(fails("no int8 engine")),
            other => Ok(other),
        })
        .unwrap();
        assert_eq!(sel.precision, PrecisionMode::FP16);
        assert!(sel.degraded(), "a skipped rung is a degradation");
        assert_eq!(sel.rejected.len(), 1);
        assert_eq!(sel.rejected[0].0, PrecisionMode::INT8);
    }

    #[test]
    fn anchored_fp32_keeps_the_doer_running_when_the_whole_list_fails() {
        // Operator configured int8-only; int8 won't build; the appended anchor
        // lands the selection on FP32 — availability-preserving, and both the
        // anchor and the rejection are visible.
        let ladder = PrecisionLadder::parse("int8").unwrap();
        let sel = select_by_ladder(&ladder, |p| match p {
            PrecisionMode::FP32 => Ok(p),
            _ => Err(fails("unavailable")),
        })
        .unwrap();
        assert_eq!(sel.precision, PrecisionMode::FP32);
        assert!(sel.degraded());
        assert!(sel.fp32_anchored);
    }

    #[test]
    fn exhausted_ladder_is_a_typed_error_naming_every_rung() {
        let ladder = PrecisionLadder::parse("int8,fp32").unwrap();
        let err = select_by_ladder::<()>(&ladder, |p| Err(fails(match p {
            PrecisionMode::INT8 => "engine build failed",
            _ => "model file missing",
        })))
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("INT8") && msg.contains("engine build failed"));
        assert!(msg.contains("FP32") && msg.contains("model file missing"));
    }
}
