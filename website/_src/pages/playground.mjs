import { SITE, ev, evRow, pageHero } from "../template.mjs";

export const meta = {
  slug: "playground",
  title: "Verdict Playground",
  desc: "Run Kirra's real, frozen safety checker in your browser: the kinematics-contract talisman compiled verbatim to WebAssembly. Propose a command, watch the envelope decide.",
};

export const body = `
${pageHero({
  eyebrow: "Verdict playground",
  title: "The real checker.<br>Running in your browser.",
  lede: "This is not a demo that imitates the checker — it IS the checker. The frozen kinematics-contract source (git blob ed00f4da…) is included verbatim, compiled to 54 KB of WebAssembly, and loaded into this page. The same file the Kani proofs verify. Propose a command; the envelope decides.",
})}

    <section aria-labelledby="h-pg" id="playground">
      <div class="container">
        <h2 id="h-pg" class="visually-hidden">Interactive verdict evaluation</h2>
        <div class="grid grid--2" style="align-items:start">
          <div class="card">
            <h3>Propose a command</h3>
            <p class="dim" style="font-size:0.85rem;margin-top:4px">Values accept <code>NaN</code> and <code>Infinity</code> — the checker certainly does not.</p>
            <div class="grid" style="grid-template-columns:1fr 1fr;gap:14px;margin-top:18px">
              <div class="field" style="grid-column:1/-1">
                <label for="pg-class">Vehicle class (Nominal envelope)</label>
                <select id="pg-class">
                  <option value="2" selected>Robotaxi — frozen reference + 22.35 m/s ODD cap</option>
                  <option value="1">Delivery AV — road pod</option>
                  <option value="0">Courier — sidewalk</option>
                </select>
              </div>
              <div class="field" style="grid-column:1/-1">
                <label id="pg-posture-label">Fleet posture</label>
                <div class="posture-switch" id="pg-degraded-group" role="group" aria-labelledby="pg-posture-label">
                  <button type="button" data-posture="nominal" aria-pressed="true">NOMINAL</button>
                  <button type="button" data-posture="degraded" aria-pressed="false">DEGRADED</button>
                </div>
                <span id="pg-degraded" aria-pressed="false" hidden></span>
              </div>
              <div class="field"><label for="pg-linear">Proposed speed (m/s)</label><input id="pg-linear" type="text" inputmode="decimal" value="20.1"></div>
              <div class="field"><label for="pg-current">Current speed (m/s)</label><input id="pg-current" type="text" inputmode="decimal" value="20"></div>
              <div class="field"><label for="pg-dt">Time step dt (s)</label><input id="pg-dt" type="text" inputmode="decimal" value="0.1"></div>
              <div class="field"><label for="pg-steer">Proposed steering (°)</label><input id="pg-steer" type="text" inputmode="decimal" value="0"></div>
              <div class="field"><label for="pg-csteer">Current steering (°)</label><input id="pg-csteer" type="text" inputmode="decimal" value="0"></div>
              <div class="field" style="align-self:end"><button class="btn btn--primary" id="pg-eval" disabled style="justify-content:center">Evaluate</button></div>
            </div>
            <p class="dim mono" id="pg-status" style="font-size:0.74rem;margin-top:14px">loading checker…</p>
            <hr class="rule" style="margin:18px 0">
            <p class="dim" style="font-size:0.8rem;margin-bottom:10px">Try an attack:</p>
            <div class="preset-row">
              <button class="preset" data-preset="hallucinate">LLM hallucination — 999 m/s</button>
              <button class="preset" data-preset="nan">NaN injection</button>
              <button class="preset" data-preset="dtzero">dt = 0</button>
              <button class="preset" data-preset="swerve">Hard swerve at speed</button>
              <button class="preset" data-preset="reinit">Degraded: move from a stop</button>
              <button class="preset" data-preset="speedup">Degraded: speed up</button>
              <button class="preset" data-preset="brake">Degraded: brake to stop</button>
              <button class="preset" data-preset="cruise">Gentle cruise</button>
            </div>
          </div>
          <div>
            <div class="verdict-panel" id="pg-result" role="status" aria-live="polite" data-kind="idle"></div>
            <div class="card" style="margin-top:16px">
              <h3 style="font-size:1.02rem">What you're running</h3>
              <p style="font-size:0.9rem">The WASM module <code>#[path]</code>-includes
              <code>crates/kirra-core/src/kinematics_contract.rs</code> verbatim — the same fidelity pattern the
              Kani proofs use, so the frozen file is never modified. The shim only marshals numbers across the
              boundary; the class profiles mirror the normative table in <code>docs/CONTRACT_PROFILES.md</code>,
              and Degraded runs the talisman's own MRC fallback profile (ADR-0012). The crate's native test suite
              runs the talisman's 47 embedded unit tests plus shim mirrors of everything this page demonstrates.</p>
              ${evRow("tools/wasm-verdict/src/lib.rs", "crates/kirra-core/src/kinematics_contract.rs", "verification/kani/src/proofs_kinematics.rs")}
            </div>
          </div>
        </div>
        <div class="compare-note" style="margin-top:32px" data-reveal>
          <strong>Honest scope:</strong> this page runs the kinematic envelope and Degraded decel-to-stop gate —
          one layer of the full checker. In production the same command would also face posture routing, RSS and
          containment over the whole trajectory, and the Ed25519 verify-before-release chain at the actuator.
          The playground shows the innermost wall, not the whole fortress.
        </div>
      </div>
    </section>

    <section class="cta-band">
      <div class="container">
        <p class="eyebrow" style="justify-content:center">Verified fidelity</p>
        <h2>Don't trust the page. Diff the blob.</h2>
        <p class="lede">The talisman's git blob hash is pinned in CI — the file this page compiles is bit-identical
        to the one the fleet runs and the proofs verify.</p>
        <div class="hero__ctas">
          <a class="btn btn--primary" href="${SITE.repo}/blob/main/crates/kirra-core/src/kinematics_contract.rs" target="_blank" rel="noopener">Read the talisman <span class="arrow" aria-hidden="true">→</span></a>
          <a class="btn btn--ghost" href="determinism.html">Why determinism matters</a>
        </div>
      </div>
    </section>
    <script src="js/playground.js" defer></script>
`;
