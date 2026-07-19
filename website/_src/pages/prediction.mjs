import { ev, evRow, pageHero, ctaBand } from "../template.mjs";

export const meta = {
  slug: "prediction",
  title: "Prediction",
  desc: "Multi-modal predictive RSS in Kirra: constant-velocity and turn-rate hypotheses rolled forward in time against the ego trajectory, worst-case over modes, fail-closed per mode.",
};

const predSvg = `
<svg viewBox="0 0 960 280" role="img" aria-labelledby="prT prD">
  <title id="prT">Multi-modal predictive RSS</title>
  <desc id="prD">A tracked object spawns two motion hypotheses: constant velocity, straight ahead, and constant turn rate, curving toward the ego lane. Each mode is rolled forward in time and checked against the time-matched ego pose. The curving hypothesis breaches RSS in the future, so the trajectory is refused now.</desc>
  <path class="dg-edge" d="M40 200 C 300 200, 620 200, 920 200" style="stroke-width:2"/>
  <text class="dg-mono" x="40" y="228">ego trajectory →</text>
  <circle class="dg-dot" cx="120" cy="200" r="7"/>
  <text class="dg-mono" x="104" y="184">ego t₀</text>
  <circle cx="480" cy="200" r="6" fill="none" stroke="var(--accent)" stroke-dasharray="3 3"/>
  <text class="dg-mono" x="462" y="184">ego t₁</text>

  <rect class="dg-box" x="380" y="40" width="130" height="46" rx="8"/>
  <text class="dg-label" x="396" y="68">object t₀</text>
  <path class="dg-edge dg-edge--flow" d="M510 62 L 900 62"/>
  <text class="dg-mono" x="700" y="50">CV mode — stays clear ✓</text>
  <path class="dg-edge dg-edge--deny" d="M470 86 C 480 140, 478 160, 480 192"/>
  <text class="dg-mono" x="500" y="150">CTRV mode — cuts in at t₁ ✗</text>
  <circle class="dg-dot dg-dot--deny dg-pulse" cx="480" cy="196" r="6"/>
  <text class="dg-mono" x="520" y="250">one dangerous hypothesis ⇒ the whole trajectory is refused</text>
</svg>`;

export const body = `
${pageHero({
  eyebrow: "Prediction",
  title: "Judged where they'll be,<br>not where they are.",
  lede: "Snapshot RSS evaluates an object at its current position — and quietly misses the cut-in that hasn't happened yet. Kirra's predictive pass rolls every motion hypothesis forward in time and checks it against the time-matched ego pose. One dangerous future is enough to say no.",
})}

    <section aria-labelledby="h-modes">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Multi-modal RSS · ADR-0017</p>
          <h2 id="h-modes" data-reveal>Worst case over modes, fail-closed per mode</h2>
          <p class="lede" data-reveal>Each tracked object produces predicted modes: constant-velocity always, and
          constant-turn-rate when the tracker's yaw feed is fresh. A stale yaw degrades gracefully to CV-only — a
          data quality problem, not a fault. But evaluability is judged <em>per mode</em>: a mode whose samples fall
          outside the ego trajectory's time span resolves to a minimum-risk fallback, so one object's evaluable
          hypothesis can never mask another's unevaluable one.</p>
        </div>
        <div class="diagram-frame flow-anim" data-reveal>${predSvg}</div>
        ${evRow("crates/kirra-trajectory/src/prediction.rs:199", "crates/kirra-trajectory/src/validation.rs:999", "docs/adr/0017-multi-modal-predictive-rss.md")}
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-conjunction">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>The RSS core</p>
          <h2 id="h-conjunction" data-reveal>Danger requires both dimensions</h2>
          <p class="lede" data-reveal>A collision needs an object to be unsafe longitudinally <em>and</em> laterally
          at once — RSS §4's conjunction. The lateral check fires only when abreast or on a closing cut-in, which
          is what lets the ego calmly hold behind a stopped queue instead of over-rejecting every stationary object
          on the street.</p>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Published primitives, open code</h3>
            <p><code>longitudinal_safe_distance</code>, split lateral distances, opposite-direction terms, and
            occlusion-limited speed — the formal RSS repertoire implemented in plain Rust, with the §4 conjunction
            carrying its own written proof note.</p>
            ${evRow("parko/crates/parko-core/src/rss.rs:317", "docs/safety/RFC_2846_SECTION_4_PROOF.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Machine-checked properties</h3>
            <p>Kani proves totality of the longitudinal distance over integer-scaled grids (R1), the closing-speed
            monotonicity the occlusion bisection relies on (R2, weekly deep lane with an exhaustive concrete mirror
            gating every PR), and exact fail-safe behavior on invalid braking parameters (R3).</p>
            ${evRow("verification/kani/src/proofs_rss.rs", ".github/workflows/kani-deep-weekly.yml")}
          </div>
          <div class="card" data-reveal>
            <h3>Occlusion is Rule 4</h3>
            <p>What you cannot see is modeled as the worst thing it could be: approach speed at blind junctions is
            capped to the assured-clear-distance bound for that lane's sight distance, carried on the map itself.</p>
            ${evRow("parko/crates/parko-core/src/rss.rs:454", "docs/adr/0016-occlusion-aware-junction-speed-bound.md")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-interaction">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Interaction models</p>
          <h2 id="h-interaction" data-reveal>Gap acceptance without mind-reading</h2>
          <p class="lede" data-reveal>Unprotected turns use explicit gap-acceptance decision records — including a
          Stackelberg observed-cooperation model where the ego commits only on <em>observed</em> yielding, never
          assumed politeness. Design decisions, argued in the open, with their limits stated.</p>
        </div>
        ${evRow("docs/adr/0021-unprotected-turn-gap-acceptance.md", "docs/adr/0026-stackelberg-interaction-model.md", "docs/adr/0022-permitted-vs-protected-turns.md")}
      </div>
    </section>

${ctaBand("Prediction is a bound, not a guess.", "Every hypothesis the checker can't evaluate resolves toward stillness — read the per-mode fail-closed logic yourself.")}
`;
