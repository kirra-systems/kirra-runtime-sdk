import { ev, evRow, pageHero, ctaBand, LIVE, PLANNED } from "../template.mjs";

export const meta = {
  slug: "vision",
  title: "Vision",
  desc: "Why runtime governance is the missing layer of the autonomy stack: the embodied-AI wave, the regulatory shift to runtime assurance, and Kirra's place between certified platforms and unverifiable AI.",
};

const stackSvg = `
<svg viewBox="0 0 960 380" role="img" aria-labelledby="stT stD">
  <title id="stT">The autonomy stack of tomorrow</title>
  <desc id="stD">Four layers: AI doer stacks at the top; the Kirra governed layer beneath them; certified operating systems and hypervisors below that; and safety-rated silicon at the base. Kirra is the layer that makes the unverifiable top compatible with the certified bottom.</desc>
  <g>
    <rect class="dg-box" x="80" y="16" width="800" height="70" rx="12"/>
    <text class="dg-label" x="104" y="46">AI doer stacks — capable, fast-moving, unverifiable</text>
    <text class="dg-sub" x="104" y="68">Autoware-class AV stacks · learned planners · LLM agents · whatever comes next</text>
  </g>
  <g>
    <rect class="dg-box dg-box--accent" x="80" y="106" width="800" height="86" rx="12"/>
    <text class="dg-label" x="104" y="138">KIRRA — the governed layer</text>
    <text class="dg-mono" x="104" y="160">typed intents · posture · kinematic envelopes · RSS · signed release</text>
    <text class="dg-sub" x="104" y="180">an SEooC: a safety element designed to be integrated, not a stack that competes</text>
  </g>
  <g>
    <rect class="dg-box" x="80" y="212" width="800" height="70" rx="12"/>
    <text class="dg-label" x="104" y="242">Certified OS &amp; hypervisors</text>
    <text class="dg-sub" x="104" y="264">QNX 8.0 for Safety (the recorded deployment target) · safety-certified RTOS class</text>
  </g>
  <g>
    <rect class="dg-box" x="80" y="302" width="800" height="62" rx="12"/>
    <text class="dg-label" x="104" y="330">Safety-rated silicon</text>
    <text class="dg-sub" x="104" y="352">DRIVE-class targets · Jetson Orin today · vendor-neutral inference: ONNX / OpenVINO / TensorRT, Qualcomm QNN &amp; TI TIDL on the roadmap</text>
  </g>
  <path class="dg-edge dg-edge--deny" d="M480 86 L 480 104"/>
  <path class="dg-edge dg-edge--flow" d="M480 192 L 480 210"/>
  <path class="dg-edge dg-edge--flow" d="M480 282 L 480 300"/>
  <text class="dg-mono" x="500" y="100">bounded, never trusted</text>
</svg>`;

export const body = `
${pageHero({
  eyebrow: "Vision",
  title: "The missing layer of<br>the autonomy stack.",
  lede: "Every machine that moves is getting a model. Models are getting more capable and less inspectable at the same time — and no amount of training makes a neural network certifiable. The industry's honest answer is an old one, rebuilt for the AI era: don't verify the driver; verify the envelope. That layer is what Kirra is.",
})}

    <section aria-labelledby="h-need">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>The need</p>
          <h2 id="h-need" data-reveal>Three forces, one gap</h2>
          <p class="lede" data-reveal>The next decade of robotics is already legible: capability is outrunning
          verification, regulation is converging on runtime assurance, and compute is consolidating onto
          certified platforms. All three point at the same missing piece.</p>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>AI is outrunning verification</h3>
            <p>Foundation models are entering planners, and language is becoming a robot interface. You cannot
            test a distribution into being safe — but you can bound what any of it is permitted to do. Kirra's
            safety case is deliberately <em>doer-invariant</em>: it holds whether the doer is geometric code,
            a learned policy, or an LLM, which means it also holds for the doer nobody has built yet.</p>
            ${evRow("docs/adr/0020-doer-invariant-safety-case.md", "docs/llm_integration_guide.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Regulation is converging on runtime assurance</h3>
            <p>UL 4600 safety cases, SOTIF, ASTM F3269 run-time assurance, and the emerging AI-safety standards
            (ISO/IEC TR 5469, ISO/PAS 8800) all circle the same architecture: an untrusted complex function
            behind a verifiable monitor. Kirra maintains mappings to each of these — drafted in the open,
            versioned with the code — so the compliance story grows with the product instead of after it.</p>
            ${evRow("docs/safety/UL4600_SAFETY_CASE.md", "docs/safety/ASTM_F3269_RTA_MAPPING.md", "docs/safety/ISO_IEC_TR_5469_MAPPING.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Platforms need a governed layer</h3>
            <p>Certified hypervisors and safety-rated silicon solve isolation and compute — they don't decide
            whether a command is safe. Kirra is built to be that decision layer on exactly those platforms:
            the deployment decision of record is a QNX 8.0 safety partition, and the inference substrate is
            vendor-neutral by design.</p>
            ${evRow("docs/adr/0032-governor-deployment-platform.md", "parko/README.md")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-stack">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Where it fits</p>
          <h2 id="h-stack" data-reveal>Between certified platforms<br>and unverifiable AI</h2>
          <p class="lede" data-reveal>Kirra doesn't compete with the layers above or below it. It's engineered as an
          <strong>SEooC</strong> — ISO 26262's term for a safety element built out of context, for integration into
          someone else's product — with its assumptions of use written down and its integration seams frozen.</p>
        </div>
        <div class="diagram-frame flow-anim" data-reveal>${stackSvg}</div>
        <p class="diagram-caption">
          <span>Vendor-neutral on purpose: today's inference backends are ONNX Runtime, OpenVINO, and TensorRT; Qualcomm QNN and TI TIDL are tracked on the roadmap ${PLANNED}</span>
        </p>
        ${evRow("docs/safety/GOVERNOR_SAFETY_MANUAL.md", "docs/safety/ASSUMPTIONS_OF_USE.md", "docs/adr/0003-two-tier-base-and-d1-addon.md")}
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-asset">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Built to be built upon</p>
          <h2 id="h-asset" data-reveal>Engineered for integration<br>from the first commit</h2>
          <p class="lede" data-reveal>Whether Kirra ships inside a vehicle program, an industrial platform, or a
          larger safety portfolio, the properties an integrator has to diligence are already in place — and
          machine-checked where possible.</p>
        </div>
        <div class="grid grid--2">
          <div class="card" data-reveal>
            <h3>Clean, permissive IP posture</h3>
            <p>The dependency license policy is an enforced allowlist — MIT, Apache-2.0, BSD-family and peers only,
            gated by cargo-deny on every push, with crates.io as the sole permitted source. The robot MCU firmware
            is a documented clean-room implementation. No copyleft surprises, no vendored mystery code.</p>
            ${evRow("deny.toml", "firmware/rosmaster-r2/README.md")}
          </div>
          <div class="card" data-reveal>
            <h3>The paper trail ships with every release</h3>
            <p>Tagged releases build a safety-case evidence bundle in CI — traceability matrix, safety constants
            provenance, gate results — so an assessment (or a diligence review) starts from artifacts, not
            interviews. Actions are SHA-pinned; containers are digest-pinned and cosign-signed.</p>
            ${evRow(".github/workflows/release.yml", "ci/build_safety_case.py")}
          </div>
          <div class="card" data-reveal>
            <h3>One repository, fully auditable</h3>
            <p>The checker, the runtime, the proofs, the safety case drafts, the hardware runbooks, and this
            website live in a single versioned tree with 27 blocking CI lanes. What you can diligence in a
            week here would take a quarter against a stack of private wikis.</p>
            ${evRow(".github/workflows/ci.yml")}
          </div>
          <div class="card" data-reveal>
            <h3>Small, certifiable core</h3>
            <p>The path to certification runs through a minimal <code>no_std</code> verdict core with a purity gate —
            no allocation, no clock, no crypto inside the decision — and a Ferrocene-ready toolchain direction.
            The expensive artifact to certify is deliberately the smallest one.</p>
            ${evRow("ci/check_verdict_core_purity.py", "docs/adr/0032-governor-deployment-platform.md")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-bet">
      <div class="container container--narrow" style="text-align:center">
        <p class="eyebrow" data-reveal style="justify-content:center">The bet</p>
        <h2 id="h-bet" data-reveal>Autonomy will be won by whoever<br>can prove safety at runtime.</h2>
        <p class="lede" data-reveal style="margin:20px auto 0">Models will keep changing. Sensors will keep changing.
        The one durable interface in the stack is the envelope between intent and actuation — and whoever owns a
        proven, portable, certifiable version of that layer owns a seat in every machine that moves. Kirra is
        that layer, built in the open, with its evidence attached.</p>
        <div class="hero__ctas" style="justify-content:center" data-reveal>
          <a class="btn btn--primary" href="solutions.html">See it applied <span class="arrow" aria-hidden="true">→</span></a>
          <a class="btn btn--ghost" href="architecture.html">See how it's built</a>
        </div>
      </div>
    </section>
`;
