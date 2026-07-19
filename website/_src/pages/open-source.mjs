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
