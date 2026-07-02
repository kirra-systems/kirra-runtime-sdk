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
        let h = self.hidden(x);
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

    /// The post-tanh hidden activations — layer 1 of [`Self::forward`], factored out
    /// so PTQ calibration can observe the hidden-activation distribution.
    fn hidden(&self, x: &[f64; IN]) -> [f64; H] {
        let mut h = [0.0; H];
        for (hj, (w1row, b1j)) in h.iter_mut().zip(self.w1.iter().zip(self.b1.iter())) {
            let mut a = *b1j;
            for (w, xi) in w1row.iter().zip(x.iter()) {
                a += w * xi;
            }
            *hj = a.tanh();
        }
        h
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
pub(crate) struct Rng(pub(crate) u64);
impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
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
    pub(crate) fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub(crate) fn gaussian(&mut self) -> f64 {
        // Box-Muller.
        let u1 = self.unit().max(1e-12);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
    pub(crate) fn range(&mut self, lo: f64, hi: f64) -> f64 {
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
// Post-training quantization (PTQ) — Q-1a step 3
// ============================================================================
//
// A real per-tensor int8 quantization of the speed-only scorer, calibrated over a
// scenario corpus. Weights AND activations are quantized (symmetric per-tensor,
// int8). This is a QUALITY artifact: it snaps the scorer onto the int8 grid so the
// doer-eval harness can measure the argmax / admissibility delta vs. FP32
// (parko/QUANTIZATION_Q1_SCOPE.md §2). It is NOT a latency artifact — real int8
// kernel timing is Q-1b on the target silicon; here the dequantized matmul runs in
// f64 (a faithful *quality* model of int8, not a bit-exact kernel).
//
// PTQ is monotone-in-safety-relaxation = 0: the int8 scorer can only pick a
// different (still checker-admissible) vocabulary entry, or a more-conservative one.
// The checker bounds it exactly as it bounds the FP32 planner.

/// A scorer-backed planner exposing its chosen vocabulary index + materialized plan
/// in a single pass — the seam the doer-eval harness scores. Implemented by both the
/// FP32 [`LearnedPlanner`] and its int8 [`QuantizedLearnedPlanner`], so the harness
/// can compare them through one interface.
pub trait ScoredPlanner {
    /// The vocabulary index the scorer ranks highest for `input`.
    fn chosen_index(&self, input: &PlanInput) -> usize;
    /// The chosen index + materialized plan from one scorer pass.
    fn plan_with_chosen_index(&self, input: &PlanInput) -> (usize, PlanOutput);
}

impl ScoredPlanner for LearnedPlanner {
    fn chosen_index(&self, input: &PlanInput) -> usize {
        // Fully-qualified → the inherent method (no trait recursion).
        LearnedPlanner::chosen_index(self, input)
    }
    fn plan_with_chosen_index(&self, input: &PlanInput) -> (usize, PlanOutput) {
        LearnedPlanner::plan_with_chosen_index(self, input)
    }
}

// ----------------------------------------------------------------------------
// Offline export views (Q-1b) — the weights leave the crate, read-only
// ----------------------------------------------------------------------------

/// The FP32 scorer's trained weights, exported for OFFLINE artifact generation
/// (the ONNX export in `kirra-doer-eval`; Q-1b). Row-major, dims explicit so the
/// consumer does not depend on this crate's private layout constants. Read-only
/// snapshot — nothing here can write back into the planner.
#[derive(Debug, Clone, PartialEq)]
pub struct ScorerWeights {
    pub input_dim: usize,
    pub hidden_dim: usize,
    pub output_dim: usize,
    /// `hidden_dim × input_dim`, row-major (`w1[h * input_dim + i]`).
    pub w1: Vec<f64>,
    pub b1: Vec<f64>,
    /// `output_dim × hidden_dim`, row-major.
    pub w2: Vec<f64>,
    pub b2: Vec<f64>,
}

/// The int8-PTQ scorer's codes + scales, exported for OFFLINE artifact generation
/// (the QDQ ONNX export; Q-1b). The codes/scales are EXACTLY the ones the in-Rust
/// [`QuantizedLearnedPlanner`] runs on — one quantization, reused by every backend
/// (design note §6: the calibration artifact is produced once).
#[derive(Debug, Clone, PartialEq)]
pub struct QuantizedScorerWeights {
    pub input_dim: usize,
    pub hidden_dim: usize,
    pub output_dim: usize,
    /// `hidden_dim × input_dim` int8 codes, row-major.
    pub w1_codes: Vec<i8>,
    pub w1_scale: f64,
    pub b1: Vec<f64>,
    /// `output_dim × hidden_dim` int8 codes, row-major.
    pub w2_codes: Vec<i8>,
    pub w2_scale: f64,
    pub b2: Vec<f64>,
    /// Calibrated activation scales (input features / hidden activations).
    pub input_scale: f64,
    pub hidden_scale: f64,
}

impl LearnedPlanner {
    /// Export the trained FP32 scorer weights (offline artifact generation).
    #[must_use]
    pub fn scorer_weights(&self) -> ScorerWeights {
        ScorerWeights {
            input_dim: IN,
            hidden_dim: H,
            output_dim: K,
            w1: self.scorer.w1.iter().flat_map(|r| r.iter().copied()).collect(),
            b1: self.scorer.b1.to_vec(),
            w2: self.scorer.w2.iter().flat_map(|r| r.iter().copied()).collect(),
            b2: self.scorer.b2.to_vec(),
        }
    }

    /// The featurized input and the FP32 vocabulary scores for `input` — export /
    /// round-trip verification introspection (an exported ONNX model fed the same
    /// features must reproduce these scores within float tolerance).
    #[must_use]
    pub fn features_and_scores(&self, input: &PlanInput) -> (Vec<f64>, Vec<f64>) {
        let x = featurize(&scene_of(input));
        (x.to_vec(), self.scorer.forward(&x).to_vec())
    }
}

impl QuantizedLearnedPlanner {
    /// Export the int8 codes + calibrated scales (offline QDQ artifact generation).
    #[must_use]
    pub fn scorer_weights(&self) -> QuantizedScorerWeights {
        QuantizedScorerWeights {
            input_dim: IN,
            hidden_dim: H,
            output_dim: K,
            w1_codes: self.scorer.w1.iter().flat_map(|r| r.iter().copied()).collect(),
            w1_scale: self.scorer.s_w1,
            b1: self.scorer.b1.to_vec(),
            w2_codes: self.scorer.w2.iter().flat_map(|r| r.iter().copied()).collect(),
            w2_scale: self.scorer.s_w2,
            b2: self.scorer.b2.to_vec(),
            input_scale: self.scorer.s_x,
            hidden_scale: self.scorer.s_h,
        }
    }

    /// The int8 scorer's vocabulary scores for `input` — round-trip verification
    /// introspection (the exported QDQ ONNX model must reproduce these).
    #[must_use]
    pub fn scores(&self, input: &PlanInput) -> Vec<f64> {
        self.scorer.forward(&featurize(&scene_of(input))).to_vec()
    }
}

/// Symmetric per-tensor int8 scale: `absmax / 127`. A zero/degenerate tensor gets a
/// unit scale (all codes then 0) rather than a divide-by-zero.
pub(crate) fn int8_scale(absmax: f64) -> f64 {
    if absmax > 0.0 && absmax.is_finite() {
        absmax / 127.0
    } else {
        1.0
    }
}

/// Quantize one value to the symmetric int8 grid at `scale` (round-to-nearest,
/// clamped to `[-127, 127]`).
pub(crate) fn quantize_i8(v: f64, scale: f64) -> i8 {
    (v / scale).round().clamp(-127.0, 127.0) as i8
}

/// Fake-quantize one activation: quantize to int8 then dequantize back to `f64`.
pub(crate) fn fake_quant(v: f64, scale: f64) -> f64 {
    f64::from(quantize_i8(v, scale)) * scale
}

/// Per-tensor absmax over a 2-D weight matrix.
fn absmax2<const R: usize, const C: usize>(m: &[[f64; C]; R]) -> f64 {
    let mut a = 0.0_f64;
    for row in m {
        for &v in row {
            a = a.max(v.abs());
        }
    }
    a
}

/// Quantize a 2-D weight matrix to int8 codes at `scale`.
fn quantize_tensor2<const R: usize, const C: usize>(m: &[[f64; C]; R], scale: f64) -> [[i8; C]; R] {
    let mut out = [[0i8; C]; R];
    for (orow, mrow) in out.iter_mut().zip(m.iter()) {
        for (o, &v) in orow.iter_mut().zip(mrow.iter()) {
            *o = quantize_i8(v, scale);
        }
    }
    out
}

/// An int8-quantized copy of an [`Mlp`]: i8 weight codes + per-tensor weight scales,
/// plus per-tensor *activation* scales (`s_x` on the inputs, `s_h` on the hidden
/// layer) from calibration. Biases stay `f64`.
#[derive(Clone, Copy)]
struct QuantMlp<const M: usize> {
    w1: [[i8; IN]; H],
    b1: [f64; H],
    s_w1: f64,
    w2: [[i8; H]; M],
    b2: [f64; M],
    s_w2: f64,
    s_x: f64,
    s_h: f64,
}

impl<const M: usize> QuantMlp<M> {
    /// The int8 forward: activations are fake-quantized entering each matmul, weights
    /// are dequantized from their i8 codes; accumulation is `f64` (a *quality* model
    /// of int8, not a bit-exact kernel — that is the backend's job in Q-1b).
    fn forward(&self, x: &[f64; IN]) -> [f64; M] {
        let mut h = [0.0; H];
        for (hj, (w1row, b1j)) in h.iter_mut().zip(self.w1.iter().zip(self.b1.iter())) {
            let mut a = *b1j;
            for (w, xi) in w1row.iter().zip(x.iter()) {
                a += (f64::from(*w) * self.s_w1) * fake_quant(*xi, self.s_x);
            }
            *hj = a.tanh();
        }
        let mut y = [0.0; M];
        for (yk, (w2row, b2k)) in y.iter_mut().zip(self.w2.iter().zip(self.b2.iter())) {
            let mut a = *b2k;
            for (w, hj) in w2row.iter().zip(h.iter()) {
                a += (f64::from(*w) * self.s_w2) * fake_quant(*hj, self.s_h);
            }
            *yk = a;
        }
        y
    }
}

/// A [`LearnedPlanner`] whose scorer has been int8-quantized (PTQ). Same trajectory
/// vocabulary + teacher; the only difference is the scorer runs on the int8 grid.
#[derive(Clone, Copy)]
pub struct QuantizedLearnedPlanner {
    scorer: QuantMlp<K>,
    teacher: Teacher,
}

impl QuantizedLearnedPlanner {
    /// The teacher the underlying FP32 planner was distilled from.
    pub fn teacher(&self) -> Teacher {
        self.teacher
    }

    /// The calibrated scales `(w1, w2, input, hidden)` — all finite and `> 0` for a
    /// non-degenerate calibration. A hook for tests and the eval scorecard.
    pub fn scales(&self) -> (f64, f64, f64, f64) {
        (self.scorer.s_w1, self.scorer.s_w2, self.scorer.s_x, self.scorer.s_h)
    }
}

impl ScoredPlanner for QuantizedLearnedPlanner {
    fn chosen_index(&self, input: &PlanInput) -> usize {
        argmax(&self.scorer.forward(&featurize(&scene_of(input))))
    }
    fn plan_with_chosen_index(&self, input: &PlanInput) -> (usize, PlanOutput) {
        let scene = scene_of(input);
        let best = argmax(&self.scorer.forward(&featurize(&scene)));
        (best, PlanOutput { trajectory: materialize(&scene, TARGET_SPEEDS[best]), kind: ProposalKind::Motion })
    }
}

impl LearnedPlanner {
    /// **Post-training int8 quantization** (PTQ) of this planner's scorer, calibrated
    /// over `calibration` inputs. Weights are quantized per-tensor (symmetric int8);
    /// the activation scales — `s_x` on the input features, `s_h` on the hidden layer
    /// — are the absmax observed across the calibration corpus (a real PTQ
    /// calibration pass). Returns an int8 [`QuantizedLearnedPlanner`] with the same
    /// vocabulary + teacher.
    ///
    /// PTQ is a QUALITY operation, not a safety one (see the section header): the int8
    /// scorer's proposal is still bounded by the unchanged checker.
    pub fn quantize_int8(&self, calibration: &[PlanInput]) -> QuantizedLearnedPlanner {
        // Weight scales: per-tensor absmax over the (static) trained weights.
        let s_w1 = int8_scale(absmax2(&self.scorer.w1));
        let s_w2 = int8_scale(absmax2(&self.scorer.w2));

        // Activation scales: absmax of the input features and the hidden activations
        // observed over the calibration corpus.
        let mut ax = 0.0_f64;
        let mut ah = 0.0_f64;
        for input in calibration {
            let x = featurize(&scene_of(input));
            for &xi in &x {
                ax = ax.max(xi.abs());
            }
            for hj in self.scorer.hidden(&x) {
                ah = ah.max(hj.abs());
            }
        }
        let s_x = int8_scale(ax);
        let s_h = int8_scale(ah);

        QuantizedLearnedPlanner {
            scorer: QuantMlp {
                w1: quantize_tensor2(&self.scorer.w1, s_w1),
                b1: self.scorer.b1,
                s_w1,
                w2: quantize_tensor2(&self.scorer.w2, s_w2),
                b2: self.scorer.b2,
                s_w2,
                s_x,
                s_h,
            },
            teacher: self.teacher,
        }
    }
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

// ============================================================================
// PTQ unit tests — the quantization is a real, bounded perturbation
// ============================================================================

#[cfg(test)]
mod ptq_tests {
    use super::*;

    #[test]
    fn int8_scale_guards_degenerate_tensor() {
        assert_eq!(int8_scale(0.0), 1.0); // all-zero ⇒ unit scale, no div-by-zero
        assert_eq!(int8_scale(f64::NAN), 1.0);
        assert!((int8_scale(1.27) - 0.01).abs() < 1e-12); // 1.27 / 127
    }

    #[test]
    fn quantize_i8_clamps_to_range() {
        assert_eq!(quantize_i8(1000.0, 0.01), 127);
        assert_eq!(quantize_i8(-1000.0, 0.01), -127);
        assert_eq!(quantize_i8(0.0, 0.01), 0);
    }

    /// The load-bearing honesty test: the int8 forward genuinely DIFFERS from the
    /// FP32 forward on the same input (so a passing admissibility/argmax test upstream
    /// reflects real quantization robustness, not a silent passthrough) — yet the
    /// perturbation is small (a ranking MLP is quantization-tolerant).
    #[test]
    fn int8_forward_is_lossy_but_close() {
        // A hand-built scorer with clearly off-grid weights.
        let mut m = Mlp::<K> { w1: [[0.0; IN]; H], b1: [0.0; H], w2: [[0.0; H]; K], b2: [0.0; K] };
        for (j, row) in m.w1.iter_mut().enumerate() {
            for (i, w) in row.iter_mut().enumerate() {
                *w = 0.1234 * (j as f64 + 1.0) - 0.037 * (i as f64);
            }
        }
        for (k, row) in m.w2.iter_mut().enumerate() {
            for (j, w) in row.iter_mut().enumerate() {
                *w = 0.211 - 0.019 * (k as f64) + 0.007 * (j as f64);
            }
        }
        let x: [f64; IN] = [0.371, 0.62, 0.44, 1.0];

        let s_w1 = int8_scale(absmax2(&m.w1));
        let s_w2 = int8_scale(absmax2(&m.w2));
        let s_x = int8_scale(x.iter().fold(0.0_f64, |a, &v| a.max(v.abs())));
        let h = m.hidden(&x);
        let s_h = int8_scale(h.iter().fold(0.0_f64, |a, &v| a.max(v.abs())));
        let q = QuantMlp::<K> {
            w1: quantize_tensor2(&m.w1, s_w1),
            b1: m.b1,
            s_w1,
            w2: quantize_tensor2(&m.w2, s_w2),
            b2: m.b2,
            s_w2,
            s_x,
            s_h,
        };

        let yf = m.forward(&x);
        let yq = q.forward(&x);
        let moved = yf.iter().zip(yq.iter()).any(|(a, b)| (a - b).abs() > 1e-9);
        assert!(moved, "int8 forward must differ from fp32 — quantization is non-trivial");
        let maxdiff = yf.iter().zip(yq.iter()).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        assert!(maxdiff < 0.5, "int8 perturbation should be small, got {maxdiff}");
    }
}
