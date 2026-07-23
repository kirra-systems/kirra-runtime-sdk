import { SITE, ev, evRow, pageHero, ctaBand } from "../template.mjs";

export const meta = {
  slug: "open-source",
  title: "Open Source",
  desc: "Get started with the kirra-runtime-sdk: clone, run 3,305 tests, bring up the verifier with Docker Compose, and read the contribution gates that keep the safety case intact.",
};

export const body = `
${pageHero({
  eyebrow: "Open source",
  title: "Safety-critical,<br>in public.",
  lede: "The entire platform — checker, runtime, proofs, safety case drafts, even the hardware bring-up runbooks — is developed in one public repository. Openness is a safety property here: an envelope you can read is an envelope you can challenge.",
})}

    <section aria-labelledby="h-start">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Quickstart</p>
          <h2 id="h-start" data-reveal>Three commands to a governed fleet</h2>
        </div>
        <div class="grid grid--2" style="align-items:start">
          <div class="codeblock" data-reveal>
            <div class="codeblock__bar"><span>terminal</span><button class="copy-btn" data-copy="#qs1">copy</button></div>
            <pre id="qs1"><code><span class="tok-c"># clone and run the full test suite</span>
git clone https://github.com/kirra-systems/kirra-runtime-sdk
cd kirra-runtime-sdk
cargo test --workspace

<span class="tok-c"># bring up verifier + dashboard (+ optional HA standby)</span>
<span class="tok-k">KIRRA_ADMIN_TOKEN</span>=change-me docker compose up

<span class="tok-c"># watch the posture stream</span>
curl -N localhost:8090/system/posture/stream \\
  -H <span class="tok-s">"authorization: Bearer change-me"</span> \\
  -H <span class="tok-s">"x-kirra-client-id: dev"</span></code></pre>
          </div>
          <div>
            <div class="card" data-reveal>
              <h3>What you're running</h3>
              <p>A Cargo workspace of 36 crates plus the separate <code>parko</code> ML workspace — pinned toolchain
              1.94.1, MSRV 1.88 enforced in CI, reproducible builds. The verifier binds on port 8090 with a
              fail-closed admin token: absent or empty means 503, never open.</p>
              ${evRow("rust-toolchain.toml", "docker-compose.yml", "INSTALL.md")}
            </div>
            <div class="card" data-reveal style="margin-top:16px">
              <h3>Deployment paths</h3>
              <p>Docker (digest-pinned, cosign-signed images), Helm chart, systemd units for bare metal — including
              the on-robot installer that turns a fresh Jetson into a governed vehicle — and a one-line installer
              for x86_64 / aarch64 / armv7.</p>
              ${evRow("helm/kirra", "deploy/systemd", "robot/install/README.md", "install.sh")}
            </div>
          </div>
        </div>

        <div class="stats stats--4" data-reveal style="margin-top:24px">
          <div class="stat">
            <p class="stat__value">100<span class="unit">µs</span></p>
            <p class="stat__label">verdict target — SG9 fail-closed timeout budget</p>
            <p class="stat__evidence">${ev("src/wcet_gate.rs:92", "wcet_gate.rs")}</p>
          </div>
          <div class="stat">
            <p class="stat__value">~116<span class="unit">µs p99.9</span></p>
            <p class="stat__label">assembled in-line loop, Ed25519-dominated — host-indicative</p>
            <p class="stat__evidence">${ev("crates/kirra-inline-governor/README.md", "inline-governor")}</p>
          </div>
          <div class="stat">
            <p class="stat__value">~60<span class="unit">ns</span></p>
            <p class="stat__label">shared-memory seqlock read — no serialization on the enforced path</p>
            <p class="stat__evidence">${ev("crates/kirra-hv-carrier/README.md", "hv-carrier")}</p>
          </div>
          <div class="stat">
            <p class="stat__value">O(1)</p>
            <p class="stat__label">structural boundedness — no alloc, no unbounded loops in the verdict</p>
            <p class="stat__evidence">${ev("src/wcet_gate.rs", "wcet_gate.rs")}</p>
          </div>
        </div>
        <p class="evidence-note" data-reveal>Host-measured, not certified: these are regression tripwires gated on p99.9 in CI, not
        a WCET claim. Certified worst-case timing is defined as QNX-target-under-FIFO and hasn't been produced yet —
        the repository says so, and so do we. <a class="evidence" href="performance.html">The full timing story →</a></p>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-orient">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Developer orientation</p>
          <h2 id="h-orient" data-reveal>Find your way around in five minutes</h2>
          <p class="lede" data-reveal>The tree is organized by trust, not by feature — knowing that makes the whole
          repository legible.</p>
        </div>
        <div class="grid grid--2" style="align-items:start">
          <div class="table-wrap" data-reveal>
            <table class="tbl" style="min-width:0">
              <caption class="visually-hidden">Repository map</caption>
              <thead><tr><th scope="col">Path</th><th scope="col">What lives there</th></tr></thead>
              <tbody>
                <tr><td class="mono">crates/</td><td>The checker &amp; platform — trajectory validation, planner, maps, transports, OTA</td></tr>
                <tr><td class="mono">src/</td><td>The verifier control plane — posture, attestation, audit, HTTP service</td></tr>
                <tr><td class="mono">parko/</td><td>Separate ML workspace — inference backends, diverse second governor</td></tr>
                <tr><td class="mono">verification/ · fuzz/</td><td>Kani proofs over shipped source; standing fuzz targets</td></tr>
                <tr><td class="mono">tools/</td><td>Isolated spikes — iceoryx2 carrier, QNX shim/judge harness</td></tr>
                <tr><td class="mono">robot/ · firmware/</td><td>The physical R2 testbed scripts and clean-room MCU firmware</td></tr>
                <tr><td class="mono">docs/</td><td>36 ADRs, 65 safety artifacts, hardware runbooks, specs</td></tr>
              </tbody>
            </table>
          </div>
          <div style="display:grid;gap:14px">
            <div class="card" data-reveal>
              <h3 style="font-size:1.05rem">Runnable demonstrations</h3>
              <p>The in-line governor demo drives the full read→decide→sign→verify→release loop across a real
              process boundary; the CARLA client runs the governor against a simulator; the proposal bench sweeps
              a live governor with hostile proposals and prints its verdicts. Or skip the terminal entirely:
              the <a href="playground.html">Verdict Playground</a> runs the frozen checker in your browser.</p>
              ${evRow("crates/kirra-inline-governor", "src/bin/kirra_carla_client.rs", "crates/kirra-proposal-bench")}
            </div>
            <div class="card" data-reveal>
              <h3 style="font-size:1.05rem">APIs &amp; examples</h3>
              <p><code>cargo doc --open</code> builds the full API reference (deny-warnings enforced). The governor
              quickstart example and the C SDK example both build and run in CI, so they're never stale.</p>
              ${evRow("examples/", ".github/workflows/ci.yml:323")}
            </div>
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-contrib">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Contributing</p>
          <h2 id="h-contrib" data-reveal>The gates protect the safety case — including from us</h2>
          <p class="lede" data-reveal>Every PR faces the same 27 blocking lanes: tests, Miri, Loom, Kani mirrors,
          fuzz builds, mutation testing on checker diffs, coverage floors, clippy at deny-warnings, formatting,
          supply-chain audit, and the invariant checks that have rejected real attempts to weaken attestation and
          auth. The security invariants are documented precisely so a reviewer can reject a violation on sight.</p>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Quality guardrails</h3>
            <p>Per-file line budgets, panic/unwrap/expect budgets, orphan-core detection, and a purity gate that keeps
            crypto, clocks, and allocation out of the verdict core — all scripted, all blocking.</p>
            ${evRow("ci/check_quality_guardrails.py", "ci/check_verdict_core_purity.py")}
          </div>
          <div class="card" data-reveal>
            <h3>Docs that must compile</h3>
            <p><code>cargo doc</code> runs at deny-warnings, the C SDK examples build and run in CI, and the governor
            quickstart example is executed — documentation drift fails the build.</p>
            ${evRow(".github/workflows/ci.yml:323", "examples/")}
          </div>
          <div class="card" data-reveal>
            <h3>Report a vulnerability</h3>
            <p>Coordinated disclosure with a 48-hour first response. If you can get an unsafe command past the
            checker, we want the writeup more than you do.</p>
            ${evRow("SECURITY.md")}
          </div>
        </div>
      </div>
    </section>

${ctaBand("Star it, break it, or ship on it.", "kirra-runtime-sdk is one clone away — and the CI badge means more here than most places.")}
`;
