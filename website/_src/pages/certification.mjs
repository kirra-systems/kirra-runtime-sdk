import { ev, evRow, pageHero, ctaBand, LIVE, DRAFT } from "../template.mjs";

export const meta = {
  slug: "certification",
  title: "Certification",
  desc: "Kirra's certification posture, stated honestly: designed in alignment with ISO 26262 ASIL-D and IEC 61508 SIL 3, with a UL 4600 safety case, HARA, SOTIF and TARA maintained in the open. Independent assessment has not yet been performed.",
};

export const body = `
${pageHero({
  eyebrow: "Certification posture",
  title: "The strongest claim we make<br>is the one we don't.",
  lede: "Kirra is designed in alignment with ISO 26262 ASIL-D and IEC 61508 SIL 3 requirements. Independent third-party assessment has not yet been performed. That sentence is in the repository's README, and it governs every word on this page: what follows is the evidence program — its real status, artifact by artifact.",
})}

    <section aria-labelledby="h-matrix">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>The safety case, in the open</p>
          <h2 id="h-matrix" data-reveal>65 safety artifacts under version control</h2>
          <p class="lede" data-reveal>Most safety cases live in a filing cabinet. Kirra's lives in
          <code>docs/safety/</code> — versioned, reviewable, and honest about its own maturity. Draft means draft.</p>
        </div>
        <div class="table-wrap" data-reveal>
          <table class="tbl">
            <caption class="visually-hidden">Certification artifact inventory and status</caption>
            <thead><tr><th scope="col">Artifact</th><th scope="col">Standard</th><th scope="col">Doc ID</th><th scope="col">Status</th></tr></thead>
            <tbody>
              <tr><td>Safe-state specification</td><td>ISO 26262 (ASIL-D scope)</td><td class="mono">KIRRA-SSS-001</td><td>${LIVE}</td></tr>
              <tr><td>Hazard analysis &amp; risk assessment</td><td>ISO 26262 Part 3</td><td class="mono">AEGIS-HARA-001</td><td>${DRAFT}</td></tr>
              <tr><td>UL 4600 safety case (GSN)</td><td>UL 4600</td><td class="mono">KIRRA-OCCY-UL4600-001</td><td>${DRAFT}</td></tr>
              <tr><td>SOTIF analysis + ODD definition</td><td>ISO 21448</td><td class="mono">KIRRA-OCCY-ODD-001</td><td>${DRAFT} — ODD §1 signed off</td></tr>
              <tr><td>Cybersecurity threat analysis</td><td>ISO/SAE 21434</td><td class="mono">KIRRA-OCCY-TARA-001</td><td>${DRAFT}</td></tr>
              <tr><td>SIL 3 mapping</td><td>IEC 61508</td><td class="mono">KIRRA-SIL3-001</td><td>${DRAFT} — pre-assessment</td></tr>
              <tr><td>Run-time assurance mapping</td><td>ASTM F3269-21</td><td class="mono">KIRRA-RTA-001</td><td>${DRAFT}</td></tr>
              <tr><td>AI-safety alignment statement</td><td>ISO/IEC TR 5469</td><td class="mono">KIRRA-TR5469-001</td><td>${DRAFT} — explicitly not a certification claim</td></tr>
              <tr><td>Assumptions of use (SEooC)</td><td>ISO 26262 Part 10</td><td class="mono">KIRRA-OCCY-AOU-001</td><td>${DRAFT} — living register</td></tr>
              <tr><td>Requirements traceability matrix</td><td>—</td><td class="mono">AEGIS-RTM-001</td><td>${LIVE} — CI-gated</td></tr>
            </tbody>
          </table>
        </div>
        ${evRow("docs/safety/SAFE_STATE_SPECIFICATION.md", "docs/safety/HARA.md", "docs/safety/UL4600_SAFETY_CASE.md", "docs/safety/OCCY_SOTIF.md", "docs/safety/OCCY_TARA.md", "docs/safety/ASSUMPTIONS_OF_USE.md")}
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-trace">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Traceability</p>
          <h2 id="h-trace" data-reveal>Requirements that CI can see</h2>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Machine-extracted matrix</h3>
            <p>Safety requirements are tagged in source with <code>// SAFETY:</code> annotations, the traceability
            matrix is auto-generated from them, and a CI gate keeps code and matrix from drifting apart.</p>
            ${evRow("docs/safety/TRACEABILITY_MATRIX.md", "src/traceability_gate.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Gaps stay visible</h3>
            <p>The RTM gap report tracks exactly which safety goals lack test evidence — and the stub policy is
            explicit: never falsify coverage with a passing placeholder. A genuinely missing mechanism stays
            documented as missing.</p>
            ${evRow("docs/safety/RTM_GAP_REPORT.md", "tests/cert_003_rtm_gap_stubs.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Safety goals, enforced</h3>
            <p>Nine Occy safety goals — speed envelope, lateral containment, RSS distances, MRC publication,
            staleness fail-closed, NaN/Inf rejection — with per-goal status honestly split between ENFORCED and
            Specified, and safety constants provenance-gated in CI.</p>
            ${evRow("docs/safety/OCCY_SAFETY_GOALS.md", "ci/check_safety_constants.py")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-evidence">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Verification program</p>
          <h2 id="h-evidence" data-reveal>What's measured today — and what isn't yet</h2>
          <p class="lede" data-reveal>The distinction between an evidence plan and achieved evidence is the entire
          credibility of a safety case. Here is ours, plainly.</p>
        </div>
        <div class="grid grid--2">
          <div class="card" data-reveal>
            <h3>Measured and gated now</h3>
            <p>3,305 tests · 12 Kani proofs on the shipped source · Loom + Miri concurrency checking · 4 fuzz targets
            (60 s per push, 90 min weekly) · mutation testing on checker diffs · decision-coverage floors of 77–79%
            on the checker crates (measured ≈80%) · a Codecov anti-regression ratchet · a 248-scenario KPI gate with
            nightly 40,000-sample Monte-Carlo bounds · fault-injection suites mapped to safety goals.</p>
            ${evRow(".github/workflows/ci.yml", "tests/fault_injection.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Deferred, and labeled as such</h3>
            <p>True MC/DC coverage is toolchain-blocked upstream (branch coverage gates in the meantime; the evidence
            package tracks the Ferrocene path). Certified WCET awaits QNX-target-under-FIFO measurement.
            Quantified hardware metrics already shaped the design: single-supply PMHF fails the ASIL-D target
            (17.7 FIT), so the deployment requirement is a redundant supply (8.7 FIT — pass).</p>
            ${evRow("docs/safety/OCCY_MCDC_EVIDENCE.md", "docs/safety/WCET_MEASUREMENT_METHODOLOGY.md", "docs/safety/OCCY_QUANTITATIVE_METRICS.md")}
          </div>
        </div>
        <div class="compare-note" data-reveal style="margin-top:24px">
          <strong>Release-time evidence bundling:</strong> every tagged release ships a safety-case evidence bundle
          built by CI — the assessment-ready paper trail grows with the code, not after it.
          ${evRow(".github/workflows/release.yml", "ci/build_safety_case.py")}
        </div>
      </div>
    </section>

${ctaBand("Assessors welcome.", "The safety case index, the RTM, and every draft artifact are public. If you assess safety-critical systems for a living, we'd like to hear what we got wrong.")}
`;
