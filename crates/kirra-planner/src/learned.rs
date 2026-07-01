//! **A genuinely learned planner behind the generic `Planner` seam** — the literal
//! form of the §5 thesis: *KIRRA bounds a learned doer.*
//!
//! This is not a hand-coded heuristic dressed up as "learned." It is **Hydra-MDP-
//! shaped** (NVIDIA's CVPR-winning design): a fixed **trajectory vocabulary** +
//! a **learned scorer** that ranks the candidates; inference picks the top-scored
//! one. The scorer is a small MLP whose weights are **fit by optimization on data**
//! (a seeded `(1+1)` evolution strategy minimizing MSE to a teacher's per-candidate
//! scores) — exactly the "distill a rule-based teacher's verdict into the net" move
//! Hydra-MDP uses, in miniature and self-contained (no GPU, no model files, pure
//! Rust, deterministic from a seed).
//!
//! The point of the demo: a learned planner is **imperfect / can be misaligned**.
//! Trained with a safety-aware teacher it learns to slow for hazards (and KIRRA
//! admits it — the bound is *precise*); trained progress-only it learns to barrel
//! through (and KIRRA **rejects** it — the bound *catches* the misaligned net). The
//! safety case is invariant to which: KIRRA is the runtime bound the net answers to.

use crate::{PlanInput, PlanOutput, Planner, Pose, ProposalKind, TrajectoryPoint};

const IN: usize = 4; // features
const H: usize = 8; // hidden units
const K: usize = 4; // vocabulary size
/// ES steps for the speed-only scorer (4 outputs).
const SPEED_FIT_ITERS: usize = 6000;
/// ES steps for the maneuver scorer — the 2-D vocabulary is 3× wider (12 outputs), so it gets
/// a larger optimization budget to fit the per-candidate map well enough for a correct argmax.
const MANEUVER_FIT_ITERS: usize = 14_000;
const HORIZON: usize = 50; // == MAX_TRAJECTORY_HORIZON (KIRRA's WCET cap)
const DT: f64 = 0.1;
const ACCEL: f64 = 1.2; // m/s^2 toward a candidate's target speed
/// The candidate target speeds the scorer chooses among (the "trajectory vocabulary").
const TARGET_SPEEDS: [f64; K] = [0.0, 1.5, 3.0, 6.0];
/// Teacher proximity margin (m): a candidate that comes within this of a hazard is
/// penalized (safety-aware regime), a collision (< 1.5 m) heavily so.
const MARGIN: f64 = 10.0;

/// Which teacher the scorer was distilled from — the *only* difference between a
/// well-aligned learned planner and a misaligned one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Teacher {
    /// Rewards progress AND penalizes approaching a hazard at speed.
    SafetyAware,
    /// Rewards progress only — a stand-in for an imperfect / misaligned learned net.
    ProgressOnly,
}

/// The primitive world the scorer sees (extracted from a `PlanInput`, or synthesized
/// for training). Road is along +x in the demo frame.
#[derive(Clone, Copy)]
struct Scene {
    ego_x: f64,
    ego_y: f64,
    ego_speed: f64,
    goal_x: f64,
    goal_y: f64,
    obstacle_x: Option<f64>,
}

// ----------------------------------------------------------------------------
// Trajectory vocabulary + featurization (shared by training and inference)
// ----------------------------------------------------------------------------

/// Materialize a candidate: a straight line toward the goal whose speed integrates
/// from the ego's current speed toward `target` at `ACCEL`. Kinematically continuous,
/// within the horizon cap — so any KIRRA rejection is about the hazard, not the shape.
fn materialize(s: &Scene, target: f64) -> Vec<TrajectoryPoint> {
    let heading = (s.goal_y - s.ego_y).atan2(s.goal_x - s.ego_x);
    let (cos_h, sin_h) = (heading.cos(), heading.sin());
    let mut v = s.ego_speed.max(0.0);
    let mut arc = 0.0;
    (0..HORIZON)
        .map(|i| {
            let p = TrajectoryPoint {
                pose: Pose { x_m: s.ego_x + arc * cos_h, y_m: s.ego_y + arc * sin_h, heading_rad: heading },
                velocity_mps: v,
                time_from_start_s: i as f64 * DT,
            };
            arc += v * DT;
            let step = (ACCEL * DT).min((target - v).abs());
            v = (v + step * (target - v).signum()).max(0.0);
            p
        })
        .collect()
}

fn featurize(s: &Scene) -> [f64; IN] {
    let goal_dist = (s.goal_x - s.ego_x).hypot(s.goal_y - s.ego_y).min(50.0);
    let (present, obj_dist) = match s.obstacle_x {
        Some(ox) if ox > s.ego_x => (1.0, (ox - s.ego_x).min(50.0)),
        _ => (0.0, 50.0),
    };
    [s.ego_speed / 8.0, goal_dist / 50.0, obj_dist / 50.0, present]
}

/// Progress term: fraction of the max attainable forward travel the trajectory achieves.
fn progress_of(traj: &[TrajectoryPoint], ego_x: f64) -> f64 {
    (traj.last().unwrap().pose.x_m - ego_x) / (HORIZON as f64 * DT * TARGET_SPEEDS[K - 1])
}

/// The safety-aware hazard penalty (`0` for `ProgressOnly`, or no obstacle): heavy for a
/// collision (< 1.5 m), graded for approaching within `MARGIN`. The min-gap is measured to the
/// obstacle on the centerline, so a path that eases laterally away EARNS clearance — the same
/// term that rewards slowing also rewards routing around, which is what lets the 2-D
/// [`LearnedManeuverPlanner`] learn the pass.
fn hazard_penalty(traj: &[TrajectoryPoint], scene: &Scene, teacher: Teacher, margin: f64) -> f64 {
    let Some(ox) = scene.obstacle_x else { return 0.0 };
    if teacher != Teacher::SafetyAware {
        return 0.0;
    }
    let min_gap = traj.iter().map(|p| (p.pose.x_m - ox).hypot(p.pose.y_m)).fold(f64::MAX, f64::min);
    if min_gap < 1.5 {
        -5.0 // collision
    } else if min_gap < margin {
        -(1.0 - min_gap / margin) // approach-at-distance
    } else {
        0.0
    }
}

/// The teacher's score for one speed-only candidate — the signal the scorer is distilled to
/// predict. Uses the conservative `MARGIN`: with only a speed knob, "keep your distance" means
/// slow down, so a wide safe margin is what teaches the safety-aware net to brake early.
fn teacher_score(s: &Scene, target: f64, teacher: Teacher) -> f64 {
    let traj = materialize(s, target);
    progress_of(&traj, s.ego_x) + hazard_penalty(&traj, s, teacher, MARGIN)
}

// ----------------------------------------------------------------------------
// The learned scorer: a small MLP, weights fit by a seeded (1+1)-ES
// ----------------------------------------------------------------------------

/// A small two-layer MLP scorer, generic over its output width `M` (the vocabulary
/// size). `M = K` for the speed-only [`LearnedPlanner`]; `M = MANEUVER_K` for the 2-D
/// [`LearnedManeuverPlanner`]. The arithmetic and the RNG draw order are identical for a
/// given `M`, so generalizing the width does not perturb either planner's trained weights.
#[derive(Clone, Copy)]
struct Mlp<const M: usize> {
    w1: [[f64; IN]; H],
    b1: [f64; H],
    w2: [[f64; H]; M],
    b2: [f64; M],
}

impl<const M: usize> Mlp<M> {
    fn forward(&self, x: &[f64; IN]) -> [f64; M] {
        let mut h = [0.0; H];
        for (hj, (w1row, b1j)) in h.iter_mut().zip(self.w1.iter().zip(self.b1.iter())) {
            let mut a = *b1j;
            for (w, xi) in w1row.iter().zip(x.iter()) {
                a += w * xi;
            }
            *hj = a.tanh();
        }
        let mut y = [0.0; M];
        for (yk, (w2row, b2k)) in y.iter_mut().zip(self.w2.iter().zip(self.b2.iter())) {
            let mut a = *b2k;
            for (w, hj) in w2row.iter().zip(h.iter()) {
                a += w * hj;
            }
            *yk = a;
        }
        y
    }

    fn perturb(&mut self, rng: &mut Rng, sigma: f64) {
        for row in self.w1.iter_mut() {
            for w in row.iter_mut() {
                *w += rng.gaussian() * sigma;
            }
        }
        for b in self.b1.iter_mut() {
            *b += rng.gaussian() * sigma;
        }
        for row in self.w2.iter_mut() {
            for w in row.iter_mut() {
                *w += rng.gaussian() * sigma;
            }
        }
        for b in self.b2.iter_mut() {
            *b += rng.gaussian() * sigma;
        }
    }
}

/// Seeded xorshift64* PRNG — keeps training deterministic with no `rand` dependency.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn gaussian(&mut self) -> f64 {
        // Box-Muller.
        let u1 = self.unit().max(1e-12);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
}

/// One synthetic training scene: ego at the origin, goal far ahead, a hazard present ~60% of
/// the time at a random distance. The RNG draw order is fixed here so both planners train on
/// the SAME scene stream for a given seed (only their per-candidate labels differ).
fn training_scene(rng: &mut Rng) -> Scene {
    Scene {
        ego_x: 5.0,
        ego_y: 0.0,
        ego_speed: rng.range(0.0, 4.0),
        goal_x: 40.0,
        goal_y: 0.0,
        obstacle_x: if rng.unit() < 0.6 { Some(rng.range(12.0, 35.0)) } else { None },
    }
}

/// Fit an `Mlp<M>` to labelled `(features, per-candidate score)` data by a seeded `(1+1)`
/// evolution strategy (anneal σ 0.22 → 0.02 over 6000 steps). Deterministic given the RNG
/// state; shared by both learned planners so the speed-only and maneuvering scorers are fit by
/// the identical optimizer.
fn fit<const M: usize>(rng: &mut Rng, data: &[([f64; IN], [f64; M])], iters: usize) -> Mlp<M> {
    let loss = |m: &Mlp<M>| -> f64 {
        let mut acc = 0.0;
        for (x, y) in data {
            let p = m.forward(x);
            for k in 0..M {
                let e = p[k] - y[k];
                acc += e * e;
            }
        }
        acc / (data.len() * M) as f64
    };

    // Small random init.
    let mut best = Mlp::<M> { w1: [[0.0; IN]; H], b1: [0.0; H], w2: [[0.0; H]; M], b2: [0.0; M] };
    best.perturb(rng, 0.3);
    let mut best_loss = loss(&best);

    for t in 0..iters {
        let sigma = 0.2 * (1.0 - t as f64 / iters as f64) + 0.02; // anneal 0.22 → 0.02
        let mut cand = best;
        cand.perturb(rng, sigma);
        let l = loss(&cand);
        if l < best_loss {
            best = cand;
            best_loss = l;
        }
    }
    best
}

/// A learned planner: the trajectory vocabulary `TARGET_SPEEDS`, scored by a small
/// MLP distilled from `teacher`. Construct with [`LearnedPlanner::trained`].
pub struct LearnedPlanner {
    scorer: Mlp<K>,
    teacher: Teacher,
}

impl LearnedPlanner {
    /// Fit the scorer to the chosen teacher by a seeded `(1+1)` evolution strategy
    /// over synthetic scenes. Deterministic: same `(seed, teacher)` → same weights.
    pub fn trained(seed: u64, teacher: Teacher) -> Self {
        let mut rng = Rng::new(seed);

        // Synthetic training set: assorted ego speeds / goal distances, hazard present
        // ~60% of the time at a random distance ahead. Each scene labelled with the
        // teacher's per-candidate score vector.
        let mut data: Vec<([f64; IN], [f64; K])> = Vec::new();
        for _ in 0..160 {
            let scene = training_scene(&mut rng);
            let feats = featurize(&scene);
            let mut label = [0.0; K];
            for k in 0..K {
                label[k] = teacher_score(&scene, TARGET_SPEEDS[k], teacher);
            }
            data.push((feats, label));
        }

        LearnedPlanner { scorer: fit(&mut rng, &data, SPEED_FIT_ITERS), teacher }
    }

    /// The teacher this planner was distilled from (audit / test introspection).
    pub fn teacher(&self) -> Teacher {
        self.teacher
    }

    /// The vocabulary index the scorer ranks highest for `input` (test introspection).
    pub fn chosen_index(&self, input: &PlanInput) -> usize {
        let scores = self.scorer.forward(&featurize(&scene_of(input)));
        argmax(&scores)
    }

    /// The chosen vocabulary index **and** the materialized plan from a **single**
    /// scorer pass. `plan` + `chosen_index` each run a forward pass on the same
    /// features; a caller that needs both (e.g. the doer-eval harness, which scores
    /// the plan through the checker *and* compares the argmax to a reference) uses
    /// this to score once. `&self` — the argmax does not mutate the planner. The
    /// index rides alongside the [`PlanOutput`] rather than inside it: `PlanOutput`
    /// is the checker-consumed shape (PHASE-0 LOCKED) and gains no audit fields.
    pub fn plan_with_chosen_index(&self, input: &PlanInput) -> (usize, PlanOutput) {
        let scene = scene_of(input);
        let best = argmax(&self.scorer.forward(&featurize(&scene)));
        (best, PlanOutput { trajectory: materialize(&scene, TARGET_SPEEDS[best]), kind: ProposalKind::Motion })
    }
}

impl Planner for LearnedPlanner {
    fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
        let scene = scene_of(input);
        let best = argmax(&self.scorer.forward(&featurize(&scene)));
        PlanOutput { trajectory: materialize(&scene, TARGET_SPEEDS[best]), kind: ProposalKind::Motion }
    }
}

/// Extract the scorer's primitive [`Scene`] from a `PlanInput` — shared by both learned
/// planners. Nearest object ahead along +x (the demo road frame, obstacle assumed on the
/// centerline so the lateral gap is the path's own `y`).
fn scene_of(input: &PlanInput) -> Scene {
    let ego = input.ego.pose;
    let goal = input.goal.target;
    let obstacle_x = input
        .objects
        .iter()
        .map(|o| o.pos.x_m)
        .filter(|&x| x > ego.x_m)
        .fold(None, |acc: Option<f64>, x| Some(acc.map_or(x, |a| a.min(x))));
    Scene { ego_x: ego.x_m, ego_y: ego.y_m, ego_speed: input.ego.linear_x_mps, goal_x: goal.x_m, goal_y: goal.y_m, obstacle_x }
}

fn argmax<const M: usize>(v: &[f64; M]) -> usize {
    let mut best = 0;
    for (k, &val) in v.iter().enumerate() {
        if val > v[best] {
            best = k;
        }
    }
    best
}

// ============================================================================
// The 2-D maneuvering planner: a real Hydra-MDP trajectory vocabulary
// ============================================================================
//
// The speed-only LearnedPlanner above proved the thesis (KIRRA bounds a learned doer) but
// its vocabulary is four STRAIGHT-line speeds — it can only pick how fast to drive at the
// goal, never a path. Real Hydra-MDP scores a vocabulary of diverse trajectory *shapes*. This
// generalizes to a 2-D vocabulary (lateral offset × speed), so the learned scorer can choose
// to ROUTE AROUND a hazard. KIRRA stays the bound: a pass that fits the corridor is admitted,
// one that does not is rejected → safe-stop. Same MLP, same teacher, same seam.

/// Lateral pass offsets the vocabulary chooses among (m, +Y left). `0` = straight ahead;
/// `±4.5` clears KIRRA's 4 m RSS lateral-alignment band, so a pass is RSS-filtered rather than
/// longitudinally MRC'd (the same 4 m band #536/#451 turn on). A pass only *fits* where the
/// corridor is wide enough for the offset — else KIRRA rejects it (honest: a narrow road
/// cannot admit a >4 m pass).
const LATERAL_OFFSETS: [f64; 3] = [0.0, 4.5, -4.5];
/// Vocabulary size: every (lateral offset, target speed) pair.
const MANEUVER_K: usize = LATERAL_OFFSETS.len() * K;
/// Forward distance over which a candidate eases to its lateral offset, then holds it.
const TRANSITION_M: f64 = 12.0;

/// Smoothstep on `[0,1]` (clamped) — the lateral easing profile (C¹, zero end-slopes).
fn smoothstep01(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Candidate index → `(lateral offset, target speed)`. Offset-major so index `0` is
/// `(straight, stop)` and the straight-ahead candidates occupy the first `K` slots.
fn maneuver_candidate(c: usize) -> (f64, f64) {
    (LATERAL_OFFSETS[c / K], TARGET_SPEEDS[c % K])
}

/// Materialize a 2-D candidate: forward progress under the speed profile while easing the
/// lateral position to `offset` over the first [`TRANSITION_M`] and holding it — a
/// lane-change-and-stay shape, heading following the path tangent. Within the horizon cap, so
/// any KIRRA rejection is about the hazard / corridor fit, not the shape.
fn materialize_maneuver(s: &Scene, offset: f64, target: f64) -> Vec<TrajectoryPoint> {
    let mut x = s.ego_x;
    let mut v = s.ego_speed.max(0.0);
    let mut prev = (s.ego_x, s.ego_y);
    (0..HORIZON)
        .map(|i| {
            let y = s.ego_y + offset * smoothstep01((x - s.ego_x) / TRANSITION_M);
            let heading = if i == 0 { 0.0 } else { (y - prev.1).atan2((x - prev.0).max(1e-6)) };
            let p = TrajectoryPoint {
                pose: Pose { x_m: x, y_m: y, heading_rad: heading },
                velocity_mps: v,
                time_from_start_s: i as f64 * DT,
            };
            prev = (x, y);
            x += v * DT;
            let step = (ACCEL * DT).min((target - v).abs());
            v = (v + step * (target - v).signum()).max(0.0);
            p
        })
        .collect()
}

/// Per-metre cost of a candidate's TERMINAL lateral offset. Forward progress alone is blind to
/// lateral offset (the path advances in x regardless), so without this both teachers would be
/// indifferent to a gratuitous detour. A small cost makes straight the default — a pass is
/// preferred ONLY when its clearance avoids a hazard penalty (which dwarfs it); it never speeds
/// the vehicle laterally for nothing. Crucially this means a progress-only (misaligned) net
/// still barrels STRAIGHT through, exactly as in the speed-only demo.
const LATERAL_DETOUR_COST: f64 = 0.05;

/// The maneuver teacher's safe-clearance margin (m). Unlike the speed-only `MARGIN` (10 m, "keep
/// far"), this is aligned with KIRRA's 4 m RSS lateral-alignment band: a pass that clears the
/// band is genuinely safe, so the teacher must NOT keep penalizing it (else it would always
/// prefer slowing to passing, and the route-around would never be learned). A `LATERAL_OFFSETS`
/// pass of ±4.5 m clears this, so the safety-aware net learns the pass is both safe AND faster.
const MANEUVER_CLEAR_MARGIN: f64 = 4.0;

/// The teacher's score for one 2-D candidate. The speed-only teacher's progress + [`hazard_penalty`]
/// shape, over the maneuvered path (so a candidate easing laterally clear of the hazard earns the
/// clearance the penalty rewards — what teaches the safety-aware net to PASS, not just slow), minus
/// a small terminal-offset cost so the pass is never gratuitous. The penalty uses the KIRRA-aligned
/// [`MANEUVER_CLEAR_MARGIN`] so a band-clearing pass is scored safe.
fn teacher_score_maneuver(s: &Scene, offset: f64, target: f64, teacher: Teacher) -> f64 {
    let traj = materialize_maneuver(s, offset, target);
    let terminal_offset = (traj.last().unwrap().pose.y_m - s.ego_y).abs();
    progress_of(&traj, s.ego_x) - LATERAL_DETOUR_COST * terminal_offset
        + hazard_penalty(&traj, s, teacher, MANEUVER_CLEAR_MARGIN)
}

/// A **maneuvering** learned planner — [`LearnedPlanner`] generalized to a real 2-D
/// Hydra-MDP trajectory vocabulary (lateral offset × speed). The learned scorer can now route
/// AROUND a hazard, not merely slow for it; KIRRA remains the invariant bound. Distilled from
/// the same [`Teacher`] by the same optimizer ([`fit`]) over the same scene stream.
pub struct LearnedManeuverPlanner {
    scorer: Mlp<MANEUVER_K>,
    teacher: Teacher,
}

impl LearnedManeuverPlanner {
    /// Fit the 2-D scorer to `teacher` by the shared seeded `(1+1)`-ES. Deterministic: same
    /// `(seed, teacher)` → same weights. Trains on the SAME synthetic scenes as
    /// [`LearnedPlanner::trained`] (only the per-candidate labels differ — 12 candidates here).
    pub fn trained(seed: u64, teacher: Teacher) -> Self {
        let mut rng = Rng::new(seed);
        let mut data: Vec<([f64; IN], [f64; MANEUVER_K])> = Vec::new();
        for _ in 0..180 {
            let scene = training_scene(&mut rng);
            let feats = featurize(&scene);
            let mut label = [0.0; MANEUVER_K];
            for (c, slot) in label.iter_mut().enumerate() {
                let (off, spd) = maneuver_candidate(c);
                *slot = teacher_score_maneuver(&scene, off, spd, teacher);
            }
            data.push((feats, label));
        }
        LearnedManeuverPlanner { scorer: fit(&mut rng, &data, MANEUVER_FIT_ITERS), teacher }
    }

    /// The teacher this planner was distilled from (audit / test introspection).
    pub fn teacher(&self) -> Teacher {
        self.teacher
    }

    /// The `(lateral offset, target speed)` the scorer ranks highest for `input` (introspection).
    #[must_use]
    pub fn chosen_candidate(&self, input: &PlanInput) -> (f64, f64) {
        maneuver_candidate(argmax(&self.scorer.forward(&featurize(&scene_of(input)))))
    }
}

impl Planner for LearnedManeuverPlanner {
    fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
        let scene = scene_of(input);
        let (offset, speed) = maneuver_candidate(argmax(&self.scorer.forward(&featurize(&scene))));
        PlanOutput { trajectory: materialize_maneuver(&scene, offset, speed), kind: ProposalKind::Motion }
    }
}
