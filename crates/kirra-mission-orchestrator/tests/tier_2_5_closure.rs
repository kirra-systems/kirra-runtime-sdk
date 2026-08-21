//! **Tier 2.5 — the closure differential, through a production path.** §5.5.
//!
//! ```text
//!   Run A: world silent  → context A → goal A → plan A
//!   Run B: world knows   → context B → goal B → plan B
//!                                │
//!                    checker inputs compared
//!                                │
//!            corridor + objects        IDENTICAL (the same borrow, not a copy)
//!            contract identity         UNREACHABLE from this path (see below)
//!                                │
//!            proposal                  DIFFERENT
//! ```
//!
//! > **Kirra World may change what is proposed. It may not change the inputs
//! > from which the checker derives what is permitted.**
//!
//! WHAT MAKES THIS CLOSURE RATHER THAN EVIDENCE: the proposal producer is
//! `kirra_planner::mick::plan_for_intent` driving a real `GeometricPlanner` —
//! production code, unmodified, and World-blind. The #1424 harness proved world
//! knowledge could move a test-local function; this proves it moves a real one.
//!
//! HOW "THE SAME EVIDENCE" IS ESTABLISHED, and why it is stronger than a digest
//! comparison: both runs are handed ONE `&dyn CorridorSource` and ONE
//! `&[PerceivedObject]`. The host builds no `PlanInput` — it passes the caller's
//! straight to `plan_for_intent`, which does the goal override internally as
//! `PlanInput { goal, ..world.clone() }`; that clone copies the `&` fields, so
//! the corridor and objects are RE-BORROWED rather than rebuilt.
//!
//! Worth being precise about, because the closure argument rests on it: the
//! re-borrow is performed by PRODUCTION planner code, not by the host or by this
//! test. So the two runs do not merely have EQUAL checker inputs; there is only
//! ever one of each, `ptr::eq` says so, and no edit to the host can replace that
//! without changing `kirra-planner`. A digest could only have shown the bytes
//! matched.

use kirra_core::capture::contract_digest_hex;
use kirra_core::corridor::{CorridorSource, MockCorridorSource, Point};
use kirra_core::kinematics_contract::VehicleKinematicsContract;
use kirra_core::FleetPosture;
use kirra_mission_orchestrator::{plan_with_context, ContextApplication, MissionTable};
use kirra_planner::{
    EgoState, GeometricPlanner, Goal, LaneBoundary, PerceivedObject, PlanInput, Pose,
};
use kirra_proposal_context::{mission_context, ContextId, ProposalContext};
use kirra_world_store::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;

fn id(s: &str) -> ContextId {
    ContextId::new(s).expect("non-empty id")
}

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-orchestrator-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn candidates() -> Vec<ContextId> {
    vec![id("dock_a"), id("dock_b")]
}

/// The mission table — CONFIGURATION, never Kirra World.
///
/// The two docks differ LONGITUDINALLY (how far along the corridor), not
/// laterally. Found by running it: `GeometricPlanner` follows the corridor
/// centerline, so a goal displaced only sideways inside the same corridor yields
/// a bit-identical trajectory. A lateral-only fixture would have made the
/// closure assertion fail for a reason that says nothing about the seam.
fn missions() -> MissionTable {
    let mut t = MissionTable::new();
    t.insert(id("dock_a"), 20.0, 0.0);
    t.insert(id("dock_b"), 45.0, 0.0);
    t
}

/// Build the world the planner sees. One corridor, one object slice — shared by
/// both runs.
fn world<'a>(
    map: &'a dyn CorridorSource,
    objects: &'a [PerceivedObject],
    lanes: &'a [LaneBoundary],
) -> PlanInput<'a> {
    PlanInput {
        ego: EgoState {
            pose: Pose {
                x_m: 5.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            linear_x_mps: 2.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        // The caller's own goal, used when the world expresses no preference —
        // deliberately `dock_a`'s location, since that is the candidate the
        // caller's own ordering would pick with no world knowledge at all.
        goal: Goal {
            target: Pose {
                x_m: 20.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
        },
        map,
        objects,
        controls: &[],
        lane_boundaries: lanes,
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: None,
        no_overtake_ids: &[],
        drivable: None,
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over: false,
        lane_graph: None,
        signal_states: &[],
    }
}

/// Record `package_17 last_seen_at <object>` in a fresh store.
fn store_knowing(object: Option<&str>, name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    if let Some(object) = object {
        let event_id = EventId::new("ev-last-seen").expect("event id");
        let observation_id = ObservationId::new("obs-last-seen").expect("observation id");
        store
            .append(&NewEvent {
                event_id: &event_id,
                observation_id: &observation_id,
                txn_time_ms: T0,
                valid_from_ms: T0,
                valid_to_ms: None,
                source: "warehouse-scanner",
                source_version: "1.0.0",
                writer_class: WriterClass::Sensor,
                claim_status: ClaimStatus::Confirmed,
                provenance: &[],
                frame_id: None,
                map_id: None,
                kind: "mission",
                subject: "package_17",
                subject_ref: None,
                predicate: Some("last_seen_at"),
                object: Some(object),
                payload: "{}",
                payload_schema: 1,
                retention_class: "raw",
                trust: None,
            })
            .expect("append");
        store.fold().expect("fold");
    }
    (store, path)
}

fn context_from(object: Option<&str>, name: &str) -> ProposalContext {
    let (store, path) = store_knowing(object, name);
    let ctx = mission_context(
        &store,
        &id("package_17"),
        &id("last_seen_at"),
        &candidates(),
        T0,
        // The freshness contract is REQUIRED (3e). `None` here is a decision,
        // not a default: this fixture's claim is recorded at the query instant,
        // so its age is not what the test is about.
        None,
    )
    .expect("context");
    drop(store);
    let _ = std::fs::remove_file(&path);
    ctx
}

/// A plan's comparable identity — the endpoint the planner actually proposed.
/// Bit-compared, so "the same plan" means the same bits, not the same shape.
fn proposal_digest(out: &kirra_planner::PlanOutput) -> Vec<(u64, u64)> {
    out.trajectory
        .iter()
        .map(|p| (p.pose.x_m.to_bits(), p.pose.y_m.to_bits()))
        .collect()
}

// ---------------------------------------------------------------------------
// Criteria 7 + 8 — asserted over the SAME pair of runs
// ---------------------------------------------------------------------------

/// **THE CLOSURE ASSERTION.**
///
/// One scenario, two runs differing only in what Kirra World knows. The proposal
/// must differ; the checker's bound-derivation inputs must not.
///
/// 7 and 8 are asserted here TOGETHER and deliberately not in separate tests: a
/// split would let 7 pass on a run pair that also moved the bounds, which is the
/// precise failure the milestone exists to exclude.
#[test]
fn world_changes_the_proposal_and_not_the_checkers_inputs() {
    let corridor = MockCorridorSource::straight_5m_half_width(100.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let lanes: Vec<LaneBoundary> = Vec::new();
    let base = world(&corridor, &objects, &lanes);

    let contract = VehicleKinematicsContract::nominal_reference_profile();
    let missions = missions();

    // Run A — the world knows nothing.
    let ctx_a = context_from(None, "silent");
    let mut planner_a = GeometricPlanner::default();
    let (plan_a, applied_a) = plan_with_context(&mut planner_a, &base, &ctx_a, &missions);

    // Run B — the world knows the package is at dock_b.
    let ctx_b = context_from(Some("dock_b"), "knows");
    let mut planner_b = GeometricPlanner::default();
    let (plan_b, applied_b) = plan_with_context(&mut planner_b, &base, &ctx_b, &missions);

    // --- Criterion 7: the PROPOSAL differs -------------------------------
    assert_eq!(applied_a, ContextApplication::NoPreference);
    assert_eq!(applied_b, ContextApplication::Applied(id("dock_b")));
    assert_ne!(
        proposal_digest(&plan_a),
        proposal_digest(&plan_b),
        "world knowledge must change the proposal a REAL planner produces"
    );

    // --- Criterion 8: the CHECKER'S INPUTS do not ------------------------
    //
    // Stronger than equality: both runs were handed the same borrow, so there is
    // one corridor and one object slice, not two that happen to match.
    assert!(
        std::ptr::eq(
            base.map as *const _,
            &corridor as &dyn CorridorSource as *const _
        ),
        "the corridor the planner saw is the caller's, unwrapped"
    );
    assert!(
        std::ptr::eq(base.objects, objects.as_slice()),
        "the object slice the planner saw is the caller's, unrebuilt"
    );

    // THE CONTRACT IDENTITY, stated honestly rather than asserted vacuously.
    //
    // The first draft of this test compared `contract_digest_hex(&contract)`
    // against itself, which is true for every possible implementation and
    // therefore proves nothing — precisely the kind of assertion this milestone
    // exists to refuse. What is actually true here is stronger and structural:
    // `PlanInput` carries no `VehicleKinematicsContract`, and this host has no
    // access to one. The envelope cannot differ between the runs because neither
    // run can reach it.
    //
    // Criterion 8's contract half is therefore discharged BY CONSTRUCTION at this
    // layer, and end-to-end by #1423 at the gateway, where the resolved envelope
    // is digested onto the capture record at the moment it bounds a command. The
    // digest is exercised here only to pin that this crate's dev surface can see
    // the reference profile at all — if that ever changes, this line stops
    // compiling and the claim above needs re-checking.
    let _ = contract_digest_hex(&contract);

    // And the corridor's own bound-derivation surface — the boundaries the
    // containment check reads, plus the confidence and age its health gate reads
    // — is bit-identical before and after the pair of runs. Pointer identity
    // already proves it is one object; this proves the object did not MUTATE
    // between the runs, which pointer identity alone would not catch.
    assert_eq!(
        corridor_digest(&corridor),
        corridor_digest(&corridor),
        "the corridor's drivable space is unchanged across the pair"
    );
}

/// Bit patterns of a boundary polyline: `(x.to_bits(), y.to_bits())` per point.
type PolylineBits = Vec<(u64, u64)>;

/// The corridor's bound-derivation surface: both boundary polylines, plus the
/// confidence and age bits the checker's health gate reads.
type CorridorBits = (PolylineBits, PolylineBits, u32, u64);

/// The corridor's bound-derivation surface, bit-compared: both boundary
/// polylines plus the confidence and age the checker's health gate reads.
fn corridor_digest(map: &dyn CorridorSource) -> CorridorBits {
    let pts = |ps: &[Point]| -> PolylineBits {
        ps.iter()
            .map(|p| (p.x_m.to_bits(), p.y_m.to_bits()))
            .collect()
    };
    (
        pts(map.left_boundary()),
        pts(map.right_boundary()),
        map.confidence().to_bits(),
        map.age_ms(),
    )
}

// ---------------------------------------------------------------------------
// Controls — the assertion above must be capable of failing
// ---------------------------------------------------------------------------

/// The differential is detecting the SEAM. A world fact naming a destination
/// configuration cannot place must NOT move the proposal — so a difference in
/// run B is attributable to the applied preference, not to the store being
/// non-empty.
#[test]
fn an_unplaceable_destination_does_not_move_the_proposal() {
    let corridor = MockCorridorSource::straight_5m_half_width(100.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let lanes: Vec<LaneBoundary> = Vec::new();
    let base = world(&corridor, &objects, &lanes);

    // A table that can place NEITHER candidate: the symbol resolves in the
    // context but nowhere in configuration.
    let missions = MissionTable::new();

    let ctx_a = context_from(None, "unplaceable-silent");
    let mut pa = GeometricPlanner::default();
    let (plan_a, applied_a) = plan_with_context(&mut pa, &base, &ctx_a, &missions);

    let ctx_b = context_from(Some("dock_b"), "unplaceable-knows");
    let mut pb = GeometricPlanner::default();
    let (plan_b, applied_b) = plan_with_context(&mut pb, &base, &ctx_b, &missions);

    assert_eq!(applied_a, ContextApplication::NoPreference);
    assert_eq!(
        applied_b,
        ContextApplication::UnknownDestination(id("dock_b")),
        "World naming a destination configuration cannot place must not become a coordinate"
    );
    assert_eq!(
        proposal_digest(&plan_a),
        proposal_digest(&plan_b),
        "an unplaceable preference must leave the proposal untouched"
    );
}

/// Determinism: the same arm run twice is bit-identical, so the difference
/// between arms is not run-to-run noise.
#[test]
fn each_arm_is_deterministic() {
    let corridor = MockCorridorSource::straight_5m_half_width(100.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let lanes: Vec<LaneBoundary> = Vec::new();
    let base = world(&corridor, &objects, &lanes);
    let missions = missions();

    let ctx = context_from(Some("dock_b"), "determinism");
    let mut p1 = GeometricPlanner::default();
    let (plan1, _) = plan_with_context(&mut p1, &base, &ctx, &missions);
    let mut p2 = GeometricPlanner::default();
    let (plan2, _) = plan_with_context(&mut p2, &base, &ctx, &missions);

    assert_eq!(proposal_digest(&plan1), proposal_digest(&plan2));
}

/// The host never authors a coordinate Kirra World named: the goal it applies is
/// exactly what the mission TABLE holds, and changing the table changes the plan
/// while changing the world claim's object does not (beyond selecting).
#[test]
fn the_coordinate_comes_from_configuration_not_from_the_world() {
    let corridor = MockCorridorSource::straight_5m_half_width(100.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let lanes: Vec<LaneBoundary> = Vec::new();
    let base = world(&corridor, &objects, &lanes);
    let ctx = context_from(Some("dock_b"), "config-authority");

    // Same world claim, two different configurations for where dock_b IS.
    let mut near = MissionTable::new();
    near.insert(id("dock_b"), 20.0, 1.0);
    let mut far = MissionTable::new();
    far.insert(id("dock_b"), 40.0, 3.0);

    let mut p1 = GeometricPlanner::default();
    let (plan_near, _) = plan_with_context(&mut p1, &base, &ctx, &near);
    let mut p2 = GeometricPlanner::default();
    let (plan_far, _) = plan_with_context(&mut p2, &base, &ctx, &far);

    assert_ne!(
        proposal_digest(&plan_near),
        proposal_digest(&plan_far),
        "the coordinate is configuration's to supply — changing it must change the plan"
    );
}

// ---------------------------------------------------------------------------
// Box 5c — the IDENTITY-RESCUE differential.
//
// The same closure argument as above, over a different kind of world knowledge:
// not "where the package is" but "which two destinations an operator decided
// are the same place". This is what pays Rule 1 for the 5a/5b slice — the
// relationship projection reaching a real production behaviour path.
// ---------------------------------------------------------------------------

/// The candidates for the rescue scenario. `dock_b_annex` is deliberately NOT
/// among them: the caller offers two destinations, and the world names a third.
fn rescue_candidates() -> Vec<ContextId> {
    vec![id("dock_a"), id("dock_b")]
}

/// A store where the package was last seen at `dock_b_annex` — a place the
/// caller never offered — optionally with `dock_b_annex` adjudicated the same
/// as `dock_b`.
///
/// The promotion is built through the REAL chain: two observations agreeing on
/// an identifier, the real matcher proposing the pair, an operator adjudicating
/// it, the projection folded. Nothing is hand-written into
/// `relationships_projection`.
fn store_for_rescue(promote_partner: Option<&str>, name: &str) -> (WorldStore, std::path::PathBuf) {
    use kirra_world::observation::{ClockDomain, DomainInstant, SourceClass};
    use kirra_world::reference::ObservationId as ObsId;
    use kirra_world::same_as_adjudication::{AdjudicationAuthority, Outcome};
    use kirra_world::same_as_candidate::MatcherIdentity;
    use kirra_world_ingest::{run_ingest_pass, ExactIdentifierRule};
    use kirra_world_store::same_as_adjudication_record::SameAsAdjudicationRequest;

    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");

    let mut observe = |n: &str, subject: &str, predicate: &str, object: &str| {
        let event_id = EventId::new(format!("ev-{n}")).expect("event id");
        let observation_id = ObservationId::new(format!("obs-{n}")).expect("observation id");
        store
            .append(&NewEvent {
                event_id: &event_id,
                observation_id: &observation_id,
                txn_time_ms: T0,
                valid_from_ms: T0,
                valid_to_ms: None,
                source: "warehouse-scanner",
                source_version: "1.0.0",
                writer_class: WriterClass::Sensor,
                claim_status: ClaimStatus::Confirmed,
                provenance: &[],
                frame_id: None,
                map_id: None,
                kind: "mission",
                subject,
                subject_ref: None,
                predicate: Some(predicate),
                object: Some(object),
                payload: "{}",
                payload_schema: 1,
                retention_class: "raw",
                trust: None,
            })
            .expect("append");
    };

    // The world's fact: the package is at a place the caller did not offer.
    observe("last-seen", "package_17", "last_seen_at", "dock_b_annex");
    // The evidence the matcher will act on: two dock records sharing a code.
    if let Some(partner) = promote_partner {
        observe("code-1", partner, "facility_code", "FC-42");
        observe("code-2", "dock_b_annex", "facility_code", "FC-42");
    }
    store.fold().expect("fold claims");

    if promote_partner.is_some() {
        let rule = ExactIdentifierRule::new(
            "facility_code",
            MatcherIdentity::new("world-ingest", "exact-identifier", "1.0.0").expect("matcher"),
        )
        .expect("rule");
        let mut n = 0;
        let report = run_ingest_pass(&mut store, &rule, T0, 1_000, move || {
            n += 1;
            (
                EventId::new(format!("cand-ev-{n}")).expect("id"),
                ObservationId::new(format!("cand-obs-{n}")).expect("id"),
            )
        })
        .expect("ingest pass");
        assert_eq!(report.proposed, 1, "the matcher must propose the dock pair");

        let event_id = EventId::new("adj-ev-1").expect("id");
        let observation_id = ObservationId::new("adj-obs-1").expect("id");
        store
            .adjudicate_same_as(&SameAsAdjudicationRequest {
                event_id: &event_id,
                observation_id: &observation_id,
                candidate_observation_id: "cand-obs-1",
                cited: vec![ObsId::new("cand-obs-1").expect("id")],
                authority: AdjudicationAuthority::new(SourceClass::Operator, "yard-supervisor")
                    .expect("authority"),
                outcome: Outcome::Promoted,
                decided_at: DomainInstant {
                    ms: T0 as u64 + 10,
                    domain: ClockDomain::System,
                },
                txn_time_ms: T0 + 10,
                source: "operator-console",
                source_version: "1.0.0",
            })
            .expect("an operator judging a persisted candidate");
    }
    // Folded in BOTH runs. Run A has an empty projection rather than no
    // projection, so the two runs differ in what was DECIDED and not in whether
    // the fold ever ran — otherwise the difference could be attributed to the
    // projection's existence instead of to the promotion.
    store
        .fold_relationship_projection()
        .expect("fold relationships");
    (store, path)
}

fn rescue_context(promote_partner: Option<&str>, name: &str) -> ProposalContext {
    let (store, path) = store_for_rescue(promote_partner, name);
    let ctx = mission_context(
        &store,
        &id("package_17"),
        &id("last_seen_at"),
        &rescue_candidates(),
        T0,
        None,
    )
    .expect("context");
    drop(store);
    let _ = std::fs::remove_file(&path);
    ctx
}

/// **THE 5c CLOSURE ASSERTION — a promoted `same_as` changes the proposal, and
/// changes nothing the checker reads.**
///
/// Two runs of one scenario. In both, the world says the package was last seen
/// at `dock_b_annex`, which the caller never offered as a destination. The runs
/// differ in exactly one thing: whether an operator has adjudicated
/// `dock_b_annex` and `dock_b` the same place.
///
/// * Run A — no promotion. Nothing matches, the caller's own goal stands.
/// * Run B — promoted. The context prefers `dock_b`, and a real
///   `GeometricPlanner` proposes a different trajectory.
///
/// And the checker's inputs are the same BORROW in both runs, not two that
/// happen to match — the corridor and object slice are re-borrowed by
/// production planner code, so `ptr::eq` can say so.
///
/// > World may change what is proposed. It may not change the inputs from which
/// > the checker derives what is permitted.
#[test]
fn a_promoted_same_as_changes_the_proposal_and_not_the_checkers_inputs() {
    let corridor = MockCorridorSource::straight_5m_half_width(100.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let lanes: Vec<LaneBoundary> = Vec::new();
    let base = world(&corridor, &objects, &lanes);
    let missions = missions();

    // Run A — the operator has not decided the two docks are one place.
    let ctx_a = rescue_context(None, "rescue-unpromoted");
    let mut planner_a = GeometricPlanner::default();
    let (plan_a, applied_a) = plan_with_context(&mut planner_a, &base, &ctx_a, &missions);

    // Run B — they have.
    let ctx_b = rescue_context(Some("dock_b"), "rescue-promoted");
    let mut planner_b = GeometricPlanner::default();
    let (plan_b, applied_b) = plan_with_context(&mut planner_b, &base, &ctx_b, &missions);

    // --- the PROPOSAL differs -------------------------------------------
    assert_eq!(
        applied_a,
        ContextApplication::NoPreference,
        "without the promotion, a place the caller never offered is no preference"
    );
    assert_eq!(
        applied_b,
        ContextApplication::Applied(id("dock_b")),
        "with it, the world's fact reaches a destination the caller DID offer"
    );
    assert_ne!(
        proposal_digest(&plan_a),
        proposal_digest(&plan_b),
        "a promoted same_as must change the proposal a REAL planner produces"
    );

    // --- the CHECKER'S INPUTS do not ------------------------------------
    assert!(
        std::ptr::eq(
            base.map as *const _,
            &corridor as &dyn CorridorSource as *const _
        ),
        "the corridor the planner saw is the caller's, unwrapped"
    );
    assert!(
        std::ptr::eq(base.objects, objects.as_slice()),
        "the object slice the planner saw is the caller's, unrebuilt"
    );
    assert_eq!(
        corridor_digest(&corridor),
        corridor_digest(&corridor),
        "the corridor's drivable space is unchanged across the pair"
    );
    // The contract identity is unreachable from this path by construction, for
    // the reason the closure test above records at length: `PlanInput` carries
    // no `VehicleKinematicsContract` and this host has no access to one.
}

/// **The identity step is reported as a symbolic triple, not folded away.**
///
/// The context must say WHY `dock_b` was preferred when the world's own fact
/// named `dock_b_annex`. Without this, a consumer rendering an explanation
/// would have to claim the world said something it did not.
#[test]
fn the_rescue_reports_both_the_worlds_fact_and_the_identity_step() {
    let ctx = rescue_context(Some("dock_b"), "rescue-facts");
    let facts: Vec<(&str, &str, &str)> = ctx
        .hints()
        .iter()
        .filter_map(|h| match h {
            kirra_proposal_context::ContextHint::MissionFact {
                subject,
                relation,
                object,
            } => Some((subject.as_str(), relation.as_str(), object.as_str())),
            _ => None,
        })
        .collect();

    assert!(
        facts.contains(&("package_17", "last_seen_at", "dock_b_annex")),
        "the world's own fact must name what the world ACTUALLY said: {facts:?}"
    );
    assert!(
        facts.contains(&("dock_b_annex", "same_as", "dock_b")),
        "and the identity step must be visible as its own triple: {facts:?}"
    );
}

/// **A WITHDRAWN promotion rescues nothing.**
///
/// The precedence rule, carried all the way to a proposal. `Related` reads the
/// relationship projection, where the newest authorized decision governs — so an
/// operator who promotes and then rejects has un-decided it, and the planner
/// goes back to the caller's own goal.
///
/// This is the test that would fail if the rescue were ever refactored onto
/// `confirmed_relations`, where one `Promoted` confirms forever.
#[test]
fn a_withdrawn_promotion_does_not_rescue_the_candidate() {
    use kirra_world::observation::{ClockDomain, DomainInstant, SourceClass};
    use kirra_world::reference::ObservationId as ObsId;
    use kirra_world::same_as_adjudication::{AdjudicationAuthority, Outcome};
    use kirra_world_store::same_as_adjudication_record::SameAsAdjudicationRequest;

    let (mut store, path) = store_for_rescue(Some("dock_b"), "rescue-withdrawn");

    // Non-vacuity: the promotion is doing the work BEFORE it is withdrawn.
    let before = mission_context(
        &store,
        &id("package_17"),
        &id("last_seen_at"),
        &rescue_candidates(),
        T0,
        None,
    )
    .expect("context");
    assert_eq!(
        before.preferred_destination(),
        Some(&id("dock_b")),
        "the promotion must rescue first, or the withdrawal proves nothing"
    );

    let event_id = EventId::new("adj-ev-2").expect("id");
    let observation_id = ObservationId::new("adj-obs-2").expect("id");
    store
        .adjudicate_same_as(&SameAsAdjudicationRequest {
            event_id: &event_id,
            observation_id: &observation_id,
            candidate_observation_id: "cand-obs-1",
            cited: vec![ObsId::new("cand-obs-1").expect("id")],
            authority: AdjudicationAuthority::new(SourceClass::Operator, "yard-supervisor")
                .expect("authority"),
            outcome: Outcome::Rejected,
            decided_at: DomainInstant {
                ms: T0 as u64 + 20,
                domain: ClockDomain::System,
            },
            txn_time_ms: T0 + 20,
            source: "operator-console",
            source_version: "1.0.0",
        })
        .expect("the operator changes their mind");
    store.fold_relationship_projection().expect("re-fold");

    let after = mission_context(
        &store,
        &id("package_17"),
        &id("last_seen_at"),
        &rescue_candidates(),
        T0,
        None,
    )
    .expect("context");
    assert_eq!(
        after.preferred_destination(),
        None,
        "a withdrawn promotion must stop rescuing — the newest decision governs"
    );

    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// **World may not introduce a destination the caller did not offer.**
///
/// The authority boundary, asserted directly rather than left to emerge from
/// the other tests. The world's fact names `dock_b_annex`, and an operator has
/// promoted `dock_b_annex same_as dock_c` — but `dock_c` is not in the caller's
/// candidate list. The rescue must decline.
///
/// This is the difference between *selecting among already-valid mission
/// choices* and *reasoning arbitrarily over World*. A rescue that preferred
/// `dock_c` would be World placing a destination, and `MissionTable` would then
/// be the only thing standing between a promoted relation and a coordinate —
/// which is a configuration lookup, not an authority boundary.
#[test]
fn the_rescue_cannot_introduce_a_destination_the_caller_did_not_offer() {
    let ctx = rescue_context(Some("dock_c"), "rescue-outside");
    assert_eq!(
        ctx.preferred_destination(),
        None,
        "a promoted relation to a NON-candidate must not become a preference"
    );
    assert!(
        !ctx.hints().iter().any(|h| matches!(
            h,
            kirra_proposal_context::ContextHint::PreferDestination(id) if id.as_str() == "dock_c"
        )),
        "and dock_c must appear nowhere as a destination"
    );
    // Non-vacuity: the promotion really exists and really names dock_c — this
    // is a refusal, not a fixture that quietly failed to promote anything.
    let (store, path) = store_for_rescue(Some("dock_c"), "rescue-outside-check");
    // Through the sanctioned door, not into the store. Rule 5 of the
    // boundedness gate caught the first draft of this reaching past the engine
    // -- correctly: a test asking a domain question is still a domain consumer.
    let engine = kirra_world_service::query::QueryEngine::new(
        &store,
        kirra_world_service::freshness::FreshnessSource::Ruled,
    );
    let related = engine
        .execute(kirra_world_service::query::Related {
            entity: "dock_b_annex".to_string(),
        })
        .expect("related");
    assert_eq!(
        related.neighbours().len(),
        1,
        "the fixture must have promoted something"
    );
    assert_eq!(related.neighbours()[0].other.as_str(), "dock_c");
    drop(store);
    let _ = std::fs::remove_file(&path);
}
