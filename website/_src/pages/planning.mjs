import { ev, evRow, pageHero, ctaBand, LIVE, SPIKE } from "../template.mjs";

export const meta = {
  slug: "planning",
  title: "Planning",
  desc: "Occy, Kirra's swappable planner layer: geometric planning over a Lanelet2-lite lane graph, a learned trajectory-vocabulary planner, and Mick — a typed, fail-closed LLM intent seam. All of them only propose.",
};

const planSvg = `
<svg viewBox="0 0 960 240" role="img" aria-labelledby="plT plD">
  <title id="plT">Planning pipeline: intent to governed trajectory</title>
  <desc id="plD">Natural language becomes a typed MickIntent through one fail-closed parse. Occy grounds the intent against the lane graph into a trajectory proposal, which the Kirra checker validates before anything moves. A safe-stop proposal always exists.</desc>
  <g>
    <rect class="dg-box" x="14" y="70" width="188" height="80" rx="10"/>
    <text class="dg-label" x="32" y="100">“Take the next left”</text>
    <text class="dg-sub" x="32" y="122">LLM utterance (untrusted)</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M202 110 L 250 110"/>
  <g>
    <rect class="dg-box" x="252" y="70" width="188" height="80" rx="10"/>
    <text class="dg-label" x="270" y="100">MickIntent</text>
    <text class="dg-sub" x="270" y="122">one fail-closed JSON parse</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M440 110 L 488 110"/>
  <g>
    <rect class="dg-box" x="490" y="70" width="188" height="80" rx="10"/>
    <text class="dg-label" x="508" y="100">Occy grounds it</text>
    <text class="dg-sub" x="508" y="122">route → corridor → trajectory</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M678 110 L 726 110"/>
  <g>
    <rect class="dg-box dg-box--accent" x="728" y="70" width="216" height="80" rx="10"/>
    <text class="dg-label" x="746" y="100">KIRRA checks it</text>
    <text class="dg-sub" x="746" y="122">Accept · Clamp · MRCFallback</text>
  </g>
  <path class="dg-edge dg-edge--deny" d="M584 150 L 584 196"/>
  <text class="dg-mono" x="600" y="196">hallucinated / out-of-map intent ⇒ HOLD</text>
</svg>`;

export const body = `
${pageHero({
  eyebrow: "Planning · Occy &amp; Mick",
  title: "Any planner you like.<br>None of them trusted.",
  lede: "Kirra's thesis is doer-invariance: the planner is swappable — geometric, learned, even LLM-directed — because the safety case never depends on it. Occy is the reference doer that proves the seam works; the checker is what makes it safe.",
})}

    <section aria-labelledby="h-seam">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>The intent seam</p>
          <h2 id="h-seam" data-reveal>From language to a governed trajectory</h2>
          <p class="lede" data-reveal>Mick turns natural language into a typed <code>MickIntent</code> —
          <code>GoTo</code>, <code>LaneChange</code>, <code>Cruise</code>, <code>Overtake</code>, <code>PullOver</code>,
          <code>TurnAt</code>, <code>RouteTo</code> — through exactly one fail-closed parse. Free text can steer a
          conversation; it can never steer the vehicle.</p>
        </div>
        <div class="diagram-frame flow-anim" data-reveal>${planSvg}</div>
        <p class="diagram-caption"><span>The LLM runs locally (Gemma via Ollama) and its failure mode is HOLD — a model outage stops the conversation, not the safety case.</span></p>
        ${evRow("crates/kirra-planner/src/mick.rs", "crates/kirra-planner/src/mick_llm.rs", "crates/kirra-mick/src/lib.rs")}
        <div class="compare-note" data-reveal style="margin-top:24px">
          <strong>The actuation fence is structural, not procedural:</strong> a CI gate proves the LLM-facing crates
          have no dependency route to actuation — no release token, no serial consumer, no ROS/DDS — so Mick
          publishes intents and physically cannot publish commands.
          ${evRow("ci/check_mick_actuation_fence.py")}
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-geo">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Geometric planning</p>
          <h2 id="h-geo" data-reveal>A lane graph that knows what it can't see</h2>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Multi-junction routing</h3>
            <p>A Lanelet2-lite lane graph routes with Dijkstra, stitches a drivable corridor across junctions, and
            re-resolves from the ego pose every tick — a receding horizon over real geometry, with right-of-way
            derived from the map.</p>
            ${evRow("crates/kirra-map", "docs/adr/0019-map-derived-junction-right-of-way.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Occlusion-aware approach</h3>
            <p>Sight distance is carried per approach lane. At a blind junction the planner caps speed to the
            assured-clear-distance bound (RSS Rule 4), so the ego creeps in rather than betting on an empty road.</p>
            ${evRow("crates/kirra-planner/src/behavior.rs", "docs/adr/0016-occlusion-aware-junction-speed-bound.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Safe-stop always exists</h3>
            <p><code>PlanOutput::safe_stop</code> is a structural requirement: every planner must always offer a
            minimum-risk proposal. A planner with no stop output would deadlock the loop — so it can't exist.</p>
            ${evRow("crates/kirra-planner/src/lib.rs")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-learned">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Learned planning</p>
          <h2 id="h-learned" data-reveal>A neural doer the checker can catch</h2>
          <p class="lede" data-reveal>The learned planner is deliberately Hydra-MDP-shaped: a fixed trajectory
          vocabulary scored by a small MLP, fit by a seeded evolution strategy — fully deterministic from its seed,
          no GPU, no opaque model files. It exists to prove the safety case, not to win a leaderboard.</p>
        </div>
        <div class="grid grid--2">
          <div class="card" data-reveal>
            <h3>Two teachers, one lesson</h3>
            <p>Train it against a <code>SafetyAware</code> teacher and its plans sail through the checker. Train it
            against a <code>ProgressOnly</code> teacher and the checker catches the misalignment — demonstrated in
            tests, and measured continuously: the learned doer's admissibility floor (≥90.3%) is a blocking CI gate.</p>
            ${evRow("crates/kirra-planner/src/learned.rs", "ci/scenario_kpi_thresholds.json")}
          </div>
          <div class="card" data-reveal>
            <h3>Maneuver vocabulary</h3>
            <p>A 2-D lateral-offset × speed vocabulary lets the network route <em>around</em> obstacles. Kirra admits a
            band-clearing pass that fits the corridor and rejects one that doesn't — same checker, richer doer.</p>
            ${evRow("crates/kirra-planner/src/learned.rs")}
          </div>
        </div>
      </div>
    </section>

${ctaBand("Swap the doer. Keep the proof.", "The planner trait, the intent vocabulary, and the always-available safe stop are all in the open — bring your own doer and let the checker hold it.")}
`;
