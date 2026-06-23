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

/// The teacher's score for one candidate — the signal the scorer is distilled to predict.
fn teacher_score(s: &Scene, target: f64, teacher: Teacher) -> f64 {
    let traj = materialize(s, target);
    let progress = (traj.last().unwrap().pose.x_m - s.ego_x) / (HORIZON as f64 * DT * TARGET_SPEEDS[K - 1]);
    let mut score = progress;
    if let (Teacher::SafetyAware, Some(ox)) = (teacher, s.obstacle_x) {
        let min_gap = traj
            .iter()
            .map(|p| (p.pose.x_m - ox).hypot(p.pose.y_m))
            .fold(f64::MAX, f64::min);
        if min_gap < 1.5 {
            score -= 5.0; // collision
        } else if min_gap < MARGIN {
            score -= 1.0 * (1.0 - min_gap / MARGIN); // approach-at-distance
        }
    }
    score
}

// ----------------------------------------------------------------------------
// The learned scorer: a small MLP, weights fit by a seeded (1+1)-ES
// ----------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Mlp {
    w1: [[f64; IN]; H],
    b1: [f64; H],
    w2: [[f64; H]; K],
    b2: [f64; K],
}

impl Mlp {
    fn forward(&self, x: &[f64; IN]) -> [f64; K] {
        let mut h = [0.0; H];
        for (hj, (w1row, b1j)) in h.iter_mut().zip(self.w1.iter().zip(self.b1.iter())) {
            let mut a = *b1j;
            for (w, xi) in w1row.iter().zip(x.iter()) {
                a += w * xi;
            }
            *hj = a.tanh();
        }
        let mut y = [0.0; K];
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

/// A learned planner: the trajectory vocabulary `TARGET_SPEEDS`, scored by a small
/// MLP distilled from `teacher`. Construct with [`LearnedPlanner::trained`].
pub struct LearnedPlanner {
    scorer: Mlp,
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
            let scene = Scene {
                ego_x: 5.0,
                ego_y: 0.0,
                ego_speed: rng.range(0.0, 4.0),
                goal_x: 40.0,
                goal_y: 0.0,
                obstacle_x: if rng.unit() < 0.6 { Some(rng.range(12.0, 35.0)) } else { None },
            };
            let feats = featurize(&scene);
            let mut label = [0.0; K];
            for k in 0..K {
                label[k] = teacher_score(&scene, TARGET_SPEEDS[k], teacher);
            }
            data.push((feats, label));
        }

        let loss = |m: &Mlp| -> f64 {
            let mut acc = 0.0;
            for (x, y) in &data {
                let p = m.forward(x);
                for k in 0..K {
                    let e = p[k] - y[k];
                    acc += e * e;
                }
            }
            acc / (data.len() * K) as f64
        };

        // Small random init.
        let mut best = Mlp { w1: [[0.0; IN]; H], b1: [0.0; H], w2: [[0.0; H]; K], b2: [0.0; K] };
        best.perturb(&mut rng, 0.3);
        let mut best_loss = loss(&best);

        let iters = 6000;
        for t in 0..iters {
            let sigma = 0.2 * (1.0 - t as f64 / iters as f64) + 0.02; // anneal 0.22 → 0.02
            let mut cand = best;
            cand.perturb(&mut rng, sigma);
            let l = loss(&cand);
            if l < best_loss {
                best = cand;
                best_loss = l;
            }
        }

        LearnedPlanner { scorer: best, teacher }
    }

    /// The teacher this planner was distilled from (audit / test introspection).
    pub fn teacher(&self) -> Teacher {
        self.teacher
    }

    fn scene_of(input: &PlanInput) -> Scene {
        let ego = input.ego.pose;
        let goal = input.goal.target;
        // Nearest object ahead along +x (the demo road frame).
        let obstacle_x = input
            .objects
            .iter()
            .map(|o| o.pos.x_m)
            .filter(|&x| x > ego.x_m)
            .fold(None, |acc: Option<f64>, x| Some(acc.map_or(x, |a| a.min(x))));
        Scene { ego_x: ego.x_m, ego_y: ego.y_m, ego_speed: input.ego.linear_x_mps, goal_x: goal.x_m, goal_y: goal.y_m, obstacle_x }
    }

    /// The vocabulary index the scorer ranks highest for `input` (test introspection).
    pub fn chosen_index(&self, input: &PlanInput) -> usize {
        let scores = self.scorer.forward(&featurize(&Self::scene_of(input)));
        argmax(&scores)
    }
}

impl Planner for LearnedPlanner {
    fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
        let scene = Self::scene_of(input);
        let best = argmax(&self.scorer.forward(&featurize(&scene)));
        PlanOutput { trajectory: materialize(&scene, TARGET_SPEEDS[best]), kind: ProposalKind::Motion }
    }
}

fn argmax(v: &[f64; K]) -> usize {
    let mut best = 0;
    for (k, &val) in v.iter().enumerate() {
        if val > v[best] {
            best = k;
        }
    }
    best
}
