import { SITE, ev, evRow, pageHero, ctaBand, LIVE, SPIKE, GATED, DRAFT } from "../template.mjs";

export const meta = {
  slug: "architecture",
  title: "Architecture",
  desc: "How Kirra is built: a layered Rust workspace, a frozen #[repr(C)] contract at the partition boundary, QNX hypervisor deployment target, and three transport lanes with three trust models.",
};

const topologySvg = `
<svg viewBox="0 0 960 430" role="img" aria-labelledby="topoT topoD">
  <title id="topoT">Deployment topology (ADR-0032)</title>
  <desc id="topoD">A QNX Hypervisor hosts two partitions: an untrusted guest VM running the ROS 2 or Autoware doer stack, and a safety partition running the no_std verdict core. They communicate only through a frozen repr-C contract in hypervisor shared memory. The safety partition releases Ed25519-signed commands to the actuator bus. A separate fleet lane connects vehicles to the verifier control plane over Zenoh, treated as an untrusted carrier.</desc>

  <rect x="10" y="14" width="700" height="270" rx="14" fill="none" stroke="var(--line-strong)" stroke-dasharray="6 6"/>
  <text class="dg-mono" x="28" y="40">QNX HYPERVISOR 8.0 FOR SAFETY — certification target (ADR-0032)</text>

  <g>
    <rect class="dg-box" x="34" y="60" width="280" height="200" rx="12"/>
    <text class="dg-label" x="54" y="90">Guest VM — untrusted doer</text>
    <text class="dg-mono" x="54" y="116">Ubuntu · ROS 2 · Autoware stack</text>
    <text class="dg-mono" x="54" y="136">Occy planner · Taj perception</text>
    <text class="dg-mono" x="54" y="156">Mick LLM intent (Ollama, local)</text>
    <text class="dg-sub" x="54" y="182">crashes, hallucinations and exploits</text>
    <text class="dg-sub" x="54" y="198">stay inside this box</text>
  </g>

  <g>
    <rect class="dg-box dg-box--accent" x="420" y="60" width="264" height="200" rx="12"/>
    <text class="dg-label" x="440" y="90">Safety partition</text>
    <text class="dg-mono" x="440" y="116">no_std verdict core · zero alloc</text>
    <text class="dg-mono" x="440" y="136">seqlock snapshot → validate</text>
    <text class="dg-mono" x="440" y="156">envelope · RSS · posture</text>
    <text class="dg-mono" x="440" y="176">Ed25519 release token</text>
    <text class="dg-sub" x="440" y="202">the certified artifact candidate</text>
  </g>

  <path class="dg-edge dg-edge--flow" d="M314 160 L 418 160"/>
  <text class="dg-mono" x="318" y="146">#[repr(C)]</text>
  <text class="dg-sub" x="318" y="180">frozen contract · SHM</text>

  <path class="dg-edge dg-edge--flow" d="M684 160 L 790 160"/>
  <g>
    <rect class="dg-box" x="792" y="122" width="158" height="76" rx="10"/>
    <text class="dg-label" x="810" y="152">Actuator bus</text>
    <text class="dg-sub" x="810" y="172">verify-before-release</text>
  </g>

  <g>
    <rect class="dg-box" x="10" y="322" width="290" height="88" rx="12"/>
    <text class="dg-label" x="30" y="352">Fleet control plane</text>
    <text class="dg-mono" x="30" y="374">kirra_verifier_service · Linux</text>
    <text class="dg-sub" x="30" y="392">attestation · posture · OTA · audit</text>
  </g>
  <path class="dg-edge" d="M300 364 C 380 364, 380 300, 360 284" stroke-dasharray="4 6"/>
  <text class="dg-mono" x="392" y="330">Zenoh fleet lane — untrusted carrier,</text>
  <text class="dg-mono" x="392" y="348">Ed25519 verify-before-use on every ingest</text>
  <text class="dg-sub" x="392" y="372">trust never comes from the transport</text>
</svg>`;

const layersSvg = `
<svg viewBox="0 0 960 300" role="img" aria-labelledby="layT layD">
  <title id="layT">Layered crate decomposition (ADR-0035)</title>
  <desc id="layD">Dependency layers from zero-dependency leaf crates up to adapters: kirra-policy-types and kirra-core at the bottom; kirra-trajectory, kirra-planner, kirra-map in the middle; the ros2 adapter, inline governor and verifier service above; with detached hardening harnesses — loom, kani, fuzz, postgres conformance — alongside.</desc>
  <g>
    <rect class="dg-box dg-box--accent" x="20" y="230" width="440" height="52" rx="10"/>
    <text class="dg-label" x="40" y="252">kirra-policy-types · kirra-core</text>
    <text class="dg-sub" x="40" y="270">zero/lean-dependency leaves — the types everything shares</text>
    <rect class="dg-box" x="20" y="150" width="440" height="60" rx="10"/>
    <text class="dg-label" x="40" y="174">kirra-trajectory · kirra-planner · kirra-map · kirra-taj</text>
    <text class="dg-sub" x="40" y="194">the checker &amp; doer logic — no ROS, no HTTP, no DB</text>
    <rect class="dg-box" x="20" y="70" width="440" height="60" rx="10"/>
    <text class="dg-label" x="40" y="94">kirra-ros2-adapter · kirra-inline-governor · sidecars</text>
    <text class="dg-sub" x="40" y="114">integration shells — features gate every heavy dependency</text>
    <rect class="dg-box" x="20" y="10" width="440" height="42" rx="10"/>
    <text class="dg-label" x="40" y="36">kirra-verifier (root) — control plane, HTTP, SQLite</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M240 230 L 240 212"/>
  <path class="dg-edge dg-edge--flow" d="M240 150 L 240 132"/>
  <path class="dg-edge dg-edge--flow" d="M240 70 L 240 54"/>
  <g>
    <rect class="dg-box" x="520" y="10" width="430" height="130" rx="12"/>
    <text class="dg-label" x="540" y="38">Detached hardening harnesses</text>
    <text class="dg-mono" x="540" y="62">verification/kani — 12 proofs over shipped source</text>
    <text class="dg-mono" x="540" y="82">kirra-loom-models — 4 concurrency models</text>
    <text class="dg-mono" x="540" y="102">fuzz/ — 4 targets · kirra-verifier-pg — live PG</text>
    <text class="dg-sub" x="540" y="124">compile to nothing in a normal build; own CI lanes</text>
  </g>
  <g>
    <rect class="dg-box" x="520" y="170" width="430" height="112" rx="12"/>
    <text class="dg-label" x="540" y="198">parko/ — separate workspace</text>
    <text class="dg-mono" x="540" y="222">ML inference substrate: ONNX · OpenVINO · TensorRT</text>
    <text class="dg-mono" x="540" y="242">diverse second governor + divergence comparator</text>
    <text class="dg-sub" x="540" y="264">isolation enforced by CI — runtime deps can't leak in</text>
  </g>
</svg>`;

export const body = `
${pageHero({
  eyebrow: "Architecture",
  title: "Small enough to prove.<br>Isolated enough to trust.",
  lede: "Kirra's architecture is one long argument for isolation: untrusted doers in their own partition, a minimal verdict core at the boundary, and a control plane that assumes every carrier is hostile. Here is how the pieces fit — with the decision records that shaped them.",
})}

    <section aria-labelledby="h-topo">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Deployment topology</p>
          <h2 id="h-topo" data-reveal>The partition is the safety argument</h2>
          <p class="lede" data-reveal>The decision of record (ADR-0032): the governor runs as the safety-domain payload
          on QNX Hypervisor 8.0 for Safety, with the entire ROS 2 / Autoware doer stack as an isolated guest VM.
          The only thing that crosses the boundary is a frozen, pointer-free <code>#[repr(C)]</code> struct.</p>
        </div>
        <div class="diagram-frame flow-anim" data-reveal>${topologySvg}</div>
        <p class="diagram-caption"><span>${GATED}</span><span>QNX-target execution is the recorded remainder: cross-builds are reproducible in CI today; certified timing awaits target hardware.</span></p>
        ${evRow("docs/adr/0032-governor-deployment-platform.md", "docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md", "crates/kirra-contract-channel/src/lib.rs", ".github/workflows/ci.yml:122")}
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-layers">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Code structure</p>
          <h2 id="h-layers" data-reveal>36 crates, layered by trust</h2>
          <p class="lede" data-reveal>ADR-0035 decomposed a monolith into layers where dependency direction encodes
          criticality: the checker logic depends on nothing heavy, and nothing heavy can reach into the checker.
          A CI fence proves the LLM-facing crates have <em>no compile path</em> to actuation.</p>
        </div>
        <div class="diagram-frame" data-reveal>${layersSvg}</div>
        ${evRow("docs/adr/0035-verifier-crate-decomposition.md", "ci/check_mick_actuation_fence.py", "parko/ci/check-runtime-isolation.sh", "Cargo.toml")}
        <div class="grid grid--3" style="margin-top:32px">
          <div class="card" data-reveal>
            <h3>Memory ownership</h3>
            <p>Exactly one crate in the enforced path may use <code>unsafe</code>: the SHM carrier, with its unsafe
            budget enumerated (<code>shm_open</code>/<code>mmap</code>, atomic field access). The contract channel and
            in-line governor are <code>#![forbid(unsafe_code)]</code>.</p>
            ${evRow("crates/kirra-hv-carrier/README.md", "crates/kirra-inline-governor/src/lib.rs", "crates/kirra-contract-channel/src/seqlock.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Threading model</h3>
            <p>The verifier's supervised loops are declared as data — a task manifest with dependencies, criticality,
            and deadlines — and started in a topologically-sorted order that fails closed on cycles or missing tasks.</p>
            ${evRow("src/execution_manager.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Storage seams</h3>
            <p>Persistence is a set of storage traits (epoch fence, node store, federation, posture state, OTA) with
            SQLite, in-memory, and live-Postgres backends all running the <em>same</em> conformance suites in CI.</p>
            ${evRow("crates/kirra-persistence", "crates/kirra-verifier-pg", ".github/workflows/ci.yml:503")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-transport">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Transport lanes</p>
          <h2 id="h-transport" data-reveal>Three lanes, three trust models</h2>
          <p class="lede" data-reveal>There is no single “message bus.” Each lane gets exactly the trust model its
          criticality demands — and none of them trusts the carrier.</p>
        </div>
        <div class="table-wrap" data-reveal>
          <table class="tbl">
            <caption class="visually-hidden">Transport lanes and their trust models</caption>
            <thead><tr><th scope="col">Lane</th><th scope="col">Carrier</th><th scope="col">Trust model</th><th scope="col">Status</th></tr></thead>
            <tbody>
              <tr>
                <td>Enforcement (in-vehicle)</td>
                <td>POSIX SHM seqlock · frozen <code>#[repr(C)]</code> contract</td>
                <td>Coherent-snapshot read → validate → Ed25519 over the enforced bytes → verify-before-release</td>
                <td>${LIVE}</td>
              </tr>
              <tr>
                <td>ROS 2 / DDS (doer side)</td>
                <td>DDS via <code>r2r</code> (Humble/Iron/Jazzy) · CycloneDDS writer feature-gated</td>
                <td>Fail-closed QoS: actuator topics must be Volatile — negotiated QoS is read back and a downgraded topic is refused at open</td>
                <td>${LIVE}</td>
              </tr>
              <tr>
                <td>Fleet (vehicle ↔ ops)</td>
                <td>Zenoh (ADR-0007)</td>
                <td>Untrusted carrier: Ed25519 payload signatures verified before use; rejection counters; QM-only, fenced off the safety path</td>
                <td>${SPIKE}</td>
              </tr>
              <tr>
                <td>Zero-copy candidate</td>
                <td>iceoryx2 (pinned =0.9.2, isolated workspace)</td>
                <td>Same frozen contract over a zero-copy channel; 9-class fault matrix passes in full and minimal feature configs</td>
                <td>${SPIKE}</td>
              </tr>
            </tbody>
          </table>
        </div>
        ${evRow("src/dds_bridge.rs", "src/dds_cyclonedds.rs", "crates/kirra-fleet-transport/README.md", "tools/iceoryx2-spike/README.md", "docs/adr/0006-governor-transport-iceoryx2.md")}
        <div class="compare-note" data-reveal style="margin-top:24px">
          <strong>Why this differs from ROS-centric architectures:</strong> in a conventional stack, safety rides the
          same middleware as everything else, and a QoS misconfiguration can silently replay a stale actuator command
          to a late-joining subscriber. Kirra's actuator QoS is enforced fail-closed in code
          (TransientLocal is refused, lifespan = deadline), and the enforcement path doesn't ride middleware at all —
          it's a seqlock over shared memory with a signature over the exact bytes.
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-compare">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Positioning</p>
          <h2 id="h-compare" data-reveal>Where Kirra sits in the landscape</h2>
          <p class="lede" data-reveal>Kirra is not an end-to-end driving stack, and we won't pretend it is.
          It is the governed layer <em>under</em> one. That makes most comparisons complementary, not competitive —
          and where the repository gives us no evidence, we make no claim.</p>
        </div>
        <div class="grid grid--2">
          <div class="card" data-reveal>
            <h3>Autoware</h3>
            <p>The decision of record keeps Autoware as the AV-stack doer — Kirra does not reimplement localization
            or closed-loop control, and the gap analysis says so plainly. What Kirra adds is what the analysis found
            Autoware has no equivalent of: formal, fail-closed command bounding (RSS conjunction, occlusion caps,
            decel-to-stop) applied to the doer's output at a frozen contract boundary.</p>
            ${evRow("docs/adr/0036-autoware-distro-migration-occy-gap.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Mobileye RSS</h3>
            <p>RSS is a published formal model, and Kirra implements it in the open: longitudinal/lateral safe
            distances as a conjunction, Rule-4 occlusion bounds, opposite-direction terms — with property tests,
            an exhaustive concrete mirror, and two of the primitives machine-checked by Kani. You can read every line.</p>
            ${evRow("parko/crates/parko-core/src/rss.rs", "verification/kani/src/proofs_rss.rs", "docs/safety/KIRRA_RSS_FORMAL_SPECIFICATION.md")}
          </div>
          <div class="card" data-reveal>
            <h3>NVIDIA DRIVE</h3>
            <p>Complementary by design: Kirra's certified-timing target is DRIVE-class hardware running QNX for
            Safety, with Jetson Orin as the doer/dev rig. The repository is explicit that Jetson never hosts the
            certification target — and neither do we.</p>
            ${evRow("docs/hardware/TARGET_PLATFORM_MATRIX.md", "tools/qnx-rtm-harness/README.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Waymo · Wayve · Apollo</h3>
            <p>Vertically-integrated or end-to-end learned stacks solve a different problem: driving. Kirra solves
            governing — a doer-invariant safety case that holds whether the doer is geometric, learned, or an LLM.
            We publish no benchmark against closed systems we cannot measure.</p>
            ${evRow("docs/adr/0020-doer-invariant-safety-case.md")}
          </div>
        </div>
      </div>
    </section>

${ctaBand("The architecture is the argument.", "Every box on this page is a crate you can read, a decision record you can audit, or a CI lane you can re-run.")}
`;
