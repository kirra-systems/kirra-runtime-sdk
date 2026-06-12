// parko/crates/parko-ros2/src/clearance_gate.rs
//
// Phase-B deploy step (#304 deferral): wire `ClearanceDelivery` + a node-owned
// `ClearanceLoop` into the parko-ros2 tick, so a console-recorded operator
// clearance grant releases the vehicle on the node's OWN tick — no manual
// `deliver_clearance` example run.
//
// The two touches this adds to the tick (`run_pipeline_tick_with_clearance`):
//
//   1. DELIVERY — `poll_and_deliver` once per tick. Cheap NoGrant no-op when no
//      grant is pending (the Phase-B design point: pickup is idempotent-empty).
//      Done FIRST, so a grant delivered this tick releases the loop in time for
//      the veto below to lift on the SAME tick.
//
//   2. THE STOP TIE-IN (held line) — while the loop is immobilized (`Latched` OR
//      `EscalationRaised`, i.e. `ClearanceLoop::is_immobilized`), the tick
//      publishes the STOPPED command REGARDLESS of posture. This sits ALONGSIDE
//      the existing LockedOut stop path (which already lives inside the governor
//      inside `run_pipeline_tick_inner`): the latch is belt-and-suspenders with
//      posture, never a bypass of it. A delivered grant clears the loop back to
//      `Normal`; the veto then lifts and command publishing resumes.
//
// GROUNDING (surfaced, not bolted): the SG6 detection chain is NOT yet invoked
// from the tick — nothing here calls `ClearanceLoop::observe` from live impact
// evidence. This PR wires the loop + delivery (the half that releases the
// vehicle); FEEDING the latch from live detection is the named follow-up. Until
// that lands, the node-owned loop only ever leaves `Normal` in tests that drive
// it directly — which is exactly the wiring under test here.
//
// API note: `ClearanceDelivery::poll_and_deliver` takes `&mut ClearanceLoop`
// (the bare loop, as the `deliver_clearance` example does). `RecordedClearanceLoop`
// is the DETECTION-recording wrapper (it emits `ImpactDetected` /
// `ImpactEscalationRaised` on `observe`); it wraps a *private* `ClearanceLoop`, so
// it is not what delivery operates on. Since this PR wires delivery (not the live
// detection feed), the node owns the bare loop; the recording wrapper becomes
// relevant in the detection-feed follow-up.

use std::sync::{Arc, Mutex};

use parko_core::backend::InferenceBackend;
use parko_core::safety::SafetyPosture;
use parko_core::scheduler::InferenceLoop;
use parko_core::sensor::SensorFrame;
use parko_core::{ClearanceLoop, ClearanceState};
use tokio::sync::Mutex as AsyncMutex;

use kirra_runtime_sdk::verifier_store::VerifierStore;
use parko_kirra::clearance_delivery::{ClearanceDelivery, DeliveryOutcome};

use crate::command_mapping::OutgoingTwist;
use crate::config::ParkoNodeConfig;
use crate::tick_pipeline::{current_time_ms, run_pipeline_tick_inner, TickOutcome};

/// Node-owned clearance components: the per-vehicle [`ClearanceDelivery`] (store
/// pickup, scoped to THIS node's id) plus the node's own [`ClearanceLoop`] (the
/// SG6 motion veto). One per node; constructed at startup when delivery is
/// enabled, then borrowed `&mut` by the tick.
pub struct NodeClearance {
    delivery: ClearanceDelivery,
    clearance_loop: ClearanceLoop,
}

impl NodeClearance {
    /// Wrap a pre-built [`ClearanceDelivery`] with a fresh [`ClearanceLoop`].
    /// (The binary builds the delivery via [`ClearanceDelivery::open_signed`];
    /// tests build it from an in-memory store.)
    #[must_use]
    pub fn new(delivery: ClearanceDelivery) -> Self {
        Self {
            delivery,
            clearance_loop: ClearanceLoop::new(),
        }
    }

    /// Build a node clearance over an existing shared store handle and node id.
    /// The store-open + signing-key path is [`NodeClearance::open_signed`]; this
    /// is the seam tests use with an in-memory store.
    #[must_use]
    pub fn from_store(store: Arc<Mutex<VerifierStore>>, node_id: impl Into<String>) -> Self {
        Self::new(ClearanceDelivery::new(store, node_id))
    }

    /// Open the co-located store at `db_path`, install the base64 Ed25519 signing
    /// key so delivered-grant outcomes are signed, and build the gate for
    /// `node_id`. Fail-closed: an unopenable store / undecodable key is an `Err`.
    /// The node never names `base64` / `ed25519` itself — `parko-kirra` owns that.
    pub fn open_signed(db_path: &str, node_id: &str, signing_key_b64: &str) -> Result<Self, String> {
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
        self.delivery.poll_and_deliver(&mut self.clearance_loop, now_ms)
    }

    /// Drive one detection observation into the node-owned loop. This is the seam
    /// the live-detection-feed follow-up wires to real impact evidence; the tick
    /// integration here does NOT call it (detection-from-the-tick is out of scope).
    /// Exposed so tests can drive the loop into an immobilized state through the
    /// REAL state machine (never by poking internals).
    pub fn observe(
        &mut self,
        evidence: &parko_core::ImpactEvidence,
        cfg: &parko_core::ImpactCfg,
        now_ms: u64,
    ) {
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
    /// was immobilized after delivery). Lets the node log the held line.
    pub vetoed: bool,
}

/// Drive one tick with the node-owned clearance gate wired in. When `clearance`
/// is `None` (delivery disabled — the dev lane) this is exactly the existing
/// posture-gated tick with no veto and no pickup.
///
/// Order is load-bearing: deliver FIRST (a grant delivered this tick releases the
/// loop), then run the posture-gated tick, then apply the veto reading the
/// POST-delivery immobilization state — so a just-cleared loop resumes motion the
/// same tick, and a still-immobilized loop stops regardless of posture.
///
// SAFETY: SG6 | REQ: parko-ros2-clearance-veto-and-delivery | TEST: latched_loop_stops_tick_even_at_nominal,pending_grant_delivers_and_resumes,no_clearance_gate_ticks_normally,grant_for_other_node_not_picked_up
pub async fn run_pipeline_tick_with_clearance<B>(
    config: &ParkoNodeConfig,
    loop_mutex: Arc<AsyncMutex<InferenceLoop<B>>>,
    frame: SensorFrame,
    posture: SafetyPosture,
    clearance: Option<&mut NodeClearance>,
) -> ClearedTickOutcome
where
    B: InferenceBackend + 'static,
{
    let now_ms = current_time_ms();
    let mut clearance = clearance;

    // 1. DELIVERY — one pickup per tick (NoGrant no-op when nothing pending).
    let delivery = clearance.as_deref_mut().map(|c| c.poll(now_ms));

    // 2. The normal posture-gated tick (the LockedOut stop path lives inside).
    let mut tick = run_pipeline_tick_inner(config, loop_mutex, frame, posture).await;

    // 3. THE STOP TIE-IN — immobilized (post-delivery) → stop regardless of
    //    posture, alongside the governor's LockedOut stop.
    let vetoed = clearance.as_deref().is_some_and(NodeClearance::is_immobilized);
    if vetoed {
        tick.twist = OutgoingTwist::stopped(now_ms);
    }

    ClearedTickOutcome {
        tick,
        delivery,
        vetoed,
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
        let comparator = GovernorComparator::new(KirraGovernor::new(), KirraGovernor::new());
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

    fn store() -> Arc<Mutex<VerifierStore>> {
        Arc::new(Mutex::new(
            VerifierStore::new(":memory:").expect("in-memory store"),
        ))
    }

    /// Drive a `NodeClearance`'s loop into an immobilized state through the REAL
    /// state machine. `ticks` observes: 1 → `Latched`, 2 → `EscalationRaised`.
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
        immobilize(&mut nc, 1); // Normal -> Latched
        assert_eq!(nc.state(), ClearanceState::Latched);

        let out = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            infer,
            fresh_frame(1),
            SafetyPosture::Nominal,
            Some(&mut nc),
        )
        .await;

        assert!(out.vetoed, "an immobilized loop must veto motion regardless of posture");
        assert_eq!(out.tick.twist.linear_x_mps, 0.0, "latched → stopped twist (linear)");
        assert_eq!(out.tick.twist.angular_z_rads, 0.0, "latched → stopped twist (angular)");
        assert_eq!(out.delivery, Some(DeliveryOutcome::NoGrant), "no grant pending");
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
        s.lock()
            .unwrap()
            .save_clearance_grant_chained("KIRRA-DEMO-03", "alice", now)
            .expect("record grant");

        // ONE tick delivers it.
        let infer = build_loop(0.1, 0.2);
        let out = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            infer,
            fresh_frame(2),
            SafetyPosture::Nominal,
            Some(&mut nc),
        )
        .await;

        assert!(
            matches!(out.delivery, Some(DeliveryOutcome::Cleared { .. })),
            "the pending grant must be delivered this tick; got {:?}",
            out.delivery
        );
        assert_eq!(nc.state(), ClearanceState::Normal, "the loop clears back to Normal");
        assert!(!out.vetoed, "veto lifts the same tick the grant clears the loop");
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
        )
        .await;
        assert_eq!(out2.delivery, Some(DeliveryOutcome::NoGrant), "grant consumed — no retry");
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
        s.lock()
            .unwrap()
            .save_clearance_grant_chained("KIRRA-DEMO-06", "mallory", now)
            .expect("record grant for a different node");

        let infer = build_loop(0.1, 0.2);
        let out = run_pipeline_tick_with_clearance(
            &ParkoNodeConfig::default(),
            infer,
            fresh_frame(5),
            SafetyPosture::Nominal,
            Some(&mut nc),
        )
        .await;

        assert_eq!(
            out.delivery,
            Some(DeliveryOutcome::NoGrant),
            "a grant scoped to another node must NOT be picked up"
        );
        assert!(nc.is_immobilized(), "loop stays immobilized — nothing cleared it");
        assert!(out.vetoed, "still immobilized → still vetoed to a stop");
        assert_eq!(out.tick.twist.linear_x_mps, 0.0);
    }
}
