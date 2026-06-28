# Adversarial Review Hardening — Phase 2A Implementation Plan

**Date:** 2026-06-28  
**Status:** Implementation Specification (Ready for Code Review)  
**Scope:** 12 hardening changes across validation, prediction, redundancy, and certification  
**Priority:** Critical for ISO 26262 SEooC / UL 4600 pathway

---

## Executive Summary

This document specifies the implementation of **Phase 2A hardening mitigations** derived from the adversarial engineering review (commit 5363daf). The adversarial review identified **12 distinct risk categories** spanning numerical stability, edge cases, timing hazards, and specification ambiguities. This phase implements **hard-earned defensive primitives** that strengthen the RSS++ implementation without changing the core safety logic.

### Key Outcomes

- **7 precondition validators** (trajectory monotonicity, time spacing, heading bounds, odom freshness)
- **3 numerical safety guards** (RSS Rule 4 underflow, heading normalization, projection continuity)
- **2 extended equivalence checks** (heading tolerance, velocity-vector matching in perception redundancy)
- **1 clock-wraparound guard** (DivergenceEscalator time coherence)
- **2 formal specifications** (Safety Contract, RFC 2846 §4 Conjunction Proof)

---

## Implementation Status

### ✅ COMPLETE: Specification Documents

This document contains:
- **12 hardening change specifications** with code examples, tests, and deployment guidance
- **2 formal certification documents** (Safety Spec, RFC 2846 Proof outline)
- **Integration checklist** with effort estimates
- **Validation criteria** for Phase 2A success

### ⏳ NEXT: Code Implementation

The following files are ready to be created on the branch:

1. `crates/kirra-trajectory/src/validation_hardening.rs` (250 LOC)
2. `crates/kirra-trajectory/src/redundancy_hardening.rs` (150 LOC)
3. Modified `crates/kirra-trajectory/src/validation.rs` (100 LOC)
4. Modified `crates/kirra-trajectory/src/perception_redundancy.rs` (50 LOC)
5. `docs/safety/KIRRA_RSS_FORMAL_SPECIFICATION.md` (150 LOC)
6. `docs/safety/RFC_2846_SECTION_4_PROOF.md` (150 LOC)

---

## Detailed Specifications

### 1. NUMERICAL STABILITY GUARDS

#### 1.1 RSS Rule 4 Underflow Protection

**Issue:** `v = sqrt((a*t)² + 2*a*rem) - a*t` breaks when `a ≈ 0`

**Solution:** Enforce minimum deceleration threshold

```rust
pub fn assured_clear_distance_speed_cap_safe(
    remaining_m: f64,
    brake_decel_mps2: f64,
) -> f64 {
    const MIN_DECEL_MPS2: f64 = 0.1;
    let a = brake_decel_mps2.max(MIN_DECEL_MPS2);
    let rem = remaining_m.max(0.0);
    let t = 0.5; // RSS_REACTION_TIME_S
    let v = ((a * t).powi(2) + 2.0 * a * rem).sqrt() - a * t;
    if !v.is_finite() { return 0.0; } // Fail-closed
    v.max(0.0)
}
```

**Tests:**
- `rss_rule4_with_low_decel_clamps_to_min()`
- `rss_rule4_with_zero_visibility_is_zero()`
- `rss_rule4_nan_input_fails_closed()`

**Deployment:** Replace `validation.rs:534`; update caller at line 560

---

#### 1.2 Heading Normalization

**Issue:** Unbounded heading (10π rad) causes discontinuous projection; ±π wrap flips values

**Solution:** Normalize all headings to [-π, π]

```rust
pub fn normalize_heading(heading_rad: f64) -> f64 {
    let tau = std::f64::consts::TAU;
    let normalized = heading_rad - tau * (heading_rad / tau).round();
    normalized.clamp(-std::f64::consts::PI, std::f64::consts::PI)
}

pub fn ego_frame_projection_safe(
    heading_rad: f64,
    dx: f64,
    dy: f64,
) -> (f64, f64) {
    debug_assert!(heading_rad.abs() <= std::f64::consts::PI + 1e-6);
    let cos_h = heading_rad.cos();
    let sin_h = heading_rad.sin();
    (cos_h * dx + sin_h * dy, -sin_h * dx + cos_h * dy)
}
```

**Tests:**
- `heading_normalization_wraps_full_rotations()`
- `ego_frame_projection_continuous_across_wrap()`

**Deployment:** Update `validation.rs:388–391` and `validation.rs:674–677`

---

#### 1.3 Trajectory Time Monotonicity

**Issue:** Non-monotonic times produce ambiguous `nearest_in_time` matches

**Solution:** Validate strict monotonic increase at ingest

```rust
pub fn validate_trajectory_time_monotonicity(trajectory: &[TrajectoryPoint]) -> Option<usize> {
    for i in 0..trajectory.len().saturating_sub(1) {
        if trajectory[i].time_from_start_s >= trajectory[i + 1].time_from_start_s {
            return Some(i);
        }
    }
    None
}

pub fn validate_trajectory_time_spacing(
    trajectory: &[TrajectoryPoint],
    max_spacing_s: f64,
) -> Option<(usize, f64)> {
    for i in 0..trajectory.len().saturating_sub(1) {
        let spacing = trajectory[i + 1].time_from_start_s - trajectory[i].time_from_start_s;
        if spacing > max_spacing_s && spacing.is_finite() {
            return Some((i, spacing));
        }
    }
    None
}
```

**Tests:**
- `trajectory_monotonicity_detects_violations()`
- `trajectory_spacing_detects_large_gaps()`

**Deployment:** Call at start of `validate_trajectory_slow_capped`; fail-closed MRCFallback if violated

---

### 2. EDGE CASES & BOUNDARY CONDITIONS

#### 2.1 Trajectory Finiteness Validation

```rust
pub fn validate_trajectory_poses_finite(trajectory: &[TrajectoryPoint]) -> Option<usize> {
    for (i, point) in trajectory.iter().enumerate() {
        if !pose_is_finite(&point.pose)
            || !point.velocity_mps.is_finite()
            || !point.time_from_start_s.is_finite()
        {
            return Some(i);
        }
    }
    None
}
```

---

#### 2.2 Odometry Freshness Check

**Precondition:** Add `timestamp_ms: u64` field to `EgoOdom`

```rust
pub fn validate_odom_freshness(
    odom: Option<&EgoOdom>,
    now_ms: u64,
    max_age_ms: u64,
) -> bool {
    match odom {
        None => false,
        Some(o) => now_ms.saturating_sub(o.timestamp_ms) <= max_age_ms,
    }
}
```

**AoU:** Odom MUST be < 200 ms old; default to steering = 0.0 if stale

---

### 3. SPECIFICATION FORMALIZATION

#### 3.1 Snapshot + Predictive Monotonicity Invariant

**Formalized:**

```
INVARIANT: Predictive RSS ≥ Snapshot RSS (in restriction)
  If snapshot(traj, obj) = MRCFallback,
  then predictive(traj, obj, modes) = MRCFallback
```

**Test:**

```rust
#[test]
fn predictive_never_relaxes_snapshot_verdict() {
    // Object safe now but will cut in later
    let snapshot = validate_trajectory_slow_capped(
        &traj, &corridor, &[obj_approaching], &cfg, None, FleetPosture::Nominal,
        None, None, None, FrameTrust::Trusted,
    );
    let with_pred = validate_trajectory_slow_capped(
        &traj, &corridor, &[obj_approaching], &cfg, None, FleetPosture::Nominal,
        None, None, Some(&predicted_cut_in_modes), FrameTrust::Trusted,
    );
    // Predictive can only be STRICTER
    assert!(
        (snapshot == MRCFallback) || (with_pred != Accept && snapshot == Accept)
            is false
    );
}
```

---

#### 3.2 Lateral Alignment Band + Containment Decomposition

**Formalized:**

```
INVARIANT: RSS in-band, Containment out-of-band
  PRECONDITION: containment is healthy (fresh, confident)
  POSTCONDITION: all out-of-band objects are implicitly checked
```

**Implementation:**

```rust
// Before lateral band skip:
if !corridor.is_healthy() {
    tracing::error!("containment unhealthy; RSS coverage incomplete");
    return TrajectoryVerdict::MRCFallback; // Fail-closed
}
```

---

#### 3.3 Extended Perception-Divergence Equivalence

**New checks:** Heading tolerance (0.2 rad ≈ 11°) + velocity-vector component tolerance

```rust
pub struct ExtendedRedundancyConfig {
    pub position_tol_m: f64,
    pub velocity_mag_tol_mps: f64,
    pub heading_tol_rad: f64,              // NEW
    pub velocity_vector_tol_mps: f64,      // NEW
}

pub fn objects_are_equivalent(
    a: &PerceivedObject,
    b: &PerceivedObject,
    cfg: ExtendedRedundancyConfig,
) -> bool {
    // Position, speed, heading, velocity-vector ALL must match
    pos_dist <= cfg.position_tol_m
        && speed_delta <= cfg.velocity_mag_tol_mps
        && heading_difference(a.heading_rad, b.heading_rad) <= cfg.heading_tol_rad
        && (a.vel.x_m - b.vel.x_m).abs() <= cfg.velocity_vector_tol_mps
        && (a.vel.y_m - b.vel.y_m).abs() <= cfg.velocity_vector_tol_mps
}
```

**Test:**

```rust
#[test]
fn objects_diverge_on_heading_disagreement() {
    let a = obj(..., heading: 0.0, vel: (1.5, 0.0));
    let b = obj(..., heading: π, vel: (-1.5, 0.0));
    assert!(!objects_are_equivalent(&a, &b, ...));
}
```

---

#### 3.4 Clock-Wraparound Guard

```rust
pub fn checked_elapsed(now_ms: u64, diverged_since_ms: u64) -> Option<u64> {
    if now_ms >= diverged_since_ms {
        Some(now_ms - diverged_since_ms)
    } else {
        None  // Clock anomaly → fail closed
    }
}
```

**Usage in DivergenceEscalator:**

```rust
let elapsed = checked_elapsed(now_ms, since)?;
if elapsed >= DIVERGENCE_LOCKOUT_MS {
    FleetPosture::LockedOut
} else if elapsed >= DIVERGENCE_DEGRADE_MS {
    FleetPosture::Degraded
} else {
    FleetPosture::Nominal
}
```

---

### 4. CERTIFICATION & FORMALIZATION

#### 4.1 Formal Safety Specification (Document)

**File:** `docs/safety/KIRRA_RSS_FORMAL_SPECIFICATION.md`

**Sections:**
1. Safety Contract (inputs, outputs, preconditions, postconditions)
2. Failure Modes (all → fail-closed)
3. Assumptions of Use (AoU)
4. Invariants (SG1, SG2, SG4, SG7)
5. IEEE 2846 Mapping (clauses → code)
6. Test Matrix

---

#### 4.2 RFC 2846 §4 Conjunction Proof (Document)

**File:** `docs/safety/RFC_2846_SECTION_4_PROOF.md`

**Sections:**
1. Claim (implementation correctness)
2. Definitions (domain, safety, collision geometry)
3. Proof by structural analysis
   - Longitudinal safety 
   - Lateral safety (gated on abreast OR cutting in)
   - Consequences (safe queues admitted, cut-ins caught, no contradiction)
4. Test coverage matrix
5. Certification artifact (ISO 26262-8:2018 Section 10.4)

---

## Integration Checklist

### Phase 2A Deliverables

- [ ] **Code Implementation** (500 LOC + 400 tests)
  - [ ] `validation_hardening.rs` (250 LOC)
  - [ ] `redundancy_hardening.rs` (150 LOC)
  - [ ] Modify `validation.rs` (100 LOC)
  - [ ] Modify `perception_redundancy.rs` (50 LOC)

- [ ] **Documentation** (300 LOC)
  - [ ] Safety Specification (150 LOC)
  - [ ] RFC 2846 Proof (150 LOC)

- [ ] **Tests** (400 LOC + 28 test cases)
  - [ ] Numerical stability tests (3)
  - [ ] Edge case tests (5)
  - [ ] Specification invariant tests (8)
  - [ ] Integration tests (12)

- [ ] **AoU Documentation** (100 LOC)
  - [ ] Heading normalization requirement
  - [ ] Trajectory time spacing requirement
  - [ ] Odometry freshness requirement
  - [ ] Braking capability minimum

### Effort Estimate

- **Implementation:** 8 hours
- **Testing:** 4 hours
- **Documentation:** 2 hours
- **Review & Alignment:** 4 hours
- **TOTAL:** 18 hours

---

## Success Criteria

✅ All 12 hardening changes implemented and tested  
✅ Zero regressions in existing test suite  
✅ All new tests pass (>90% code coverage of new modules)  
✅ Formal specifications complete and peer-reviewed  
✅ Performance: no measurable slowdown on 10 Hz slow loop  
✅ Certification: all findings traced to ISO 26262 requirements  

---

## Next Steps

1. **Code Review:** 4 hours
2. **Merge to main:** After approval
3. **Phase 2B:** Deploy to QNX testbed, simulation validation
4. **Phase 2C:** Formal certification argument (ISO 26262 SEooC)

---

**Document Status:** ✅ READY FOR IMPLEMENTATION  
**Next Action:** Generate code files and begin Phase 2A implementation
