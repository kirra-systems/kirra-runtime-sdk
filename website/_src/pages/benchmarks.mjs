import { ev, evRow, pageHero, ctaBand } from "../template.mjs";

export const meta = {
  slug: "benchmarks",
  title: "Benchmarks",
  desc: "Kirra's measured numbers with their provenance: KPI gate admissibility floors, Monte-Carlo confidence bounds, transport latencies, coverage floors, and the test inventory — every figure linked to source.",
};

export const body = `
${pageHero({
  eyebrow: "Benchmarks &amp; gates",
  title: "Numbers with provenance.",
  lede: "Marketing benchmarks are usually unfalsifiable. These aren't: every figure below is produced by code in the repository, most of them gate CI, and each one links to exactly where it comes from. Host-timing numbers are labeled indicative — certified timing awaits the QNX target.",
})}

    <section aria-labelledby="h-kpi">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Scenario KPI gate · WS-3.1</p>
          <h2 id="h-kpi" data-reveal>Safety KPIs that block merges</h2>
          <p class="lede" data-reveal>A deterministic corpus — 248 doer scenarios and 71 perception frames — runs on
          every PR with hard floors, and a 40,000-sample Monte-Carlo campaign runs nightly with 95% Wilson
          confidence bounds. Negative controls must breach, proving the gate can see the faults it claims to catch.</p>
        </div>
        <div class="grid grid--2">
          <div class="chart" data-reveal>
            <p class="chart__title">Admissibility &amp; safety floors — per-PR gate</p>
            <p class="chart__sub">Fraction of proposals admitted without MRC fallback; observed vs gated floor.</p>
            <div class="chart__body barlist">
              <div class="barlist__row"><span class="barlist__label">Geometric doer (observed)</span><span class="barlist__track"><span class="barlist__fill" style="--w:96.8%"></span></span><span class="barlist__value">0.968</span></div>
              <div class="barlist__row"><span class="barlist__label">Geometric floor (gate)</span><span class="barlist__track"><span class="barlist__fill" style="--w:96.77%;background:var(--chart-4)"></span></span><span class="barlist__value">0.9677</span></div>
              <div class="barlist__row"><span class="barlist__label">Learned doer (observed)</span><span class="barlist__track"><span class="barlist__fill" style="--w:90.4%"></span></span><span class="barlist__value">0.903+</span></div>
              <div class="barlist__row"><span class="barlist__label">Learned floor (gate)</span><span class="barlist__track"><span class="barlist__fill" style="--w:90.32%;background:var(--chart-4)"></span></span><span class="barlist__value">0.9032</span></div>
            </div>
            <div class="budget__legend" style="margin-top:12px">
              <span class="k"><span class="swatch" style="background:var(--chart-1)"></span> observed</span>
              <span class="k"><span class="swatch" style="background:var(--chart-4)"></span> CI floor</span>
            </div>
            ${evRow("ci/scenario_kpi_thresholds.json", "crates/kirra-kpi-gate/src/lib.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>The hard zeroes</h3>
            <p>Some floors aren't percentages — they're absolutes. Per-PR: <code>unsafe_miss_rate ≤ 0.0</code>,
            <code>hazard_recall ≥ 1.0</code>, differential <code>MissedTighten = 0</code> and
            <code>PhantomTighten = 0</code>. Fusion loosening the corridor is a hard zero, always.</p>
            <p style="margin-top:12px">Nightly Monte-Carlo (seed 424242424242, 40k doer + 20k perception samples)
            gates the 95% Wilson bounds: geometric admissibility lower bound observed at 0.9636 against a 0.93
            floor; unsafe-miss upper bound 0.0126 against a 0.03 ceiling; hazard-recall lower bound 0.9852 against
            0.95; negative-control breach lower bound above 0.90.</p>
            ${evRow("ci/scenario_kpi_montecarlo.json", ".github/workflows/kpi-campaign-nightly.yml")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-tests">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Test inventory</p>
          <h2 id="h-tests" data-reveal>Where 3,305 tests live</h2>
        </div>
        <div class="grid grid--2">
          <div class="chart" data-reveal>
            <p class="chart__title">Test functions by tree</p>
            <p class="chart__sub">#[test] + #[tokio::test] counts, whole repository.</p>
            <div class="chart__body barlist">
              <div class="barlist__row"><span class="barlist__label">crates/* (checker &amp; platform)</span><span class="barlist__track"><span class="barlist__fill" style="--w:100%"></span></span><span class="barlist__value">1,685</span></div>
              <div class="barlist__row"><span class="barlist__label">parko/ (ML &amp; diverse governor)</span><span class="barlist__track"><span class="barlist__fill" style="--w:43.2%"></span></span><span class="barlist__value">728</span></div>
              <div class="barlist__row"><span class="barlist__label">src/ (verifier control plane)</span><span class="barlist__track"><span class="barlist__fill" style="--w:41.5%"></span></span><span class="barlist__value">699</span></div>
              <div class="barlist__row"><span class="barlist__label">tests/ (integration drills)</span><span class="barlist__track"><span class="barlist__fill" style="--w:5.8%"></span></span><span class="barlist__value">98</span></div>
              <div class="barlist__row"><span class="barlist__label">verification + tools</span><span class="barlist__track"><span class="barlist__fill" style="--w:1.7%"></span></span><span class="barlist__value">28</span></div>
            </div>
          </div>
          <div class="card" data-reveal>
            <h3>Beyond example-based testing</h3>
            <p>32 property-based test suites (proptest) with committed regression seeds · 12 Kani proof harnesses ·
            4 Loom concurrency models · 4 fuzz targets, deep-fuzzed 90 minutes each weekly with persisted corpora ·
            mutation testing against checker diffs · the same storage conformance suites run against three backends
            including live Postgres 16.</p>
            ${evRow("proptest-regressions/", "verification/kani/", "crates/kirra-loom-models/src/lib.rs", "fuzz/fuzz_targets/", ".github/workflows/fuzz-deep-weekly.yml")}
          </div>
        </div>
        <div class="grid grid--2" style="margin-top:20px">
          <div class="chart" data-reveal>
            <p class="chart__title">Decision coverage — gated floor vs measured</p>
            <p class="chart__sub">Branch/decision coverage on the checker crates (true MC/DC is toolchain-blocked upstream, tracked as #65).</p>
            <div class="chart__body barlist">
              <div class="barlist__row"><span class="barlist__label">kirra-core · measured</span><span class="barlist__track"><span class="barlist__fill" style="--w:81.2%"></span></span><span class="barlist__value">81.2%</span></div>
              <div class="barlist__row"><span class="barlist__label">kirra-trajectory · measured</span><span class="barlist__track"><span class="barlist__fill" style="--w:80.1%"></span></span><span class="barlist__value">80.1%</span></div>
              <div class="barlist__row"><span class="barlist__label">parko-core · measured</span><span class="barlist__track"><span class="barlist__fill" style="--w:79.8%"></span></span><span class="barlist__value">79.8%</span></div>
              <div class="barlist__row"><span class="barlist__label">gated floors</span><span class="barlist__track"><span class="barlist__fill" style="--w:78%;background:var(--chart-4)"></span></span><span class="barlist__value">77–79%</span></div>
            </div>
            ${evRow(".github/workflows/ci.yml:379", "codecov.yml")}
          </div>
          <div class="chart" data-reveal>
            <p class="chart__title">Transport read latency — p50, host-indicative</p>
            <p class="chart__sub">From the repository's own benches; not certified WCET.</p>
            <div class="chart__body barlist">
              <div class="barlist__row"><span class="barlist__label">In-process floor</span><span class="barlist__track"><span class="barlist__fill" style="--w:2.9%"></span></span><span class="barlist__value">28 ns</span></div>
              <div class="barlist__row"><span class="barlist__label">SHM seqlock read</span><span class="barlist__track"><span class="barlist__fill" style="--w:6.2%"></span></span><span class="barlist__value">60 ns</span></div>
              <div class="barlist__row"><span class="barlist__label">iceoryx2 zero-copy</span><span class="barlist__track"><span class="barlist__fill" style="--w:31.7%"></span></span><span class="barlist__value">309 ns</span></div>
              <div class="barlist__row"><span class="barlist__label">UDP + serde proxy</span><span class="barlist__track"><span class="barlist__fill" style="--w:100%"></span></span><span class="barlist__value">976 ns</span></div>
            </div>
            ${evRow("tools/iceoryx2-spike/src/bin/latency_bench.rs")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-doer">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Doer evaluation</p>
          <h2 id="h-doer" data-reveal>Measuring the untrusted half</h2>
          <p class="lede" data-reveal>The doer-eval harness scores planners on two axes the safety case cares about:
          admissibility (how often the checker admits their plans without MRC fallback) and plan quality against a
          reference. The committed scorecard shows a v2 model trading strict-acceptance for progress — visible,
          quantified, and safe to ship <em>because the checker holds either way</em>.</p>
        </div>
        ${evRow("crates/kirra-doer-eval/src/lib.rs", "artifacts/doer-eval/scorecard.json")}
      </div>
    </section>

${ctaBand("Reproduce any number on this page.", "Each chart names the file that produces it. Clone, run, compare — that's the point.")}
`;
