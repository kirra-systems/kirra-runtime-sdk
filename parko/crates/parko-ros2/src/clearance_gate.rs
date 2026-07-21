// parko/crates/parko-ros2/src/clearance_gate.rs
//
// Phase-B deploy step (#304 deferral): wire `ClearanceDelivery` + a node-owned
// `ClearanceLoop` into the parko-ros2 tick, so a console-recorded operator
// clearance grant releases the vehicle on the node's OWN tick — no manual
// `deliver_clearance` example run.
//
// THE THREE TOUCHES this adds to the tick (`run_pipeline_tick_with_clearance`),
// in this load-bearing order:
//
//   1. DELIVERY — `poll_and_deliver` once per tick. Cheap NoGrant no-op when no
//      grant is pending (the Phase-B design point: pickup is idempotent-empty).
//      Done FIRST.
//
//   2. DETECTION (#309) — assemble this tick's `ImpactEvidence` from the node's
//      live sensors and `loop.observe(evidence, cfg, now)`, which may LATCH. The
//      vehicle now latches ITSELF from real evidence, not only via external
//      drivers.
//
//   3. THE STOP TIE-IN (held line) — while the loop is immobilized (`Latched` OR
//      `EscalationRaised`, i.e. `ClearanceLoop::is_immobilized`), the tick
//      publishes the STOPPED command REGARDLESS of posture. This sits ALONGSIDE
//      the existing LockedOut stop path (which already lives inside the governor
//      inside `run_pipeline_tick_inner`): the latch is belt-and-suspenders with
//      posture, never a bypass of it. A delivered grant clears the loop back to
//      `Normal`; the veto then lifts and command publishing resumes.
//
// THE ORDERING HELD LINE (delivery BEFORE detection): a grant consumed in step 1
// is matched against the loop state at poll time — it can NEVER clear an impact
// that latches in step 2. If a pending grant AND new impact evidence arrive on
// the SAME tick, the grant clears the OLD escalation, step 2 re-latches on the
// new evidence, and step 3 holds the vehicle stopped. The operator re-issues a
// fresh grant against the NEW escalation. Releasing first then re-latching is the
// safe direction; the reverse (latch then let a stale grant clear it) is not.
//
// WHAT IS ARMED (#309 — all three triggers now wired):
//   * decel (IMU)  — ARMED when `imu_topic` is configured (vector-magnitude
//                    decel proxy vs `ImpactCfg::spike_threshold_mps2`).
//   * contact      — ARMED when `contact_topic` is configured.
//   * vanished     — ARMED when `vanished_detection_enabled` AND object
//                    perception (lidar + `platform_profile`) is configured: the
//                    node sources an `AgentScene` per tick from Taj's perceived
//                    objects (the SAME snapshot the object-RSS gate uses) and
//                    passes `Some(&scene)` here. A missing/stale snapshot →
//                    `AgentScene::Absent` (a gap; never a fabricated latch). The
//                    seam was a per-tick scene `Option` (the node passed `None`);
//                    sourcing that scene was the named remainder of #309, now done.
//
// HONESTY RULE: a missing sensor is REDUCED detection coverage, stated loudly at
// startup — never a fabricated spike and never a fabricated veto. An absent IMU
// contributes no decel (not a NaN, not a default spike); an absent contact topic
// reads `false`; an absent scene yields `vanished = false`.
//
// API note: `ClearanceDelivery::poll_and_deliver` takes `&mut ClearanceLoop`
// (the bare loop, as the `deliver_clearance` example does). `RecordedClearanceLoop`
// is the DETECTION-recording wrapper (it emits `ImpactDetected` /
// `ImpactEscalationRaised` on `observe`); it wraps a *private* `ClearanceLoop`, so
// it is not what delivery operates on. The node owns the bare loop; recording the
// detection edges to the signed chain is a separate concern tracked with #309.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parko_core::backend::InferenceBackend;
use parko_core::safety::SafetyPosture;
use parko_core::scheduler::InferenceLoop;
use parko_core::sensor::SensorFrame;
use parko_core::{
    AgentScene, ClearanceLoop, ClearanceState, ImpactCfg, ImpactEvidence, VanishedCfg,
    VanishedObjectDetector,
};
use tokio::sync::Mutex as AsyncMutex;

#[cfg(test)]
use kirra_persistence::VerifierStore;
use parko_kirra::clearance_delivery::{ClearanceDelivery, DeliveryOutcome};

use crate::command_mapping::OutgoingTwist;
use crate::config::ParkoNodeConfig;
use crate::sensor_mapping::ImuSample;
use crate::tick_pipeline::{current_time_ms, run_pipeline_tick_inner, TickOutcome};

/// The node's live impact-sensor readings for ONE tick, assembled into an
/// [`ImpactEvidence`] inside the gate. The node fills this from its optional IMU
/// + contact subscriptions; absent sources stay at their no-signal defaults.
#[derive(Debug, Clone, Default)]
pub struct ImpactInputs {
    /// The latest IMU sample, or `None` when no IMU topic is configured. `None`
    /// contributes NO deceleration spike (never a fabricated value).
    pub imu: Option<ImuSample>,
    /// The latest contact-sensor reading (`false` when no contact topic is
    /// configured — a missing sensor reads as "no contact", reduced coverage).
    pub contact: bool,
}

/// Sticky-until-read contact flag (#320). Contact is a definitive SG6 collision
/// trigger and a boolean EDGE — the most likely signal to be transient. The
/// subscriber writes via [`assert`](Self::assert) (OR in any `true`; a later
/// `false` is a no-op), and the tick reads via [`drain`](Self::drain) (consume the
/// sticky `true` and reset). So a contact pulse that asserts and de-asserts between
/// two 50 ms ticks is **not lost** (the bug was a plain `store`/`load`, which a
/// `false` write would overwrite away), and one contact event latches exactly once
/// rather than every subsequent tick. Lives here (the default-lane tick harness,
/// with [`ImpactInputs`]) so the semantics are CI-tested without ROS; `node.rs`
/// (fully `ros2`-gated) only wires the subscription to it.
#[derive(Debug, Default)]
pub struct ContactCell {
    fired: AtomicBool,
}

impl ContactCell {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Write side: OR in the latest reading — sticky on any `true`.
    pub fn assert(&self, value: bool) {
        self.fired.fetch_or(value, Ordering::Release);
    }

    /// Read side: drain — return whether contact fired since the last drain, and
    /// reset the flag atomically.
    pub fn drain(&self) -> bool {
        self.fired.swap(false, Ordering::Acquire)
    }
}

/// Standard gravity (m/s², ISO 80000-3 / CGPM). The decel-deviation baseline.
const STANDARD_GRAVITY_MPS2: f64 = 9.80665;

/// Gravity-deviation decel proxy (#321 / ADL-013): `| ‖a‖ − G |`, the absolute
/// deviation of the accelerometer-vector magnitude from standard gravity. At rest
/// `‖a‖ ≈ G` ⇒ ≈ 0, so **no class false-latches on gravity** (the H2 floor bug:
/// the old raw-norm `‖a‖` read ~9.81 at rest, below the courier threshold).
///
/// RESIDUAL (named, not fixed here): because `‖a‖` combines the impulse with gravity
/// vectorially, this under-represents a purely horizontal impulse
/// (`√(c²+G²) − G < c`). The better convention subtracts the gravity VECTOR via the
/// orientation quaternion (`‖a − R·g‖`), but that needs a *reliable* orientation
/// (`ImuSample::orientation` is `Option` and a fabricated identity quaternion would
/// assert a false attitude). Orientation-corrected projection is the named future
/// improvement; the deviation convention is the floor-bug fix that ships now, with
/// thresholds set conservatively to absorb the residual.
#[must_use]
fn decel_deviation(imu: &ImuSample) -> f64 {
    let a = imu.linear_acceleration;
    let norm = ((a[0] as f64).powi(2) + (a[1] as f64).powi(2) + (a[2] as f64).powi(2)).sqrt();
    (norm - STANDARD_GRAVITY_MPS2).abs()
}

/// Decel confirmation debounce (#321 / ADL-013): a detection is confirmed only on a
/// run of `≥ M` **CONSECUTIVE** above-threshold ticks within the last `N`
/// observations — a debounce against single-tick jolts (pothole/curb), NOT an
/// M-of-N vote (so `T,F,T` does NOT confirm). Holds a `VecDeque<bool>` bounded at
/// capacity `N`. `M=1/N=1` (the default cfg) = single-tick / frozen behavior.
///
/// Lives here (the default-lane tick harness, alongside [`ContactCell`]) so the
/// stateful confirmation is CI-tested without ROS; `node.rs` stays a thin transport.
#[derive(Debug, Default)]
pub struct SpikeDebouncer {
    window: std::collections::VecDeque<bool>,
}

impl SpikeDebouncer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one observation: `deviation > threshold` is a "hit". Evicts the
    /// oldest if the window is at capacity `N`.
    pub fn observe(&mut self, deviation: f64, cfg: &ImpactCfg) {
        let n = cfg.confirmation_n.max(1) as usize;
        self.window.push_back(deviation > cfg.spike_threshold_mps2);
        while self.window.len() > n {
            self.window.pop_front();
        }
    }

    /// True iff the window holds a run of `≥ M` consecutive hits.
    #[must_use]
    pub fn is_confirmed(&self, cfg: &ImpactCfg) -> bool {
        let m = cfg.confirmation_m.max(1) as usize;
        let mut run = 0usize;
        for &hit in &self.window {
            if hit {
                run += 1;
                if run >= m {
                    return true;
                }
            } else {
                run = 0;
            }
        }
        false
    }

    /// [`is_confirmed`](Self::is_confirmed), and on a `true` CONSUME it by resetting
    /// the window (so one collision confirms once, and the next incident needs a
    /// fresh run). Mirrors [`ContactCell::drain`]'s consume semantics — but only
    /// resets on a confirm, so the window can ACCUMULATE across ticks toward `M`.
    pub fn drain_confirmed(&mut self, cfg: &ImpactCfg) -> bool {
        let confirmed = self.is_confirmed(cfg);
        if confirmed {
            self.window.clear();
        }
        confirmed
    }
}

/// IMU staleness watchdog (#324). A configured-but-silent or stale IMU is a SENSOR
/// FAULT, not "reduced coverage": without a fresh deceleration reading the gate
/// cannot detect a hard-decel impact, so the safe response is to force the MRC
/// (stop) — the same convention the tick pipeline already applies to a stale sensor
/// FRAME (`sensor_staleness_budget_ms`). Distinct from an UNCONFIGURED IMU (no guard
/// armed → reduced coverage, never a forced stop).
///
/// The guard is stamped each time a FRESH sample arrives (the node's IMU drain
/// records the arrival time) and is consulted per tick. `None` last-update (armed
/// but nothing ever arrived) reads as stale → MRC at startup until the first sample.
/// Lives here (the default-lane harness, alongside [`ContactCell`] / [`SpikeDebouncer`])
/// so the watchdog logic is CI-tested without ROS; `node.rs` only wires the stamp.
#[derive(Debug)]
pub struct StalenessGuard {
    /// Wall-clock (ms) of the most recent fresh sample, or `None` if none yet.
    last_update_ms: Option<u64>,
    /// Max age (ms) before a sample is considered stale → force MRC.
    window_ms: u64,
    /// Throttle for the per-tick stale log (warn at most once per window).
    last_warn_ms: Option<u64>,
}

impl StalenessGuard {
    #[must_use]
    pub fn new(window_ms: u64) -> Self {
        Self {
            last_update_ms: None,
            window_ms,
            last_warn_ms: None,
        }
    }

    /// Record a fresh sample arrival at `now_ms`. Idempotent if called repeatedly
    /// with the same (unchanged) arrival time — the guard tracks the LAST arrival.
    pub fn stamp(&mut self, now_ms: u64) {
        self.last_update_ms = Some(now_ms);
    }

    /// True when the sensor is MISSING (never stamped) or STALE (last sample older
    /// than the window) — in both cases the gate must force the MRC.
    #[must_use]
    pub fn is_stale(&self, now_ms: u64) -> bool {
        match self.last_update_ms {
            None => true, // armed but nothing has ever arrived → missing → MRC
            Some(t) => now_ms.saturating_sub(t) > self.window_ms,
        }
    }

    /// Rate-limited stale signal for logging: returns `true` at most once per
    /// `window_ms` while stale, so a persistently-silent IMU does not flood the log.
    pub fn take_warning(&mut self, now_ms: u64) -> bool {
        if !self.is_stale(now_ms) {
            return false;
        }
        let due = match self.last_warn_ms {
            None => true,
            Some(w) => now_ms.saturating_sub(w) >= self.window_ms,
        };
        if due {
            self.last_warn_ms = Some(now_ms);
        }
        due
    }
}

/// Node-owned clearance components: the per-vehicle [`ClearanceDelivery`] (store
/// pickup, scoped to THIS node's id) plus the node's own [`ClearanceLoop`] (the
/// SG6 motion veto). One per node; constructed at startup when delivery is
/// enabled, then borrowed `&mut` by the tick.
pub struct NodeClearance {
    delivery: ClearanceDelivery,
    clearance_loop: ClearanceLoop,
    /// SG6 fusion config (the decel threshold). Deployment-tunable via the node
    /// config; defaults to [`ImpactCfg::default`].
    cfg: ImpactCfg,
    /// The vanished-object detector, behind an `Option` GATED ON A SCENE SOURCE.
    /// `Some` when armed via [`with_vanished_detection`](Self::with_vanished_detection):
    /// the node sources an `AgentScene` per tick from Taj objects (#309) and the
    /// detector runs against it; `None` leaves it unfed (reduced coverage).
    vanished: Option<VanishedObjectDetector>,
    /// Config for the vanished detector (parko-core default until tuned).
    vanished_cfg: VanishedCfg,
    /// Decel confirmation debounce (#321) — per-node stateful, M-of-N consecutive
    /// from `cfg.confirmation_*`.
    spike_debouncer: SpikeDebouncer,
    /// IMU staleness watchdog (#324). `None` when no IMU source is configured
    /// (reduced coverage, never a forced stop); `Some` when an IMU is expected, so
    /// a stale/silent sensor forces the MRC.
    imu_staleness: Option<StalenessGuard>,
}

impl NodeClearance {
    /// Wrap a pre-built [`ClearanceDelivery`] with a fresh [`ClearanceLoop`].
    /// (The binary builds the delivery via [`ClearanceDelivery::open_signed`];
    /// tests build it from an in-memory store.) Impact detection uses
    /// [`ImpactCfg::default`] until [`with_impact_cfg`](Self::with_impact_cfg)
    /// tunes it; the vanished trigger is OFF (no scene source) until
    /// [`with_vanished_detection`](Self::with_vanished_detection) enables it.
    #[must_use]
    pub fn new(delivery: ClearanceDelivery) -> Self {
        Self {
            delivery,
            clearance_loop: ClearanceLoop::new(),
            cfg: ImpactCfg::default(),
            vanished: None,
            vanished_cfg: VanishedCfg::default(),
            spike_debouncer: SpikeDebouncer::new(),
            imu_staleness: None,
        }
    }

    /// Set the SG6 impact-fusion config (the decel spike threshold). The binary
    /// threads this from `ParkoNodeConfig::impact_cfg()`.
    #[must_use]
    pub fn with_impact_cfg(mut self, cfg: ImpactCfg) -> Self {
        self.cfg = cfg;
        self
    }

    /// Arm the IMU staleness watchdog (#324) with the given window. The binary
    /// calls this ONLY when an IMU source is configured — so an UNCONFIGURED IMU
    /// stays reduced-coverage (no guard, no forced stop), while a CONFIGURED IMU
    /// that goes silent/stale forces the MRC. Without this, `imu_staleness` is
    /// `None` and staleness never forces a stop.
    #[must_use]
    pub fn with_imu_staleness(mut self, window_ms: u64) -> Self {
        self.imu_staleness = Some(StalenessGuard::new(window_ms));
        self
    }

    /// Record a fresh IMU sample arrival (#324). No-op when the watchdog is not
    /// armed. The node's IMU drain calls this with the sample's arrival time.
    pub fn stamp_imu(&mut self, now_ms: u64) {
        if let Some(g) = self.imu_staleness.as_mut() {
            g.stamp(now_ms);
        }
    }

    /// True when an ARMED IMU watchdog reports a stale/missing sensor → the tick
    /// must force the MRC. Always false when no IMU source is configured.
    #[must_use]
    pub fn imu_mrc_required(&self, now_ms: u64) -> bool {
        self.imu_staleness
            .as_ref()
            .is_some_and(|g| g.is_stale(now_ms))
    }

    /// Rate-limited stale-IMU warning for the tick log (#324). At most once per
    /// window while stale; false when not armed or not stale.
    pub fn imu_stale_warning(&mut self, now_ms: u64) -> bool {
        self.imu_staleness
            .as_mut()
            .is_some_and(|g| g.take_warning(now_ms))
    }

    /// Enable the vanished-object trigger with the given config. The caller MUST
    /// also supply an `AgentScene` per tick (via
    /// [`observe_tick`](Self::observe_tick)) for it to fire — enabling the
    /// detector without a scene source still yields `vanished = false`. Off by
    /// default; this is the seam the scene-sourcing follow-up flips on.
    #[must_use]
    pub fn with_vanished_detection(mut self, cfg: VanishedCfg) -> Self {
        self.vanished = Some(VanishedObjectDetector::new());
        self.vanished_cfg = cfg;
        self
    }

    /// Build a node clearance over an existing shared store handle and node id.
    /// The store-open + signing-key path is [`NodeClearance::open_signed`]; this
    /// is the seam tests use with an in-memory store.
    #[must_use]
    pub fn from_store(
        store: kirra_verifier::store_handle::StoreHandle,
        node_id: impl Into<String>,
    ) -> Self {
        Self::new(ClearanceDelivery::new(store, node_id))
    }

    /// Open the co-located store at `db_path`, install the base64 Ed25519 signing
    /// key so delivered-grant outcomes are signed, and build the gate for
    /// `node_id`. Fail-closed: an unopenable store / undecodable key is an `Err`.
    /// The node never names `base64` / `ed25519` itself — `parko-kirra` owns that.
    pub fn open_signed(
        db_path: &str,
        node_id: &str,
        signing_key_b64: &str,
    ) -> Result<Self, String> {
        Ok(Self::new(ClearanceDelivery::open_signed(
            db_path,
            node_id,
            signing_key_b64,
        )?))
    }

    /// The motion veto: true while the loop is immobilized (`Latched` OR
    /// `EscalationRaised`). The tick forces a stopped command while this holds.
    #[must_use]
    pub fn is_immobilized(&self) -> bool {
        self.clearance_loop.is_immobilized()
    }

    /// The loop's current lifecycle state (diagnostic / tests).
    #[must_use]
    pub fn state(&self) -> ClearanceState {
        self.clearance_loop.state()
    }

    /// Deliver at most one pending grant to the node-owned loop. Cheap
    /// `NoGrant` no-op when nothing is pending. Node-scoped pickup: a grant for a
    /// different node id is never taken.
    pub fn poll(&mut self, now_ms: u64) -> DeliveryOutcome {
        self.delivery
            .poll_and_deliver(&mut self.clearance_loop, now_ms)
    }

    /// Assemble this tick's [`ImpactEvidence`] from the node's live inputs and
    /// drive ONE observation into the loop (#309). The vanished trigger runs the
    /// detector ONLY when both it is enabled AND a `scene` is supplied; otherwise
    /// `vanished = false` (the deferred trigger, never fabricated). This is the
    /// detection step the tick calls between delivery and the immobilized gate.
    pub fn observe_tick(&mut self, inputs: &ImpactInputs, scene: Option<&AgentScene>, now_ms: u64) {
        let vanished = match (self.vanished.as_mut(), scene) {
            (Some(det), Some(sc)) => det.observe(sc, now_ms, &self.vanished_cfg),
            _ => false,
        };
        // Decel (#321): gravity-DEVIATION through the M-of-N confirmation debounce.
        // An absent IMU contributes NO decel and does not advance the window (never
        // a fabricated spike). A confirmed run reports the deviation; else 0.0.
        let imu_accel_spike_mps2 = match &inputs.imu {
            Some(imu) => {
                let dev = decel_deviation(imu);
                self.spike_debouncer.observe(dev, &self.cfg);
                if self.spike_debouncer.drain_confirmed(&self.cfg) {
                    dev
                } else {
                    0.0
                }
            }
            None => 0.0,
        };
        let evidence = ImpactEvidence {
            imu_accel_spike_mps2,
            contact_sensor: inputs.contact,
            vanished_object: vanished,
        };
        self.clearance_loop.observe(&evidence, &self.cfg, now_ms);
    }

    /// Drive one observation into the loop from EXPLICIT evidence (bypassing the
    /// input-assembly + config). The seam tests use to drive the loop into an
    /// immobilized state through the REAL state machine (never by poking
    /// internals).
    pub fn observe(&mut self, evidence: &ImpactEvidence, cfg: &ImpactCfg, now_ms: u64) {
        self.clearance_loop.observe(evidence, cfg, now_ms);
    }
}

/// What one [`run_pipeline_tick_with_clearance`] produced: the normal
/// [`TickOutcome`] plus the clearance side-effects so the node can log them.
#[derive(Debug, Clone, PartialEq)]
pub struct ClearedTickOutcome {
    /// The tick's outcome. `twist` is the command to publish — overridden to a
    /// stopped twist when `vetoed` is true.
    pub tick: TickOutcome,
    /// The per-tick delivery result. `None` when no clearance gate is configured
    /// (the dev lane); `Some(NoGrant)` on the common idempotent-empty pickup.
    pub delivery: Option<DeliveryOutcome>,
    /// True when the clearance veto forced the stopped twist this tick (the loop
    /// was immobilized after delivery, OR the IMU watchdog forced the MRC). Lets the
    /// node log the held line.
    pub vetoed: bool,
    /// True when the veto this tick was driven (wholly or partly) by a stale/missing
    /// IMU watchdog (#324), distinct from a loop-immobilization veto. Lets the node
    /// log the sensor-fault MRC distinctly.
    pub imu_stale: bool,
}

/// Drive one tick with the node-owned clearance gate wired in. When `clearance`
/// is `None` (delivery disabled — the dev lane) this is exactly the existing
/// posture-gated tick with no veto, no pickup, and no detection.
///
/// Order is load-bearing (see the module doc's HELD LINE): (1) deliver FIRST so a
/// grant clears the loop against its poll-time state; (2) THEN run detection
/// (`observe_tick`) which may latch on this tick's evidence — so a grant consumed
/// in (1) can never clear an impact latched in (2); (3) run the posture-gated
/// tick; (4) apply the veto reading the POST-delivery, POST-detection
/// immobilization state. A just-cleared loop with no new impact resumes motion
/// the same tick; a freshly-latched (or still-immobilized) loop stops regardless
/// of posture.
///
/// `inputs` are the node's live impact-sensor readings for this tick; `scene` is
/// the optional `AgentScene` for the vanished trigger (`None` until a scene
/// source is wired — the #309 remainder).
///
// SAFETY: SG6 | REQ: parko-ros2-clearance-detect-veto-and-delivery | TEST: decel_spike_latches_and_stops_at_nominal,contact_latches,no_signals_never_latch_across_many_ticks,delivery_before_detection_ordering_holds,full_lifecycle_detect_clear_resume,latched_loop_stops_tick_even_at_nominal,pending_grant_delivers_and_resumes,no_clearance_gate_ticks_normally,grant_for_other_node_not_picked_up,stale_imu_forces_mrc_even_at_nominal_with_normal_loop,fresh_imu_does_not_force_mrc
#[allow(clippy::too_many_arguments)]
pub async fn run_pipeline_tick_with_clearance<B>(
    config: &ParkoNodeConfig,
    loop_mutex: Arc<AsyncMutex<InferenceLoop<B>>>,
    frame: SensorFrame,
    posture: SafetyPosture,
    clearance: Option<&mut NodeClearance>,
    inputs: &ImpactInputs,
    scene: Option<&AgentScene>,
) -> ClearedTickOutcome
where
    B: InferenceBackend + 'static,
{
    let now_ms = current_time_ms();
    let mut clearance = clearance;

    // 1. DELIVERY — one pickup per tick (NoGrant no-op when nothing pending).
    //    Matched against the loop's poll-time state, BEFORE this tick's detection.
    let delivery = clearance.as_deref_mut().map(|c| c.poll(now_ms));

    // 2. DETECTION (#309) — assemble live evidence + observe; may latch. Because
    //    this follows delivery, a grant consumed in step 1 can never clear an
    //    impact that latches here (the held line).
    if let Some(c) = clearance.as_deref_mut() {
        c.observe_tick(inputs, scene, now_ms);
    }

    // 3. The normal posture-gated tick (the LockedOut stop path lives inside).
    let mut tick = run_pipeline_tick_inner(config, loop_mutex, frame, posture).await;

    // 4. THE STOP TIE-IN — immobilized (post-delivery, post-detection) OR a
    //    stale/missing IMU watchdog (#324) → stop regardless of posture, alongside
    //    the governor's LockedOut stop. The stale-IMU warning is rate-limited.
    let (immobilized, imu_stale, warn) = match clearance {
        Some(c) => {
            let immobilized = c.is_immobilized();
            let imu_stale = c.imu_mrc_required(now_ms);
            let warn = imu_stale && c.imu_stale_warning(now_ms);
            (immobilized, imu_stale, warn)
        }
        None => (false, false, false),
    };
    let vetoed = immobilized || imu_stale;
    if vetoed {
        tick.twist = OutgoingTwist::stopped(now_ms);
    }
    if warn {
        tracing::warn!(
            "parko-ros2: SG6 IMU sample stale/missing beyond the staleness window — forcing MRC \
             (stopped) this tick; decel impact detection is BLIND until fresh IMU resumes."
        );
    }

    ClearedTickOutcome {
        tick,
        delivery,
        vetoed,
        imu_stale,
    }
}

// ---------------------------------------------------------------------------
// Tests — MockBackend lane (no ROS, no ORT, no model file). Mirrors the
// `tick_pipeline` async test harness.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ---- #320: contact ContactCell sticky-until-read --------------------

    /// A contact pulse that asserts then de-asserts BEFORE the tick reads it is
    /// NOT lost — the old `store`/`load` would have seen the trailing `false`.
    #[test]
    fn contact_pulse_between_ticks_is_not_lost() {
        let cell = ContactCell::new();
        cell.assert(true); // the pulse
        cell.assert(false); // de-assert before the tick reads
        assert!(
            cell.drain(),
            "a sub-tick contact pulse must survive to the next tick read"
        );
    }

    /// One contact event latches exactly ONCE — after the tick drains a `true`,
    /// the next tick sees `false` (no double-latch every subsequent tick).
    #[test]
    fn contact_resets_after_tick_read() {
        let cell = ContactCell::new();
        cell.assert(true);
        assert!(cell.drain(), "first read sees the contact");
        assert!(
            !cell.drain(),
            "second read is reset — one event latches once"
        );
    }

    // ---- #321: decel deviation convention + SpikeDebouncer --------------

    fn imu_with(ax: f32, ay: f32, az: f32) -> ImuSample {
        ImuSample {
            linear_acceleration: [ax, ay, az],
            angular_velocity: [0.0; 3],
            orientation: None,
        }
    }

    /// A flat, RESTING IMU (gravity only) reads ≈ 0 deviation — the #321 fix: it is
    /// FAR below the courier threshold (2.5), so a static courier never false-latches
    /// (the old raw-norm read ~9.81 > 8.0 and latched on gravity alone).
    #[test]
    fn static_resting_imu_deviation_is_below_courier_threshold() {
        let dev = decel_deviation(&imu_with(0.0, 0.0, 9.80665));
        assert!(dev < 1e-3, "resting deviation must be ≈ 0, got {dev}");
        let courier = parko_core::impact_cfg_for_class(parko_core::VehicleClass::Courier);
        assert!(
            dev < courier.spike_threshold_mps2,
            "resting deviation {dev} must be below the courier threshold {}",
            courier.spike_threshold_mps2
        );
    }

    /// Parametric deviation: a known acceleration vector → expected `|‖a‖ − G|`.
    #[test]
    fn decel_deviation_matches_formula() {
        // ‖(40,0,0)‖ = 40 → |40 − 9.80665| = 30.19335
        let dev = decel_deviation(&imu_with(40.0, 0.0, 0.0));
        assert!((dev - (40.0 - 9.80665)).abs() < 1e-3, "got {dev}");
        // free-fall ‖(0,0,0)‖ = 0 → |0 − G| = G (a secondary anomaly trigger)
        let ff = decel_deviation(&imu_with(0.0, 0.0, 0.0));
        assert!(
            (ff - 9.80665).abs() < 1e-3,
            "free-fall deviation = G, got {ff}"
        );
    }

    fn cfg_mn(threshold: f64, m: u8, n: u8) -> ImpactCfg {
        ImpactCfg {
            spike_threshold_mps2: threshold,
            confirmation_m: m,
            confirmation_n: n,
        }
    }

    /// M=2/N=3: one hit not confirmed; two CONSECUTIVE hits confirmed + drain
    /// resets; the non-consecutive T,F,T never confirms; sub-threshold never confirms.
    #[test]
    fn spike_debouncer_m2_n3_requires_consecutive() {
        let cfg = cfg_mn(2.5, 2, 3);
        let mut d = SpikeDebouncer::new();

        // one hit → not confirmed
        d.observe(5.0, &cfg);
        assert!(!d.drain_confirmed(&cfg), "single hit must not confirm M=2");
        // a SECOND consecutive hit → confirmed, and drain resets the window
        d.observe(5.0, &cfg);
        assert!(d.drain_confirmed(&cfg), "two consecutive hits confirm M=2");
        assert!(
            !d.is_confirmed(&cfg),
            "drain reset the window after a confirm"
        );

        // non-consecutive T,F,T → never confirms (the debounce, not a vote)
        let mut d2 = SpikeDebouncer::new();
        d2.observe(5.0, &cfg);
        assert!(!d2.drain_confirmed(&cfg));
        d2.observe(0.0, &cfg);
        assert!(!d2.drain_confirmed(&cfg));
        d2.observe(5.0, &cfg);
        assert!(
            !d2.drain_confirmed(&cfg),
            "T,F,T is non-consecutive → not confirmed"
        );

        // sub-threshold forever → never confirms
        let mut d3 = SpikeDebouncer::new();
        for _ in 0..10 {
            d3.observe(1.0, &cfg);
            assert!(!d3.drain_confirmed(&cfg));
        }
    }

    /// M=1/N=1 (robotaxi / default): a single above-threshold tick confirms
    /// immediately — the frozen single-tick behavior (zero regression).
    #[test]
    fn spike_debouncer_m1_n1_confirms_single_tick() {
        let cfg = cfg_mn(22.0, 1, 1);
        let mut d = SpikeDebouncer::new();
        d.observe(25.0, &cfg);
        assert!(d.drain_confirmed(&cfg), "M=1/N=1 confirms on one hit");
        // a sub-threshold tick does not
        d.observe(10.0, &cfg);
        assert!(!d.drain_confirmed(&cfg));
    }

    // ---- #324: IMU StalenessGuard ---------------------------------------

    /// STARTUP-MISSING → MRC: an armed-but-never-stamped guard reads stale at any
    /// time (a configured IMU that has not yet published forces the MRC).
    #[test]
    fn staleness_guard_never_stamped_is_stale() {
        let g = StalenessGuard::new(100);
        assert!(
            g.is_stale(0),
            "never stamped → stale (missing sensor → MRC)"
        );
        assert!(g.is_stale(10_000), "still stale far later");
    }

    /// STALE-CELL → MRC, and FRESH-UPDATE RESETS: within the window a stamped guard
    /// is fresh; past the window it is stale; a fresh stamp clears it again.
    #[test]
    fn staleness_guard_window_and_reset() {
        let mut g = StalenessGuard::new(100);
        g.stamp(1_000);
        assert!(!g.is_stale(1_050), "within the 100ms window → fresh");
        assert!(
            !g.is_stale(1_100),
            "exactly at the window edge → still fresh (inclusive)"
        );
        assert!(g.is_stale(1_101), "one ms past the window → stale → MRC");
        // A fresh sample resets the watchdog.
        g.stamp(1_200);
        assert!(!g.is_stale(1_250), "a fresh stamp resets staleness");
    }

    /// The per-tick stale log is rate-limited: at most one warning per window.
    #[test]
    fn staleness_guard_warning_is_rate_limited() {
        let mut g = StalenessGuard::new(100); // never stamped → always stale
        assert!(g.take_warning(1_000), "first stale tick warns");
        assert!(
            !g.take_warning(1_050),
            "a second tick within the window does not re-warn"
        );
        assert!(g.take_warning(1_100), "after a full window it warns again");
    }

    /// An UNARMED NodeClearance never forces an MRC on staleness (an unconfigured
    /// IMU is reduced coverage, not a fault). An armed one does until stamped fresh.
    #[test]
    fn node_clearance_imu_mrc_only_when_armed() {
        let mut unarmed = NodeClearance::from_store(store(), "node-x");
        assert!(
            !unarmed.imu_mrc_required(10_000),
            "no IMU configured → never forces MRC"
        );
        unarmed.stamp_imu(1); // no-op when unarmed

        let mut armed = NodeClearance::from_store(store(), "node-x").with_imu_staleness(100);
        assert!(
            armed.imu_mrc_required(0),
            "armed + never stamped → MRC (startup-missing)"
        );
        armed.stamp_imu(1_000);
        assert!(!armed.imu_mrc_required(1_050), "fresh sample → no MRC");
        assert!(armed.imu_mrc_required(1_500), "gone stale → MRC again");
    }

    use parko_core::backend::{BackendDescriptor, TensorBatch};
    use parko_core::backends::mock::MockBackend;
    use parko_core::commands::ControlCommand;
    use parko_core::{ImpactCfg, ImpactEvidence};
    use parko_kirra::{GovernorComparator, KirraGovernor};
    use tokio::sync::mpsc;

    use crate::comparator_adapter::ComparatorAsGovernor;

    fn build_loop(
        linear_out: f32,
        angular_out: f32,
    ) -> Arc<AsyncMutex<InferenceLoop<MockBackend>>> {
        let mut outputs: HashMap<String, Vec<f32>> = HashMap::new();
        outputs.insert("cmd_vel_linear".to_string(), vec![linear_out]);
        outputs.insert("cmd_vel_angular".to_string(), vec![angular_out]);
        let backend = Arc::new(MockBackend::new(outputs, BackendDescriptor::Cpu));
        let model = backend.load_model("test.onnx").expect("mock model loads");
        let (tx, _rx) = mpsc::channel::<ControlCommand>(8);
        // See tick_pipeline tests: enforcement lives at the publication seam;
        // the in-loop governors declare external gating (unfed = HOLD).
        let comparator = GovernorComparator::new(
            KirraGovernor::new().with_external_rss_gate(),
            KirraGovernor::new().with_external_rss_gate(),
        );
        let infer = InferenceLoop::new(backend, model, tx)
            .with_governor(ComparatorAsGovernor(comparator))
            .with_tick_period(0.05);
        Arc::new(AsyncMutex::new(infer))
    }

    fn fresh_frame(frame_id: u64) -> SensorFrame {
        SensorFrame {
            frame_id,
            timestamp_ms: current_time_ms(),
            payload: TensorBatch {
                named_tensors: HashMap::new(),
                metadata: HashMap::new(),
            },
        }
    }

    fn store() -> kirra_verifier::store_handle::StoreHandle {
        kirra_verifier::store_handle::StoreHandle::new(
            VerifierStore::new(":memory:").expect("in-memory store"),
        )
    }

    /// Drive a `NodeClearance`'s loop into an immobilized state through the REAL
    /// state machine. Post-#328 the first latching observe reaches `EscalationRaised`
    /// in one step; further observes are harmless no-ops (stay `EscalationRaised`).
    fn immobilize(nc: &mut NodeClearance, ticks: u32) {
        let ev = ImpactEvidence {
            imu_accel_spike_mps2: 0.0,
            contact_sensor: true,
            vanished_object: false,
        };
        let cfg = ImpactCfg::default();
        for _ in 0..ticks {
            nc.observe(&ev, &cfg, current_time_ms());
        }
    }

    /// THE TIE-IN: a latched loop forces a stopped twist EVEN at Nominal posture,
    /// where the governor alone would admit the forward command.
    #[tokio::test(start_paused = true)]
    async fn latched_loop_stops_tick_even_at_nominal() {
        let infer = build_loop(0.1, 0.2);
        let mut nc = NodeClearance::from_store(store(), "KIRRA-DEMO-03");
        immobilize(&mut nc, 1); // Normal -> EscalationRaised (one step, #328)
        assert_eq!(nc.state(), ClearanceState::EscalationRaised);

        let out = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            infer,
            fresh_frame(1),
            SafetyPosture::Nominal,
            Some(&mut nc),
            &ImpactInputs::default(),
            None,
        )
        .await;

        assert!(
            out.vetoed,
            "an immobilized loop must veto motion regardless of posture"
        );
        assert_eq!(
            out.tick.twist.linear_x_mps, 0.0,
            "latched → stopped twist (linear)"
        );
        assert_eq!(
            out.tick.twist.angular_z_rads, 0.0,
            "latched → stopped twist (angular)"
        );
        assert_eq!(
            out.delivery,
            Some(DeliveryOutcome::NoGrant),
            "no grant pending"
        );
    }

    /// #324 — STALE IMU FORCES MRC: with the loop in Normal (not immobilized) at
    /// Nominal posture, an armed-but-never-stamped IMU watchdog forces the stopped
    /// twist anyway — a configured IMU that is silent is a sensor fault, not reduced
    /// coverage. The veto reason is surfaced as `imu_stale`, distinct from the loop.
    #[tokio::test(start_paused = true)]
    async fn stale_imu_forces_mrc_even_at_nominal_with_normal_loop() {
        let infer = build_loop(0.1, 0.2);
        // Armed watchdog, NEVER stamped → stale at any tick time (startup-missing).
        let mut nc = NodeClearance::from_store(store(), "KIRRA-DEMO-03").with_imu_staleness(100);
        assert_eq!(
            nc.state(),
            ClearanceState::Normal,
            "loop is NOT immobilized"
        );

        let out = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            infer,
            fresh_frame(700),
            SafetyPosture::Nominal,
            Some(&mut nc),
            &ImpactInputs::default(),
            None,
        )
        .await;

        assert!(
            out.imu_stale,
            "a configured-but-silent IMU is reported stale"
        );
        assert!(out.vetoed, "stale IMU forces the MRC veto");
        assert_eq!(
            nc.state(),
            ClearanceState::Normal,
            "the stop came from staleness, not the loop"
        );
        assert_eq!(
            out.tick.twist.linear_x_mps, 0.0,
            "stale IMU → stopped twist"
        );
    }

    /// #324 — A FRESH IMU does NOT force the MRC: an armed watchdog stamped fresh
    /// within the window lets the governed command flow (loop Normal, no veto).
    #[tokio::test(start_paused = true)]
    async fn fresh_imu_does_not_force_mrc() {
        let infer = build_loop(0.1, 0.2);
        // Generous window so the stamp-then-tick clock advance can't flip it stale.
        let mut nc = NodeClearance::from_store(store(), "KIRRA-DEMO-03").with_imu_staleness(10_000);
        nc.stamp_imu(current_time_ms()); // a fresh sample just arrived

        let out = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            infer,
            fresh_frame(701),
            SafetyPosture::Nominal,
            Some(&mut nc),
            &ImpactInputs::default(),
            None,
        )
        .await;

        assert!(!out.imu_stale, "a fresh IMU is not stale");
        assert!(!out.vetoed, "fresh IMU → no forced stop");
        assert!(
            (out.tick.twist.linear_x_mps - 0.1).abs() < 1e-4,
            "command flows when the IMU is fresh; got {}",
            out.tick.twist.linear_x_mps
        );
    }

    /// END-TO-END: a pending grant in the store is delivered on ONE tick, the loop
    /// clears to Normal, the veto lifts, and command publishing RESUMES — on the
    /// node's own tick, with no manual example run.
    #[tokio::test(start_paused = true)]
    async fn pending_grant_delivers_and_resumes() {
        let s = store();
        let mut nc = NodeClearance::from_store(s.clone(), "KIRRA-DEMO-03");
        immobilize(&mut nc, 2); // -> EscalationRaised (immobilized, escalation pending)
        assert_eq!(nc.state(), ClearanceState::EscalationRaised);

        // The operator records a grant through the (Phase-A) store path, dated now.
        let now = current_time_ms();
        s.with(|store| store.save_clearance_grant_chained("KIRRA-DEMO-03", "alice", now))
            .expect("record grant");

        // ONE tick delivers it.
        let infer = build_loop(0.1, 0.2);
        let out = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            infer,
            fresh_frame(2),
            SafetyPosture::Nominal,
            Some(&mut nc),
            &ImpactInputs::default(),
            None,
        )
        .await;

        assert!(
            matches!(out.delivery, Some(DeliveryOutcome::Cleared { .. })),
            "the pending grant must be delivered this tick; got {:?}",
            out.delivery
        );
        assert_eq!(
            nc.state(),
            ClearanceState::Normal,
            "the loop clears back to Normal"
        );
        assert!(
            !out.vetoed,
            "veto lifts the same tick the grant clears the loop"
        );
        assert!(
            (out.tick.twist.linear_x_mps - 0.1).abs() < 1e-4,
            "command publishing resumes — governed forward command, not a stop; got {}",
            out.tick.twist.linear_x_mps
        );

        // A subsequent tick: nothing pending, normal publishing continues.
        let infer2 = build_loop(0.1, 0.2);
        let out2 = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            infer2,
            fresh_frame(3),
            SafetyPosture::Nominal,
            Some(&mut nc),
            &ImpactInputs::default(),
            None,
        )
        .await;
        assert_eq!(
            out2.delivery,
            Some(DeliveryOutcome::NoGrant),
            "grant consumed — no retry"
        );
        assert!(!out2.vetoed);
    }

    /// THE DEV LANE: no clearance gate → the node ticks normally, delivery
    /// disabled, no panic. The forward command is published unmodified.
    #[tokio::test(start_paused = true)]
    async fn no_clearance_gate_ticks_normally() {
        let infer = build_loop(0.1, 0.2);
        let out = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            infer,
            fresh_frame(4),
            SafetyPosture::Nominal,
            None,
            &ImpactInputs::default(),
            None,
        )
        .await;
        assert_eq!(out.delivery, None, "no gate → no delivery attempt");
        assert!(!out.vetoed, "no gate → no veto");
        assert!(out.tick.error.is_none());
        assert!(
            (out.tick.twist.linear_x_mps - 0.1).abs() < 1e-4,
            "dev lane publishes the governed command unchanged; got {}",
            out.tick.twist.linear_x_mps
        );
    }

    /// NODE-SCOPED PICKUP: a grant recorded for a DIFFERENT node id is never taken
    /// by this node's gate — the one-shot consume is node-scoped, so a wrong-node
    /// pickup is impossible. The loop stays immobilized.
    #[tokio::test(start_paused = true)]
    async fn grant_for_other_node_not_picked_up() {
        let s = store();
        let mut nc = NodeClearance::from_store(s.clone(), "KIRRA-DEMO-03");
        immobilize(&mut nc, 2); // EscalationRaised

        // A grant exists, but for ANOTHER node.
        let now = current_time_ms();
        s.with(|store| store.save_clearance_grant_chained("KIRRA-DEMO-06", "mallory", now))
            .expect("record grant for a different node");

        let infer = build_loop(0.1, 0.2);
        let out = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            infer,
            fresh_frame(5),
            SafetyPosture::Nominal,
            Some(&mut nc),
            &ImpactInputs::default(),
            None,
        )
        .await;

        assert_eq!(
            out.delivery,
            Some(DeliveryOutcome::NoGrant),
            "a grant scoped to another node must NOT be picked up"
        );
        assert!(
            nc.is_immobilized(),
            "loop stays immobilized — nothing cleared it"
        );
        assert!(out.vetoed, "still immobilized → still vetoed to a stop");
        assert_eq!(out.tick.twist.linear_x_mps, 0.0);
    }

    // -----------------------------------------------------------------------
    // #309 — DETECTION-ARMED tick. The loop now latches from live evidence
    // assembled inside `run_pipeline_tick_with_clearance`, not only via the
    // manual `observe` driver.
    // -----------------------------------------------------------------------

    /// An IMU sample whose linear-acceleration vector has magnitude `mag` (m/s²)
    /// along x. orientation absent (the decel proxy is convention-free).
    fn imu_accel_mag(mag: f32) -> ImuSample {
        ImuSample {
            linear_acceleration: [mag, 0.0, 0.0],
            angular_velocity: [0.0, 0.0, 0.0],
            orientation: None,
        }
    }

    /// One Nominal-posture tick driving the gate with `inputs` (no scene).
    async fn tick(nc: &mut NodeClearance, inputs: &ImpactInputs) -> ClearedTickOutcome {
        run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            build_loop(0.1, 0.2),
            fresh_frame(900),
            SafetyPosture::Nominal,
            Some(nc),
            inputs,
            None,
        )
        .await
    }

    /// DECEL: a spike above threshold latches the loop and the tick publishes a
    /// stop, even at Nominal posture — the vehicle latches ITSELF.
    #[tokio::test(start_paused = true)]
    async fn decel_spike_latches_and_stops_at_nominal() {
        let infer = build_loop(0.1, 0.2);
        let mut nc = NodeClearance::from_store(store(), "KIRRA-DEMO-03");
        assert_eq!(nc.state(), ClearanceState::Normal);

        // 40 m/s² total accel > the 30 m/s² default threshold → impact.
        let inputs = ImpactInputs {
            imu: Some(imu_accel_mag(40.0)),
            contact: false,
        };
        let out = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            infer,
            fresh_frame(10),
            SafetyPosture::Nominal,
            Some(&mut nc),
            &inputs,
            None,
        )
        .await;

        assert!(
            nc.is_immobilized(),
            "a decel spike must latch the loop from the tick"
        );
        assert!(out.vetoed, "latched → stop regardless of posture");
        assert_eq!(out.tick.twist.linear_x_mps, 0.0);
        assert_eq!(out.tick.twist.angular_z_rads, 0.0);
    }

    /// CONTACT: a contact-sensor true latches the loop and stops the vehicle.
    #[tokio::test(start_paused = true)]
    async fn contact_latches() {
        let infer = build_loop(0.1, 0.2);
        let mut nc = NodeClearance::from_store(store(), "KIRRA-DEMO-03");

        let inputs = ImpactInputs {
            imu: None,
            contact: true,
        };
        let out = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            infer,
            fresh_frame(11),
            SafetyPosture::Nominal,
            Some(&mut nc),
            &inputs,
            None,
        )
        .await;

        assert!(
            nc.is_immobilized(),
            "contact=true is a definitive impact → latch"
        );
        assert!(out.vetoed);
        assert_eq!(out.tick.twist.linear_x_mps, 0.0);
    }

    /// VANISHED (#309): with the detector ARMED and a scene sourced per tick, a
    /// close agent that VANISHES between frames latches the loop and stops the
    /// vehicle at Nominal — the end-to-end pure path the node's scene sourcing
    /// drives (`Some(&scene)` instead of the old `None`).
    #[tokio::test(start_paused = true)]
    async fn vanished_object_latches_from_scene() {
        let mut nc = NodeClearance::from_store(store(), "KIRRA-DEMO-03")
            .with_vanished_detection(VanishedCfg::default());
        let no_impact = ImpactInputs {
            imu: Some(imu_accel_mag(9.81)),
            contact: false,
        };

        // Tick 1 — a close agent 1 m ahead (gap ≤ r_close 2.0): opens the
        // close-agent obligation; nothing has vanished yet → no latch.
        let close = AgentScene::Agents(vec![parko_core::RssAgent {
            ego_vel: 0.0,
            lead_vel: 0.0,
            actual_longitudinal_gap_m: 1.0,
            ego_lat_vel: 0.0,
            obj_lat_vel: 0.0,
            actual_lateral_separation_m: 0.0,
            oncoming: false,
        }]);
        let out1 = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            build_loop(0.1, 0.2),
            fresh_frame(20),
            SafetyPosture::Nominal,
            Some(&mut nc),
            &no_impact,
            Some(&close),
        )
        .await;
        assert_eq!(
            nc.state(),
            ClearanceState::Normal,
            "a present close agent must not latch"
        );
        assert!(!out1.vetoed);

        // Tick 2 — perception ran and is verified-empty (`KnownEmpty`): the close
        // agent vanished within the plausibility horizon → latch + stop.
        let out2 = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            build_loop(0.1, 0.2),
            fresh_frame(21),
            SafetyPosture::Nominal,
            Some(&mut nc),
            &no_impact,
            Some(&AgentScene::KnownEmpty),
        )
        .await;
        assert!(
            nc.is_immobilized(),
            "a close agent that vanished must latch the loop"
        );
        assert!(out2.vetoed, "latched → stop regardless of posture");
        assert_eq!(out2.tick.twist.linear_x_mps, 0.0);
    }

    /// VANISHED NEGATIVE (#309): the same armed loop, but with the detector fed
    /// `None`/`Absent` scenes (the unarmed-source path) NEVER latches — a missing
    /// scene is a gap, never a fabricated vanish.
    #[tokio::test(start_paused = true)]
    async fn vanished_detector_with_no_scene_never_latches() {
        let mut nc = NodeClearance::from_store(store(), "KIRRA-DEMO-03")
            .with_vanished_detection(VanishedCfg::default());
        let no_impact = ImpactInputs {
            imu: Some(imu_accel_mag(9.81)),
            contact: false,
        };
        for i in 0..5 {
            let out = run_pipeline_tick_with_clearance(
                &ParkoNodeConfig::default(),
                build_loop(0.1, 0.2),
                fresh_frame(30 + i),
                SafetyPosture::Nominal,
                Some(&mut nc),
                &no_impact,
                None, // no scene source this tick
            )
            .await;
            assert_eq!(
                nc.state(),
                ClearanceState::Normal,
                "tick {i}: no scene must never latch"
            );
            assert!(!out.vetoed);
        }
    }

    /// NO-FALSE-LATCH: gravity-only IMU (≈9.81 m/s², below threshold) + no
    /// contact must NEVER latch, across many ticks. The proof a static / cruising
    /// vehicle is not spuriously immobilized.
    #[tokio::test(start_paused = true)]
    async fn no_signals_never_latch_across_many_ticks() {
        let mut nc = NodeClearance::from_store(store(), "KIRRA-DEMO-03");
        // gravity baseline magnitude, well under the 30 m/s² threshold.
        let inputs = ImpactInputs {
            imu: Some(imu_accel_mag(9.81)),
            contact: false,
        };

        for i in 0..50 {
            let infer = build_loop(0.1, 0.2);
            let out = run_pipeline_tick_with_clearance(
                &ParkoNodeConfig::default(),
                infer,
                fresh_frame(200 + i),
                SafetyPosture::Nominal,
                Some(&mut nc),
                &inputs,
                None,
            )
            .await;
            assert_eq!(
                nc.state(),
                ClearanceState::Normal,
                "tick {i}: must not latch on sub-threshold accel"
            );
            assert!(!out.vetoed, "tick {i}: no veto");
            assert!(
                (out.tick.twist.linear_x_mps - 0.1).abs() < 1e-4,
                "tick {i}: command flows (no spurious stop); got {}",
                out.tick.twist.linear_x_mps
            );
        }
    }

    /// THE ORDERING PROOF (held line): a pending grant AND new impact evidence on
    /// the SAME tick. Delivery (step 1) clears the OLD escalation; detection
    /// (step 2) re-latches on the new evidence; the loop ENDS the tick latched and
    /// the command is stopped. The grant could not clear the new impact.
    #[tokio::test(start_paused = true)]
    async fn delivery_before_detection_ordering_holds() {
        let s = store();
        let mut nc = NodeClearance::from_store(s.clone(), "KIRRA-DEMO-03");
        immobilize(&mut nc, 2); // -> EscalationRaised (the OLD incident)
        assert_eq!(nc.state(), ClearanceState::EscalationRaised);

        // A grant is pending for the OLD escalation.
        let now = current_time_ms();
        s.with(|store| store.save_clearance_grant_chained("KIRRA-DEMO-03", "alice", now))
            .expect("record grant");

        // Same tick: a NEW impact arrives (contact).
        let infer = build_loop(0.1, 0.2);
        let inputs = ImpactInputs {
            imu: None,
            contact: true,
        };
        let out = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            infer,
            fresh_frame(12),
            SafetyPosture::Nominal,
            Some(&mut nc),
            &inputs,
            None,
        )
        .await;

        assert!(
            matches!(out.delivery, Some(DeliveryOutcome::Cleared { .. })),
            "the grant is consumed against the OLD escalation; got {:?}",
            out.delivery
        );
        assert_eq!(
            nc.state(),
            ClearanceState::EscalationRaised,
            "detection re-latches AFTER delivery (one step, #328) — the grant cannot clear the new impact"
        );
        assert!(out.vetoed, "re-latched → still stopped");
        assert_eq!(out.tick.twist.linear_x_mps, 0.0);
    }

    /// FULL VEHICLE LIFECYCLE, now detection-armed: live impact latches → escalates
    /// → operator grant clears → motion resumes, all driven through the tick.
    #[tokio::test(start_paused = true)]
    async fn full_lifecycle_detect_clear_resume() {
        let s = store();
        let mut nc = NodeClearance::from_store(s.clone(), "KIRRA-DEMO-03");
        let no_impact = ImpactInputs::default();

        // Tick 1: a decel spike LATCHES the loop and raises escalation in one step (#328).
        let spike = ImpactInputs {
            imu: Some(imu_accel_mag(40.0)),
            contact: false,
        };
        let out1 = tick(&mut nc, &spike).await;
        assert_eq!(
            nc.state(),
            ClearanceState::EscalationRaised,
            "tick 1 latches + escalates (#328)"
        );
        assert!(out1.vetoed);

        // Tick 2: no new impact — stays escalated (operator-required), still vetoed.
        let out2 = tick(&mut nc, &no_impact).await;
        assert_eq!(
            nc.state(),
            ClearanceState::EscalationRaised,
            "tick 2 stays escalated"
        );
        assert!(out2.vetoed, "still immobilized → still stopped");

        // The operator records a grant.
        let now = current_time_ms();
        s.with(|store| store.save_clearance_grant_chained("KIRRA-DEMO-03", "alice", now))
            .expect("record grant");

        // Tick 3: delivery clears the loop; no new impact → veto lifts, motion resumes.
        let out3 = tick(&mut nc, &no_impact).await;
        assert!(
            matches!(out3.delivery, Some(DeliveryOutcome::Cleared { .. })),
            "tick 3 delivers the grant; got {:?}",
            out3.delivery
        );
        assert_eq!(
            nc.state(),
            ClearanceState::Normal,
            "loop recovers to Normal"
        );
        assert!(!out3.vetoed, "veto lifts");
        assert!(
            (out3.tick.twist.linear_x_mps - 0.1).abs() < 1e-4,
            "motion resumes — governed forward command; got {}",
            out3.tick.twist.linear_x_mps
        );

        // Tick 4: stays recovered, command continues.
        let out4 = tick(&mut nc, &no_impact).await;
        assert_eq!(nc.state(), ClearanceState::Normal);
        assert!(!out4.vetoed);
    }
}
