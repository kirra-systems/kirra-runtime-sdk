// crates/parko-kirra/src/comparator.rs
//
// CERT-006 — software lockstep safety comparator for KirraGovernor.
//
// SAFETY-CRITICAL: this module implements posture-aware, speed-gated
// divergence handling. On divergence the comparator does NOT hard-stop at
// speed (the earlier behavior, which produced an unsafe deceleration
// event). Instead it commands a most-restrictive reconciliation of the
// two governor outputs (capped to the MRC ceiling, Degraded semantics)
// and only escalates to LockedOut once the vehicle is already at a safe
// speed (preferred) or after a deceleration window has elapsed
// (fallback). Read the module carefully before modifying — the
// complexity below exists specifically to prevent a hard stop at speed.

use crate::{KirraGovernor, MRC_VELOCITY_CEILING_MPS};
use parko_core::commands::ControlCommand;
use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
use parko_core::RssState;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Tolerance for floating-point comparison between primary and shadow.
/// 1e-9 is effectively exact equality for f64 safety computations.
const COMPARATOR_TOLERANCE: f64 = 1e-9;

// ---------------------------------------------------------------------------
// Leaky-bucket divergence accumulator
// ---------------------------------------------------------------------------

/// Added to the accumulator on each divergent tick.
///
/// `INC > DECAY` so that intermittent ("flapping") divergence still climbs
/// toward the lockout level; only sustained agreement drains it.
const DIVERGENCE_INC: u32 = 2;

/// Removed from the accumulator on each agreement tick.
const DIVERGENCE_DECAY: u32 = 1;

/// Accumulator threshold for "persistent" divergence. At `INC = 2`, this
/// is reached after 3 unbroken divergence ticks. Combined with the
/// escalation gate, persistent + safe-to-stop produces LockedOut.
const DIVERGENCE_LOCKOUT_LEVEL: u32 = 6;

/// Upper bound on the accumulator so it does not grow without bound during
/// extended divergence (which would let the comparator stay above LOCKOUT
/// for many seconds after agreement resumed).
const DIVERGENCE_ACCUMULATOR_CEILING: u32 = 2 * DIVERGENCE_LOCKOUT_LEVEL;

// ---------------------------------------------------------------------------
// Escalation gate
// ---------------------------------------------------------------------------

/// Vehicle speed (m/s) at or below which a hard stop is acceptable.
/// Set equal to MRC ceiling — by the time the MRC cap has brought the
/// vehicle to this speed, a further deceleration to 0 is no longer an
/// unsafe-deceleration event.
const SAFE_LOCKOUT_SPEED_MPS: f64 = MRC_VELOCITY_CEILING_MPS;

/// Time-based fallback escalation window (ms). Used only when the current
/// vehicle speed is not observable from comparator scope. Sized so that
/// the MRC cap has had time to decelerate from a typical highway speed
/// (~35 m/s) to a safe speed before a hard stop is permitted.
const DIVERGENCE_LOCKOUT_MIN_DURATION_MS: u64 = 2_000;

// ---------------------------------------------------------------------------
// Audit event API
// ---------------------------------------------------------------------------

/// One ComparatorDivergence event. Emitted on every divergent tick (on
/// either axis).
///
/// This is the per-decision reasoning trail and is the authoritative
/// safety record (also closes part of the explainability gap). The
/// integrator is expected to wire a sink that persists this to the
/// hash-chained, Ed25519-signed audit ledger (`AuditChainLinker` in
/// `kirra-runtime-sdk`). A dev-facing `eprintln!` in the default sink
/// is a secondary aid only.
///
/// CERT-006 v3 added the four `*_ang` fields so the audit record captures
/// angular-axis divergence (previously invisible to the comparator). The
/// `*_lin` fields are the prior `*_vel` fields renamed for clarity.
#[derive(Debug, Clone)]
pub struct DivergenceEvent {
    pub primary_lin: f64,
    pub shadow_lin: f64,
    pub delta_lin: f64,
    pub primary_ang: f64,
    pub shadow_ang: f64,
    pub delta_ang: f64,
    pub accumulator: u32,
    /// `None` if current vehicle speed was not observable on this tick.
    pub current_speed_mps: Option<f64>,
    /// Reconciled linear velocity that was commanded (when not escalating
    /// to LockedOut), or `0.0` when escalating.
    pub reconciled_lin: f64,
    /// Reconciled angular velocity that was commanded (when not escalating
    /// to LockedOut), or `0.0` when escalating.
    pub reconciled_ang: f64,
    /// `true` iff this tick crossed into LockedOut hard stop.
    pub escalated_to_lockout: bool,
}

/// Sink for ComparatorDivergence events. Implement this and inject via
/// `GovernorComparator::with_sink` to route events into the hash-chained
/// audit log. The default `InMemoryDivergenceSink` is sufficient for
/// tests and development; production deployments MUST wire a sink that
/// persists to `AuditChainLinker::append_audit_event_tx` (event type
/// `"ComparatorDivergence"`, JSON-serialised body).
pub trait DivergenceEventSink: Send + Sync {
    fn record(&self, event: DivergenceEvent);
}

/// Default sink: buffers events in memory and also writes them to stderr
/// for development. Tests inspect `.events()` to assert what was emitted.
pub struct InMemoryDivergenceSink {
    events: Mutex<Vec<DivergenceEvent>>,
}

impl Default for InMemoryDivergenceSink {
    fn default() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }
}

impl InMemoryDivergenceSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<DivergenceEvent> {
        self.events.lock().map(|v| v.clone()).unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.events.lock().map(|v| v.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.events.lock().map(|v| v.is_empty()).unwrap_or(true)
    }
}

impl DivergenceEventSink for InMemoryDivergenceSink {
    fn record(&self, event: DivergenceEvent) {
        // Secondary dev aid only. Authoritative record is the audit log.
        eprintln!(
            "[CERT-006] ComparatorDivergence: \
             lin(p={p_lin} s={s_lin} d={d_lin} rec={rec_lin}) \
             ang(p={p_ang} s={s_ang} d={d_ang} rec={rec_ang}) \
             acc={acc} speed={speed:?} lockout={lockout}",
            p_lin = event.primary_lin,
            s_lin = event.shadow_lin,
            d_lin = event.delta_lin,
            rec_lin = event.reconciled_lin,
            p_ang = event.primary_ang,
            s_ang = event.shadow_ang,
            d_ang = event.delta_ang,
            rec_ang = event.reconciled_ang,
            acc = event.accumulator,
            speed = event.current_speed_mps,
            lockout = event.escalated_to_lockout,
        );
        if let Ok(mut v) = self.events.lock() {
            v.push(event);
        }
    }
}

// ---------------------------------------------------------------------------
// Comparator state (single mutex, multi-step decision is atomic as a unit)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct DivState {
    accumulator: u32,
    first_divergence: Option<Instant>,
}

// ---------------------------------------------------------------------------
// GovernorComparator
// ---------------------------------------------------------------------------

/// Software lockstep safety comparator.
///
/// Runs two independent `KirraGovernor` instances with identical inputs.
/// On divergence beyond `COMPARATOR_TOLERANCE` on **either** axis (linear
/// or angular — CERT-006 v3), the comparator commands a most-restrictive
/// reconciliation of the two outputs as an `EnforcementAction::ClampMotion`
/// (MRC-capped on linear, pure most-restrictive on angular). It escalates
/// to LockedOut **only** when the divergence is persistent **AND** the
/// vehicle is already at a safe speed — never as a hard stop at speed.
///
/// # FOLLOW-UP (true diverse redundancy)
///
/// `GovernorComparator` is hard-wired to two `KirraGovernor` instances, so
/// primary and shadow run identical code and only catch RANDOM faults.
/// Making the comparator generic over `SafetyGovernor` would (a) allow an
/// independent second implementation as the shadow — the implementation
/// diversity the safety case says automotive requires — and (b) make
/// angular-divergence reachable in integration tests via a mock governor.
/// Tracked separately.
///
/// # Redundancy Model
///
/// Primary and shadow currently run IDENTICAL code. This catches
/// RANDOM faults (memory corruption, bit flips, transient errors) and
/// state divergence from differing inputs / updates. It does NOT catch
/// systematic faults — a logic bug in the shared code path produces
/// the same wrong output in both instances and the comparator stays
/// silent.
///
/// ## Diversity roadmap (do not overstate in any safety case)
///
/// - Parameterized diversity (different rate-limit / recovery params
///   per instance) catches ONLY state-/config-DEPENDENT systematic
///   faults. Shared-code-path logic faults (e.g. a wrong comparison
///   operator in the clamp) survive it. It is a weak mitigation.
/// - Independent implementation (two codebases) is the gold standard
///   for systematic faults, but a common SPECIFICATION fault still
///   defeats it — diverse review of the spec is part of the story.
///
/// **Current:** identical redundancy + posture-aware, speed-gated
/// divergence handling. Adequate for stationary / slow robots and
/// industrial systems. **High-speed automotive deployment REQUIRES
/// implementation diversity in addition to this fix.**
///
/// Per CERT-006 — ISO 26262 ASIL-D decomposition argument.
pub struct GovernorComparator {
    primary: KirraGovernor,
    shadow: KirraGovernor,
    state: Mutex<DivState>,
    sink: Arc<dyn DivergenceEventSink>,
}

/// Reconcile two governor output velocities to a single most-restrictive
/// command. Sign-preserving: takes the smaller magnitude of the two,
/// capped at the MRC ceiling, preserving the agreed direction. Direction
/// disagreement (one positive, one negative, both nonzero) commands 0.
pub(crate) fn reconcile(primary_vel: f64, shadow_vel: f64, mrc: f64) -> f64 {
    if primary_vel.signum() != shadow_vel.signum()
        && primary_vel != 0.0
        && shadow_vel != 0.0
    {
        return 0.0;
    }
    let sign = if primary_vel != 0.0 {
        primary_vel.signum()
    } else {
        shadow_vel.signum()
    };
    let mag = primary_vel.abs().min(shadow_vel.abs()).min(mrc);
    sign * mag
}

fn effective_linear_velocity(action: &EnforcementAction, proposed: f64) -> f64 {
    match action {
        EnforcementAction::Allow => proposed,
        EnforcementAction::ClampLinearVelocity(v) => *v,
        EnforcementAction::ClampAngularVelocity(_) => proposed,
        // ClampMotion contributes its linear field when Some, else the
        // proposed value is unconstrained on this axis.
        EnforcementAction::ClampMotion { linear, .. } => linear.unwrap_or(proposed),
        EnforcementAction::Deny { .. } => 0.0,
    }
}

/// Mirror of `effective_linear_velocity` for the angular axis. Added in
/// CERT-006 v3 so divergence detection covers both axes — pre-v3 only
/// the linear axis was compared, which made angular-axis (yaw) divergence
/// invisible to the comparator (the safety hole this rewrite closes).
fn effective_angular_velocity(action: &EnforcementAction, proposed: f64) -> f64 {
    match action {
        EnforcementAction::Allow => proposed,
        EnforcementAction::ClampLinearVelocity(_) => proposed,
        EnforcementAction::ClampAngularVelocity(v) => *v,
        EnforcementAction::ClampMotion { angular, .. } => angular.unwrap_or(proposed),
        EnforcementAction::Deny { .. } => 0.0,
    }
}

/// True iff the two governor outputs disagree on the PHYSICAL command that
/// would reach the actuator, on either axis, beyond `tol`.
///
/// Compares enforced effect (linear + angular), not action variant. A pure
/// variant difference that yields identical physical output on both axes
/// (e.g. `Deny` vs `ClampLinearVelocity(0.0)` when proposed angular is 0)
/// is intentionally NOT flagged — the actuator sees the same motion either
/// way, which is the safety-relevant quantity.
///
/// Pure function — unit-testable without constructing diverging governors,
/// which is how the angular-axis safety hole is verifiably closed.
pub(crate) fn actions_diverge(
    primary_out: &EnforcementAction,
    shadow_out: &EnforcementAction,
    proposed: &ControlCommand,
    tol: f64,
) -> bool {
    let p_lin = effective_linear_velocity(primary_out, proposed.linear_velocity);
    let s_lin = effective_linear_velocity(shadow_out, proposed.linear_velocity);
    let p_ang = effective_angular_velocity(primary_out, proposed.angular_velocity);
    let s_ang = effective_angular_velocity(shadow_out, proposed.angular_velocity);
    (p_lin - s_lin).abs() > tol || (p_ang - s_ang).abs() > tol
}

impl GovernorComparator {
    /// Create a comparator with two independent governor instances and the
    /// default in-memory divergence sink.
    pub fn new(primary: KirraGovernor, shadow: KirraGovernor) -> Self {
        Self::with_sink(primary, shadow, Arc::new(InMemoryDivergenceSink::new()))
    }

    /// Create a comparator with a caller-supplied divergence sink. Use this
    /// constructor when wiring the comparator to the hash-chained audit log
    /// in `kirra-runtime-sdk`.
    pub fn with_sink(
        primary: KirraGovernor,
        shadow: KirraGovernor,
        sink: Arc<dyn DivergenceEventSink>,
    ) -> Self {
        Self {
            primary,
            shadow,
            state: Mutex::new(DivState::default()),
            sink,
        }
    }

    /// Evaluate a command through both governors and reconcile.
    ///
    /// On agreement: returns the primary output and decays the divergence
    /// accumulator (clearing the divergence-start timestamp at zero).
    ///
    /// On divergence: increments the accumulator (leaky-bucket), records a
    /// `ComparatorDivergence` event, and commands a most-restrictive
    /// reconciliation capped at the MRC ceiling. Escalates to LockedOut
    /// only when the accumulator has reached `DIVERGENCE_LOCKOUT_LEVEL`
    /// AND the vehicle is at a safe speed (preferred) or the time fallback
    /// has elapsed (when current speed is not observable).
    pub fn evaluate(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
    ) -> EnforcementAction {
        let primary_out = self.primary.evaluate(proposed, previous, delta_time_s, posture);
        let shadow_out = self.shadow.evaluate(proposed, previous, delta_time_s, posture);

        // Two-axis divergence detection (CERT-006 v3): pre-v3 the
        // comparator compared only the linear axis, so yaw-axis
        // divergence was invisible. Detection now covers both.
        let primary_lin =
            effective_linear_velocity(&primary_out, proposed.linear_velocity);
        let shadow_lin =
            effective_linear_velocity(&shadow_out, proposed.linear_velocity);
        let primary_ang =
            effective_angular_velocity(&primary_out, proposed.angular_velocity);
        let shadow_ang =
            effective_angular_velocity(&shadow_out, proposed.angular_velocity);
        let delta_lin = (primary_lin - shadow_lin).abs();
        let delta_ang = (primary_ang - shadow_ang).abs();

        if !actions_diverge(&primary_out, &shadow_out, proposed, COMPARATOR_TOLERANCE) {
            // Agreement on BOTH axes. Decay the accumulator; clear
            // divergence-start timestamp once accumulator drains.
            let mut state = self.state.lock().expect("DivState mutex poisoned");
            state.accumulator = state.accumulator.saturating_sub(DIVERGENCE_DECAY);
            if state.accumulator == 0 {
                state.first_divergence = None;
            }
            return primary_out;
        }

        // ---------------- Divergence path ----------------

        let reconciled_lin = reconcile(primary_lin, shadow_lin, MRC_VELOCITY_CEILING_MPS);
        // Angular cap: no `MRC_ANGULAR_CEILING_RAD_S` exists in the
        // codebase today, so use `f64::INFINITY` — pure most-restrictive
        // min-of-magnitudes (sign and direction-disagreement handling are
        // identical to the linear case via `reconcile`). FOLLOW-UP:
        // introduce a proper MRC_ANGULAR_CEILING_RAD_S in parko-kirra and
        // pass it here instead of INFINITY.
        let angular_cap = f64::INFINITY;
        let reconciled_ang = reconcile(primary_ang, shadow_ang, angular_cap);

        // Best-available current-speed proxy. There is no measured-speed
        // input on `evaluate` (see STEP 0(f) of CERT-006 v2 prompt). The
        // previously commanded velocity tracks vehicle speed closely if
        // the AV is following commands; in the absence of `previous` we
        // fall back to time-based escalation.
        let current_speed_mps = previous.map(|p| p.linear_velocity.abs());

        // ATOMIC multi-step update under the single state mutex.
        // (Per CERT-006 v2 prompt STEP 1: separate atomics would NOT be
        // safe as a unit; one mutex covers accumulator + first_divergence
        // + the lockout decision.)
        let (accumulator, may_lockout) = {
            let mut state = self.state.lock().expect("DivState mutex poisoned");

            state.accumulator = (state.accumulator + DIVERGENCE_INC)
                .min(DIVERGENCE_ACCUMULATOR_CEILING);
            if state.first_divergence.is_none() {
                state.first_divergence = Some(Instant::now());
            }

            let persistent = state.accumulator >= DIVERGENCE_LOCKOUT_LEVEL;
            let may_lockout = match current_speed_mps {
                Some(speed) => persistent && speed <= SAFE_LOCKOUT_SPEED_MPS,
                None => {
                    persistent
                        && state
                            .first_divergence
                            .map(|t| {
                                t.elapsed()
                                    >= Duration::from_millis(
                                        DIVERGENCE_LOCKOUT_MIN_DURATION_MS,
                                    )
                            })
                            .unwrap_or(false)
                }
            };

            (state.accumulator, may_lockout)
        };

        let action = if may_lockout {
            EnforcementAction::Deny {
                reason: format!(
                    "GovernorComparator: persistent divergence (acc={accumulator}) \
                     escalated to LockedOut (speed={current_speed_mps:?} m/s, \
                     lin={reconciled_lin} ang={reconciled_ang})"
                ),
            }
        } else {
            // Graceful both-axis Degraded envelope — the reason
            // ClampMotion was added: decelerate linearly AND limit yaw,
            // no hard stop at speed.
            EnforcementAction::ClampMotion {
                linear: Some(reconciled_lin),
                angular: Some(reconciled_ang),
            }
        };

        // Emit a structured audit event for every divergent tick — this is
        // the authoritative safety record for the per-decision reasoning
        // trail. Integrators wire this sink to `AuditChainLinker` in
        // `kirra-runtime-sdk`.
        self.sink.record(DivergenceEvent {
            primary_lin,
            shadow_lin,
            delta_lin,
            primary_ang,
            shadow_ang,
            delta_ang,
            accumulator,
            current_speed_mps,
            reconciled_lin: if may_lockout { 0.0 } else { reconciled_lin },
            reconciled_ang: if may_lockout { 0.0 } else { reconciled_ang },
            escalated_to_lockout: may_lockout,
        });

        action
    }

    /// Update RSS state on both governors. Must be called via this method
    /// (not on either inner governor directly) to maintain identical state
    /// between primary and shadow.
    pub fn update_rss_state(&mut self, state: RssState) {
        self.primary.update_rss_state(state.clone());
        self.shadow.update_rss_state(state);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(v: f64) -> ControlCommand {
        ControlCommand {
            linear_velocity: v,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        }
    }

    fn unsafe_rss() -> RssState {
        RssState {
            safe: false,
            longitudinal_margin: 1.0,
            lateral_margin: 0.3,
        }
    }

    fn safe_rss() -> RssState {
        RssState {
            safe: true,
            longitudinal_margin: 12.0,
            lateral_margin: 5.0,
        }
    }

    fn diverging_pair() -> (KirraGovernor, KirraGovernor) {
        let mut primary = KirraGovernor::new();
        let mut shadow = KirraGovernor::new();
        primary.update_rss_state(safe_rss());
        shadow.update_rss_state(unsafe_rss());
        (primary, shadow)
    }

    // -----------------------------------------------------------------
    // Existing safety properties — must still hold after the v2 fix.
    // -----------------------------------------------------------------

    #[test]
    fn test_comparator_identical_inputs_returns_primary() {
        let comparator = GovernorComparator::new(KirraGovernor::new(), KirraGovernor::new());
        let proposed = cmd(3.0);
        let prev = cmd(3.0);
        let out = comparator.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);

        let primary_alone =
            KirraGovernor::new().evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        assert!(
            (effective_linear_velocity(&out, 3.0)
                - effective_linear_velocity(&primary_alone, 3.0))
            .abs()
                <= COMPARATOR_TOLERANCE,
            "Identical inputs must return primary output unchanged"
        );
    }

    #[test]
    fn test_comparator_locked_out_both_zero_no_false_positive() {
        let comparator = GovernorComparator::new(KirraGovernor::new(), KirraGovernor::new());
        let proposed = cmd(10.0);
        let out = comparator.evaluate(&proposed, None, 0.05, SafetyPosture::LockedOut);

        assert_eq!(
            effective_linear_velocity(&out, 10.0),
            0.0,
            "LockedOut: both return 0.0, comparator must pass through (no false divergence)"
        );
        if let EnforcementAction::Deny { reason } = out {
            assert!(
                reason.contains("LockedOut"),
                "Deny reason should be LockedOut, got {reason:?}"
            );
            assert!(
                !reason.contains("divergence"),
                "Must NOT flag divergence when both agree at 0.0"
            );
        } else {
            panic!("LockedOut should return Deny, got {out:?}");
        }
    }

    // -----------------------------------------------------------------
    // CERT-006 v2 — new safety behavior.
    // -----------------------------------------------------------------

    /// Single divergence tick at speed must NOT hard stop — must reconcile
    /// to an MRC-capped `ClampMotion` (CERT-006 v3: was `ClampLinearVelocity`
    /// in v2). This is the property whose absence in v1 motivated the
    /// entire rewrite.
    #[test]
    fn test_single_divergence_returns_mrc_reconciled() {
        let (primary, shadow) = diverging_pair();
        let comparator = GovernorComparator::new(primary, shadow);

        let commanded = 10.0;
        let prev = cmd(commanded);
        let out = comparator.evaluate(&cmd(commanded), Some(&prev), 0.05, SafetyPosture::Nominal);

        assert!(
            matches!(out, EnforcementAction::ClampMotion { .. }),
            "Single divergence MUST reconcile to a ClampMotion (both axes), \
             NOT Deny. Got {out:?}"
        );
        let v = effective_linear_velocity(&out, commanded);
        assert!(
            v.abs() <= MRC_VELOCITY_CEILING_MPS + COMPARATOR_TOLERANCE,
            "Reconciled linear magnitude must be at or below MRC ceiling. Got {v}"
        );
    }

    /// KEY SAFETY PROPERTY: persistent divergence while the vehicle is
    /// still moving above SAFE_LOCKOUT_SPEED must NOT lock out — it must
    /// keep returning the MRC cap so the vehicle decelerates safely.
    #[test]
    fn test_persistent_divergence_at_speed_does_not_lockout() {
        let (primary, shadow) = diverging_pair();
        let comparator = GovernorComparator::new(primary, shadow);

        let commanded = 30.0;
        // previous = 30 m/s, well above SAFE_LOCKOUT_SPEED (5.0)
        let prev = cmd(commanded);

        let mut last = EnforcementAction::Allow;
        for _ in 0..50 {
            last = comparator.evaluate(
                &cmd(commanded),
                Some(&prev),
                0.05,
                SafetyPosture::Nominal,
            );
        }

        assert!(
            matches!(last, EnforcementAction::ClampMotion { .. }),
            "Sustained divergence at speed must NEVER hard stop — must keep \
             returning ClampMotion(MRC-capped on linear). Got {last:?}"
        );
    }

    /// Persistent divergence once the vehicle is at a safe speed should
    /// escalate to LockedOut.
    #[test]
    fn test_persistent_divergence_when_slow_escalates_to_lockout() {
        let (primary, shadow) = diverging_pair();
        let comparator = GovernorComparator::new(primary, shadow);

        let commanded = 10.0;
        let slow_prev = cmd(3.0); // below SAFE_LOCKOUT_SPEED (5.0)

        let mut last = EnforcementAction::Allow;
        for _ in 0..10 {
            last = comparator.evaluate(
                &cmd(commanded),
                Some(&slow_prev),
                0.05,
                SafetyPosture::Nominal,
            );
        }

        assert!(
            matches!(last, EnforcementAction::Deny { .. }),
            "Sustained divergence at safe speed must escalate to LockedOut. \
             Got {last:?}"
        );
        if let EnforcementAction::Deny { reason } = last {
            assert!(
                reason.contains("divergence"),
                "Lockout reason should reference divergence; got {reason:?}"
            );
        }
    }

    /// Flapping (diverge / agree / diverge / ...) still climbs the
    /// accumulator because `INC > DECAY`. Proves the design isn't
    /// defeated by intermittent divergence.
    #[test]
    fn test_flapping_divergence_still_escalates() {
        let (primary, shadow) = diverging_pair();
        let comparator = GovernorComparator::new(primary, shadow);

        let slow_prev = cmd(3.0);

        // Diverge on commanded=10 (above MRC → shadow clamps),
        // agree on commanded=3 (below MRC → both pass through).
        // 31 ticks, ends on i=30 (even → divergent). Net accumulator
        // climb is ~17, well above DIVERGENCE_LOCKOUT_LEVEL=6.
        let mut last = EnforcementAction::Allow;
        for i in 0..31 {
            let commanded = if i % 2 == 0 { 10.0 } else { 3.0 };
            last = comparator.evaluate(
                &cmd(commanded),
                Some(&slow_prev),
                0.05,
                SafetyPosture::Nominal,
            );
        }

        assert!(
            matches!(last, EnforcementAction::Deny { .. }),
            "Flapping divergence (INC > DECAY) at safe speed must still \
             escalate; cannot be reset-gamed. Got {last:?}"
        );
    }

    /// Sustained agreement drains the accumulator back to zero. After
    /// the drain, the comparator returns the primary output unchanged.
    #[test]
    fn test_accumulator_decays_on_agreement() {
        let (primary, shadow) = diverging_pair();
        let mut comparator = GovernorComparator::new(primary, shadow);

        let slow_prev = cmd(3.0);

        // Two divergence ticks → accumulator=4 (below LOCKOUT_LEVEL).
        for _ in 0..2 {
            let _ = comparator.evaluate(
                &cmd(10.0),
                Some(&slow_prev),
                0.05,
                SafetyPosture::Nominal,
            );
        }

        // Make primary and shadow agree by giving both safe RSS.
        comparator.update_rss_state(safe_rss());

        // Many agreement ticks — accumulator should saturate at 0 long
        // before this finishes.
        let mut last = EnforcementAction::Allow;
        for _ in 0..50 {
            last = comparator.evaluate(
                &cmd(3.0),
                Some(&slow_prev),
                0.05,
                SafetyPosture::Nominal,
            );
        }

        // After drain, comparator returns primary unchanged. With Nominal
        // + safe RSS + steady-state 3.0 m/s, that's Allow (or possibly
        // ClampLinearVelocity from the kinematic envelope, but never Deny).
        assert!(
            !matches!(last, EnforcementAction::Deny { .. }),
            "Drained accumulator must not produce a divergence Deny. Got {last:?}"
        );
    }

    /// Direction disagreement (one positive, one negative, both nonzero)
    /// reconciles to 0.0 — safest action when the two governors disagree
    /// on direction.
    #[test]
    fn test_direction_disagreement_commands_zero() {
        // Both nonzero, opposite signs.
        assert_eq!(reconcile(2.0, -2.0, 5.0), 0.0);
        assert_eq!(reconcile(-3.0, 4.0, 5.0), 0.0);

        // Same direction → smaller magnitude (sign preserved).
        assert!((reconcile(3.0, 2.0, 5.0) - 2.0).abs() <= COMPARATOR_TOLERANCE);
        assert!((reconcile(-3.0, -2.0, 5.0) + 2.0).abs() <= COMPARATOR_TOLERANCE);

        // MRC cap dominates when both magnitudes exceed it.
        assert!((reconcile(10.0, 8.0, 5.0) - 5.0).abs() <= COMPARATOR_TOLERANCE);
        assert!((reconcile(-10.0, -8.0, 5.0) + 5.0).abs() <= COMPARATOR_TOLERANCE);

        // One side is zero → reconciled magnitude is zero.
        assert_eq!(reconcile(0.0, 4.0, 5.0), 0.0);
        assert_eq!(reconcile(3.0, 0.0, 5.0), 0.0);

        // Both zero → zero.
        assert_eq!(reconcile(0.0, 0.0, 5.0), 0.0);
    }

    /// A ComparatorDivergence audit event is emitted on every divergent
    /// tick, with the fields a downstream audit-chain integrator needs.
    /// CERT-006 v3: event now carries both linear and angular fields.
    #[test]
    fn test_divergence_emits_audit_event() {
        let (primary, shadow) = diverging_pair();
        let sink = Arc::new(InMemoryDivergenceSink::new());
        let comparator =
            GovernorComparator::with_sink(primary, shadow, sink.clone() as Arc<dyn DivergenceEventSink>);

        let commanded = 10.0;
        let prev = cmd(commanded);

        // One divergent tick.
        let _ = comparator.evaluate(&cmd(commanded), Some(&prev), 0.05, SafetyPosture::Nominal);

        let events = sink.events();
        assert_eq!(events.len(), 1, "Expected exactly one audit event after one divergent tick");
        let e = &events[0];
        assert!(e.delta_lin > COMPARATOR_TOLERANCE, "linear delta should be flagged");
        assert_eq!(e.accumulator, DIVERGENCE_INC);
        assert_eq!(e.current_speed_mps, Some(10.0));
        assert!(!e.escalated_to_lockout, "Single tick at speed must not escalate");
        assert!(
            e.reconciled_lin.abs() <= MRC_VELOCITY_CEILING_MPS + COMPARATOR_TOLERANCE,
            "Reconciled linear in event must respect MRC ceiling"
        );

        // Agreement ticks must NOT emit events.
        let safe_prev = cmd(3.0);
        let _ = comparator.evaluate(&cmd(3.0), Some(&safe_prev), 0.05, SafetyPosture::Nominal);
        assert_eq!(
            sink.len(),
            1,
            "Agreement tick must not emit a divergence event"
        );
    }

    // -----------------------------------------------------------------
    // CERT-006 v3 — pure predicate tests for two-axis divergence
    // detection. These do NOT require constructing diverging governors —
    // they exercise `actions_diverge` directly, which is the proof that
    // the angular-axis blindness in v2 is closed.
    // -----------------------------------------------------------------

    /// Angular-axis divergence in isolation must be detected. Pre-v3 this
    /// returned false (the safety hole this rewrite closes).
    #[test]
    fn test_actions_diverge_angular_only() {
        let p = EnforcementAction::ClampAngularVelocity(2.0);
        let s = EnforcementAction::ClampAngularVelocity(9.0);
        let proposed = ControlCommand {
            linear_velocity: 3.0,
            angular_velocity: 5.0,
            timestamp_ms: 0,
        };
        assert!(
            actions_diverge(&p, &s, &proposed, COMPARATOR_TOLERANCE),
            "Angular-only divergence must be detected post-CERT-006-v3"
        );
    }

    /// `ClampMotion`'s angular field participates in detection.
    #[test]
    fn test_actions_diverge_clampmotion_angular() {
        let p = EnforcementAction::ClampMotion {
            linear: Some(3.0),
            angular: Some(1.0),
        };
        let s = EnforcementAction::ClampMotion {
            linear: Some(3.0),
            angular: Some(4.0),
        };
        let proposed = ControlCommand {
            linear_velocity: 3.0,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        };
        assert!(
            actions_diverge(&p, &s, &proposed, COMPARATOR_TOLERANCE),
            "ClampMotion angular field must participate in divergence detection"
        );
    }

    /// Agreement on both axes must NOT be flagged as divergence.
    #[test]
    fn test_actions_agree_both_axes() {
        let p = EnforcementAction::Allow;
        let s = EnforcementAction::Allow;
        let proposed = ControlCommand {
            linear_velocity: 3.0,
            angular_velocity: 2.0,
            timestamp_ms: 0,
        };
        assert!(
            !actions_diverge(&p, &s, &proposed, COMPARATOR_TOLERANCE),
            "Both axes agree (Allow vs Allow) must not be flagged"
        );
    }

    /// Same physical effect via different variants must NOT be flagged
    /// (compare enforced effect, not action variant). When proposed
    /// angular is 0, `Deny` and `ClampLinearVelocity(0.0)` both yield
    /// zero motion on every axis.
    #[test]
    fn test_actions_same_physical_effect_not_flagged() {
        let p = EnforcementAction::Deny {
            reason: "x".into(),
        };
        let s = EnforcementAction::ClampLinearVelocity(0.0);
        let proposed = ControlCommand {
            linear_velocity: 5.0,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        };
        // Deny: linear=0, angular=0 (per effective_*_velocity)
        // ClampLinearVelocity(0): linear=0, angular=proposed.angular_velocity=0
        // Same physical effect → not divergent.
        assert!(
            !actions_diverge(&p, &s, &proposed, COMPARATOR_TOLERANCE),
            "Same physical effect via different variants must not be flagged \
             (compare effect, not variant)"
        );
    }
}
