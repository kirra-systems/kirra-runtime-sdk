import { ev, evRow, pageHero, ctaBand } from "../template.mjs";

export const meta = {
  slug: "performance",
  title: "Performance",
  desc: "Kirra's timing story, honestly scoped: a 100 µs verdict target, p99.9-gated CI regression tripwires, nanosecond-scale shared-memory reads, and a WCET methodology that refuses to mint numbers from the wrong clock.",
};

export const body = `
${pageHero({
  eyebrow: "Performance",
  title: "Fast enough to prove.<br>Honest about the clock.",
  lede: "Kirra's performance discipline starts with a rule most benchmarks break: host timing is indicative, never worst-case-execution-time. Every number below is labeled with what it is — a budget, a gate, or a measurement — and where it came from.",
})}

    <section aria-labelledby="h-budget">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>The verdict budget</p>
          <h2 id="h-budget" data-reveal>100 µs to decide. Crypto excluded by design.</h2>
          <p class="lede" data-reveal>The governor verdict has a 100 µs deployment target (SG9's fail-closed timeout)
          and a 10× CI regression ceiling gated on p99.9 over 100,000 iterations — not on the mean, because tails
          are where timing claims go to die. The Ed25519 release signature deliberately rides the actuation path,
          outside the verdict budget (ADR-0031).</p>
        </div>
        <div class="chart" data-reveal>
          <p class="chart__title">Assembled in-line loop — read → validate → decide → sign → verify → release</p>
          <p class="chart__sub">Release build, CI hardware. Host-indicative regression tripwire, not certified WCET.</p>
          <div class="chart__body budget">
            <div class="budget__track">
              <span class="budget__fill" style="--w:11.6%"></span>
              <span class="budget__limit" style="--limit:10%"></span>
            </div>
            <div class="budget__legend">
              <span class="k"><span class="swatch" style="background:var(--chart-1)"></span> ≈65 µs p50 · ≈116 µs p99.9 measured (Ed25519-dominated)</span>
              <span class="k"><span class="swatch" style="background:var(--danger)"></span> 100 µs verdict target</span>
              <span class="k"><span class="swatch" style="background:var(--surface-3);border:1px solid var(--line-strong)"></span> 1,000 µs CI ceiling</span>
            </div>
          </div>
          ${evRow("src/wcet_gate.rs:92", "crates/kirra-inline-governor/README.md")}
        </div>
        <div class="grid grid--3" style="margin-top:20px">
          <div class="card" data-reveal>
            <h3>Structural boundedness</h3>
            <p>The verdict path carries an O(1) structural argument, not just measurements: no allocation, no
            unbounded loops, worst cases enumerated — containment's worst case is exactly 50 × 256 × 4 = 51,200
            polygon-edge tests, gated at 10 ms.</p>
            ${evRow("src/wcet_gate.rs:121")}
          </div>
          <div class="card" data-reveal>
            <h3>A timing substrate built for the target</h3>
            <p>The measurement library is <code>no_std</code>, zero-dependency, integer-only, O(1) per record — and it
            refuses to label a host run as certified: only a QNX-target-under-FIFO environment qualifies.</p>
            ${evRow("crates/kirra-timing", "crates/kirra-wcet-bench/src/main.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Why the tail matters</h3>
            <p>On a stock Jetson kernel, the repository measured a reproducible ~47–51 ms scheduling tail under
            zero-copy transport — precisely why certified WCET is defined on QNX under FIFO, and why we don't quote
            Linux numbers as guarantees.</p>
            ${evRow("tools/iceoryx2-spike/results/orin-nx-16gb-linux-isolated.txt", "docs/safety/WCET_MEASUREMENT_METHODOLOGY.md")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-latency">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Transport measurements</p>
          <h2 id="h-latency" data-reveal>Nanoseconds where it counts</h2>
          <p class="lede" data-reveal>The enforced path avoids serialization entirely: a seqlock read over a frozen
          <code>#[repr(C)]</code> struct. The repository's own benches put that read at ~60 ns p50 — roughly 16×
          cheaper than a UDP + serde hop — and read + validate + decode at ~297 ns.</p>
        </div>
        <div class="grid grid--2">
          <div class="chart" data-reveal>
            <p class="chart__title">Read latency by transport — p50, host</p>
            <p class="chart__sub">From the iceoryx2 spike bench and the SHM carrier README.</p>
            <div class="chart__body barlist">
              <div class="barlist__row"><span class="barlist__label">In-process floor</span><span class="barlist__track"><span class="barlist__fill" style="--w:2.9%"></span></span><span class="barlist__value">28 ns</span></div>
              <div class="barlist__row"><span class="barlist__label">SHM seqlock read</span><span class="barlist__track"><span class="barlist__fill" style="--w:6.2%"></span></span><span class="barlist__value">60 ns</span></div>
              <div class="barlist__row"><span class="barlist__label">SHM read+validate+decode</span><span class="barlist__track"><span class="barlist__fill" style="--w:30.4%"></span></span><span class="barlist__value">297 ns</span></div>
              <div class="barlist__row"><span class="barlist__label">iceoryx2 zero-copy</span><span class="barlist__track"><span class="barlist__fill" style="--w:31.7%"></span></span><span class="barlist__value">309 ns</span></div>
              <div class="barlist__row"><span class="barlist__label">UDP + serde proxy</span><span class="barlist__track"><span class="barlist__fill" style="--w:100%"></span></span><span class="barlist__value">976 ns</span></div>
            </div>
            ${evRow("tools/iceoryx2-spike/src/bin/latency_bench.rs", "crates/kirra-hv-carrier/README.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Zero-copy, isolated on purpose</h3>
            <p>The iceoryx2 evaluation lives in its own workspace so the dependency can never leak into the safety
            tree before it's earned adoption. It carries the production frozen contract over a real channel, passes
            a 9-class fault matrix in full <em>and</em> minimal feature configurations — and eliminated torn headers
            at the transport layer entirely.</p>
            <p style="margin-top:12px">On real Jetson Orin NX silicon with isolated cores: 1.4 µs p50 round-trip,
            2.3 µs p99.9 — with the Linux scheduling tail documented right next to the good news.</p>
            ${evRow("tools/iceoryx2-spike/README.md", "tools/iceoryx2-spike/results/orin-nx-16gb-linux-isolated.txt")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-tuning">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Build discipline</p>
          <h2 id="h-tuning" data-reveal>Tuned control plane. Untouched safety element.</h2>
          <p class="lede" data-reveal>LTO, target-cpu flags, and PGO are applied to the untrusted control-plane
          binaries only. The certification-track judge builds with fixed, conservative flags — no LTO, no PGO,
          no target-cpu — because a reproducible artifact beats a fast one where it counts. And CI double-builds
          the QNX judge and byte-compares the results.</p>
        </div>
        ${evRow("docs/PERFORMANCE_BUILD_TUNING.md", ".github/workflows/ci.yml:122")}
      </div>
    </section>

${ctaBand("Every number, labeled.", "Budgets, gates, and measurements are different things. The repository keeps them separate — and so does this page.")}
`;
