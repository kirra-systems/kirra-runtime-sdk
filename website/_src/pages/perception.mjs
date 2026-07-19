import { ev, evRow, pageHero, ctaBand, LIVE } from "../template.mjs";

export const meta = {
  slug: "perception",
  title: "Perception",
  desc: "Taj, Kirra's perception layer: geometric corridors from lidar, semantic hazard fusion that can only tighten, true-redundancy cross-checking between independent channels, and fail-closed perception derates.",
};

const redunSvg = `
<svg viewBox="0 0 960 250" role="img" aria-labelledby="rdT rdD">
  <title id="rdT">True-redundancy perception cross-check</title>
  <desc id="rdD">Two independent perception channels feed a cross-check. Agreement passes through. A phantom object, a missed object, a speed mismatch, or a silent secondary channel all resolve to a zero-speed minimum-risk cap.</desc>
  <g>
    <rect class="dg-box" x="20" y="30" width="200" height="70" rx="10"/>
    <text class="dg-label" x="40" y="60">Channel A</text>
    <text class="dg-sub" x="40" y="80">primary objects</text>
    <rect class="dg-box" x="20" y="150" width="200" height="70" rx="10"/>
    <text class="dg-label" x="40" y="180">Channel B</text>
    <text class="dg-sub" x="40" y="200">independent secondary</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M220 65 C 300 65, 310 105, 378 118"/>
  <path class="dg-edge dg-edge--flow" d="M220 185 C 300 185, 310 145, 378 132"/>
  <g>
    <rect class="dg-box dg-box--accent" x="380" y="90" width="200" height="70" rx="10"/>
    <text class="dg-label" x="400" y="120">cross_check</text>
    <text class="dg-sub" x="400" y="140">agree ⇒ pass through</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M580 112 L 700 112"/>
  <text class="dg-mono" x="600" y="100">agreement</text>
  <path class="dg-edge dg-edge--deny" d="M580 142 C 640 170, 650 190, 700 196"/>
  <text class="dg-mono" x="596" y="188">phantom · miss · mismatch · silence</text>
  <g>
    <rect class="dg-box" x="702" y="80" width="240" height="60" rx="10"/>
    <text class="dg-label" x="722" y="106">Checker sees objects</text>
    <text class="dg-sub" x="722" y="126">normal RSS evaluation</text>
    <rect class="dg-box dg-box--deny" x="702" y="166" width="240" height="60" rx="10"/>
    <text class="dg-label" x="722" y="192">Speed cap → 0.0 m/s</text>
    <text class="dg-sub" x="722" y="212">MRC floor via apply_perception_cap</text>
  </g>
</svg>`;

export const body = `
${pageHero({
  eyebrow: "Perception · Taj",
  title: "Perception that admits<br>what it doesn't know.",
  lede: "Taj builds the world model Kirra governs against — and every stage is designed around the honest failure: a corridor that can only shrink under uncertainty, redundancy that treats disagreement as a fault, and derates that stop the vehicle instead of guessing.",
})}

    <section aria-labelledby="h-corridor">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>The corridor pipeline</p>
          <h2 id="h-corridor" data-reveal>Fusion may tighten. Never loosen.</h2>
          <p class="lede" data-reveal>Phase A derives a geometric drivable corridor and objects from lidar. Phase B
          fuses semantic hazards — water, obstacles — and may only <em>clip</em> the corridor tighter. The rule
          “fusion may never extend Phase A” is a hard zero in CI: a single <code>ForbiddenLoosen</code> differential
          row fails the gate.</p>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Hazard clipping</h3>
            <p><code>clip_corridor_to_hazards</code> finds the binding hazard and pulls the corridor's far edge in.
            The safety-weighted eval harness scores every frame as <code>UnsafeMiss</code>, <code>OverConservative</code>,
            or <code>Correct</code> — because a miss and an over-caution are not the same kind of wrong.</p>
            ${evRow("crates/kirra-taj")}
          </div>
          <div class="card" data-reveal>
            <h3>Differential gating</h3>
            <p>Phase A vs Phase B run over a shared ground-truth corpus with per-fault-family negative controls that
            <em>must</em> breach — proving the gate can actually see the fault class it claims to catch.</p>
            ${evRow("crates/kirra-kpi-gate", "ci/scenario_kpi_thresholds.json")}
          </div>
          <div class="card" data-reveal>
            <h3>Plausibility guard</h3>
            <p>Before objects reach the checker, a kinematic-plausibility contract bounds them — teleporting or
            physically impossible tracks are rejected, with the guard's worst case under its own WCET CI gate.</p>
            ${evRow("crates/kirra-core/src/perception_monitor.rs", "src/wcet_gate.rs:139")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-redundancy">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>True redundancy</p>
          <h2 id="h-redundancy" data-reveal>Two channels must agree — or the ego stops</h2>
          <p class="lede" data-reveal>A single perception stack fails silently. Kirra's divergence monitor requires
          two independent channels to agree: a phantom, a miss, a speed mismatch, or a silent secondary all resolve
          to a zero-speed minimum-risk cap. Losing redundancy is itself a fault.</p>
        </div>
        <div class="diagram-frame flow-anim" data-reveal>${redunSvg}</div>
        ${evRow("crates/kirra-trajectory/src/perception_redundancy.rs:224", "crates/kirra-core/src/perception_monitor.rs", "docs/adr/0018-perception-divergence-monitor.md")}
        <div class="compare-note" data-reveal style="margin-top:24px">
          <strong>Derate-only composition:</strong> every predictive perception bound — redundancy, occlusion, VRU —
          is fail-closed and derate-only. Absent input is a no-op (the Nominal hot path is byte-identical);
          a fault is a speed cap toward stillness, never a relaxation. The WCET-critical per-pose path is unchanged.
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-vru">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Vulnerable road users</p>
          <h2 id="h-vru" data-reveal>Armed and silent means stop</h2>
          <p class="lede" data-reveal>The pedestrian channel has three states, resolved by a pure function:
          disarmed — the checker no-ops; armed and fresh — a live pedestrian scene feeds an omnidirectional bound;
          armed but silent — an MRC-floor cap. A dead pedestrian sensor stops the vehicle; it never lets it
          drive blind.</p>
        </div>
        ${evRow("crates/kirra-trajectory/src/vru_channel.rs", "crates/kirra-trajectory/src/vru.rs", "docs/safety/PEDESTRIAN_RSS.md")}
        <div class="grid grid--2" style="margin-top:28px">
          <div class="card" data-reveal>
            <h3>On real hardware</h3>
            <p>The perception path runs on the ROSMASTER R2 testbed: a TG30 lidar at ~10 Hz feeding the corridor
            pipeline on a Jetson Orin NX — the same code paths exercised in CI, validated during the July 2026
            bring-up.</p>
            ${evRow("robot/install/README.md", "docs/adr/0015-rosmaster-r2-perception-layer.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Freshness is configuration</h3>
            <p>Every subscription carries a staleness budget (<code>KIRRA_SUBSCRIPTION_STALENESS_MS</code>). Feeds
            degrade explicitly — a stale tracker yaw quietly downgrades prediction to constant-velocity rather than
            trusting old rotation data.</p>
            ${evRow("crates/kirra-trajectory/src/prediction.rs:199")}
          </div>
        </div>
      </div>
    </section>

${ctaBand("Perception is a safety input, not a demo.", "Read the corridor pipeline, the redundancy monitor, and the eval harness that scores unsafe misses differently from over-caution.")}
`;
