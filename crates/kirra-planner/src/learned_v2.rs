//! **The real-sized doer scorer (M-1; `parko/DOER_MODEL_SCALEUP.md`).**
//!
//! Scales the miniature [`crate::learned`] demo to a real Hydra-MDP shape while
//! keeping every seam it proved: a fixed **trajectory vocabulary** (lateral
//! offset × speed-profile grid, hundreds of candidates at full size), a
//! **learned scorer** that ranks it (an N-layer MLP over a ~32-dim scene
//! encoding), inference = argmax, and the same [`Teacher`] distillation signals
//! (`SafetyAware` / `ProgressOnly`) so the misalignment-detection story keeps
//! holding at scale.
//!
//! What actually changes vs. v1:
//!
//! - **Training is gradient descent, not evolution.** A v1-style `(1+1)`-ES
//!   cannot fit ~10⁵ parameters; this module implements seeded mini-batch
//!   **SGD + momentum backprop** in plain Rust (no deps, `f64` accumulation,
//!   the same xorshift RNG as v1). A finite-difference **gradient check** in the
//!   tests proves the backprop math.
//! - **The scene encoding is real**: ego kinematics, goal, corridor clearance
//!   (sampled from the [`CorridorSource`] boundary polylines), and the K=4
//!   nearest objects ahead with relative position AND velocity — zero-padded
//!   slots for absent objects (absent = zeros, fail-safe).
//! - **Config-driven sizing** ([`ScorerConfigV2`]): the full model
//!   (~256 candidates, `[256, 256]` hidden) and the CI-bounded reduced config
//!   run the SAME code path — M-1's tests train the reduced config at test time
//!   to prove the loop; the full-size training + checked-in weights artifact is
//!   the next M-1 step (scope §5.3).
//!
//! Safety framing (unchanged, load-bearing): this is the UNTRUSTED doer. The
//! scorer only ever picks WHICH vocabulary candidate to propose; every candidate
//! is materialized kinematically continuous and horizon-capped, and the KIRRA
//! checker bounds the proposal exactly as it bounds v1's. `PlanOutput::safe_stop`
//! and the checker are untouched.

use kirra_core::corridor::CorridorSource;

use crate::learned::Rng;
use crate::{PlanInput, PlanOutput, Pose, ProposalKind, ScoredPlanner, Teacher, TrajectoryPoint};

/// Fixed feature-vector width (reserved slots zero-padded — additive headroom
/// without an artifact-format change).
pub const FEATURE_DIM_V2: usize = 32;

/// Nearest-objects-ahead slots in the encoding.
const OBJECT_SLOTS: usize = 4;
/// Per-object features: `[dx, dy, vx, vy, present]` (normalized).
const OBJECT_FEATS: usize = 5;

// Demo-frame trajectory constants (same values as v1 — the checker-facing
// materialization contract is unchanged).
const HORIZON: usize = 50; // == MAX_TRAJECTORY_HORIZON (KIRRA's WCET cap)
const DT: f64 = 0.1;
const ACCEL: f64 = 1.2;
/// Forward distance over which a candidate eases to its lateral offset.
const TRANSITION_M: f64 = 12.0;
/// Teacher proximity margin (m) — see v1's `MARGIN` rationale.
const MARGIN: f64 = 10.0;
/// Per-metre teacher cost of a candidate's terminal lateral offset (both
/// teachers — progress alone is blind to lateral wandering).
const OFFSET_COST_PER_M: f64 = 0.03;
/// Teacher progress gain. Label shaping: the normalized progress term spans
/// ~0.4 between the best and worst clear-road candidates while hazard labels
/// span 5.0, so an MSE distillation underweights clear-road SPEED ORDERING —
/// the net learns to brake for hazards but dawdles on open road. ×2 makes the
/// ordering salient without letting progress outbid a collision (−5) or a
/// containment breach (−5).
const PROGRESS_GAIN: f64 = 2.0;
/// Half-vehicle lateral margin the teacher's containment term reserves: a pass
/// offset must fit inside the corridor clearance minus this.
const VEHICLE_HALF_WIDTH_M: f64 = 1.2;

// ---------------------------------------------------------------------------
// Config: the vocabulary grid + the net shape
// ---------------------------------------------------------------------------

/// Sizing for the v2 scorer: the trajectory-vocabulary grid and the MLP hidden
/// widths. Full and reduced configs run the same code path.
#[derive(Debug, Clone, PartialEq)]
pub struct ScorerConfigV2 {
    /// Hidden-layer widths (tanh); the output layer (linear, `vocab_size()`
    /// wide) is implied.
    pub hidden: Vec<usize>,
    /// Lateral pass offsets (m, +Y left), the slow axis of the grid.
    pub lateral_offsets: Vec<f64>,
    /// Target speeds (m/s), the fast axis of the grid.
    pub speed_targets: Vec<f64>,
}

impl ScorerConfigV2 {
    /// The full M-lane model (`DOER_MODEL_SCALEUP.md` §1): 16×16 = 256
    /// candidates, `[256, 256]` hidden (~140k params). Trained OFFLINE by the
    /// trainer binary (scope §5.3) — not at test time.
    #[must_use]
    pub fn full() -> Self {
        Self {
            hidden: vec![256, 256],
            lateral_offsets: linspace(-4.5, 4.5, 16),
            speed_targets: linspace(0.0, 7.5, 16),
        }
    }

    /// The CI-bounded reduced config: same code path, trains in seconds at test
    /// time. 3×4 = 12 candidates, one 16-wide hidden layer.
    #[must_use]
    pub fn reduced() -> Self {
        Self {
            hidden: vec![16],
            lateral_offsets: vec![-4.5, 0.0, 4.5],
            speed_targets: vec![0.0, 1.5, 3.0, 6.0],
        }
    }

    /// Vocabulary size = the full offset × speed grid.
    #[must_use]
    pub fn vocab_size(&self) -> usize {
        self.lateral_offsets.len() * self.speed_targets.len()
    }

    /// Candidate index → `(lateral offset, target speed)`; offset-major, so the
    /// straight-ahead candidates occupy one contiguous block.
    #[must_use]
    pub fn candidate(&self, c: usize) -> (f64, f64) {
        let n = self.speed_targets.len();
        (self.lateral_offsets[c / n], self.speed_targets[c % n])
    }

    /// The MLP layer dims: `FEATURE_DIM_V2 → hidden… → vocab_size`.
    fn dims(&self) -> Vec<usize> {
        let mut d = Vec::with_capacity(self.hidden.len() + 2);
        d.push(FEATURE_DIM_V2);
        d.extend_from_slice(&self.hidden);
        d.push(self.vocab_size());
        d
    }
}

fn linspace(lo: f64, hi: f64, n: usize) -> Vec<f64> {
    (0..n).map(|i| lo + (hi - lo) * i as f64 / (n - 1) as f64).collect()
}

// ---------------------------------------------------------------------------
// The scene the scorer sees + the feature encoding
// ---------------------------------------------------------------------------

/// One object as the encoder sees it — position AND velocity relative to the
/// ego (demo frame: ego velocity = `(linear_x_mps, 0)`), so `vx` is the closing
/// speed (a stopped world-frame car reads `vx = -ego_speed`).
#[derive(Debug, Clone, Copy)]
struct SceneObject {
    dx: f64,
    dy: f64,
    vx: f64,
    vy: f64,
}

/// Slot normalization shared by BOTH the inference extraction and the synthetic
/// training generator — nearest-ahead first, truncated to [`OBJECT_SLOTS`] — so
/// slot 0 always means "nearest ahead" in training AND at inference (a silent
/// train/inference order mismatch materially degrades the learned behavior).
fn normalize_objects(mut objects: Vec<SceneObject>) -> Vec<SceneObject> {
    objects.sort_by(|a, b| a.dx.total_cmp(&b.dx));
    objects.truncate(OBJECT_SLOTS);
    objects
}

/// The v2 scene: richer than v1's single-obstacle `Scene` — up to
/// [`OBJECT_SLOTS`] nearest objects ahead, plus corridor clearance.
#[derive(Debug, Clone)]
struct SceneV2 {
    ego_x: f64,
    ego_y: f64,
    ego_speed: f64,
    goal_dx: f64,
    goal_dy: f64,
    /// Lateral clearance from the ego to the left/right corridor boundary,
    /// taken over a forward window (route-around headroom).
    left_clear: f64,
    right_clear: f64,
    /// Nearest-ahead first; at most [`OBJECT_SLOTS`].
    objects: Vec<SceneObject>,
}

impl SceneV2 {
    fn from_input(input: &PlanInput) -> Self {
        let ego = input.ego.pose;
        let (left_clear, right_clear) = corridor_clearance(input.map, ego.x_m, ego.y_m);
        let objects = normalize_objects(
            input
                .objects
                .iter()
                .filter(|o| o.pos.x_m > ego.x_m)
                .map(|o| SceneObject {
                    dx: o.pos.x_m - ego.x_m,
                    dy: o.pos.y_m - ego.y_m,
                    // Ego-RELATIVE velocity (closing speed); `PerceivedObject.vel`
                    // is world-frame, the ego moves at (linear_x_mps, 0) here.
                    vx: o.vel.x_m - input.ego.linear_x_mps,
                    vy: o.vel.y_m,
                })
                .collect(),
        );
        Self {
            ego_x: ego.x_m,
            ego_y: ego.y_m,
            ego_speed: input.ego.linear_x_mps,
            goal_dx: input.goal.target.x_m - ego.x_m,
            goal_dy: input.goal.target.y_m - ego.y_m,
            left_clear,
            right_clear,
            objects,
        }
    }
}

/// Lateral clearance to the corridor boundaries over a forward window
/// `[ego_x, ego_x + 40 m]` — the most conservative (narrowest) point governs.
/// An empty/degenerate boundary reads as zero clearance (fail-safe: the encoder
/// tells the net there is no room, never phantom room).
fn corridor_clearance(map: &dyn CorridorSource, ego_x: f64, ego_y: f64) -> (f64, f64) {
    let window = |pts: &[kirra_core::corridor::Point]| -> Vec<f64> {
        pts.iter()
            .filter(|p| p.x_m >= ego_x && p.x_m <= ego_x + 40.0)
            .map(|p| p.y_m)
            .collect()
    };
    let left_ys = window(map.left_boundary());
    let right_ys = window(map.right_boundary());
    let left = left_ys.iter().copied().fold(f64::INFINITY, f64::min) - ego_y;
    let right = ego_y - right_ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let clamp = |v: f64| if v.is_finite() { v.max(0.0) } else { 0.0 };
    (clamp(left), clamp(right))
}

/// The fixed 32-dim encoding. Layout (normalized):
/// `[ego_speed/8, goal_dx/50, goal_dy/10, left_clear/5, right_clear/5,
///   4 × (dx/50, dy/10, vx/8, vy/8, present), 0-padding…]`
fn featurize_v2(s: &SceneV2) -> [f64; FEATURE_DIM_V2] {
    let mut f = [0.0; FEATURE_DIM_V2];
    f[0] = s.ego_speed / 8.0;
    f[1] = (s.goal_dx / 50.0).clamp(-2.0, 2.0);
    f[2] = (s.goal_dy / 10.0).clamp(-2.0, 2.0);
    f[3] = (s.left_clear / 5.0).min(2.0);
    f[4] = (s.right_clear / 5.0).min(2.0);
    for (slot, o) in s.objects.iter().take(OBJECT_SLOTS).enumerate() {
        let base = 5 + slot * OBJECT_FEATS;
        f[base] = (o.dx / 50.0).min(2.0);
        f[base + 1] = (o.dy / 10.0).clamp(-2.0, 2.0);
        f[base + 2] = (o.vx / 8.0).clamp(-2.0, 2.0);
        f[base + 3] = (o.vy / 8.0).clamp(-2.0, 2.0);
        f[base + 4] = 1.0;
    }
    f
}

// ---------------------------------------------------------------------------
// Candidate materialization + the teacher (multi-object v2 generalization)
// ---------------------------------------------------------------------------

/// Materialize one `(offset, target-speed)` candidate: forward progress under
/// the speed profile while easing laterally to `offset` over [`TRANSITION_M`]
/// and holding — the same lane-change-and-stay shape as v1's maneuver planner,
/// kinematically continuous and horizon-capped, so a checker rejection is about
/// the hazard, never the shape.
fn materialize_v2(s: &SceneV2, offset: f64, target: f64) -> Vec<TrajectoryPoint> {
    let mut x = s.ego_x;
    let mut v = s.ego_speed.max(0.0);
    let mut prev = (s.ego_x, s.ego_y);
    (0..HORIZON)
        .map(|i| {
            let t = ((x - s.ego_x) / TRANSITION_M).clamp(0.0, 1.0);
            let y = s.ego_y + offset * (t * t * (3.0 - 2.0 * t)); // smoothstep
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

/// The v2 teacher score for one candidate: progress toward the goal, minus the
/// hazard penalty (SafetyAware only; min gap over ALL objects — the multi-object
/// generalization of v1's single-obstacle term), minus the terminal-offset cost
/// (both teachers — progress alone is blind to lateral wandering).
fn teacher_score_v2(s: &SceneV2, cfg: &ScorerConfigV2, c: usize, teacher: Teacher) -> f64 {
    let (offset, target) = cfg.candidate(c);
    let traj = materialize_v2(s, offset, target);
    let max_speed = cfg.speed_targets.iter().copied().fold(0.0, f64::max).max(1e-9);
    let progress = (traj.last().unwrap().pose.x_m - s.ego_x) / (HORIZON as f64 * DT * max_speed);

    let hazard = if teacher == Teacher::SafetyAware && !s.objects.is_empty() {
        let min_gap = traj
            .iter()
            .map(|p| {
                s.objects
                    .iter()
                    .map(|o| {
                        let ox = s.ego_x + o.dx;
                        let oy = s.ego_y + o.dy;
                        (p.pose.x_m - ox).hypot(p.pose.y_m - oy)
                    })
                    .fold(f64::MAX, f64::min)
            })
            .fold(f64::MAX, f64::min);
        if min_gap < 1.5 {
            -5.0
        } else if min_gap < MARGIN {
            -(1.0 - min_gap / MARGIN)
        } else {
            0.0
        }
    } else {
        0.0
    };

    // Corridor-containment term (BOTH teachers — feasibility, not alignment): a
    // lateral pass is only free where the corridor has room for it. A candidate
    // whose offset exceeds the available clearance minus a half-vehicle margin
    // is scored like a collision — the checker would reject it on containment,
    // and a teacher that ignores that trains a net whose "safe" swerves are
    // inadmissible (found by the M-1 drop-in test, fixed here).
    let clearance = if offset > 0.0 { s.left_clear } else { s.right_clear };
    let containment = if offset.abs() > (clearance - VEHICLE_HALF_WIDTH_M).max(0.0) && offset != 0.0
    {
        -5.0
    } else {
        0.0
    };

    PROGRESS_GAIN * progress + hazard + containment - OFFSET_COST_PER_M * offset.abs()
}

// ---------------------------------------------------------------------------
// The N-layer MLP + backprop (plain Rust, f64, no deps)
// ---------------------------------------------------------------------------

struct LayerV2 {
    /// `out_dim × in_dim`, row-major.
    w: Vec<f64>,
    b: Vec<f64>,
    in_dim: usize,
    out_dim: usize,
}

pub(crate) struct MlpV2 {
    layers: Vec<LayerV2>,
}

impl MlpV2 {
    /// Seeded uniform init scaled by `1/sqrt(in_dim)` per layer.
    fn new_seeded(dims: &[usize], rng: &mut Rng) -> Self {
        let layers = dims
            .windows(2)
            .map(|d| {
                let (i, o) = (d[0], d[1]);
                let s = (1.0 / i as f64).sqrt();
                LayerV2 {
                    w: (0..o * i).map(|_| rng.range(-s, s)).collect(),
                    b: vec![0.0; o],
                    in_dim: i,
                    out_dim: o,
                }
            })
            .collect();
        Self { layers }
    }

    /// Forward pass returning ALL activations: `acts[0]` = input,
    /// `acts[l]` = post-tanh output of hidden layer `l` (or the linear output
    /// for the last layer). The backward pass consumes this.
    fn forward_acts(&self, x: &[f64]) -> Vec<Vec<f64>> {
        let n = self.layers.len();
        let mut acts = Vec::with_capacity(n + 1);
        acts.push(x.to_vec());
        for (l, layer) in self.layers.iter().enumerate() {
            let prev = &acts[l];
            let mut out = vec![0.0; layer.out_dim];
            for (o, out_o) in out.iter_mut().enumerate() {
                let mut a = layer.b[o];
                let row = &layer.w[o * layer.in_dim..(o + 1) * layer.in_dim];
                for (w, p) in row.iter().zip(prev.iter()) {
                    a += w * p;
                }
                *out_o = if l + 1 == n { a } else { a.tanh() };
            }
            acts.push(out);
        }
        acts
    }

    fn forward(&self, x: &[f64]) -> Vec<f64> {
        self.forward_acts(x).pop().expect("non-empty activations")
    }

    /// Backprop `dL/dy` through the cached activations, ACCUMULATING parameter
    /// gradients into `grads` (same shapes as the layers). Standard chain rule;
    /// tanh' recovered from the cached post-activation (`1 - a²`).
    fn backward_accumulate(&self, acts: &[Vec<f64>], dl_dy: &[f64], grads: &mut [LayerGrads]) {
        let n = self.layers.len();
        let mut delta = dl_dy.to_vec();
        for l in (0..n).rev() {
            let layer = &self.layers[l];
            let prev = &acts[l];
            let g = &mut grads[l];
            for (o, &d_o) in delta.iter().enumerate() {
                g.db[o] += d_o;
                let grow = &mut g.dw[o * layer.in_dim..(o + 1) * layer.in_dim];
                for (gw, p) in grow.iter_mut().zip(prev.iter()) {
                    *gw += d_o * p;
                }
            }
            if l > 0 {
                let mut prev_delta = vec![0.0; layer.in_dim];
                for (o, &d_o) in delta.iter().enumerate() {
                    let row = &layer.w[o * layer.in_dim..(o + 1) * layer.in_dim];
                    for (pd, w) in prev_delta.iter_mut().zip(row.iter()) {
                        *pd += d_o * w;
                    }
                }
                // Through the tanh of the layer below (post-activation cached).
                for (pd, a) in prev_delta.iter_mut().zip(acts[l].iter()) {
                    *pd *= 1.0 - a * a;
                }
                delta = prev_delta;
            }
        }
    }

    fn zero_grads(&self) -> Vec<LayerGrads> {
        self.layers
            .iter()
            .map(|l| LayerGrads { dw: vec![0.0; l.w.len()], db: vec![0.0; l.b.len()] })
            .collect()
    }

    /// SGD + momentum step: `vel = momentum·vel − lr·grad; param += vel`.
    fn sgd_step(&mut self, grads: &[LayerGrads], vel: &mut [LayerGrads], lr: f64, momentum: f64) {
        for ((layer, g), v) in self.layers.iter_mut().zip(grads.iter()).zip(vel.iter_mut()) {
            for ((w, gw), vw) in layer.w.iter_mut().zip(g.dw.iter()).zip(v.dw.iter_mut()) {
                *vw = momentum * *vw - lr * gw;
                *w += *vw;
            }
            for ((b, gb), vb) in layer.b.iter_mut().zip(g.db.iter()).zip(v.db.iter_mut()) {
                *vb = momentum * *vb - lr * gb;
                *b += *vb;
            }
        }
    }
}

struct LayerGrads {
    dw: Vec<f64>,
    db: Vec<f64>,
}

// ---------------------------------------------------------------------------
// Training: seeded synthetic scenes, teacher distillation, mini-batch SGD
// ---------------------------------------------------------------------------

/// Training hyper-parameters. Deterministic given `seed` (fixed schedule, the
/// seeded xorshift RNG, `f64` accumulation).
#[derive(Debug, Clone, PartialEq)]
pub struct TrainConfigV2 {
    pub seed: u64,
    pub scenes: usize,
    pub epochs: usize,
    pub batch: usize,
    pub lr: f64,
    pub momentum: f64,
}

impl TrainConfigV2 {
    /// A CI-bounded schedule for [`ScorerConfigV2::reduced`] — trains in seconds
    /// (debug build included) and is enough for the behavioral tests. Sized for
    /// the ego-relative velocity features (larger magnitudes, no label signal
    /// yet → more variance to fit than the naive first cut).
    #[must_use]
    pub fn reduced(seed: u64) -> Self {
        Self { seed, scenes: 320, epochs: 420, batch: 16, lr: 0.02, momentum: 0.9 }
    }
}

/// One synthetic training scene: ego on the corridor centerline, goal ahead,
/// 0..=3 objects ahead at varied lateral positions/velocities. Same RNG draw
/// ORDER regardless of teacher, so both regimes train on the same scene stream.
fn training_scene_v2(rng: &mut Rng) -> SceneV2 {
    let ego_speed = rng.range(0.0, 4.0);
    let n_objects = (rng.unit() * 4.0) as usize; // 0..=3
    // Same normalization as the inference path (nearest-ahead first) and the
    // same EGO-RELATIVE velocity semantics: world-frame object speeds in
    // [-1, +1] m/s become relative vx in [-ego_speed-1, -ego_speed+1] — so the
    // canonical stopped-car case (relative vx = -ego_speed) is IN distribution.
    let objects = normalize_objects(
        (0..n_objects)
            .map(|_| SceneObject {
                dx: rng.range(8.0, 35.0),
                dy: rng.range(-4.0, 4.0),
                vx: rng.range(-1.0, 1.0) - ego_speed,
                vy: rng.range(-0.5, 0.5),
            })
            .collect(),
    );
    SceneV2 {
        ego_x: 5.0,
        ego_y: 0.0,
        ego_speed,
        goal_dx: 35.0,
        goal_dy: 0.0,
        // Varied clearance so the containment term is LEARNABLE: sometimes a
        // pass fits (wide corridor), sometimes only braking does (narrow).
        left_clear: rng.range(2.0, 8.0),
        right_clear: rng.range(2.0, 8.0),
        objects,
    }
}

/// Train a v2 planner by distilling `teacher` over seeded synthetic scenes —
/// the OFFLINE step (scope §2 M-1): the vehicle loads a pre-trained artifact;
/// tests train the reduced config to prove the loop. Returns the planner and
/// the final mean-squared distillation loss (a training diagnostic, not a
/// safety metric).
#[must_use]
pub fn train_planner_v2(
    cfg: &ScorerConfigV2,
    tcfg: &TrainConfigV2,
    teacher: Teacher,
) -> (LearnedPlannerV2, f64) {
    let mut rng = Rng::new(tcfg.seed);
    let k = cfg.vocab_size();

    // Synthesize the labelled corpus once: (features, per-candidate teacher scores).
    let data: Vec<([f64; FEATURE_DIM_V2], Vec<f64>)> = (0..tcfg.scenes)
        .map(|_| {
            let scene = training_scene_v2(&mut rng);
            let feats = featurize_v2(&scene);
            let labels = (0..k).map(|c| teacher_score_v2(&scene, cfg, c, teacher)).collect();
            (feats, labels)
        })
        .collect();

    let mut net = MlpV2::new_seeded(&cfg.dims(), &mut rng);
    let mut vel = net.zero_grads();
    let mut last_loss = f64::INFINITY;

    for _ in 0..tcfg.epochs {
        let mut epoch_loss = 0.0;
        let mut i = 0;
        while i < data.len() {
            let end = (i + tcfg.batch).min(data.len());
            let mut grads = net.zero_grads();
            for (x, t) in &data[i..end] {
                let acts = net.forward_acts(x);
                let y = acts.last().expect("output activations");
                // MSE; dL/dy = 2(y-t)/K, averaged over the batch below via lr scaling.
                let mut dl = vec![0.0; k];
                for (j, (yj, tj)) in y.iter().zip(t.iter()).enumerate() {
                    let e = yj - tj;
                    epoch_loss += e * e;
                    dl[j] = 2.0 * e / k as f64;
                }
                net.backward_accumulate(&acts, &dl, &mut grads);
            }
            let batch_n = (end - i) as f64;
            for g in &mut grads {
                for v in g.dw.iter_mut().chain(g.db.iter_mut()) {
                    *v /= batch_n;
                }
            }
            net.sgd_step(&grads, &mut vel, tcfg.lr, tcfg.momentum);
            i = end;
        }
        last_loss = epoch_loss / (data.len() * k) as f64;
    }

    (LearnedPlannerV2 { scorer: net, cfg: cfg.clone(), teacher }, last_loss)
}

// ---------------------------------------------------------------------------
// The planner
// ---------------------------------------------------------------------------

/// The v2 doer: the trained N-layer scorer over the offset × speed vocabulary.
/// Drops into the doer-eval harness via [`ScoredPlanner`] exactly like v1.
pub struct LearnedPlannerV2 {
    scorer: MlpV2,
    cfg: ScorerConfigV2,
    teacher: Teacher,
}

impl LearnedPlannerV2 {
    /// The teacher this planner was distilled from (audit / test introspection).
    #[must_use]
    pub fn teacher(&self) -> Teacher {
        self.teacher
    }

    /// The vocabulary/net sizing this planner was built with.
    #[must_use]
    pub fn config(&self) -> &ScorerConfigV2 {
        &self.cfg
    }

    /// The vocabulary scores for `input` (introspection; the export/round-trip
    /// seams in M-2 build on this).
    #[must_use]
    pub fn scores(&self, input: &PlanInput) -> Vec<f64> {
        self.scorer.forward(&featurize_v2(&SceneV2::from_input(input)))
    }
}

fn argmax(v: &[f64]) -> usize {
    let mut best = 0;
    for (k, &val) in v.iter().enumerate() {
        if val > v[best] {
            best = k;
        }
    }
    best
}

impl ScoredPlanner for LearnedPlannerV2 {
    fn chosen_index(&self, input: &PlanInput) -> usize {
        argmax(&self.scores(input))
    }

    fn plan_with_chosen_index(&self, input: &PlanInput) -> (usize, PlanOutput) {
        let scene = SceneV2::from_input(input);
        let best = argmax(&self.scorer.forward(&featurize_v2(&scene)));
        let (offset, target) = self.cfg.candidate(best);
        (
            best,
            PlanOutput {
                trajectory: materialize_v2(&scene, offset, target),
                kind: ProposalKind::Motion,
            },
        )
    }
}

// ---------------------------------------------------------------------------
// Tests: gradient check (the math), training behavior (the loop), determinism
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_core::corridor::MockCorridorSource;
    use kirra_core::trajectory::PerceivedObject;
    use kirra_core::FleetPosture;
    use crate::{EgoState, Goal};

    const SEED: u64 = 0xC0FFEE;

    /// THE load-bearing math test: backprop gradients equal central finite
    /// differences on a tiny seeded net, for every weight and bias. If this
    /// holds, the training loop optimizes what it claims to.
    // Index loops are intrinsic here: each parameter is perturbed IN PLACE
    // (net.layers[l].w[wi] = …) while re-evaluating the whole-net loss.
    #[allow(clippy::needless_range_loop)]
    #[test]
    fn gradient_check_backprop_matches_finite_differences() {
        let dims = [5, 4, 3];
        let mut rng = Rng::new(SEED);
        let mut net = MlpV2::new_seeded(&dims, &mut rng);
        let x: Vec<f64> = (0..5).map(|_| rng.range(-1.0, 1.0)).collect();
        let t: Vec<f64> = (0..3).map(|_| rng.range(-1.0, 1.0)).collect();

        let loss = |net: &MlpV2| -> f64 {
            net.forward(&x).iter().zip(t.iter()).map(|(y, t)| (y - t) * (y - t)).sum::<f64>()
                / t.len() as f64
        };

        // Analytic gradients.
        let acts = net.forward_acts(&x);
        let y = acts.last().unwrap().clone();
        let dl: Vec<f64> =
            y.iter().zip(t.iter()).map(|(yj, tj)| 2.0 * (yj - tj) / t.len() as f64).collect();
        let mut grads = net.zero_grads();
        net.backward_accumulate(&acts, &dl, &mut grads);

        // Central finite differences, every parameter.
        const H: f64 = 1e-5;
        for l in 0..net.layers.len() {
            for wi in 0..net.layers[l].w.len() {
                let orig = net.layers[l].w[wi];
                net.layers[l].w[wi] = orig + H;
                let lp = loss(&net);
                net.layers[l].w[wi] = orig - H;
                let lm = loss(&net);
                net.layers[l].w[wi] = orig;
                let fd = (lp - lm) / (2.0 * H);
                let an = grads[l].dw[wi];
                assert!(
                    (fd - an).abs() <= 1e-6 + 1e-4 * fd.abs().max(an.abs()),
                    "layer {l} w[{wi}]: finite-diff {fd} vs backprop {an}"
                );
            }
            for bi in 0..net.layers[l].b.len() {
                let orig = net.layers[l].b[bi];
                net.layers[l].b[bi] = orig + H;
                let lp = loss(&net);
                net.layers[l].b[bi] = orig - H;
                let lm = loss(&net);
                net.layers[l].b[bi] = orig;
                let fd = (lp - lm) / (2.0 * H);
                let an = grads[l].db[bi];
                assert!(
                    (fd - an).abs() <= 1e-6 + 1e-4 * fd.abs().max(an.abs()),
                    "layer {l} b[{bi}]: finite-diff {fd} vs backprop {an}"
                );
            }
        }
    }

    fn world<'a>(
        map: &'a MockCorridorSource,
        objects: &'a [PerceivedObject],
    ) -> PlanInput<'a> {
        PlanInput {
            ego: EgoState {
                pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 },
                linear_x_mps: 2.0,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal { target: Pose { x_m: 40.0, y_m: 0.0, heading_rad: 0.0 } },
            map,
            objects,
            controls: &[],
            lane_boundaries: &[],
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

    fn stopped_car(x: f64) -> PerceivedObject {
        PerceivedObject {
            id: 1,
            pos: kirra_core::corridor::Point { x_m: x, y_m: 0.0 },
            velocity_mps: 0.0,
            heading_rad: 0.0,
            vel: kirra_core::corridor::Point { x_m: 0.0, y_m: 0.0 },
        }
    }

    fn reach(out: &PlanOutput) -> f64 {
        out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max)
    }

    #[test]
    fn training_reduces_the_distillation_loss() {
        let cfg = ScorerConfigV2::reduced();
        // One epoch vs the full schedule: the full schedule must land lower.
        let short = TrainConfigV2 { epochs: 1, ..TrainConfigV2::reduced(SEED) };
        let (_, loss_short) = train_planner_v2(&cfg, &short, Teacher::SafetyAware);
        let (_, loss_full) = train_planner_v2(&cfg, &TrainConfigV2::reduced(SEED), Teacher::SafetyAware);
        assert!(
            loss_full < loss_short,
            "training must reduce the loss: 1-epoch {loss_short} vs full {loss_full}"
        );
    }

    #[test]
    fn training_is_deterministic_from_the_seed() {
        let cfg = ScorerConfigV2::reduced();
        let tcfg = TrainConfigV2::reduced(SEED);
        let (a, la) = train_planner_v2(&cfg, &tcfg, Teacher::SafetyAware);
        let (b, lb) = train_planner_v2(&cfg, &tcfg, Teacher::SafetyAware);
        assert_eq!(la.to_bits(), lb.to_bits(), "identical final loss");
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [stopped_car(22.0)];
        let w = world(&corr, &objs);
        assert_eq!(a.scores(&w), b.scores(&w), "identical scores on a probe input");
    }

    /// The v1 behavioral story must hold at v2: the safety-aware net stops short
    /// of the hazard (or routes clear of it laterally); the progress-only net
    /// barrels through at speed. Same architecture, same training, different
    /// teacher — the only difference that matters.
    #[test]
    fn safety_aware_v2_avoids_the_hazard_progress_only_barrels() {
        let cfg = ScorerConfigV2::reduced();
        let tcfg = TrainConfigV2::reduced(SEED);
        let (safe, _) = train_planner_v2(&cfg, &tcfg, Teacher::SafetyAware);
        let (prog, _) = train_planner_v2(&cfg, &tcfg, Teacher::ProgressOnly);

        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [stopped_car(20.0)];
        let w = world(&corr, &objs);

        let (_, safe_out) = safe.plan_with_chosen_index(&w);
        let (safe_off, _) = cfg.candidate(safe.chosen_index(&w));
        let (_, prog_out) = prog.plan_with_chosen_index(&w);

        // Safety-aware: either brakes short of the hazard OR routes around it
        // (terminal lateral offset clears the centerline hazard).
        let safe_clears = reach(&safe_out) < 20.0 - 1.5 || safe_off.abs() >= 4.0;
        assert!(
            safe_clears,
            "safety-aware v2 must brake short or route around: reach {}, offset {}",
            reach(&safe_out),
            safe_off
        );
        // Progress-only: drives past the hazard x.
        assert!(
            reach(&prog_out) > 20.0,
            "progress-only v2 barrels through: reach {}",
            reach(&prog_out)
        );
    }

    #[test]
    fn clear_road_makes_progress_for_both_teachers() {
        let cfg = ScorerConfigV2::reduced();
        let tcfg = TrainConfigV2::reduced(SEED);
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[]);
        for teacher in [Teacher::SafetyAware, Teacher::ProgressOnly] {
            let (p, _) = train_planner_v2(&cfg, &tcfg, teacher);
            let (_, out) = p.plan_with_chosen_index(&w);
            assert!(
                reach(&out) > 15.0,
                "{teacher:?}: v2 makes progress on a clear road, got {}",
                reach(&out)
            );
        }
    }

    #[test]
    fn full_config_has_the_scoped_shape() {
        let cfg = ScorerConfigV2::full();
        assert_eq!(cfg.vocab_size(), 256);
        let dims = cfg.dims();
        assert_eq!(dims, vec![32, 256, 256, 256]);
        // ~140k params as scoped (weights + biases).
        let params: usize = dims.windows(2).map(|d| d[0] * d[1] + d[1]).sum();
        assert!(
            (100_000..200_000).contains(&params),
            "full model is ~140k params, got {params}"
        );
    }

    #[test]
    fn absent_objects_encode_as_zeros() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[]);
        let f = featurize_v2(&SceneV2::from_input(&w));
        // All four object slots empty ⇒ their 20 dims are zero (fail-safe).
        assert!(f[5..5 + OBJECT_SLOTS * OBJECT_FEATS].iter().all(|&v| v == 0.0));
        // And the padding tail is zero.
        assert!(f[5 + OBJECT_SLOTS * OBJECT_FEATS..].iter().all(|&v| v == 0.0));
    }
}
