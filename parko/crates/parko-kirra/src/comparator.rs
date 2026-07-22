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

use crate::diverse::DiverseKirraGovernor;
use crate::{KirraGovernor, MRC_VELOCITY_CEILING_MPS};
use parko_core::commands::ControlCommand;
use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
use parko_core::RssState;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// RSS-aware governor capability
// ---------------------------------------------------------------------------

/// A [`SafetyGovernor`] whose RSS safe-distance state can be updated.
///
/// `SafetyGovernor::evaluate` is stateless w.r.t. RSS, but the Kirra
/// governors hold an RSS gate that the control loop refreshes each cycle.
/// The comparator is generic over the shadow governor (CERT-006 diversity),
/// so it needs a trait to push RSS state into whatever shadow is wired —
/// the primary `KirraGovernor` or the `DiverseKirraGovernor`.
///
/// The method is named `set_rss_state` (not `update_rss_state`) to avoid
/// shadowing the governors' existing inherent `update_rss_state` methods.
pub trait RssAwareGovernor: SafetyGovernor {
    fn set_rss_state(&mut self, state: RssState);
}

impl RssAwareGovernor for KirraGovernor {
    fn set_rss_state(&mut self, state: RssState) {
        self.update_rss_state(state);
    }
}

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
#[derive(Debug, Clone, serde::Serialize)]
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
    /// The fleet posture this divergence recommends — `"degraded"` on a divergent tick,
    /// `"locked_out"` once the escalation fires. The audit form of
    /// [`GovernorComparator::recommended_posture`], so the posture signal and its
    /// justification are one record.
    pub recommended_posture: &'static str,
}

/// Sink for ComparatorDivergence events. Implement this and inject via
/// `GovernorComparator::with_sink` to route events into the hash-chained
/// audit log. The default `InMemoryDivergenceSink` is sufficient for
/// tests and development; production deployments MUST wire a sink that
/// persists to `AuditChainLinker::append_audit_event_tx` (event type
/// `"ComparatorDivergence"`, JSON-serialised body).
/// S-DG1 (STAGE_S-DG1_DIVERGENCE_POSTURE.md) — the OPTIONAL fleet-posture
/// signal seam, dependency-injected exactly like [`DivergenceEventSink`].
///
/// Called once per [`GovernorComparator::evaluate`] tick with the comparator's
/// OWN posture-relevant state — reused, never reimplemented:
///   - `significant` — this tick's divergence state is posture-relevant:
///     `true` on every divergent tick AND on agreement ticks while the
///     leaky-bucket accumulator is still draining (the comparator's existing
///     Degraded-recommendation hysteresis); `false` only once fully drained
///     (an agreeing, recovered tick — the integrator's recovery streak feeds
///     on these).
///   - `escalated` — the comparator's own sustained-divergence escalation
///     (`escalated_to_lockout`) fired this tick.
///
/// Integrators wire this to the verifier's posture engine (see the
/// `verifier-sink` feature's `PostureEngineSenderSink`); `None` (the default)
/// is byte-for-byte today's audit-only behavior. Implementations MUST be
/// non-blocking: this is called on the per-command evaluate path.
pub trait PostureSignalSink: Send + Sync {
    fn divergence_posture_tick(&self, significant: bool, escalated: bool);
}

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
             acc={acc} speed={speed:?} lockout={lockout} posture={posture}",
            posture = event.recommended_posture,
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

struct DivState {
    accumulator: u32,
    first_divergence: Option<Instant>,
    /// Posture the divergence state recommends to the fleet — derived from the
    /// accumulator each tick (see [`GovernorComparator::recommended_posture`]). A
    /// disagreement between the independent governors is a fault SIGNATURE, not just an
    /// audit line: it drives the system to `Degraded` (and `LockedOut` once persistent),
    /// with hysteresis (it stays `Degraded` while the accumulator drains).
    recommended_posture: SafetyPosture,
    /// WS-0.4 F5 — the PEAK best-available speed proxy observed since the current
    /// divergence episode began, m/s. The angular rollover cap is evaluated here,
    /// not at the reconciled (≤ 5 m/s) command. `previous` is the last *issued*
    /// command, so once the comparator clamps the linear axis to ~5 the naive
    /// per-tick proxy collapses to 5 while the vehicle is still physically at
    /// speed (Copilot #781) — but a decaying estimate is UNSAFE (any decay faster
    /// than the vehicle's actual decel undershoots true speed → looser cap). With
    /// only past commands available, the sound choice is to HOLD the peak for the
    /// (bounded) lost-trust episode: it captures the pre-divergence speed on the
    /// first divergent tick and holds it until TRUST RETURNS — i.e. the governors
    /// agree and the accumulator drains to 0 (the agreement path resets the peak).
    /// A LockedOut escalation deliberately does NOT reset it: on a `may_lockout`
    /// tick the reconciled command is `Deny` (hard stop), so the cap is moot that
    /// tick; and in the post-lockout drain/hysteresis window the episode is STILL
    /// lost-trust, so persisting the peak keeps the yaw cap tight (more
    /// conservative) — resetting there would LOOSEN it, the wrong direction.
    /// Over-conservative on availability for a few ticks, which is the correct
    /// direction under lost trust. A true through-transient bound needs a
    /// MEASURED-speed input the comparator does not have (tracked: add
    /// `measured_speed_mps` to `evaluate`).
    episode_peak_speed_mps: f64,
}

impl Default for DivState {
    fn default() -> Self {
        Self {
            accumulator: 0,
            first_divergence: None,
            recommended_posture: SafetyPosture::Nominal,
            episode_peak_speed_mps: 0.0,
        }
    }
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
/// (MRC-capped on BOTH axes — WS-0.4: `MRC_VELOCITY_CEILING_MPS` on linear,
/// the SOTIF-derived MRC `ω_max(v)` on angular). It escalates
/// to LockedOut **only** when the divergence is persistent **AND** the
/// vehicle is already at a safe speed — never as a hard stop at speed.
///
/// # Redundancy Model (CERT-006)
///
/// The comparator is generic over the shadow governor `S`. The default,
/// and the production wiring, pairs the primary `KirraGovernor` with the
/// structurally diverse `DiverseKirraGovernor` (`S = DiverseKirraGovernor`).
///
/// - **Diverse redundancy (default).** Primary and shadow enforce the SAME
///   safety properties via DIFFERENT computation (see
///   `crate::diverse::DiverseKirraGovernor`). This catches RANDOM faults AND
///   the most common class of SYSTEMATIC faults: implementation-level logic
///   / numerical bugs, which are unlikely to manifest identically in two
///   structurally different code paths.
/// - **Identical redundancy (`S = KirraGovernor`).** Still constructible
///   (e.g. for tests). Catches RANDOM faults only — a logic bug in the
///   shared code path produces the same wrong output in both copies and the
///   comparator stays silent.
///
/// ## Honest limit (do not overstate in any safety case)
///
/// The diverse shadow shares the SPECIFICATION and the config/contract with
/// the primary. A SPEC-level fault (a wrong limit value, or a flaw in the
/// shared ω_max derivation) appears identically in both and is NOT caught.
/// Closing that requires diverse spec review and ultimately an N-version
/// clean-room reimplementation. See `docs/safety/COMPARATOR_DIVERSITY.md`
/// (DRAFT — pending safety-engineer review).
///
/// Per CERT-006 — ISO 26262 ASIL-D decomposition argument.
pub struct GovernorComparator<S: SafetyGovernor = DiverseKirraGovernor> {
    primary: KirraGovernor,
    shadow: S,
    state: Mutex<DivState>,
    sink: Arc<dyn DivergenceEventSink>,
    /// S-DG1 — optional fleet-posture signal seam. `None` = audit-only
    /// (today's behavior, byte-identical).
    posture_sink: Option<Arc<dyn PostureSignalSink>>,
}

/// Reconcile two governor output velocities to a single most-restrictive
/// command. Sign-preserving: takes the smaller magnitude of the two,
/// capped at the MRC ceiling, preserving the agreed direction. Direction
/// disagreement (one positive, one negative, both nonzero) commands 0.
pub(crate) fn reconcile(primary_vel: f64, shadow_vel: f64, mrc: f64) -> f64 {
    if primary_vel.signum() != shadow_vel.signum() && primary_vel != 0.0 && shadow_vel != 0.0 {
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

impl GovernorComparator<DiverseKirraGovernor> {
    /// #795 F11 — declare `ExternallyGated` on **both** arms at once.
    ///
    /// The scene-RSS declaration MUST match on the primary and the shadow: a
    /// one-sided `with_external_rss_gate` is a PERMANENT false divergence (one arm
    /// quiescent, the other still `NeverFed`). The per-arm builders warn about
    /// this in prose, but the symmetry was convention-only. This applies the
    /// declaration to both arms in one call, so it cannot drift. Production
    /// wiring (primary `KirraGovernor` + shadow `DiverseKirraGovernor`) should use
    /// this instead of gating each arm by hand.
    #[must_use]
    pub fn with_external_rss_gate(mut self) -> Self {
        self.primary = self.primary.with_external_rss_gate();
        self.shadow = self.shadow.with_external_rss_gate();
        self
    }

    /// #795 F11 — declare `OperatorWaived` on **both** arms at once (the waiver
    /// twin of [`with_external_rss_gate`](Self::with_external_rss_gate); same
    /// no-drift guarantee).
    #[must_use]
    pub fn with_operator_waived(mut self) -> Self {
        self.primary = self.primary.with_operator_waived();
        self.shadow = self.shadow.with_operator_waived();
        self
    }
}

impl<S: SafetyGovernor> GovernorComparator<S> {
    /// Create a comparator with a primary `KirraGovernor` and a shadow
    /// governor `S`, and the default in-memory divergence sink.
    ///
    /// Production wiring passes a `DiverseKirraGovernor` shadow (CERT-006
    /// implementation diversity); passing a second `KirraGovernor` yields the
    /// legacy identical-redundancy comparator.
    pub fn new(primary: KirraGovernor, shadow: S) -> Self {
        Self::with_sink(primary, shadow, Arc::new(InMemoryDivergenceSink::new()))
    }

    /// Create a comparator with a caller-supplied divergence sink. Use this
    /// constructor when wiring the comparator to the hash-chained audit log
    /// in `kirra-runtime-sdk`.
    pub fn with_sink(
        primary: KirraGovernor,
        shadow: S,
        sink: Arc<dyn DivergenceEventSink>,
    ) -> Self {
        Self {
            primary,
            shadow,
            state: Mutex::new(DivState::default()),
            sink,
            posture_sink: None,
        }
    }

    /// S-DG1c — attach the optional fleet-posture signal sink (builder-style;
    /// see [`PostureSignalSink`]). Without it the comparator is audit-only.
    #[must_use]
    pub fn with_posture_sink(mut self, posture_sink: Arc<dyn PostureSignalSink>) -> Self {
        self.posture_sink = Some(posture_sink);
        self
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
        let primary_out = self
            .primary
            .evaluate(proposed, previous, delta_time_s, posture);
        let shadow_out = self
            .shadow
            .evaluate(proposed, previous, delta_time_s, posture);

        // Two-axis divergence detection (CERT-006 v3): pre-v3 the
        // comparator compared only the linear axis, so yaw-axis
        // divergence was invisible. Detection now covers both.
        let primary_lin = effective_linear_velocity(&primary_out, proposed.linear_velocity);
        let shadow_lin = effective_linear_velocity(&shadow_out, proposed.linear_velocity);
        let primary_ang = effective_angular_velocity(&primary_out, proposed.angular_velocity);
        let shadow_ang = effective_angular_velocity(&shadow_out, proposed.angular_velocity);
        let delta_lin = (primary_lin - shadow_lin).abs();
        let delta_ang = (primary_ang - shadow_ang).abs();

        if !actions_diverge(&primary_out, &shadow_out, proposed, COMPARATOR_TOLERANCE) {
            // Agreement on BOTH axes. Decay the accumulator; clear
            // divergence-start timestamp once accumulator drains.
            let mut state = self.state.lock().expect("DivState mutex poisoned");
            state.accumulator = state.accumulator.saturating_sub(DIVERGENCE_DECAY);
            if state.accumulator == 0 {
                state.first_divergence = None;
                // WS-0.4 F5 — trust restored: end the episode and release the
                // held peak so the rollover cap recovers availability.
                state.episode_peak_speed_mps = 0.0;
            }
            // Posture recovers only once the accumulator has fully drained (hysteresis): a
            // single agreeing tick after a burst of divergence does NOT immediately clear the
            // Degraded recommendation.
            state.recommended_posture = if state.accumulator == 0 {
                SafetyPosture::Nominal
            } else {
                SafetyPosture::Degraded
            };
            // S-DG1b: an agreeing tick is posture-significant only while the
            // accumulator is still draining (the comparator's own hysteresis —
            // one source of truth); a fully-drained agreeing tick feeds the
            // integrator's recovery streak.
            let still_draining = state.accumulator > 0;
            drop(state);
            if let Some(ps) = &self.posture_sink {
                ps.divergence_posture_tick(still_draining, false);
            }
            return primary_out;
        }

        // ---------------- Divergence path ----------------

        let reconciled_lin = reconcile(primary_lin, shadow_lin, MRC_VELOCITY_CEILING_MPS);

        // Best-available current-speed proxy: the previously *issued* command
        // (there is no measured-speed input on `evaluate` — CERT-006 v2 STEP
        // 0(f)). Tracks vehicle speed while the AV follows commands; `None`
        // (no history) falls back to time-based escalation for the lockout gate.
        let current_speed_mps = previous.map(|p| p.linear_velocity.abs());

        // ATOMIC multi-step update under the single state mutex.
        // (Per CERT-006 v2 prompt STEP 1: separate atomics would NOT be
        // safe as a unit; one mutex covers accumulator + first_divergence
        // + the lockout decision + the WS-0.4 F5 episode peak.)
        let (accumulator, may_lockout, episode_peak_speed) = {
            let mut state = self.state.lock().expect("DivState mutex poisoned");

            state.accumulator =
                (state.accumulator + DIVERGENCE_INC).min(DIVERGENCE_ACCUMULATOR_CEILING);
            if state.first_divergence.is_none() {
                state.first_divergence = Some(Instant::now());
            }
            // WS-0.4 F5 — HOLD the peak speed proxy for the divergence episode.
            state.episode_peak_speed_mps = state
                .episode_peak_speed_mps
                .max(current_speed_mps.unwrap_or(0.0));

            let persistent = state.accumulator >= DIVERGENCE_LOCKOUT_LEVEL;
            let may_lockout = match current_speed_mps {
                Some(speed) => persistent && speed <= SAFE_LOCKOUT_SPEED_MPS,
                None => {
                    persistent
                        && state
                            .first_divergence
                            .map(|t| {
                                t.elapsed()
                                    >= Duration::from_millis(DIVERGENCE_LOCKOUT_MIN_DURATION_MS)
                            })
                            .unwrap_or(false)
                }
            };

            // The divergence drives the fleet posture, mirroring the reconciled action: a hard
            // escalation → LockedOut; any other divergent tick → Degraded. The integrator reads
            // `recommended_posture()` and feeds it to the fleet posture engine.
            state.recommended_posture = if may_lockout {
                SafetyPosture::LockedOut
            } else {
                SafetyPosture::Degraded
            };

            (state.accumulator, may_lockout, state.episode_peak_speed_mps)
        };

        // WS-0.4 (F5 fix) — cap the reconciled yaw at the SOTIF-derived MRC
        // angular ceiling `ω_max(v)` (#136) evaluated at the EPISODE PEAK speed
        // proxy, not the reconciled (≤ 5 m/s) command. `ω_max` is PROVEN
        // non-increasing in v (`angular_bound::prop_omega_max_is_non_increasing_
        // in_speed`), so the reconciled command was the permissive point of the
        // curve — during a divergence-at-speed transient (the ClampMotion path
        // has no stop-and-hold gate) that admitted up to ~3.4× the rollover-safe
        // yaw at 30 m/s. Binding at `max(reconciled, episode_peak)` is strictly
        // conservative by that property and holds through the (bounded) episode
        // rather than collapsing when the command clamps (see `DivState::
        // episode_peak_speed_mps`). Read from the PRIMARY arm (production wires
        // both arms with identical platform params — a mismatch would spuriously
        // diverge every tick anyway).
        let v_bound = reconciled_lin.abs().max(episode_peak_speed);
        let angular_cap = self.primary.mrc_omega_max(v_bound);
        let reconciled_ang = reconcile(primary_ang, shadow_ang, angular_cap);

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
            recommended_posture: if may_lockout {
                "locked_out"
            } else {
                "degraded"
            },
        });

        // S-DG1b: every divergent tick is posture-significant; `escalated`
        // mirrors the comparator's own sustained-divergence lockout decision.
        // Emission is AFTER the audit record so the asymmetry invariant holds:
        // every divergence is audited; posture is the additional consequence.
        if let Some(ps) = &self.posture_sink {
            ps.divergence_posture_tick(true, may_lockout);
        }

        action
    }

    /// The fleet posture the comparator's divergence state currently recommends — the wiring
    /// that turns governor disagreement from an audit line into a live safety signal. The
    /// integrator reads this after [`evaluate`](Self::evaluate) and drives the fleet posture
    /// engine with it (e.g. `posture = max(posture, comparator.recommended_posture())`).
    ///
    /// - `Nominal` while the governors agree and the divergence accumulator is drained;
    /// - `Degraded` on any divergence, held with hysteresis until the accumulator fully drains;
    /// - `LockedOut` once the divergence is persistent enough to escalate.
    #[must_use]
    pub fn recommended_posture(&self) -> SafetyPosture {
        self.state
            .lock()
            .map(|s| s.recommended_posture)
            .unwrap_or(SafetyPosture::LockedOut) // poisoned mutex → fail closed
    }
}

impl<S: RssAwareGovernor> GovernorComparator<S> {
    /// Update RSS state on both governors. Must be called via this method
    /// (not on either inner governor directly) to maintain identical state
    /// between primary and shadow.
    pub fn update_rss_state(&mut self, state: RssState) {
        self.primary.update_rss_state(state.clone());
        self.shadow.set_rss_state(state);
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

    /// #795 F11 — the comparator-level `with_external_rss_gate` declares the SAME
    /// RSS-feed state on BOTH arms in one call, so the symmetry (which the per-arm
    /// builders warn MUST hold, or it is a permanent false divergence) cannot
    /// drift. `with_operator_waived` is the waiver twin.
    #[test]
    fn comparator_gate_helpers_declare_both_arms() {
        let gated = GovernorComparator::new(KirraGovernor::new(), DiverseKirraGovernor::new())
            .with_external_rss_gate();
        assert_eq!(gated.primary.rss_feed_label(), "externally_gated");
        assert_eq!(gated.shadow.rss_feed_label(), "externally_gated");

        let waived = GovernorComparator::new(KirraGovernor::new(), DiverseKirraGovernor::new())
            .with_operator_waived();
        assert_eq!(waived.primary.rss_feed_label(), "operator_waived");
        assert_eq!(waived.shadow.rss_feed_label(), "operator_waived");
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
            (effective_linear_velocity(&out, 3.0) - effective_linear_velocity(&primary_alone, 3.0))
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

    /// WS-0.4 DoD — "angular divergence bounded": when both arms admit a
    /// large but DIFFERING yaw rate, the reconciled angular command is
    /// capped at the primary's MRC angular ceiling `ω_max(v)`, not merely
    /// the smaller of the two outputs. (Pre-WS-0.4 the cap was
    /// `f64::INFINITY` — pure min-of-magnitudes — so a 3-vs-2 rad/s
    /// divergence commanded a 2 rad/s yaw under lost trust.)
    #[test]
    fn angular_divergence_is_capped_at_the_mrc_angular_ceiling() {
        /// Shadow that agrees on the linear axis (passes the proposal
        /// through) but clamps yaw to a fixed large value — forcing an
        /// angular-only divergence with BOTH effective yaw rates far above
        /// the MRC angular ceiling.
        struct FixedAngularClamp(f64);
        impl parko_core::safety::SafetyGovernor for FixedAngularClamp {
            fn evaluate(
                &self,
                _proposed: &ControlCommand,
                _previous: Option<&ControlCommand>,
                _delta_time_s: f64,
                _posture: SafetyPosture,
            ) -> EnforcementAction {
                EnforcementAction::ClampAngularVelocity(self.0)
            }
        }

        // Scalar bounds make the numbers exact: Nominal ω ≤ 5.0 (admits the
        // 3.0 rad/s proposal), MRC ω ceiling 0.4.
        let mut primary = KirraGovernor::new().with_angular_bounds(5.0, 0.4);
        primary.update_rss_state(safe_rss());
        let comparator = GovernorComparator::new(primary, FixedAngularClamp(2.0));

        let proposed = ControlCommand {
            linear_velocity: 1.0,
            angular_velocity: 3.0,
            timestamp_ms: 0,
        };
        let prev = proposed.clone();
        let out = comparator.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);

        assert!(
            matches!(out, EnforcementAction::ClampMotion { .. }),
            "an angular-only divergence must reconcile to ClampMotion, got {out:?}"
        );
        let ang = effective_angular_velocity(&out, proposed.angular_velocity);
        assert!(
            ang.abs() <= 0.4 + COMPARATOR_TOLERANCE,
            "reconciled yaw must be capped at the MRC angular ceiling (0.4 rad/s); \
             got {ang} — min-of-magnitudes alone would have commanded 2.0"
        );
        assert!(ang > 0.0, "the cap must preserve the agreed yaw direction");
        // The agreed linear axis passes through the reconciliation unharmed.
        let lin = effective_linear_velocity(&out, proposed.linear_velocity);
        assert!(
            (lin - 1.0).abs() <= COMPARATOR_TOLERANCE,
            "the agreed linear axis must survive an angular-only divergence; got {lin}"
        );
    }

    /// WS-0.4 — with the default (derived) bounds, the reconciled yaw under
    /// divergence never exceeds `AngularVelocityBound::mrc(...)`'s ω_max at
    /// the reconciled linear velocity — pinning that the comparator uses the
    /// POSTURE-DERATED bound, not the looser Nominal one.
    #[test]
    fn angular_divergence_cap_uses_the_derated_mrc_bound() {
        struct FixedAngularClamp(f64);
        impl parko_core::safety::SafetyGovernor for FixedAngularClamp {
            fn evaluate(
                &self,
                _proposed: &ControlCommand,
                _previous: Option<&ControlCommand>,
                _delta_time_s: f64,
                _posture: SafetyPosture,
            ) -> EnforcementAction {
                EnforcementAction::ClampAngularVelocity(self.0)
            }
        }
        use crate::angular_bound::{AngularVelocityBound, PlatformParams};

        let mut primary = KirraGovernor::new(); // conservative-default derived bounds
        primary.update_rss_state(safe_rss());
        let comparator = GovernorComparator::new(primary, FixedAngularClamp(10.0));

        // In-place rotation request far above every bound.
        let proposed = ControlCommand {
            linear_velocity: 0.0,
            angular_velocity: 12.0,
            timestamp_ms: 0,
        };
        let prev = proposed.clone();
        let out = comparator.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);

        let ang = effective_angular_velocity(&out, proposed.angular_velocity);
        let mrc_ceiling =
            AngularVelocityBound::mrc(PlatformParams::conservative_default()).omega_max(0.0);
        assert!(
            ang.abs() <= mrc_ceiling + COMPARATOR_TOLERANCE,
            "reconciled yaw {ang} must be ≤ the derated MRC ω_max(0) = {mrc_ceiling}"
        );
    }

    /// WS-0.4 F5 — the angular cap must bind at the best-available SPEED PROXY
    /// (the last commanded velocity), not the reconciled COMMANDED linear
    /// velocity. On the first divergent tick at speed (previous = 30 m/s,
    /// reconciled linear pinned at the 5 m/s MRC ceiling) capping at the
    /// commanded 5 admitted ~3.4× the rollover-safe yaw; capping at 30 (strictly
    /// conservative by the proven non-increasing ω_max) is what the fix enforces.
    #[test]
    fn angular_cap_binds_at_speed_proxy_not_commanded_on_first_divergent_tick() {
        struct FixedAngularClamp(f64);
        impl parko_core::safety::SafetyGovernor for FixedAngularClamp {
            fn evaluate(
                &self,
                _proposed: &ControlCommand,
                _previous: Option<&ControlCommand>,
                _delta_time_s: f64,
                _posture: SafetyPosture,
            ) -> EnforcementAction {
                EnforcementAction::ClampAngularVelocity(self.0)
            }
        }
        use crate::angular_bound::{AngularVelocityBound, PlatformParams};

        let mut primary = KirraGovernor::new(); // conservative-default DERIVED bounds
        primary.update_rss_state(safe_rss());
        // Shadow clamps yaw to a fixed 0.5 rad/s — differs from the primary's
        // nominal clamp, so the angular axis diverges and the cap must bind.
        let comparator = GovernorComparator::new(primary, FixedAngularClamp(0.5));

        // A large yaw request, linear at the MRC ceiling; previous = 30 m/s is
        // the best-available speed proxy (the last commanded velocity, a stand-in
        // for the vehicle decelerating from highway — see `DivState::
        // episode_peak_speed_mps` for why the proxy is HELD across the episode).
        let proposed = ControlCommand {
            linear_velocity: 5.0,
            angular_velocity: 3.0,
            timestamp_ms: 0,
        };
        let prev = ControlCommand {
            linear_velocity: 30.0,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        };
        let out = comparator.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        assert!(
            matches!(out, EnforcementAction::ClampMotion { .. }),
            "expected a divergence ClampMotion, got {out:?}"
        );

        let ang = effective_angular_velocity(&out, proposed.angular_velocity).abs();
        let mrc = AngularVelocityBound::mrc(PlatformParams::conservative_default());
        let cap_at_proxy = mrc.omega_max(30.0);
        let cap_at_commanded = mrc.omega_max(5.0);
        // Sanity: the two caps genuinely differ (else the test proves nothing).
        assert!(cap_at_proxy < cap_at_commanded,
            "precondition: ω_max(30)={cap_at_proxy} must be tighter than ω_max(5)={cap_at_commanded}");
        assert!(
            ang <= cap_at_proxy + COMPARATOR_TOLERANCE,
            "reconciled yaw {ang} must be bounded by the rollover-safe ω_max at the speed proxy 30 m/s \
             ({cap_at_proxy}); the pre-fix code bounded at the commanded 5 m/s ({cap_at_commanded})"
        );
    }

    /// WS-0.4 F5 (episode-peak, Copilot #781 follow-up) — the speed proxy is the
    /// last *issued* command, so after the first divergent tick the reconciled
    /// command collapses to the ≤ 5 m/s MRC ceiling while the vehicle is still
    /// physically near 30 m/s. A naive "cap at last-commanded speed" would let the
    /// yaw cap loosen back to ω_max(5) on the very next tick — exactly the ~3.4×
    /// over-permit the fix removes. This drives a SECOND divergent tick whose
    /// `previous` has already collapsed to 5 m/s and asserts the reconciled yaw is
    /// STILL bound at the held episode peak ω_max(30), not ω_max(5).
    #[test]
    fn angular_cap_holds_episode_peak_after_command_collapses() {
        struct FixedAngularClamp(f64);
        impl parko_core::safety::SafetyGovernor for FixedAngularClamp {
            fn evaluate(
                &self,
                _proposed: &ControlCommand,
                _previous: Option<&ControlCommand>,
                _delta_time_s: f64,
                _posture: SafetyPosture,
            ) -> EnforcementAction {
                EnforcementAction::ClampAngularVelocity(self.0)
            }
        }
        use crate::angular_bound::{AngularVelocityBound, PlatformParams};

        let mut primary = KirraGovernor::new();
        primary.update_rss_state(safe_rss());
        let comparator = GovernorComparator::new(primary, FixedAngularClamp(0.5));

        let proposed = ControlCommand {
            linear_velocity: 5.0,
            angular_velocity: 3.0,
            timestamp_ms: 0,
        };

        // Tick 1: previous = 30 m/s establishes the episode peak.
        let prev_hi = ControlCommand {
            linear_velocity: 30.0,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        };
        let _ = comparator.evaluate(&proposed, Some(&prev_hi), 0.05, SafetyPosture::Nominal);

        // Tick 2: previous has already collapsed to the 5 m/s MRC command — a
        // naive last-commanded proxy would loosen the cap here. The held episode
        // peak must keep it bound at ω_max(30).
        let prev_lo = ControlCommand {
            linear_velocity: 5.0,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        };
        let out = comparator.evaluate(&proposed, Some(&prev_lo), 0.05, SafetyPosture::Nominal);
        assert!(
            matches!(out, EnforcementAction::ClampMotion { .. }),
            "expected a divergence ClampMotion, got {out:?}"
        );

        let ang = effective_angular_velocity(&out, proposed.angular_velocity).abs();
        let mrc = AngularVelocityBound::mrc(PlatformParams::conservative_default());
        let cap_at_peak = mrc.omega_max(30.0);
        let cap_at_collapsed = mrc.omega_max(5.0);
        assert!(cap_at_peak < cap_at_collapsed,
            "precondition: ω_max(30)={cap_at_peak} must be tighter than ω_max(5)={cap_at_collapsed}");
        assert!(
            ang <= cap_at_peak + COMPARATOR_TOLERANCE,
            "reconciled yaw {ang} must stay bound at the HELD episode peak ω_max(30)={cap_at_peak} \
             even after the command collapsed to 5 m/s; a last-commanded proxy would loosen to \
             ω_max(5)={cap_at_collapsed}"
        );
    }

    /// WS-0.4 F5 — the held episode peak must RESET once trust is restored (the
    /// accumulator drains to 0 on agreement), so a later, genuinely-slow
    /// divergence episode is not perpetually over-constrained by a stale peak.
    #[test]
    fn episode_peak_resets_after_agreement_drains_accumulator() {
        struct SwitchableClamp {
            diverge: Arc<std::sync::atomic::AtomicBool>,
        }
        impl parko_core::safety::SafetyGovernor for SwitchableClamp {
            fn evaluate(
                &self,
                _proposed: &ControlCommand,
                _previous: Option<&ControlCommand>,
                _delta_time_s: f64,
                _posture: SafetyPosture,
            ) -> EnforcementAction {
                if self.diverge.load(std::sync::atomic::Ordering::SeqCst) {
                    EnforcementAction::ClampAngularVelocity(0.5)
                } else {
                    // Agree with the primary: pass the command through so
                    // `actions_diverge` is false and the accumulator decays.
                    EnforcementAction::Allow
                }
            }
        }
        use crate::angular_bound::{AngularVelocityBound, PlatformParams};
        use std::sync::atomic::Ordering;

        let mut primary = KirraGovernor::new();
        primary.update_rss_state(safe_rss());
        let diverge = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let shadow = SwitchableClamp {
            diverge: Arc::clone(&diverge),
        };
        let comparator = GovernorComparator::new(primary, shadow);

        let proposed = ControlCommand {
            linear_velocity: 5.0,
            angular_velocity: 3.0,
            timestamp_ms: 0,
        };

        // Establish a HIGH episode peak (30 m/s) over one divergent tick.
        let prev_hi = ControlCommand {
            linear_velocity: 30.0,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        };
        let _ = comparator.evaluate(&proposed, Some(&prev_hi), 0.05, SafetyPosture::Nominal);

        // Agree until the accumulator fully drains (DIVERGENCE_INC=2 added, so a
        // couple of agreeing ticks decay it back to 0 and end the episode).
        diverge.store(false, Ordering::SeqCst);
        for _ in 0..4 {
            let _ = comparator.evaluate(&cmd(3.0), Some(&cmd(3.0)), 0.05, SafetyPosture::Nominal);
        }
        assert_eq!(
            comparator.recommended_posture(),
            SafetyPosture::Nominal,
            "accumulator must have drained back to Nominal"
        );

        // New divergence episode at a genuinely LOW speed proxy (5 m/s). The cap
        // must reflect ω_max(5), not the stale ω_max(30) peak from the prior
        // episode.
        diverge.store(true, Ordering::SeqCst);
        let prev_lo = ControlCommand {
            linear_velocity: 5.0,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        };
        let out = comparator.evaluate(&proposed, Some(&prev_lo), 0.05, SafetyPosture::Nominal);
        let ang = effective_angular_velocity(&out, proposed.angular_velocity).abs();

        // Robust reset proof: a FRESH comparator that never saw the 30 m/s peak,
        // driven with the identical low-speed divergent tick, must produce the
        // SAME reconciled yaw. Equality ⇒ the stale peak was fully released (a
        // residual peak would have kept the recovered comparator's cap tighter).
        let baseline_ang = {
            let mut p = KirraGovernor::new();
            p.update_rss_state(safe_rss());
            // A diverge-locked SwitchableClamp is behaviorally identical to the
            // primed comparator's shadow on a divergent tick (ClampAngularVelocity(0.5)).
            let s = SwitchableClamp {
                diverge: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            };
            let fresh = GovernorComparator::new(p, s);
            let out = fresh.evaluate(&proposed, Some(&prev_lo), 0.05, SafetyPosture::Nominal);
            effective_angular_velocity(&out, proposed.angular_velocity).abs()
        };
        assert!(
            (ang - baseline_ang).abs() <= COMPARATOR_TOLERANCE,
            "post-recovery reconciled yaw {ang} must equal the fresh-comparator baseline \
             {baseline_ang} (episode peak reset on trust recovery); a residual stale peak would \
             keep it tighter"
        );

        // And the low-speed cap is genuinely looser than the stale one — so the
        // equality above is a non-trivial release, not both being pinned low.
        let mrc = AngularVelocityBound::mrc(PlatformParams::conservative_default());
        assert!(
            mrc.omega_max(5.0) > mrc.omega_max(30.0),
            "precondition: ω_max(5)={} must exceed ω_max(30)={}",
            mrc.omega_max(5.0),
            mrc.omega_max(30.0)
        );
    }

    /// WS-0.4 F5 (Copilot #781 follow-up) — a LockedOut escalation must NOT reset
    /// the held episode peak. The reset condition is TRUST RETURNING (agreement
    /// drains the accumulator), and LockedOut is the opposite — escalation. On a
    /// `may_lockout` tick the reconciled command is `Deny` (the cap is moot), and
    /// in the post-lockout drain window the episode is still lost-trust, so the
    /// peak must persist to keep the yaw cap tight (the conservative direction).
    /// This pins that: after establishing a 30 m/s peak, a low-speed tick escalates
    /// to LockedOut (Deny), then a subsequent still-divergent tick whose speed proxy
    /// has risen just above the safe-lockout floor drops back to ClampMotion — and
    /// its yaw is STILL bound at the held ω_max(30), not the loosened ω_max(proxy).
    #[test]
    fn episode_peak_survives_lockout_escalation() {
        struct FixedAngularClamp(f64);
        impl parko_core::safety::SafetyGovernor for FixedAngularClamp {
            fn evaluate(
                &self,
                _proposed: &ControlCommand,
                _previous: Option<&ControlCommand>,
                _delta_time_s: f64,
                _posture: SafetyPosture,
            ) -> EnforcementAction {
                EnforcementAction::ClampAngularVelocity(self.0)
            }
        }
        use crate::angular_bound::{AngularVelocityBound, PlatformParams};

        let mut primary = KirraGovernor::new();
        primary.update_rss_state(safe_rss());
        let comparator = GovernorComparator::new(primary, FixedAngularClamp(0.5));

        let proposed = ControlCommand {
            linear_velocity: 5.0,
            angular_velocity: 3.0,
            timestamp_ms: 0,
        };
        let prev_hi = ControlCommand {
            linear_velocity: 30.0,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        };

        // Three high-speed divergent ticks drive the accumulator to the lockout
        // level (INC=2, LEVEL=6) and establish the 30 m/s episode peak; speed > the
        // safe-lockout floor (5) so these stay ClampMotion, not Deny.
        for _ in 0..3 {
            let out = comparator.evaluate(&proposed, Some(&prev_hi), 0.05, SafetyPosture::Nominal);
            assert!(matches!(out, EnforcementAction::ClampMotion { .. }),
                "high-speed persistent divergence must stay ClampMotion (speed > safe floor), got {out:?}");
        }

        // A low-speed divergent tick (proxy 3 ≤ safe floor 5) with the accumulator
        // already ≥ LEVEL → may_lockout → hard-stop Deny. The peak must NOT reset here.
        let prev_lockout = ControlCommand {
            linear_velocity: 3.0,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        };
        let out = comparator.evaluate(&proposed, Some(&prev_lockout), 0.05, SafetyPosture::Nominal);
        assert!(
            matches!(out, EnforcementAction::Deny { .. }),
            "low-speed persistent divergence must escalate to LockedOut Deny, got {out:?}"
        );

        // Speed proxy rises just above the safe floor while still diverging → drops
        // back to ClampMotion. The held peak (30) must still bind the yaw cap.
        let prev_recover = ControlCommand {
            linear_velocity: 6.0,
            angular_velocity: 0.0,
            timestamp_ms: 0,
        };
        let out = comparator.evaluate(&proposed, Some(&prev_recover), 0.05, SafetyPosture::Nominal);
        assert!(
            matches!(out, EnforcementAction::ClampMotion { .. }),
            "speed above the safe floor must leave LockedOut for ClampMotion, got {out:?}"
        );

        let ang = effective_angular_velocity(&out, proposed.angular_velocity).abs();
        let mrc = AngularVelocityBound::mrc(PlatformParams::conservative_default());
        let cap_at_peak = mrc.omega_max(30.0);
        let cap_at_recover_proxy = mrc.omega_max(6.0);
        assert!(cap_at_peak < cap_at_recover_proxy,
            "precondition: ω_max(30)={cap_at_peak} must be tighter than ω_max(6)={cap_at_recover_proxy}");
        assert!(
            ang <= cap_at_peak + COMPARATOR_TOLERANCE,
            "post-lockout reconciled yaw {ang} must stay bound at the HELD episode peak \
             ω_max(30)={cap_at_peak}; a reset-on-lockout would loosen it to ω_max(6)={cap_at_recover_proxy}"
        );
    }

    // -----------------------------------------------------------------
    // Divergence → posture: governor disagreement drives the fleet posture
    // (not just an audit line). The integrator reads `recommended_posture()`.
    // -----------------------------------------------------------------

    #[test]
    fn agreement_recommends_nominal_posture() {
        let comparator = GovernorComparator::new(KirraGovernor::new(), KirraGovernor::new());
        assert_eq!(
            comparator.recommended_posture(),
            SafetyPosture::Nominal,
            "a fresh comparator is Nominal"
        );
        let _ = comparator.evaluate(&cmd(3.0), Some(&cmd(3.0)), 0.05, SafetyPosture::Nominal);
        assert_eq!(
            comparator.recommended_posture(),
            SafetyPosture::Nominal,
            "agreement keeps the fleet Nominal"
        );
    }

    #[test]
    fn a_single_divergence_recommends_degraded() {
        let (primary, shadow) = diverging_pair();
        let comparator = GovernorComparator::new(primary, shadow);
        // One divergent tick (the safe governor ramps off the stop, the unsafe one denies):
        // accumulator 2, below the lockout level → Degraded, not LockedOut.
        let out = comparator.evaluate(&cmd(3.0), Some(&cmd(0.0)), 0.05, SafetyPosture::Nominal);
        assert!(
            matches!(out, EnforcementAction::ClampMotion { .. }),
            "a single divergence clamps, not denies"
        );
        assert_eq!(
            comparator.recommended_posture(),
            SafetyPosture::Degraded,
            "divergence drives the fleet to Degraded"
        );
    }

    #[test]
    fn persistent_divergence_recommends_lockout() {
        let (primary, shadow) = diverging_pair();
        let comparator = GovernorComparator::new(primary, shadow);
        // Repeated divergence at a SAFE (~0) speed: the accumulator becomes persistent and the
        // posture escalates to LockedOut, mirroring the command-level lockout.
        for _ in 0..6 {
            let _ = comparator.evaluate(&cmd(3.0), Some(&cmd(0.0)), 0.05, SafetyPosture::Nominal);
        }
        assert_eq!(
            comparator.recommended_posture(),
            SafetyPosture::LockedOut,
            "persistent divergence → LockedOut"
        );
    }

    #[test]
    fn posture_recovers_to_nominal_only_after_the_accumulator_drains() {
        let (primary, shadow) = diverging_pair();
        let comparator = GovernorComparator::new(primary, shadow);
        // Two divergent ticks off the stop (accumulator 4, below the lockout level) → Degraded.
        for _ in 0..2 {
            let _ = comparator.evaluate(&cmd(3.0), Some(&cmd(0.0)), 0.05, SafetyPosture::Nominal);
        }
        assert_eq!(comparator.recommended_posture(), SafetyPosture::Degraded);
        // The governors AGREE on a full-stop command (both emit 0). One agreeing tick decays the
        // accumulator but the posture stays Degraded (HYSTERESIS — no instant flip on one tick).
        let _ = comparator.evaluate(&cmd(0.0), Some(&cmd(0.0)), 0.05, SafetyPosture::Nominal);
        assert_eq!(
            comparator.recommended_posture(),
            SafetyPosture::Degraded,
            "stays Degraded mid-drain"
        );
        // Enough agreeing ticks drain it fully → back to Nominal.
        for _ in 0..5 {
            let _ = comparator.evaluate(&cmd(0.0), Some(&cmd(0.0)), 0.05, SafetyPosture::Nominal);
        }
        assert_eq!(
            comparator.recommended_posture(),
            SafetyPosture::Nominal,
            "fully drained → Nominal"
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
            last = comparator.evaluate(&cmd(commanded), Some(&prev), 0.05, SafetyPosture::Nominal);
        }

        assert!(
            matches!(last, EnforcementAction::ClampMotion { .. }),
            "Sustained divergence at speed must NEVER hard stop — must keep \
             returning ClampMotion(MRC-capped on linear). Got {last:?}"
        );
    }

    // ---- S-DG1b: PostureSignalSink emission mapping (test matrix rows) ----

    /// Records every posture tick for assertion.
    #[derive(Default)]
    struct RecordingPostureSink {
        ticks: Mutex<Vec<(bool, bool)>>,
    }
    impl PostureSignalSink for RecordingPostureSink {
        fn divergence_posture_tick(&self, significant: bool, escalated: bool) {
            self.ticks.lock().unwrap().push((significant, escalated));
        }
    }

    /// Emission mapping over a full divergence episode:
    /// - every divergent tick is (significant=true, escalated=comparator's own
    ///   lockout decision);
    /// - agreement while the accumulator drains stays significant (the
    ///   comparator's hysteresis is the single source of truth);
    /// - a drained agreeing tick is (false, false) — the recovery feed.
    #[test]
    fn test_posture_sink_emission_mapping_over_episode() {
        let (primary, shadow) = diverging_pair();
        let sink = Arc::new(RecordingPostureSink::default());
        let comparator = GovernorComparator::new(primary, shadow).with_posture_sink(sink.clone());

        let fast_prev = cmd(10.0); // above SAFE_LOCKOUT_SPEED — no lockout
                                   // Two divergent ticks: significant, not escalated (at speed).
        for _ in 0..2 {
            comparator.evaluate(&cmd(10.0), Some(&fast_prev), 0.05, SafetyPosture::Nominal);
        }
        // Agreement ticks: zero-velocity commands agree trivially; the
        // accumulator (2 ticks × INC=2 = 4) drains at DECAY=1 per tick, so the
        // first agreeing ticks are still significant, the tail is not.
        for _ in 0..6 {
            comparator.evaluate(&cmd(0.0), Some(&cmd(0.0)), 0.05, SafetyPosture::Nominal);
        }

        let ticks = sink.ticks.lock().unwrap().clone();
        assert_eq!(
            ticks.len(),
            8,
            "every evaluate tick emits exactly once: {ticks:?}"
        );
        assert_eq!(
            &ticks[..2],
            &[(true, false), (true, false)],
            "divergent ticks"
        );
        assert!(
            ticks[2..]
                .iter()
                .take_while(|(s, _)| *s)
                .all(|&(s, e)| s && !e),
            "draining agreement stays significant, never escalated: {ticks:?}"
        );
        assert_eq!(
            ticks.last(),
            Some(&(false, false)),
            "a drained agreeing tick must emit the recovery signal: {ticks:?}"
        );
        // The drain boundary exists (some draining ticks, then recovered ones).
        assert!(
            ticks[2..].iter().any(|&(s, _)| !s),
            "the accumulator must drain: {ticks:?}"
        );
    }

    /// The comparator's own lockout escalation is mirrored on the sink.
    #[test]
    fn test_posture_sink_reports_escalation() {
        let (primary, shadow) = diverging_pair();
        let sink = Arc::new(RecordingPostureSink::default());
        let comparator = GovernorComparator::new(primary, shadow).with_posture_sink(sink.clone());
        let slow_prev = cmd(3.0); // below SAFE_LOCKOUT_SPEED (5.0)
        for _ in 0..10 {
            comparator.evaluate(&cmd(10.0), Some(&slow_prev), 0.05, SafetyPosture::Nominal);
        }
        let ticks = sink.ticks.lock().unwrap().clone();
        assert!(
            ticks.iter().any(|&(s, e)| s && e),
            "the sink must mirror the comparator's escalated_to_lockout: {ticks:?}"
        );
        assert!(
            ticks.iter().all(|&(s, _)| s),
            "all ticks in a divergence run are significant"
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
            let _ = comparator.evaluate(&cmd(10.0), Some(&slow_prev), 0.05, SafetyPosture::Nominal);
        }

        // Make primary and shadow agree by giving both safe RSS.
        comparator.update_rss_state(safe_rss());

        // Many agreement ticks — accumulator should saturate at 0 long
        // before this finishes.
        let mut last = EnforcementAction::Allow;
        for _ in 0..50 {
            last = comparator.evaluate(&cmd(3.0), Some(&slow_prev), 0.05, SafetyPosture::Nominal);
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
        let comparator = GovernorComparator::with_sink(
            primary,
            shadow,
            sink.clone() as Arc<dyn DivergenceEventSink>,
        );

        let commanded = 10.0;
        let prev = cmd(commanded);

        // One divergent tick.
        let _ = comparator.evaluate(&cmd(commanded), Some(&prev), 0.05, SafetyPosture::Nominal);

        let events = sink.events();
        assert_eq!(
            events.len(),
            1,
            "Expected exactly one audit event after one divergent tick"
        );
        let e = &events[0];
        assert!(
            e.delta_lin > COMPARATOR_TOLERANCE,
            "linear delta should be flagged"
        );
        assert_eq!(e.accumulator, DIVERGENCE_INC);
        assert_eq!(e.current_speed_mps, Some(10.0));
        assert!(
            !e.escalated_to_lockout,
            "Single tick at speed must not escalate"
        );
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
        let p = EnforcementAction::Deny { reason: "x".into() };
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

    // -----------------------------------------------------------------
    // CERT-006 — DETECTION: a seeded systematic fault in ONE governor
    // path must make the comparator diverge and escalate (fail-closed).
    //
    // This is the demonstrable half of the diversity argument: we inject a
    // fault into the shadow and prove the comparator catches the resulting
    // disagreement. (The CORRECTNESS / no-false-divergence half — that the
    // real DiverseKirraGovernor AGREES with the primary on valid inputs —
    // lives in `crate::diverse::tests`.)
    // -----------------------------------------------------------------

    /// A deliberately-broken shadow governor: it omits the linear
    /// ODD-ceiling / rate clamp entirely (always `Allow` outside LockedOut).
    /// This models a SYSTEMATIC implementation fault localized to the linear
    /// envelope — exactly the class identical redundancy cannot see but
    /// diverse redundancy can.
    struct CeilingBlindShadow;

    impl SafetyGovernor for CeilingBlindShadow {
        fn evaluate(
            &self,
            _proposed: &ControlCommand,
            _previous: Option<&ControlCommand>,
            _delta_time_s: f64,
            posture: SafetyPosture,
        ) -> EnforcementAction {
            match posture {
                // LockedOut still hard-stops (so the bug is narrow: it agrees
                // with the primary everywhere except the linear envelope).
                SafetyPosture::LockedOut => EnforcementAction::Deny {
                    reason: "CeilingBlindShadow: locked out".to_string(),
                },
                // BUG: never clamps the linear axis.
                _ => EnforcementAction::Allow,
            }
        }
    }

    /// An over-ceiling command makes the correct primary clamp while the
    /// fault-injected shadow passes it through — the comparator must diverge,
    /// and at a safe speed escalate to a fail-closed Deny (LockedOut).
    #[test]
    fn test_injected_fault_is_detected_and_escalates() {
        let comparator = GovernorComparator::new(KirraGovernor::new(), CeilingBlindShadow);

        // 40 m/s is above the 35 m/s ODD ceiling: primary → ClampLinear(35),
        // shadow → Allow(40). Previous = 3 m/s is below SAFE_LOCKOUT_SPEED.
        let commanded = cmd(40.0);
        let slow_prev = cmd(3.0);

        // First tick already diverges and reconciles (no hard stop at speed).
        let first = comparator.evaluate(&commanded, Some(&slow_prev), 0.05, SafetyPosture::Nominal);
        assert!(
            matches!(first, EnforcementAction::ClampMotion { .. }),
            "First divergent tick must reconcile (ClampMotion), not hard stop. Got {first:?}"
        );

        // Sustained divergence at a safe speed escalates to LockedOut.
        let mut last = first;
        for _ in 0..10 {
            last = comparator.evaluate(&commanded, Some(&slow_prev), 0.05, SafetyPosture::Nominal);
        }
        assert!(
            matches!(last, EnforcementAction::Deny { .. }),
            "Persistent injected-fault divergence at safe speed must escalate \
             to a fail-closed Deny. Got {last:?}"
        );
        if let EnforcementAction::Deny { reason } = last {
            assert!(
                reason.contains("divergence"),
                "Escalation Deny must reference divergence; got {reason:?}"
            );
        }
    }

    /// Control case: with the SAME fault-injected shadow but an in-envelope
    /// command (both governors return the same physical effect), the
    /// comparator must NOT diverge — proving the detection above is caused by
    /// the fault, not by the pairing itself.
    #[test]
    fn test_injected_fault_silent_when_command_in_envelope() {
        let comparator = GovernorComparator::new(KirraGovernor::new(), CeilingBlindShadow);
        let in_env = cmd(3.0);
        let prev = cmd(3.0);
        let out = comparator.evaluate(&in_env, Some(&prev), 0.05, SafetyPosture::Nominal);
        // Primary Allows (3 m/s steady state), shadow Allows → agreement →
        // primary output, never a divergence Deny.
        assert!(
            !matches!(out, EnforcementAction::Deny { .. }),
            "In-envelope command must not trigger divergence. Got {out:?}"
        );
    }
}
