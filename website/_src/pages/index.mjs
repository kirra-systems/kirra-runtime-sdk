import { SITE, ev, evRow, pill, LIVE, GATED, SPIKE } from "../template.mjs";

export const meta = {
  slug: "index",
  title: "Kirra Systems",
  desc: "Every machine is getting a model, and no model is certifiable. Kirra is the governed layer of autonomy: a fail-closed, machine-checked envelope between AI intent and actuation. Open source, in Rust.",
  subnav: false,
  jsonld: {
    "@context": "https://schema.org",
    "@graph": [
      {
        "@type": "Organization",
        name: "Kirra Systems",
        url: "https://kirrasystems.com",
        logo: "https://kirrasystems.com/assets/kirra-logo.png",
        sameAs: ["https://github.com/kirra-systems/kirra-runtime-sdk"],
      },
      {
        "@type": "WebSite",
        name: "Kirra Systems",
        url: "https://kirrasystems.com",
      },
      {
        "@type": "SoftwareSourceCode",
        name: "kirra-runtime-sdk",
        codeRepository: "https://github.com/kirra-systems/kirra-runtime-sdk",
        programmingLanguage: "Rust",
        description: "Distributed runtime legitimacy engine and safety governor for AI-driven robotic and edge systems.",
      },
    ],
  },
};

/* ---------- architecture flow SVG (doer / checker / actuator) ---------- */
const archSvg = `
<svg viewBox="0 0 960 400" role="img" aria-labelledby="archTitle archDesc">
  <title id="archTitle">Kirra doer–checker architecture</title>
  <desc id="archDesc">Three untrusted doers — Mick, an LLM intent layer; Occy, a planner; and Taj, perception — send typed proposals into the Kirra checker, which applies the posture gate, kinematic envelope, RSS and containment. Accepted commands receive an Ed25519 release token verified by the actuator. Denied commands become minimum-risk maneuvers and are recorded in a hash-chained audit ledger.</desc>

  <!-- doers -->
  <g>
    <rect class="dg-box" x="16" y="30" width="190" height="72" rx="10"/>
    <text class="dg-label" x="34" y="60">Mick — LLM intent</text>
    <text class="dg-sub" x="34" y="80">typed MickIntent · fail-closed parse</text>
    <rect class="dg-box" x="16" y="152" width="190" height="72" rx="10"/>
    <text class="dg-label" x="34" y="182">Occy — planner</text>
    <text class="dg-sub" x="34" y="202">geometric · learned · proposes only</text>
    <rect class="dg-box" x="16" y="274" width="190" height="72" rx="10"/>
    <text class="dg-label" x="34" y="304">Taj — perception</text>
    <text class="dg-sub" x="34" y="324">corridor · objects · redundancy</text>
    <text class="dg-sub" x="16" y="384">UNTRUSTED DOERS — swappable, never safety authorities</text>
  </g>

  <!-- claims edges -->
  <path class="dg-edge dg-edge--flow" d="M206 66 C 290 66, 300 150, 372 172"/>
  <path class="dg-edge dg-edge--flow" d="M206 188 L 372 194"/>
  <path class="dg-edge dg-edge--flow" d="M206 310 C 290 310, 300 240, 372 216"/>
  <text class="dg-mono" x="252" y="150">typed claims</text>

  <!-- checker -->
  <g>
    <rect class="dg-box dg-box--accent" x="374" y="120" width="240" height="150" rx="12"/>
    <text class="dg-label" x="396" y="150">KIRRA — the checker</text>
    <text class="dg-mono" x="396" y="176">posture gate · fail-closed</text>
    <text class="dg-mono" x="396" y="196">kinematic envelope P0–P6</text>
    <text class="dg-mono" x="396" y="216">RSS ∧ conjunction · occlusion</text>
    <text class="dg-mono" x="396" y="236">containment · predictive modes</text>
    <text class="dg-sub" x="396" y="258">sole safety authority</text>
  </g>

  <!-- release path -->
  <path class="dg-edge dg-edge--flow" d="M614 175 L 700 175"/>
  <g>
    <rect class="dg-box" x="702" y="140" width="118" height="70" rx="10"/>
    <text class="dg-label" x="716" y="168">Release</text>
    <text class="dg-sub" x="716" y="188">Ed25519 token</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M820 175 L 872 175"/>
  <g>
    <rect class="dg-box" x="874" y="140" width="74" height="70" rx="10"/>
    <text class="dg-label" x="886" y="168">Motor</text>
    <text class="dg-sub" x="886" y="188">verify first</text>
  </g>
  <circle class="dg-dot dg-pulse" cx="660" cy="175" r="4"/>

  <!-- deny path -->
  <path class="dg-edge dg-edge--deny" d="M494 270 L 494 316"/>
  <g>
    <rect class="dg-box dg-box--deny" x="380" y="318" width="228" height="58" rx="10"/>
    <text class="dg-label" x="398" y="342">Deny → MRC + audit ledger</text>
    <text class="dg-sub" x="398" y="362">SHA-256 hash-chained · verdict id</text>
  </g>
</svg>`;

/* ---------- posture pipeline SVG ---------- */
const postureSvg = `
<svg viewBox="0 0 960 210" role="img" aria-labelledby="pgTitle pgDesc">
  <title id="pgTitle">Command routing under fleet posture</title>
  <desc id="pgDesc">A command flows through classification, the posture gate, and the degraded decel-to-stop gate before reaching the actuator. Which stages pass depends on the selected fleet posture.</desc>
  <g>
    <rect class="dg-box" x="10" y="70" width="150" height="64" rx="10"/>
    <text class="dg-label" x="26" y="98">Command</text>
    <text class="dg-sub" x="26" y="118">HTTP · ROS 2 · SHM</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M160 102 L 216 102"/>
  <g>
    <rect class="dg-box" x="218" y="70" width="168" height="64" rx="10"/>
    <text class="dg-label" x="234" y="98">Classify</text>
    <text class="dg-sub" x="234" y="118">allowlist · Unknown⇒deny</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M386 102 L 442 102"/>
  <g>
    <rect class="dg-box dg-box--accent" x="444" y="70" width="168" height="64" rx="10"/>
    <text class="dg-label" x="460" y="98">Posture gate</text>
    <text class="dg-sub" x="460" y="118">stale cache ⇒ deny</text>
  </g>
  <path class="dg-edge dg-edge--flow" data-on="nominal degraded" d="M612 102 L 668 102"/>
  <g data-on="nominal degraded">
    <rect class="dg-box" x="670" y="70" width="168" height="64" rx="10"/>
    <text class="dg-label" x="686" y="98">Decel gate</text>
    <text class="dg-sub" x="686" y="118">Degraded ⇒ stop &amp; hold</text>
  </g>
  <path class="dg-edge dg-edge--flow" data-on="nominal degraded" d="M838 102 L 886 102"/>
  <g data-on="nominal degraded">
    <rect class="dg-box" x="888" y="70" width="62" height="64" rx="10"/>
    <text class="dg-label" x="900" y="106">Act</text>
  </g>
  <path class="dg-edge dg-edge--deny" data-on="degraded lockedout" d="M528 134 L 528 172"/>
  <text class="dg-mono" data-on="degraded lockedout" x="544" y="176">denied writes → 503 · MRC · audit</text>
</svg>`;

/* ---------- page body ---------- */
export const body = `
    <!-- ═══════════ HERO ═══════════ -->
    <section class="hero" aria-label="Introduction">
      <canvas class="hero__canvas" id="heroField" aria-hidden="true"></canvas>
      <div class="container">
        <p class="hero__eyebrow eyebrow">The governed layer of autonomy</p>
        <h1>Every machine gets a model.<br>Every model needs an <span class="accent">envelope.</span></h1>
        <p class="lede">AI capability is outrunning verification — and no amount of training makes a model
        certifiable. Kirra is the fail-closed runtime layer that closes that gap: planners, learned policies,
        and LLMs only ever <em>propose</em>, while a deterministic, machine-checked envelope decides what
        reaches the actuators. The models will keep changing. The envelope is forever.</p>
        <div class="hero__ctas">
          <a class="btn btn--primary" href="vision.html">Where autonomy is heading <span class="arrow" aria-hidden="true">→</span></a>
          <a class="btn btn--ghost" href="${SITE.repo}" target="_blank" rel="noopener">Read the source ↗</a>
        </div>
        <p class="hero__proof">
          <span><span class="tick" aria-hidden="true">✓</span> 3,305 tests across 330 files</span>
          <span><span class="tick" aria-hidden="true">✓</span> 12 machine-checked Kani proofs</span>
          <span><span class="tick" aria-hidden="true">✓</span> 27 blocking CI gates</span>
          <span><span class="tick" aria-hidden="true">✓</span> bit-identical incident replay</span>
          <span><span class="tick" aria-hidden="true">✓</span> open source · Rust</span>
        </p>
      </div>
    </section>

    <!-- ═══════════ THE VISION ═══════════ -->
    <section id="tomorrow" aria-labelledby="h-tomorrow">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>The world of tomorrow</p>
          <h2 id="h-tomorrow" data-reveal>Autonomy will be won by whoever<br>can prove safety at runtime.</h2>
          <p class="lede" data-reveal>Three forces make the governed layer inevitable — and every technical
          decision on this site exists to build it.</p>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>AI is outrunning verification</h3>
            <p>Foundation models are entering planners and language is becoming a robot interface. You can't
            test a distribution into being safe — but you can bound what any of it is permitted to do.</p>
            ${evRow("docs/adr/0020-doer-invariant-safety-case.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Regulation demands runtime assurance</h3>
            <p>UL 4600, SOTIF, ASTM F3269, and the emerging AI-safety standards all converge on the same
            architecture: an untrusted complex function behind a verifiable monitor. Kirra maintains open,
            versioned mappings to each.</p>
            ${evRow("docs/safety/ASTM_F3269_RTA_MAPPING.md", "docs/safety/UL4600_SAFETY_CASE.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Platforms need a decision layer</h3>
            <p>Certified hypervisors and safety-rated silicon solve isolation and compute — not whether a
            command is safe. Kirra is built as the decision layer for exactly those platforms, as an SEooC
            designed for integration.</p>
            ${evRow("docs/adr/0032-governor-deployment-platform.md")}
          </div>
        </div>
        <div class="grid grid--2" style="align-items:center;gap:48px;margin-top:40px">
          <div style="display:grid;gap:10px" data-reveal aria-hidden="true">
            <div class="card" style="padding:16px 22px"><p class="mono dim" style="font-size:0.8rem">AI doer stacks — unverifiable, swappable</p></div>
            <div class="card" style="padding:16px 22px;border-color:var(--accent-line);background:var(--accent-soft)"><p class="mono" style="font-size:0.8rem;color:var(--accent)">KIRRA — the governed layer (SEooC)</p></div>
            <div class="card" style="padding:16px 22px"><p class="mono dim" style="font-size:0.8rem">certified OS / hypervisor — QNX 8.0 target</p></div>
            <div class="card" style="padding:16px 22px"><p class="mono dim" style="font-size:0.8rem">safety-rated silicon — vendor-neutral inference</p></div>
          </div>
          <div data-reveal>
            <h3>The one durable interface</h3>
            <p class="muted" style="margin-top:10px">Models will keep changing. Sensors will keep changing. The
            interface that endures is the envelope between intent and actuation — proven, portable, and built to
            sit inside certified platforms rather than compete with them.</p>
            <p style="margin-top:16px"><a class="card__more" href="vision.html">The full vision <span aria-hidden="true">→</span></a></p>
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <!-- ═══════════ USE CASES ═══════════ -->
    <section id="use-cases" aria-labelledby="h-uses">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>The stakes today</p>
          <h2 id="h-uses" data-reveal>Machines make mistakes.<br>Kirra makes them survivable.</h2>
          <p class="lede" data-reveal>The same checker, holding very different machines to their physics —
          each of these is a real failure mode with a real mechanism behind it.</p>
        </div>
        <div class="grid grid--2">
          <a class="card" href="solutions.html#av" data-reveal>
            <h3>Autonomous vehicles</h3>
            <p>After a fault, the only admissible commands converge to a stop — designed against the 2023
            San Francisco dragging incident. And standing water is non-drivable by policy, because lidar
            can't see a flood — only the absence where a road should be.</p>
            <span class="card__more">How AVs are governed <span aria-hidden="true">→</span></span>
          </a>
          <a class="card" href="solutions.html#robots" data-reveal>
            <h3>Robots around people</h3>
            <p>A pedestrian sensor that goes silent stops the robot — because a machine that can't prove the
            sidewalk is clear shouldn't keep driving on stale confidence. Proven on physical hardware.</p>
            <span class="card__more">How robots are governed <span aria-hidden="true">→</span></span>
          </a>
          <a class="card" href="solutions.html#industrial" data-reveal>
            <h3>Industrial control</h3>
            <p>Oldsmar, 2021: a water plant's lye setpoint was raised 111× on valid credentials, caught only by
            an operator watching the screen. Kirra decodes the write on the wire and denies on magnitude —
            identity doesn't excuse physics.</p>
            <span class="card__more">How OT is governed <span aria-hidden="true">→</span></span>
          </a>
          <a class="card" href="solutions.html#llm" data-reveal>
            <h3>LLM-driven machines</h3>
            <p>The model hallucinates a 999 m/s command? Language becomes motion through exactly one typed,
            fail-closed door — and a CI gate proves the conversational code can't reach the actuators at all.</p>
            <span class="card__more">How LLMs are fenced <span aria-hidden="true">→</span></span>
          </a>
        </div>
        <a class="card__more" href="solutions.html" style="margin-top:28px" data-reveal>All solutions, including fleet operations <span aria-hidden="true">→</span></a>
      </div>
    </section>

    <hr class="rule">

    <!-- ═══════════ DOER / CHECKER ═══════════ -->
    <section id="architecture" aria-labelledby="h-arch">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>How we get there</p>
          <h2 id="h-arch" data-reveal>The AI proposes.<br>Kirra <span style="color:var(--accent)">bounds it.</span></h2>
          <p class="lede" data-reveal>The mechanism that delivers the vision: every autonomy stack has a component
          that decides what the vehicle does next, and Kirra assumes that component is wrong — hallucinating,
          compromised, or simply buggy. Doers are swappable; the checker is the invariant — a separate, smaller,
          provable layer that no doer can bypass.</p>
        </div>
        <div class="diagram-frame flow-anim" data-reveal>${archSvg}</div>
        <p class="diagram-caption">
          <span>Deny is the default: no registered proof, no posture, no envelope fit — no motion.</span>
        </p>
        ${evRow("README.md", "crates/kirra-planner/src/lib.rs", "crates/kirra-trajectory/src/validation.rs", "crates/kirra-release-token/src/lib.rs", "src/audit_chain.rs")}
        <div class="grid grid--3" style="margin-top:40px">
          <div class="card" data-reveal>
            <h3>Never trusted for safety</h3>
            <p>An LLM can invent a 999&nbsp;m/s velocity or an action type that doesn't exist. Both die at the
            typed parse or the envelope — and the attempt is permanently recorded in the hash-chained audit ledger.</p>
            ${evRow("README.md:40", "crates/kirra-planner/src/mick_llm.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>One safety authority</h3>
            <p><code>validate_trajectory_slow</code> composes containment, per-pose kinematics, and RSS into a single
            verdict: <code>Accept</code>, <code>Clamp</code>, or <code>MRCFallback</code>. First rejection wins.</p>
            ${evRow("crates/kirra-trajectory/src/validation.rs:206")}
          </div>
          <div class="card" data-reveal>
            <h3>Frozen where it matters</h3>
            <p>The kinematics checker core is pinned by git blob hash <code>ed00f4da…</code> — CI asserts the exact
            bytes survive every change, and the Kani proofs run against the shipped source verbatim.</p>
            ${evRow("verification/kani/", ".github/workflows/ci.yml:182")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <!-- ═══════════ INTERACTIVE SAFETY PIPELINE ═══════════ -->
    <section id="pipeline" aria-labelledby="h-pipeline">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Interactive — fleet posture</p>
          <h2 id="h-pipeline" data-reveal>Three postures. No exceptions.</h2>
          <p class="lede" data-reveal>Fleet trust state collapses to <strong>Nominal</strong>, <strong>Degraded</strong>, or
          <strong>LockedOut</strong>, derived from a dependency-graph traversal over every node's attested state.
          Flip the posture and watch what the gateway does to each command class — this table is the real
          <code>should_route_command</code> decision, not an illustration.</p>
        </div>
        <div data-posture-demo data-posture-initial="nominal" data-reveal>
          <div style="display:flex;flex-wrap:wrap;gap:16px;align-items:center;justify-content:space-between;margin-bottom:20px">
            <div class="posture-switch" role="group" aria-label="Select fleet posture">
              <button data-posture="nominal" aria-pressed="true">NOMINAL</button>
              <button data-posture="degraded" aria-pressed="false">DEGRADED</button>
              <button data-posture="lockedout" aria-pressed="false">LOCKED OUT</button>
            </div>
            <p class="mono dim" data-posture-status style="font-size:0.8rem;max-width:52ch" aria-live="polite"></p>
          </div>
          <div class="diagram-frame flow-anim">${postureSvg}</div>
          <div class="table-wrap" style="margin-top:20px">
            <table class="tbl">
              <caption class="visually-hidden">Command routing verdicts by fleet posture</caption>
              <thead><tr><th scope="col">Command class</th><th scope="col">Example route</th><th scope="col">Verdict</th></tr></thead>
              <tbody>
                <tr><td>ReadTelemetry</td><td class="mono">GET /fleet/posture</td>
                  <td><span data-v-nominal="allow|ALLOWED" data-v-degraded="allow|ALLOWED" data-v-lockedout="allow|ALLOWED — posture-exempt read"></span></td></tr>
                <tr><td>ActuatorMotion</td><td class="mono">POST /actuator/motion/command</td>
                  <td><span data-v-nominal="allow|ALLOWED — full envelope" data-v-degraded="gate|DEFERRED — decel-to-stop gate" data-v-lockedout="deny|DENIED"></span></td></tr>
                <tr><td>WriteState</td><td class="mono">POST /federation/reports/submit</td>
                  <td><span data-v-nominal="allow|ALLOWED" data-v-degraded="deny|DENIED — 503" data-v-lockedout="deny|DENIED"></span></td></tr>
                <tr><td>SystemMutation</td><td class="mono">POST /fleet/dependencies</td>
                  <td><span data-v-nominal="allow|ALLOWED" data-v-degraded="deny|DENIED — 503" data-v-lockedout="deny|DENIED"></span></td></tr>
                <tr><td>Unknown</td><td class="mono">anything unclassified</td>
                  <td><span data-v-nominal="deny|DENIED — always" data-v-degraded="deny|DENIED — always" data-v-lockedout="deny|DENIED — always"></span></td></tr>
              </tbody>
            </table>
          </div>
          <p class="evidence-note">Observability GETs are posture-exempt so operators can distinguish “locked out” from “down” —
          a read cannot actuate. A stale posture cache (&gt;5&nbsp;s) fails closed exactly like LockedOut.
          Degraded is not a crawl: it is a controlled decel-to-stop-and-hold; the governor never authors re-acceleration.</p>
          ${evRow("src/posture_cache.rs", "src/posture_engine_v2.rs", "src/gateway/policy_layer.rs", "docs/safety/SAFE_STATE_SPECIFICATION.md")}
        </div>
        <a class="card__more" href="safety.html" style="margin-top:24px">The full safety model <span aria-hidden="true">→</span></a>
      </div>
    </section>

    <hr class="rule">

    <!-- ═══════════ LIFE OF A COMMAND (timeline) ═══════════ -->
    <section id="lifecycle" aria-labelledby="h-life">
      <div class="container">
        <div class="grid grid--2" style="align-items:start;gap:48px">
          <div class="section-head sticky-col">
            <p class="eyebrow" data-reveal>Data flow</p>
            <h2 id="h-life" data-reveal>The life of one command</h2>
            <p class="lede" data-reveal>From an AI's proposal to a verified actuator release — every stage is a
            separate, testable function, and every stage can say no.</p>
            <p data-reveal style="margin-top:16px"><a class="card__more" href="runtime.html">Runtime internals <span aria-hidden="true">→</span></a></p>
          </div>
          <ol class="timeline" style="list-style:none;padding-left:36px;margin:0">
            <li class="timeline__item" data-reveal>
              <p class="timeline__step">01 · Propose</p>
              <h3>A doer emits a typed claim</h3>
              <p>Occy plans a trajectory; Mick grounds an LLM utterance into a typed <code>MickIntent</code> through one fail-closed parse. Free text never reaches the control path.</p>
              ${evRow("crates/kirra-planner/src/mick.rs", "crates/kirra-planner/src/mick_llm.rs")}
            </li>
            <li class="timeline__item" data-reveal>
              <p class="timeline__step">02 · Classify</p>
              <h3>Total, pure classification</h3>
              <p><code>classify_http_command</code> is a fail-closed allowlist — anything it doesn't recognize is <code>Unknown</code>, and <code>Unknown</code> is denied in every posture, including Nominal.</p>
              ${evRow("crates/kirra-policy-types/src/lib.rs")}
            </li>
            <li class="timeline__item" data-reveal>
              <p class="timeline__step">03 · Posture</p>
              <h3>The fleet gate</h3>
              <p><code>should_route_command(cache, now_ms, command)</code> — a stale cache (&gt;5,000&nbsp;ms) is treated as LockedOut. Trust is recomputed from a cycle-safe DAG traversal over attested nodes.</p>
              ${evRow("src/posture_cache.rs", "crates/kirra-safety-authority/src/dag.rs")}
            </li>
            <li class="timeline__item" data-reveal>
              <p class="timeline__step">04 · Envelope</p>
              <h3>Hard boundary first</h3>
              <p><code>validate_vehicle_command</code> clamps to the absolute kinematic boundary before any rate limit — the envelope cap always wins. NaN or Inf anywhere is an immediate deny, proved for all 2⁶⁴ bit patterns.</p>
              ${evRow("crates/kirra-core/src/kinematics_contract.rs", "verification/kani/src/proofs_kinematics.rs:84")}
            </li>
            <li class="timeline__item" data-reveal>
              <p class="timeline__step">05 · RSS</p>
              <h3>Formal distance keeping</h3>
              <p>Longitudinal <em>and</em> lateral safety evaluated as a conjunction, extended with occlusion-aware speed bounds at blind junctions and multi-modal prediction rolled forward in time.</p>
              ${evRow("parko/crates/parko-core/src/rss.rs:317", "crates/kirra-trajectory/src/validation.rs:999")}
            </li>
            <li class="timeline__item" data-reveal>
              <p class="timeline__step">06 · Verdict</p>
              <h3>Accept · Clamp · MRC</h3>
              <p>A denial mints a verdict id, binds the exact inputs into the audit chain, and returns an operator-readable explanation at <code>GET /verdicts/{id}</code>.</p>
              ${evRow("src/verdicts.rs")}
            </li>
            <li class="timeline__item" data-reveal>
              <p class="timeline__step">07 · Release</p>
              <h3>Sign the enforced bytes</h3>
              <p>The governor signs an Ed25519 release token over <em>exactly</em> the bytes it validated. The crypto rides the actuation path — never inside the verdict's timing budget.</p>
              ${evRow("crates/kirra-release-token/src/lib.rs", "docs/adr/0031-release-token-on-the-actuation-path.md")}
            </li>
            <li class="timeline__item" data-reveal>
              <p class="timeline__step">08 · Verify-before-release</p>
              <h3>The actuator checks everything</h3>
              <p>Token → strict signature over the presented bytes → strictly-advancing release sequence (equal = replay) → decode. A refusal never poisons the watermark.</p>
              ${evRow("crates/kirra-inline-governor/src/lib.rs")}
            </li>
            <li class="timeline__item" data-reveal>
              <p class="timeline__step">09 · Record</p>
              <h3>Tamper-evident memory</h3>
              <p>Every decision lands in a SHA-256 hash-chained ledger that survives <code>SIGKILL</code> mid-append as a valid prefix — proven by a power-loss drill in CI — and ships off-box to WORM storage.</p>
              ${evRow("src/audit_chain.rs", "tests/audit_chain_prefix_on_kill.rs", "src/audit_shipper.rs")}
            </li>
            <li class="timeline__item" data-reveal>
              <p class="timeline__step">10 · Replay</p>
              <h3>Bit-identical reconstruction</h3>
              <p>Any captured session re-runs through the <em>real</em> checker and must reproduce every verdict to the exact bit (<code>f64::to_bits</code>, no epsilon). Incidents are reconstructed, not approximated.</p>
              ${evRow("crates/kirra-replay/src/lib.rs", "docs/REPLAY_INCIDENT_RECONSTRUCTION.md")}
            </li>
          </ol>
        </div>
      </div>
    </section>

    <hr class="rule">

    <!-- ═══════════ ENFORCED PATH / ZERO-COPY ═══════════ -->
    <section id="enforced-path" aria-labelledby="h-fast">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>The enforced path</p>
          <h2 id="h-fast" data-reveal>No HTTP between verdict and motor.</h2>
          <p class="lede" data-reveal>On the enforcement path the governor reads commands from shared memory through a
          seqlock, decides, signs, and releases — in-process, allocation-free, with the memory model checked
          under Miri and the concurrency protocol model-checked under Loom.</p>
        </div>
        <div class="grid grid--2" style="align-items:stretch">
          <div class="chart" data-reveal>
            <p class="chart__title">Transport read latency — p50</p>
            <p class="chart__sub">Host-indicative measurements from the repository's own benches. Not certified WCET.</p>
            <div class="chart__body barlist">
              <div class="barlist__row"><span class="barlist__label">In-process floor</span><span class="barlist__track"><span class="barlist__fill" style="--w:2.9%"></span></span><span class="barlist__value">28 ns</span></div>
              <div class="barlist__row"><span class="barlist__label">Shared-memory seqlock</span><span class="barlist__track"><span class="barlist__fill" style="--w:6.2%"></span></span><span class="barlist__value">60 ns</span></div>
              <div class="barlist__row"><span class="barlist__label">iceoryx2 zero-copy</span><span class="barlist__track"><span class="barlist__fill" style="--w:31.7%"></span></span><span class="barlist__value">309 ns</span></div>
              <div class="barlist__row"><span class="barlist__label">UDP + serde proxy</span><span class="barlist__track"><span class="barlist__fill" style="--w:100%"></span></span><span class="barlist__value">976 ns</span></div>
            </div>
            ${evRow("tools/iceoryx2-spike/src/bin/latency_bench.rs", "crates/kirra-hv-carrier/README.md")}
          </div>
          <div class="card" data-reveal>
            <h3>The frozen contract</h3>
            <p><code>GovernorContractView</code> is a <code>#[repr(C)]</code>, pointer-free struct whose byte layout is pinned
            by compile-time asserts — “the freeze <em>is</em> the safety claim.” A seqlock generation protocol
            (odd = write in progress) guarantees the checker only ever judges a coherent snapshot; torn headers
            are eliminated by construction.</p>
            <p style="margin-top:12px">The verdict core is <code>no_std</code>, panic-free by policy, and gated for purity in CI —
            no allocation, no clock, no crypto inside the decision.</p>
            ${evRow("crates/kirra-contract-channel/src/lib.rs", "docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md", "ci/check_verdict_core_purity.py")}
            <div class="evidence-row" style="margin-top:16px">${LIVE} <span class="dim" style="font-size:0.8rem">cross-process over POSIX SHM, in CI</span></div>
          </div>
        </div>
        <div class="chart" style="margin-top:20px" data-reveal>
          <p class="chart__title">Assembled in-line loop — read → validate → decide → sign → verify → release</p>
          <p class="chart__sub">p99.9 over 10,000 release-build iterations on CI hardware (Ed25519-dominated). The gate asserts p99.9, not mean.</p>
          <div class="chart__body budget">
            <div class="budget__track">
              <span class="budget__fill" style="--w:11.6%"></span>
              <span class="budget__limit" style="--limit:10%"></span>
            </div>
            <div class="budget__legend">
              <span class="k"><span class="swatch" style="background:var(--chart-1)"></span> measured ≈116 µs p99.9 (host)</span>
              <span class="k"><span class="swatch" style="background:var(--danger)"></span> 100 µs deployment target (SG9)</span>
              <span class="k"><span class="swatch" style="background:var(--surface-3);border:1px solid var(--line-strong)"></span> 1,000 µs CI regression ceiling</span>
            </div>
          </div>
          <p class="evidence-note">Certified WCET is defined as QNX-target-under-FIFO measurement and has not been produced yet —
          the repository says so explicitly, and so do we. Host numbers are regression tripwires, not claims.</p>
          ${evRow("src/wcet_gate.rs:92", "crates/kirra-inline-governor/README.md", "docs/safety/WCET_MEASUREMENT_METHODOLOGY.md")}
        </div>
      </div>
    </section>

    <hr class="rule">

    <!-- ═══════════ ASSURANCE DASHBOARD ═══════════ -->
    <section id="assurance" aria-labelledby="h-assure">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Assurance, measured</p>
          <h2 id="h-assure" data-reveal>Trust is a number you can check.</h2>
          <p class="lede" data-reveal>Every figure below is computed from the repository and links to where it lives.
          If we can't cite it, we don't claim it.</p>
        </div>
        <div class="stats stats--4" data-reveal>
          <div class="stat">
            <p class="stat__value"><span data-count="3305">0</span></p>
            <p class="stat__label">test functions across 330 files, run on every push</p>
            <p class="stat__evidence">${ev(".github/workflows/ci.yml:231", "ci.yml · Test")}</p>
          </div>
          <div class="stat">
            <p class="stat__value"><span data-count="12">0</span></p>
            <p class="stat__label">Kani proof harnesses — model-checked over <em>all</em> inputs, not sampled</p>
            <p class="stat__evidence">${ev("verification/kani/", "verification/kani")}</p>
          </div>
          <div class="stat">
            <p class="stat__value"><span data-count="27">0</span></p>
            <p class="stat__label">blocking CI lanes: Miri, Loom, fuzz, mutation, coverage floors, supply chain</p>
            <p class="stat__evidence">${ev(".github/workflows/ci.yml", "ci.yml")}</p>
          </div>
          <div class="stat">
            <p class="stat__value"><span data-count="248">0</span></p>
            <p class="stat__label">scenario KPI corpus + 71 perception frames; 40k-sample Monte-Carlo nightly</p>
            <p class="stat__evidence">${ev("crates/kirra-kpi-gate/src/lib.rs", "kirra-kpi-gate")}</p>
          </div>
        </div>
        <div class="grid grid--3" style="margin-top:20px">
          <div class="card" data-reveal>
            <h3>Adversarial by default</h3>
            <p>4 fuzz targets run on every push and 90 minutes each, weekly. PR diffs to the checker face
            mutation testing: if a mutant survives your tests, the gate fails.</p>
            ${evRow("fuzz/fuzz_targets/", ".github/workflows/ci.yml:636")}
          </div>
          <div class="card" data-reveal>
            <h3>Coverage that can't regress</h3>
            <p>Gated decision-coverage floors on the checker crates (77–79%, measured ≈80%) plus a Codecov
            ratchet: any PR dropping workspace coverage by &gt;0.5% fails.</p>
            ${evRow(".github/workflows/ci.yml:379", "codecov.yml")}
          </div>
          <div class="card" data-reveal>
            <h3>Supply chain, pinned</h3>
            <p>cargo-deny + cargo-audit gate every dependency; GitHub Actions are pinned to 40-hex commit SHAs;
            container images are digest-pinned and keyless-signed with cosign.</p>
            ${evRow("deny.toml", "scripts/pin-actions.sh", ".github/workflows/docker.yml")}
          </div>
        </div>
        <a class="card__more" href="benchmarks.html" style="margin-top:28px">All benchmarks &amp; gates, with numbers <span aria-hidden="true">→</span></a>
      </div>
    </section>

    <hr class="rule">

    <!-- ═══════════ PLATFORM GRID ═══════════ -->
    <section id="platform" aria-labelledby="h-platform">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>The platform</p>
          <h2 id="h-platform" data-reveal>Explore every subsystem</h2>
        </div>
        <div class="grid grid--3">
          <a class="card" href="runtime.html" data-reveal><h3>Runtime</h3><p>The verifier service: posture engine, attestation, HA failover with epoch fencing, OTA campaigns, WAL-mode persistence.</p><span class="card__more">Runtime <span aria-hidden="true">→</span></span></a>
          <a class="card" href="safety.html" data-reveal><h3>Safety</h3><p>Postures, decel-to-stop Degraded, MRC, recovery hysteresis, watchdogs, and the safe-state specification.</p><span class="card__more">Safety <span aria-hidden="true">→</span></span></a>
          <a class="card" href="planning.html" data-reveal><h3>Planning</h3><p>Occy the geometric planner, a learned Hydra-MDP-shaped planner, and Mick's typed LLM intent seam.</p><span class="card__more">Planning <span aria-hidden="true">→</span></span></a>
          <a class="card" href="perception.html" data-reveal><h3>Perception</h3><p>Taj's corridor pipeline, true-redundancy cross-checking, and fail-closed perception derates.</p><span class="card__more">Perception <span aria-hidden="true">→</span></span></a>
          <a class="card" href="prediction.html" data-reveal><h3>Prediction</h3><p>Multi-modal predictive RSS: constant-velocity and turn-rate hypotheses rolled forward against the ego plan.</p><span class="card__more">Prediction <span aria-hidden="true">→</span></span></a>
          <a class="card" href="determinism.html" data-reveal><h3>Determinism</h3><p>Bit-identical replay, injected clocks, seqlock snapshots under Loom and Miri, machine-checked proofs.</p><span class="card__more">Determinism <span aria-hidden="true">→</span></span></a>
          <a class="card" href="security.html" data-reveal><h3>Security</h3><p>Per-node attestation, TPM quotes, scoped RBAC, Ed25519 federation, hash-chained audit, signed releases.</p><span class="card__more">Security <span aria-hidden="true">→</span></span></a>
          <a class="card" href="performance.html" data-reveal><h3>Performance</h3><p>WCET budgets and gates, the timing substrate, and honestly-scoped latency measurements.</p><span class="card__more">Performance <span aria-hidden="true">→</span></span></a>
          <a class="card" href="architecture.html" data-reveal><h3>Architecture</h3><p>The layered crate decomposition, transport decisions, partition boundaries, and deployment topology.</p><span class="card__more">Architecture <span aria-hidden="true">→</span></span></a>
        </div>
      </div>
    </section>

    <hr class="rule">

    <!-- ═══════════ HARDWARE ═══════════ -->
    <section id="hardware" aria-labelledby="h-hw">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>From thesis to metal</p>
          <h2 id="h-hw" data-reveal>The vision, driving a real robot</h2>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px"><h3 style="margin:0">ROSMASTER R2 · Jetson Orin NX</h3>${LIVE}</div>
            <p>An Ackermann-steer robot on a Jetson Orin NX 16&nbsp;GB is the physical testbed — first governed motion
            validated July 2026, with a TG30 lidar feeding the perception path.</p>
            ${evRow("docs/adr/0014-rosmaster-r2-orin-nx-kirra-integration.md", "robot/install/README.md")}
          </div>
          <div class="card" data-reveal>
            <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px"><h3 style="margin:0">Clean-room MCU firmware</h3>${LIVE}</div>
            <p>A from-scratch STM32F103 firmware foundation: jerk-limited PID, a fail-closed safety state machine,
            and COBS + CRC32C framing on the frozen SBC↔MCU wire protocol. Built in CI for Cortex-M3.</p>
            ${evRow("firmware/rosmaster-r2/README.md", ".github/workflows/rosmaster-r2-firmware.yml")}
          </div>
          <div class="card" data-reveal>
            <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px"><h3 style="margin:0">QNX 8.0 target lane</h3>${GATED}</div>
            <p>The certification target is a QNX Hypervisor 8.0 safety partition. Today: reproducible cross-builds
            for x86_64 and aarch64 QNX in CI, and a shim→judge fault harness with committed VM results.</p>
            ${evRow("docs/adr/0032-governor-deployment-platform.md", "tools/qnx-rtm-harness/results/qnx800-x86_64-vm-kvm.txt")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <!-- ═══════════ CERTIFICATION HONESTY BAND ═══════════ -->
    <section id="certification" aria-labelledby="h-cert">
      <div class="container container--narrow" style="text-align:center">
        <p class="eyebrow" data-reveal style="justify-content:center">Certification posture</p>
        <h2 id="h-cert" data-reveal>Engineered toward ASIL-D.<br>Claimed only when assessed.</h2>
        <p class="lede" data-reveal style="margin:20px auto 0">Kirra is designed in alignment with ISO 26262 ASIL-D and IEC 61508 SIL 3
        requirements, with a UL 4600 safety case, HARA, SOTIF analysis, and a CI-gated requirements-traceability
        matrix maintained in the open. Independent third-party assessment has not yet been performed —
        and nothing on this site will imply otherwise.</p>
        <div class="hero__ctas" style="justify-content:center" data-reveal>
          <a class="btn btn--ghost" href="certification.html">Read the certification posture <span class="arrow" aria-hidden="true">→</span></a>
        </div>
        <p class="evidence-row" style="justify-content:center" data-reveal>${ev("README.md:117", "README — the claim, verbatim")}${ev("docs/safety/", "docs/safety — 65 artifacts")}</p>
      </div>
    </section>

    <hr class="rule">

    <!-- ═══════════ FIELD NOTES ═══════════ -->
    <section id="notes" aria-labelledby="h-notes">
      <div class="container">
        <div class="section-head" style="display:flex;flex-wrap:wrap;align-items:end;justify-content:space-between;gap:16px;max-width:none">
          <div>
            <p class="eyebrow" data-reveal>Field Notes</p>
            <h2 id="h-notes" data-reveal>Engineering, written down</h2>
          </div>
          <a class="card__more" href="field-notes.html" data-reveal>All notes <span aria-hidden="true">→</span></a>
        </div>
        <div class="notes-grid" data-notes data-notes-max="4" data-reveal></div>
        <div class="card" data-notes-fallback data-reveal style="text-align:center">
          <p>Dispatches from the build — governed bring-ups, proof engineering, and fail-closed design.</p>
          <a class="btn btn--ghost" style="margin-top:16px" href="https://justinlooney.substack.com" target="_blank" rel="noopener">Subscribe on Substack ↗</a>
        </div>
      </div>
    </section>

    <!-- ═══════════ CLOSING CTA ═══════════ -->
    <section class="cta-band">
      <div class="container">
        <p class="eyebrow" style="justify-content:center">Open source</p>
        <h2>Don't take our word for it.<br>Take the repository's.</h2>
        <p class="lede">Clone it, run the 3,305 tests, kill the process mid-write, replay an incident bit-for-bit.
        The evidence is the product.</p>
        <div class="hero__ctas">
          <a class="btn btn--primary" href="${SITE.repo}" target="_blank" rel="noopener">kirra-runtime-sdk on GitHub <span class="arrow" aria-hidden="true">→</span></a>
          <a class="btn btn--ghost" href="open-source.html">Getting started</a>
        </div>
      </div>
    </section>
`;
