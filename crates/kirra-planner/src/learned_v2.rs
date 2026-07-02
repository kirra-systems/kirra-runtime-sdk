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

use crate::learned::{fake_quant, int8_scale, quantize_i8, Rng};
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
/// Collision-grade proximity for the teacher (m) = the CHECKER'S RSS
/// lateral-alignment band (the 4 m band v1's ±4.5 offsets were chosen to
/// clear — see `learned.rs` `LATERAL_OFFSETS`). A candidate whose path comes
/// within this of an object is scored like a collision, because the checker
/// MRCs a pass inside the band: teaching a 3.3 m "pass" as acceptable trains a
/// net whose plans the checker rejects (found by the artifact admissibility
/// gate after the clearance fix — in a 5 m-half-width corridor the admissible
/// pass window (>4.0 RSS laterally, corner-fit containment) is EMPTY, and the
/// correct behavior is braking).
const TEACHER_UNSAFE_GAP_M: f64 = 4.0;
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
// Checker-containment mirror (`kirra_core::containment::footprint_corners` +
// the urban profile in `kirra_trajectory`'s `VehicleConfig::default_urban`).
// The checker's pose sits at the REAR AXLE, so a mid-transition heading swing
// sweeps the front corners laterally by up to `TEACHER_FRONT_LEVER_M·sin(h)` —
// a 3.3 m swerve peaks at ~0.35 rad and puts the front corner ~1.26 m outside
// its own centerline, inside the checker's 0.40 m boundary margin. A teacher
// that models the vehicle as a static half-width labels those swerves "free"
// and distills a net whose picks the checker MRCs (found by the M-1 artifact
// admissibility gate — the third checker-teaches-the-teacher fix, after the
// clearance and RSS-band ones).
/// `default_urban` half width (m).
const TEACHER_HALF_WIDTH_M: f64 = 0.925;
/// Rear axle → front corner lever (m): wheelbase 2.8 + front overhang 0.9.
const TEACHER_FRONT_LEVER_M: f64 = 3.7;
/// Rear axle → rear corner lever (m): rear overhang.
const TEACHER_REAR_LEVER_M: f64 = 1.1;
/// The checker's `CONTAINMENT_LATERAL_MARGIN_M` (Trusted frame).
const TEACHER_CONTAINMENT_MARGIN_M: f64 = 0.40;

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
///
/// Evaluates polyline SEGMENTS clipped to the window, not vertices: a sparse
/// boundary (e.g. a 2-point straight line spanning the whole road) has no
/// vertex inside the window yet absolutely bounds it — vertex filtering read
/// such a corridor as ZERO clearance and degenerated the teacher's containment
/// term to "penalize everything" (found by the artifact-gate probe). A boundary
/// with NO segment overlapping the window still reads as zero clearance
/// (fail-safe: no evidence of room is not room).
fn corridor_clearance(map: &dyn CorridorSource, ego_x: f64, ego_y: f64) -> (f64, f64) {
    let (x0, x1) = (ego_x, ego_x + 40.0);
    // Candidate boundary-y values over the window: for each overlapping segment,
    // the linear extremes are at the clipped endpoints; a (near-)vertical
    // segment contributes both endpoint ys (conservative in both directions).
    let window_ys = |pts: &[kirra_core::corridor::Point]| -> Vec<f64> {
        let mut ys = Vec::new();
        for seg in pts.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            let (sx0, sx1) = if a.x_m <= b.x_m { (a.x_m, b.x_m) } else { (b.x_m, a.x_m) };
            let lo = sx0.max(x0);
            let hi = sx1.min(x1);
            if lo > hi {
                continue;
            }
            if (b.x_m - a.x_m).abs() < 1e-9 {
                ys.push(a.y_m);
                ys.push(b.y_m);
            } else {
                let y_at = |x: f64| a.y_m + (b.y_m - a.y_m) * (x - a.x_m) / (b.x_m - a.x_m);
                ys.push(y_at(lo));
                ys.push(y_at(hi));
            }
        }
        ys
    };
    let left = window_ys(map.left_boundary())
        .into_iter()
        .fold(f64::INFINITY, f64::min)
        - ego_y;
    let right = ego_y
        - window_ys(map.right_boundary())
            .into_iter()
            .fold(f64::NEG_INFINITY, f64::max);
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
        if min_gap < TEACHER_UNSAFE_GAP_M {
            -5.0
        } else if min_gap < MARGIN {
            -(1.0 - min_gap / MARGIN)
        } else {
            0.0
        }
    } else {
        0.0
    };

    // Corridor-containment term (BOTH teachers — feasibility, not alignment):
    // the CHECKER'S corner geometry over the materialized trajectory. For each
    // pose, the four footprint corners' lateral positions (rear-axle pose +
    // heading rotation, mirroring `footprint_corners`) must clear each boundary
    // by the checker's margin; a breach is scored like a collision, because the
    // checker MRCs it on containment. A static-half-width model here labeled
    // heading-swung swerves "free" and distilled a checker-refused net (found
    // by the M-1 artifact admissibility gate, fixed here).
    let mut max_left = f64::MIN;
    let mut max_right = f64::MIN;
    for p in &traj {
        let (sin_h, cos_h) = p.pose.heading_rad.sin_cos();
        for (xb, yb) in [
            (TEACHER_FRONT_LEVER_M, TEACHER_HALF_WIDTH_M),
            (TEACHER_FRONT_LEVER_M, -TEACHER_HALF_WIDTH_M),
            (-TEACHER_REAR_LEVER_M, TEACHER_HALF_WIDTH_M),
            (-TEACHER_REAR_LEVER_M, -TEACHER_HALF_WIDTH_M),
        ] {
            let corner_y = (p.pose.y_m - s.ego_y) + xb * sin_h + yb * cos_h;
            max_left = max_left.max(corner_y);
            max_right = max_right.max(-corner_y);
        }
    }
    let breach = max_left > s.left_clear - TEACHER_CONTAINMENT_MARGIN_M
        || max_right > s.right_clear - TEACHER_CONTAINMENT_MARGIN_M;
    let containment = if breach { -5.0 } else { 0.0 };

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

    /// The OFFLINE full-size schedule for [`ScorerConfigV2::full`] — minutes on
    /// a release build; run by `examples/train_v2.rs`, never at test time. CI
    /// gates the resulting checked-in artifact's BEHAVIOR, not its training.
    #[must_use]
    pub fn full(seed: u64) -> Self {
        Self { seed, scenes: 4000, epochs: 240, batch: 32, lr: 0.01, momentum: 0.9 }
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

/// The candidate the TEACHER would pick for `input` — the distillation oracle's
/// own argmax, exposed so behavior gates can measure teacher-agreement on
/// held-out scenes (the loaded artifact must still think like its teacher).
#[must_use]
pub fn teacher_choice(cfg: &ScorerConfigV2, input: &PlanInput, teacher: Teacher) -> usize {
    let scene = SceneV2::from_input(input);
    let scores: Vec<f64> =
        (0..cfg.vocab_size()).map(|c| teacher_score_v2(&scene, cfg, c, teacher)).collect();
    argmax(&scores)
}

/// The TEACHER'S score for one candidate on `input` — the oracle the behavior
/// gates measure REGRET against: with a large vocabulary the teacher's top-1
/// sits on near-tie plateaus (adjacent grid candidates differ by ~1e-2 while a
/// distilled net's regression error is ~1e-1), so exact-argmax agreement is the
/// wrong gate; "the net's pick costs ≤ ε by the teacher's own scoring" is the
/// robust one.
#[must_use]
pub fn teacher_candidate_score(
    cfg: &ScorerConfigV2,
    input: &PlanInput,
    candidate: usize,
    teacher: Teacher,
) -> f64 {
    teacher_score_v2(&SceneV2::from_input(input), cfg, candidate, teacher)
}

// ---------------------------------------------------------------------------
// The weights artifact (scope §2 M-1): versioned, self-describing, fail-closed
// ---------------------------------------------------------------------------

/// Artifact magic — 8 bytes at offset 0.
const WEIGHTS_MAGIC: &[u8; 8] = b"KIRRAMV2";
/// Current artifact format version.
const WEIGHTS_VERSION: u32 = 1;

/// Why a weights artifact failed to load. Fail-closed: any structural doubt is
/// an error, never a silently-different model.
#[derive(Debug, PartialEq, Eq)]
pub enum WeightsError {
    BadMagic,
    UnsupportedVersion(u32),
    BadTeacherTag(u8),
    Truncated(&'static str),
    /// Bytes remain after the declared layout — a length mismatch is corruption.
    TrailingBytes(usize),
    /// A declared dimension is implausible (zero, or absurdly large).
    BadDims(&'static str),
}

impl std::fmt::Display for WeightsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMagic => write!(f, "not a KIRRAMV2 weights artifact (bad magic)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported weights version {v}"),
            Self::BadTeacherTag(t) => write!(f, "unknown teacher tag {t}"),
            Self::Truncated(what) => write!(f, "artifact truncated reading {what}"),
            Self::TrailingBytes(n) => write!(f, "{n} trailing bytes after the declared layout"),
            Self::BadDims(what) => write!(f, "implausible dimension: {what}"),
        }
    }
}

impl std::error::Error for WeightsError {}

/// Upper bound sanity for any declared count in the header (offsets, speeds,
/// hidden widths). Far above any real config; a fail-closed guard against a
/// corrupt length field allocating gigabytes.
const MAX_DECLARED: u32 = 65_536;

struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], WeightsError> {
        let end = self.at.checked_add(n).ok_or(WeightsError::Truncated(what))?;
        if end > self.buf.len() {
            return Err(WeightsError::Truncated(what));
        }
        let s = &self.buf[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u32(&mut self, what: &'static str) -> Result<u32, WeightsError> {
        Ok(u32::from_le_bytes(self.take(4, what)?.try_into().expect("4 bytes")))
    }
    fn f64(&mut self, what: &'static str) -> Result<f64, WeightsError> {
        Ok(f64::from_le_bytes(self.take(8, what)?.try_into().expect("8 bytes")))
    }
    fn f32(&mut self, what: &'static str) -> Result<f32, WeightsError> {
        Ok(f32::from_le_bytes(self.take(4, what)?.try_into().expect("4 bytes")))
    }
    fn counted(&mut self, what: &'static str) -> Result<usize, WeightsError> {
        let n = self.u32(what)?;
        if n == 0 || n > MAX_DECLARED {
            return Err(WeightsError::BadDims(what));
        }
        Ok(n as usize)
    }
}

impl LearnedPlannerV2 {
    /// Serialize to the versioned artifact format: an 8-byte magic, version,
    /// teacher tag, the SELF-DESCRIBING config (hidden dims + vocabulary grid,
    /// `f64`), then per-layer weights and biases as `f32` little-endian (the
    /// storage precision of the artifact — loading widens exactly, so the
    /// loaded model is a pure function of the bytes).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(WEIGHTS_MAGIC);
        out.extend_from_slice(&WEIGHTS_VERSION.to_le_bytes());
        out.push(match self.teacher {
            Teacher::SafetyAware => 0u8,
            Teacher::ProgressOnly => 1u8,
        });
        // Fail-closed mirror of `Cursor::counted`: a count the parser would
        // reject (0, > MAX_DECLARED, or a usize that would truncate to u32)
        // must panic here, never be silently written into the artifact.
        let put_u32 = |out: &mut Vec<u8>, v: usize| {
            let v = u32::try_from(v).expect("weights artifact count must fit in u32");
            assert!(
                (1..=MAX_DECLARED).contains(&v),
                "weights artifact count {v} outside the parser's accepted 1..=MAX_DECLARED"
            );
            out.extend_from_slice(&v.to_le_bytes());
        };
        put_u32(&mut out, self.cfg.hidden.len());
        for &h in &self.cfg.hidden {
            put_u32(&mut out, h);
        }
        put_u32(&mut out, self.cfg.lateral_offsets.len());
        for &o in &self.cfg.lateral_offsets {
            out.extend_from_slice(&o.to_le_bytes());
        }
        put_u32(&mut out, self.cfg.speed_targets.len());
        for &s in &self.cfg.speed_targets {
            out.extend_from_slice(&s.to_le_bytes());
        }
        for layer in &self.scorer.layers {
            for &w in &layer.w {
                out.extend_from_slice(&(w as f32).to_le_bytes());
            }
            for &b in &layer.b {
                out.extend_from_slice(&(b as f32).to_le_bytes());
            }
        }
        out
    }

    /// Parse a weights artifact — fail-closed on bad magic/version/teacher,
    /// truncation, implausible dims, or trailing bytes. The loaded model is a
    /// pure function of the bytes (`f32` storage widened to the `f64` runtime).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, WeightsError> {
        let mut c = Cursor { buf: bytes, at: 0 };
        if c.take(8, "magic")? != WEIGHTS_MAGIC {
            return Err(WeightsError::BadMagic);
        }
        let version = c.u32("version")?;
        if version != WEIGHTS_VERSION {
            return Err(WeightsError::UnsupportedVersion(version));
        }
        let teacher = match c.take(1, "teacher tag")?[0] {
            0 => Teacher::SafetyAware,
            1 => Teacher::ProgressOnly,
            t => return Err(WeightsError::BadTeacherTag(t)),
        };
        let n_hidden = c.counted("hidden-layer count")?;
        let hidden: Vec<usize> = (0..n_hidden)
            .map(|_| c.counted("hidden width"))
            .collect::<Result<_, _>>()?;
        let n_off = c.counted("offset count")?;
        let lateral_offsets: Vec<f64> =
            (0..n_off).map(|_| c.f64("offset")).collect::<Result<_, _>>()?;
        let n_spd = c.counted("speed count")?;
        let speed_targets: Vec<f64> =
            (0..n_spd).map(|_| c.f64("speed")).collect::<Result<_, _>>()?;
        let cfg = ScorerConfigV2 { hidden, lateral_offsets, speed_targets };

        let dims = cfg.dims();
        let layers = dims
            .windows(2)
            .map(|d| {
                let (i, o) = (d[0], d[1]);
                let w: Vec<f64> = (0..o * i)
                    .map(|_| c.f32("weight").map(f64::from))
                    .collect::<Result<_, _>>()?;
                let b: Vec<f64> =
                    (0..o).map(|_| c.f32("bias").map(f64::from)).collect::<Result<_, _>>()?;
                Ok(LayerV2 { w, b, in_dim: i, out_dim: o })
            })
            .collect::<Result<Vec<_>, WeightsError>>()?;
        if c.at != bytes.len() {
            return Err(WeightsError::TrailingBytes(bytes.len() - c.at));
        }
        Ok(Self { scorer: MlpV2 { layers }, cfg, teacher })
    }
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
// M-2: N-layer PTQ + the export seams (`parko/DOER_MODEL_SCALEUP.md` §2)
// ---------------------------------------------------------------------------
//
// The v1 pipeline's PTQ and ONNX export assumed the fixed 2-layer `Mlp`; the
// v2 scorer is an N-layer chain, so both generalize here on the same design:
// per-tensor symmetric int8 weights, calibrated per-activation absmax scales,
// `f64` accumulation (a QUALITY model of int8, not a bit-exact kernel — that
// is the backend's job). PTQ is a quality operation, never a safety one: the
// int8 doer's proposal is still bounded by the unchanged checker.

/// One layer of the FP32 export seam: `w` is `out_dim × in_dim` row-major
/// (the scorer's native layout; the ONNX writer transposes for MatMul).
pub struct LayerWeightsV2 {
    pub w: Vec<f64>,
    pub b: Vec<f64>,
    pub in_dim: usize,
    pub out_dim: usize,
}

/// The N-layer FP32 export seam — what the chain ONNX writer consumes. Hidden
/// layers are tanh; the last layer is linear (the writer knows this by index).
pub struct ScorerWeightsV2 {
    pub layers: Vec<LayerWeightsV2>,
}

/// One layer of the int8 export seam: per-tensor symmetric codes + scale.
pub struct QuantLayerWeightsV2 {
    /// `out_dim × in_dim` int8 codes, row-major.
    pub codes: Vec<i8>,
    pub w_scale: f64,
    /// Biases stay f64/f32 (standard int8 practice — they add post-matmul).
    pub b: Vec<f64>,
    pub in_dim: usize,
    pub out_dim: usize,
}

/// The N-layer int8 export seam. `act_scales[l]` is the calibrated scale of the
/// activation ENTERING layer `l` (`[0]` = the input features, `[l>0]` = the
/// post-tanh output of hidden layer `l-1`) — one Q/DQ pair per matmul input.
pub struct QuantizedScorerWeightsV2 {
    pub layers: Vec<QuantLayerWeightsV2>,
    pub act_scales: Vec<f64>,
}

/// A [`LearnedPlannerV2`] whose scorer runs on the int8 grid (PTQ). Same
/// vocabulary, teacher, and materialization; only the scorer arithmetic
/// changes — the checker seam is identical.
pub struct QuantizedLearnedPlannerV2 {
    layers: Vec<QuantLayerV2>,
    act_scales: Vec<f64>,
    cfg: ScorerConfigV2,
    teacher: Teacher,
}

struct QuantLayerV2 {
    codes: Vec<i8>,
    b: Vec<f64>,
    s_w: f64,
    in_dim: usize,
    out_dim: usize,
}

impl QuantizedLearnedPlannerV2 {
    /// The teacher the underlying FP32 planner was distilled from.
    pub fn teacher(&self) -> Teacher {
        self.teacher
    }

    /// The vocabulary/shape config (identical to the FP32 planner's).
    pub fn config(&self) -> &ScorerConfigV2 {
        &self.cfg
    }

    /// The calibrated activation scales — all finite and `> 0` for a
    /// non-degenerate calibration. A hook for tests and the scorecard.
    pub fn act_scales(&self) -> &[f64] {
        &self.act_scales
    }

    /// The int8 scores for `input` (the quality model the QDQ ONNX mirrors).
    pub fn scores(&self, input: &PlanInput) -> Vec<f64> {
        self.qforward(&featurize_v2(&SceneV2::from_input(input)))
    }

    /// The int8 export seam for the chain ONNX writer.
    #[must_use]
    pub fn scorer_weights(&self) -> QuantizedScorerWeightsV2 {
        QuantizedScorerWeightsV2 {
            layers: self
                .layers
                .iter()
                .map(|l| QuantLayerWeightsV2 {
                    codes: l.codes.clone(),
                    w_scale: l.s_w,
                    b: l.b.clone(),
                    in_dim: l.in_dim,
                    out_dim: l.out_dim,
                })
                .collect(),
            act_scales: self.act_scales.clone(),
        }
    }

    /// The int8 forward: each matmul input is fake-quantized at its calibrated
    /// scale, weights are dequantized from their i8 codes, accumulation is f64
    /// — v1's `QuantMlp::forward` semantics generalized to the chain.
    fn qforward(&self, x: &[f64]) -> Vec<f64> {
        let n = self.layers.len();
        let mut act = x.to_vec();
        for (l, layer) in self.layers.iter().enumerate() {
            let s_a = self.act_scales[l];
            let mut out = vec![0.0; layer.out_dim];
            for (o, out_o) in out.iter_mut().enumerate() {
                let mut a = layer.b[o];
                let row = &layer.codes[o * layer.in_dim..(o + 1) * layer.in_dim];
                for (w, xi) in row.iter().zip(act.iter()) {
                    a += (f64::from(*w) * layer.s_w) * fake_quant(*xi, s_a);
                }
                *out_o = if l + 1 == n { a } else { a.tanh() };
            }
            act = out;
        }
        act
    }
}

impl ScoredPlanner for QuantizedLearnedPlannerV2 {
    fn chosen_index(&self, input: &PlanInput) -> usize {
        argmax(&self.qforward(&featurize_v2(&SceneV2::from_input(input))))
    }

    fn plan_with_chosen_index(&self, input: &PlanInput) -> (usize, PlanOutput) {
        let scene = SceneV2::from_input(input);
        let best = argmax(&self.qforward(&featurize_v2(&scene)));
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

impl LearnedPlannerV2 {
    /// The FP32 export seam for the chain ONNX writer.
    #[must_use]
    pub fn scorer_weights(&self) -> ScorerWeightsV2 {
        ScorerWeightsV2 {
            layers: self
                .scorer
                .layers
                .iter()
                .map(|l| LayerWeightsV2 {
                    w: l.w.clone(),
                    b: l.b.clone(),
                    in_dim: l.in_dim,
                    out_dim: l.out_dim,
                })
                .collect(),
        }
    }

    /// The feature vector AND the FP32 scores for `input` — the ONNX round-trip
    /// tests feed the features to the exported model and compare the scores.
    #[must_use]
    pub fn features_and_scores(&self, input: &PlanInput) -> (Vec<f64>, Vec<f64>) {
        let x = featurize_v2(&SceneV2::from_input(input));
        let scores = self.scorer.forward(&x);
        (x.to_vec(), scores)
    }

    /// **Post-training int8 quantization** (PTQ) of the N-layer scorer,
    /// calibrated over `calibration` inputs: per-tensor symmetric int8 weight
    /// scales (absmax of each trained weight tensor) + per-activation scales
    /// (absmax of each matmul input observed across the corpus) — v1's
    /// `quantize_int8` generalized to the chain (M-2).
    #[must_use]
    pub fn quantize_int8(&self, calibration: &[PlanInput]) -> QuantizedLearnedPlannerV2 {
        let n = self.scorer.layers.len();

        // Activation absmax per matmul input over the calibration corpus.
        // `forward_acts` returns [input, h1, …, output]; entries 0..n are the
        // matmul inputs (the final output is never re-quantized).
        let mut amax = vec![0.0_f64; n];
        for input in calibration {
            let x = featurize_v2(&SceneV2::from_input(input));
            let acts = self.scorer.forward_acts(&x);
            for (m, act) in amax.iter_mut().zip(acts.iter()) {
                for &v in act {
                    *m = m.max(v.abs());
                }
            }
        }
        let act_scales: Vec<f64> = amax.into_iter().map(int8_scale).collect();

        let layers = self
            .scorer
            .layers
            .iter()
            .map(|l| {
                let s_w = int8_scale(l.w.iter().fold(0.0_f64, |a, &v| a.max(v.abs())));
                QuantLayerV2 {
                    codes: l.w.iter().map(|&v| quantize_i8(v, s_w)).collect(),
                    b: l.b.clone(),
                    s_w,
                    in_dim: l.in_dim,
                    out_dim: l.out_dim,
                }
            })
            .collect();

        QuantizedLearnedPlannerV2 {
            layers,
            act_scales,
            cfg: self.cfg.clone(),
            teacher: self.teacher,
        }
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

    /// The artifact is a pure function of the bytes: save→load→save is
    /// byte-identical (weights land exactly on the f32 storage grid), and the
    /// loaded planner scores deterministically.
    #[test]
    fn weights_round_trip_is_byte_stable() {
        let cfg = ScorerConfigV2::reduced();
        let (p, _) = train_planner_v2(&cfg, &TrainConfigV2::reduced(SEED), Teacher::SafetyAware);
        let bytes = p.to_bytes();
        let loaded = LearnedPlannerV2::from_bytes(&bytes).expect("valid artifact");
        assert_eq!(loaded.teacher(), Teacher::SafetyAware);
        assert_eq!(loaded.config(), &cfg);
        assert_eq!(loaded.to_bytes(), bytes, "save→load→save byte-identical");

        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [stopped_car(20.0)];
        let w = world(&corr, &objs);
        // The loaded (f32-grid) net is close to the trained f64 net — same argmax
        // on the probe (f32 rounding is the artifact's storage precision).
        assert_eq!(loaded.chosen_index(&w), p.chosen_index(&w));
    }

    #[test]
    fn weights_parse_fails_closed() {
        let cfg = ScorerConfigV2::reduced();
        let (p, _) = train_planner_v2(
            &cfg,
            &TrainConfigV2 { epochs: 1, ..TrainConfigV2::reduced(SEED) },
            Teacher::SafetyAware,
        );
        let good = p.to_bytes();

        // Bad magic.
        let mut bad = good.clone();
        bad[0] ^= 0xFF;
        assert!(matches!(LearnedPlannerV2::from_bytes(&bad), Err(WeightsError::BadMagic)));
        // Unsupported version.
        let mut bad = good.clone();
        bad[8] = 99;
        assert!(matches!(
            LearnedPlannerV2::from_bytes(&bad),
            Err(WeightsError::UnsupportedVersion(_))
        ));
        // Bad teacher tag.
        let mut bad = good.clone();
        bad[12] = 7;
        assert!(matches!(
            LearnedPlannerV2::from_bytes(&bad),
            Err(WeightsError::BadTeacherTag(7))
        ));
        // Truncation (drop the last byte).
        assert!(matches!(
            LearnedPlannerV2::from_bytes(&good[..good.len() - 1]),
            Err(WeightsError::Truncated(_))
        ));
        // Trailing bytes (append one).
        let mut bad = good.clone();
        bad.push(0);
        assert!(matches!(
            LearnedPlannerV2::from_bytes(&bad),
            Err(WeightsError::TrailingBytes(1))
        ));
    }

    /// Regression (artifact-gate probe finding): a SPARSE boundary polyline — the
    /// 2-vertex straight MockCorridorSource — must read its true clearance, not
    /// zero (vertex-filtering saw no vertex in the window and fail-safed to 0,
    /// which degenerated the teacher's containment term to penalize everything).
    #[test]
    fn sparse_straight_boundary_reads_true_clearance() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[]);
        let scene_features = featurize_v2(&SceneV2::from_input(&w));
        // Feature layout: [3] = left_clear/5, [4] = right_clear/5 → both 1.0.
        assert_eq!(scene_features[3], 1.0, "left clearance 5 m from a 2-vertex boundary");
        assert_eq!(scene_features[4], 1.0, "right clearance 5 m from a 2-vertex boundary");
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

    /// M-2 PTQ: the int8-quantized reduced planner keeps its FP32 source's
    /// DECISION (argmax) on the calibration-domain scenes — int8 is a mild
    /// perturbation of a trained net, and the decision is what the harness
    /// scores. Also pins the calibration invariants: one activation scale per
    /// matmul input, all finite and positive.
    #[test]
    fn quantized_v2_keeps_the_fp32_decision_on_calibration_scenes() {
        let cfg = ScorerConfigV2::reduced();
        let (p, _) =
            train_planner_v2(&cfg, &TrainConfigV2::reduced(SEED), Teacher::SafetyAware);

        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let hazards = [vec![], vec![stopped_car(15.0)], vec![stopped_car(25.0)]];
        let worlds: Vec<PlanInput> = hazards.iter().map(|o| world(&corr, o)).collect();
        let int8 = p.quantize_int8(&worlds);

        assert_eq!(int8.act_scales().len(), cfg.hidden.len() + 1, "one scale per matmul input");
        assert!(int8.act_scales().iter().all(|&s| s.is_finite() && s > 0.0));
        assert_eq!(int8.teacher(), p.teacher());
        assert_eq!(int8.config(), p.config());

        for (w, objs) in worlds.iter().zip(hazards.iter()) {
            assert_eq!(
                int8.chosen_index(w),
                LearnedPlannerV2::chosen_index(&p, w),
                "int8 argmax diverged from fp32 on the {}-object scene",
                objs.len()
            );
        }
    }

    /// M-2 export seams: the FP32 seam mirrors the net's dims exactly, and the
    /// int8 seam carries per-layer codes of the same shapes plus the activation
    /// scales — what the chain ONNX writer consumes.
    #[test]
    fn v2_export_seams_carry_the_chain_shapes() {
        let cfg = ScorerConfigV2::reduced();
        let (p, _) = train_planner_v2(
            &cfg,
            &TrainConfigV2 { epochs: 1, ..TrainConfigV2::reduced(SEED) },
            Teacher::SafetyAware,
        );
        let dims = cfg.dims();

        let w = p.scorer_weights();
        assert_eq!(w.layers.len(), dims.len() - 1);
        for (l, d) in w.layers.iter().zip(dims.windows(2)) {
            assert_eq!((l.in_dim, l.out_dim), (d[0], d[1]));
            assert_eq!(l.w.len(), d[0] * d[1]);
            assert_eq!(l.b.len(), d[1]);
        }

        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let q = p.quantize_int8(&[world(&corr, &[])]).scorer_weights();
        assert_eq!(q.layers.len(), dims.len() - 1);
        assert_eq!(q.act_scales.len(), dims.len() - 1);
        for (l, d) in q.layers.iter().zip(dims.windows(2)) {
            assert_eq!(l.codes.len(), d[0] * d[1]);
            assert_eq!(l.b.len(), d[1]);
            assert!(l.w_scale.is_finite() && l.w_scale > 0.0);
        }
    }
}
