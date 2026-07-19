import { ev, evRow, pageHero, ctaBand } from "../template.mjs";

export const meta = {
  slug: "determinism",
  title: "Determinism",
  desc: "Why Kirra's verdicts are reproducible: bit-identical incident replay, injected clocks, seqlock snapshots model-checked under Loom and Miri, and 12 Kani proofs over the shipped source.",
};

export const body = `
${pageHero({
  eyebrow: "Determinism",
  title: "Same inputs.<br>Same bits. Every time.",
  lede: "A safety verdict you can't reproduce is a safety verdict you can't audit. Kirra treats determinism as an engineering requirement with teeth: time is injected, concurrency is model-checked, floating point is compared bit-for-bit, and the checker core is frozen by hash.",
})}

    <section aria-labelledby="h-replay">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Incident replay</p>
          <h2 id="h-replay" data-reveal>Reconstruction, not simulation</h2>
          <p class="lede" data-reveal>Feed a captured session back through the <em>real</em> checker — the same
          functions, the same per-class contract profiles — and every verdict must reproduce exactly:
          <code>f64::to_bits</code> equality, no epsilon. If a single bit diverges, the tool exits non-zero.</p>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Time never enters the verdict</h3>
            <p>The fast-loop verdict is a pure function of its inputs — which is what makes bit-identical replay
            possible at all. Time-dependent flows replay through an injected <code>VirtualClock</code> and a
            deterministic scenario runner instead.</p>
            ${evRow("src/clock.rs", "src/scenario_runner.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Honest about the edges</h3>
            <p>Records without complete context — slow-loop summaries, derate-enabled sessions, locked-out spans,
            NaN inputs — are <em>classified</em>, never guessed. And the JSON float parse is exact
            (<code>float_roundtrip</code>), because the default parse is one ulp off and one ulp is a divergence.</p>
            ${evRow("crates/kirra-replay/src/lib.rs", "docs/REPLAY_INCIDENT_RECONSTRUCTION.md")}
          </div>
          <div class="card" data-reveal>
            <h3>One command away</h3>
            <p><code>kirra-replay --class robotaxi session.jsonl</code> — incident reconstruction as a CLI, exit 1 on
            divergence. Regulators and integrators run the same tool we do.</p>
            ${evRow("crates/kirra-replay/src/bin/kirra_replay.rs")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-concurrency">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Concurrency</p>
          <h2 id="h-concurrency" data-reveal>The schedules you'll never see in a test run</h2>
          <p class="lede" data-reveal>Race conditions don't show up on demand. So the critical protocols are checked
          under tools that enumerate schedules rather than hope for them.</p>
        </div>
        <div class="grid grid--2">
          <div class="card" data-reveal>
            <h3>Loom — model-checked protocols</h3>
            <p>Four concurrency models run every interleaving CI can afford: posture generations stay unique and
            monotone, the cache holds the highest generation under concurrent replacement, a sticky lockout is
            never downgraded by a racing recalculation, and an accepted seqlock snapshot is never torn.</p>
            ${evRow("crates/kirra-loom-models/src/lib.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Miri — the memory model itself</h3>
            <p>The contract channel — the seqlock the whole enforcement path reads through — runs under Miri with
            multi-seed scheduling in CI, checking for undefined behavior at the Rust memory-model level, not just
            observable bugs.</p>
            ${evRow(".github/workflows/ci.yml:254", "crates/kirra-contract-channel/src/seqlock.rs")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-proofs">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Formal proofs</p>
          <h2 id="h-proofs" data-reveal>Twelve properties, proved on the shipped source</h2>
          <p class="lede" data-reveal>The Kani harnesses <code>#[path]</code>-include the production files verbatim —
          the frozen kinematics core is proved <em>unmodified</em>, pinned by git blob hash
          <code>ed00f4da…</code>. No proof-friendly rewrites; no divergence between what's proved and what ships.</p>
        </div>
        <div class="table-wrap" data-reveal>
          <table class="tbl">
            <caption class="visually-hidden">Kani proof inventory</caption>
            <thead><tr><th scope="col">Family</th><th scope="col">Properties</th><th scope="col">Domain</th></tr></thead>
            <tbody>
              <tr><td>L1–L4 · HA lease algebra</td><td>TTL split is total &amp; split-brain-safe; promotion only after holder expiry; clock skew fails safe; on-cadence renewal never expires</td><td class="mono">all u64</td></tr>
              <tr><td>K1–K5 · kinematics core</td><td>non-finite input always denied; non-positive dt denied; speed-ceiling clamp exact; Degraded re-initiation denied; Degraded speed increase denied</td><td class="mono">f64 is_finite case splits</td></tr>
              <tr><td>R1–R3 · RSS primitives</td><td>longitudinal distance total; monotone in closing speed (weekly deep lane + exhaustive concrete mirror per PR); invalid brake ⇒ exact fail-safe</td><td class="mono">integer-scaled grids</td></tr>
            </tbody>
          </table>
        </div>
        <p class="evidence-note">Honest scope: the P6 trigonometric path is excluded from proof (transcendentals) and covered by
        decision-coverage floors and property tests instead. The repository says exactly where the proofs end.</p>
        ${evRow("verification/kani/src/proofs_lease.rs", "verification/kani/src/proofs_kinematics.rs", "verification/kani/src/proofs_rss.rs", ".github/workflows/kani-deep-weekly.yml")}
      </div>
    </section>

${ctaBand("Determinism you can falsify.", "Clone the repo, capture a session, flip one bit — and watch the replay tool refuse it.")}
`;
