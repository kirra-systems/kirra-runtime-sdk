// parko-core/src/impact.rs
//
// SG6 — post-collision impact latch (IMU + contact + vanished-object fusion, #102).
//
// SG6 (OCCY_SAFETY_GOALS.md H6 — ASIL A, developed to elevated rigor per owner
// decision): after a *detected collision with unconfirmed clearance* — a person
// may be under / near the vehicle — the governor IMMOBILIZES in place and
// executes NO further motion until clearance is confirmed, ≤ 1 cycle from impact.
//
// The mechanism is a post-collision LATCH that is **sticky-toward-safe** (mirrors
// `control_loop`'s `EmergencyStop`: once in the safe state, clean evidence does
// not pull it back out) and is cleared ONLY by an explicit clearance signal.

/// One tick of impact evidence. Synthetic in tests; real sensor / agent-diff
/// ingestion is DEFERRED (see the PR notes).
#[derive(Debug, Clone, Copy)]
pub struct ImpactEvidence {
    /// IMU deceleration-spike magnitude (m/s²). A NON-FINITE value does NOT latch
    /// on its own (an IMU glitch must not immobilize the vehicle) — but it is
    /// NEVER read as a confident "no impact" and NEVER suppresses a contact /
    /// vanished latch (see [`is_impact`]).
    pub imu_accel_spike_mps2: f64,
    /// A physical contact sensor fired — a definitive impact.
    pub contact_sensor: bool,
    /// A close-range tracked agent vanished between frames (the
    /// person-under-vehicle case). Supplied as a flag here; the stateful
    /// `AgentScene`-diff that derives it is DEFERRED. Latches ALONE, per SG6.
    pub vanished_object: bool,
}

/// Fusion config. `spike_threshold_mps2` is a PARAMETER with a conservative
/// **VALIDATION-PENDING** default — a track-test / SOTIF-derived value, NOT a
/// certified constant (the same honesty as #98's water thresholds).
#[derive(Debug, Clone, Copy)]
pub struct ImpactCfg {
    /// IMU deceleration magnitude (m/s²) above which a *finite* spike is read as
    /// a collision-grade impact.
    pub spike_threshold_mps2: f64,
}

impl Default for ImpactCfg {
    fn default() -> Self {
        // VALIDATION-PENDING placeholder — a hard, collision-grade deceleration.
        // NOT a certified value.
        Self { spike_threshold_mps2: 30.0 }
    }
}

/// Conservative, fail-closed fusion: an impact is declared iff **ANY** signal
/// fires —
///   * `contact_sensor` (definitive), OR
///   * a *finite* IMU spike above the threshold (hard decel), OR
///   * `vanished_object` (person-under-vehicle — latches alone, per SG6).
///
/// NaN discipline (the subtle one — do NOT fail open): the IMU term is
/// `is_finite() && > threshold`. The `is_finite()` gate makes the non-latch on a
/// glitch **explicit**, rather than relying on `NaN > threshold` being `false`
/// (the implicit fail-open trap). Because fusion is an OR, a non-finite IMU value
/// never suppresses a `contact_sensor` / `vanished_object` latch and is never
/// treated as a confident "no impact".
pub fn is_impact(evidence: &ImpactEvidence, cfg: &ImpactCfg) -> bool {
    evidence.contact_sensor
        || (evidence.imu_accel_spike_mps2.is_finite()
            && evidence.imu_accel_spike_mps2 > cfg.spike_threshold_mps2)
        || evidence.vanished_object
}

/// Sticky-toward-safe post-collision latch (SG6). Once an impact is observed it
/// STAYS latched — subsequent clean evidence never clears it; only an explicit
/// clearance signal does.
// SAFETY: SG6 | REQ: post-collision-impact-latch | TEST: test_contact_latches,test_finite_spike_over_threshold_latches,test_vanished_object_latches_alone,test_no_signals_no_latch,test_latch_is_sticky,test_explicit_clearance_clears,test_nonfinite_imu_no_spurious_latch,test_nonfinite_does_not_suppress_contact_or_vanished,test_nonfinite_does_not_clear_a_latch,test_spike_threshold_boundary
#[derive(Debug, Clone, Default)]
pub struct ImpactLatch {
    latched: bool,
}

impl ImpactLatch {
    pub fn new() -> Self {
        Self { latched: false }
    }

    /// True while latched — the governor must immobilize.
    pub fn is_latched(&self) -> bool {
        self.latched
    }

    /// Observe one tick of evidence. If it fuses to an impact, latch. STICKY:
    /// once latched this never un-latches on clean evidence — only
    /// [`clear`](Self::clear) with an explicit clearance signal does.
    pub fn observe(&mut self, evidence: &ImpactEvidence, cfg: &ImpactCfg) {
        if is_impact(evidence, cfg) {
            self.latched = true;
        }
        // else: NO-OP — never clears on clean (or non-finite) evidence.
    }

    /// Clear the latch ONLY on an explicit clearance signal (`true`). A `false`
    /// is a no-op (it never re-asserts motion). Wiring this to the #103
    /// authenticated-clearance mechanism is a DEFERRED follow-up.
    pub fn clear(&mut self, clearance: bool) {
        if clearance {
            self.latched = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ImpactCfg {
        ImpactCfg::default() // spike_threshold = 30.0
    }

    fn clean() -> ImpactEvidence {
        ImpactEvidence { imu_accel_spike_mps2: 0.5, contact_sensor: false, vanished_object: false }
    }

    #[test]
    fn test_contact_latches() {
        let e = ImpactEvidence { contact_sensor: true, ..clean() };
        assert!(is_impact(&e, &cfg()));
        let mut l = ImpactLatch::new();
        l.observe(&e, &cfg());
        assert!(l.is_latched());
    }

    #[test]
    fn test_finite_spike_over_threshold_latches() {
        let e = ImpactEvidence { imu_accel_spike_mps2: 45.0, ..clean() };
        let mut l = ImpactLatch::new();
        l.observe(&e, &cfg());
        assert!(l.is_latched(), "a finite spike above the threshold latches");
    }

    /// SG6: the vanished-object (person-under-vehicle) case latches ALONE.
    #[test]
    fn test_vanished_object_latches_alone() {
        let e = ImpactEvidence { vanished_object: true, ..clean() };
        let mut l = ImpactLatch::new();
        l.observe(&e, &cfg());
        assert!(l.is_latched(), "a vanished close-range agent latches on its own");
    }

    #[test]
    fn test_no_signals_no_latch() {
        let mut l = ImpactLatch::new();
        l.observe(&clean(), &cfg());
        assert!(!l.is_latched(), "no signals → no latch");
    }

    /// THE KEY ASSERTION: once latched, clean evidence does NOT clear it.
    #[test]
    fn test_latch_is_sticky() {
        let mut l = ImpactLatch::new();
        l.observe(&ImpactEvidence { contact_sensor: true, ..clean() }, &cfg());
        assert!(l.is_latched());
        // Subsequent clean ticks must NOT un-latch.
        l.observe(&clean(), &cfg());
        l.observe(&clean(), &cfg());
        assert!(l.is_latched(), "latch is sticky — clean evidence must not clear it");
    }

    #[test]
    fn test_explicit_clearance_clears() {
        let mut l = ImpactLatch::new();
        l.observe(&ImpactEvidence { contact_sensor: true, ..clean() }, &cfg());
        assert!(l.is_latched());
        l.clear(false); // a non-clearance is a no-op
        assert!(l.is_latched(), "clear(false) must not release the latch");
        l.clear(true); // explicit clearance
        assert!(!l.is_latched(), "an explicit clearance signal clears the latch");
    }

    /// A non-finite IMU spike alone does NOT latch (no immobilizing on a glitch).
    #[test]
    fn test_nonfinite_imu_no_spurious_latch() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let e = ImpactEvidence { imu_accel_spike_mps2: bad, ..clean() };
            assert!(!is_impact(&e, &cfg()), "non-finite IMU alone must not latch ({bad})");
        }
    }

    /// A non-finite IMU spike must NEVER suppress a contact / vanished latch
    /// (fusion is OR; the bad reading just contributes `false` to the IMU term).
    #[test]
    fn test_nonfinite_does_not_suppress_contact_or_vanished() {
        let with_contact = ImpactEvidence { imu_accel_spike_mps2: f64::NAN, contact_sensor: true, vanished_object: false };
        assert!(is_impact(&with_contact, &cfg()), "NaN IMU must not suppress a contact latch");
        let with_vanished = ImpactEvidence { imu_accel_spike_mps2: f64::NAN, contact_sensor: false, vanished_object: true };
        assert!(is_impact(&with_vanished, &cfg()), "NaN IMU must not suppress a vanished latch");
    }

    /// A non-finite reading must NOT read as a clean "no impact" that releases an
    /// existing latch — observing it while latched keeps it latched.
    #[test]
    fn test_nonfinite_does_not_clear_a_latch() {
        let mut l = ImpactLatch::new();
        l.observe(&ImpactEvidence { contact_sensor: true, ..clean() }, &cfg());
        assert!(l.is_latched());
        l.observe(&ImpactEvidence { imu_accel_spike_mps2: f64::NAN, contact_sensor: false, vanished_object: false }, &cfg());
        assert!(l.is_latched(), "a non-finite reading must not release the latch (not a clean 'no impact')");
    }

    /// Hand-checked boundary: a spike EXACTLY at the threshold does NOT latch
    /// (strict `>`); one ulp above does.
    #[test]
    fn test_spike_threshold_boundary() {
        let at = ImpactEvidence { imu_accel_spike_mps2: 30.0, ..clean() };
        assert!(!is_impact(&at, &cfg()), "spike exactly at threshold must NOT latch (strict >)");
        let above = ImpactEvidence { imu_accel_spike_mps2: 30.0 + 1e-6, ..clean() };
        assert!(is_impact(&above, &cfg()), "spike just above threshold latches");
    }
}
