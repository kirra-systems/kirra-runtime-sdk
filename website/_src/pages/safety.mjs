import { ev, evRow, pageHero, ctaBand, LIVE, DRAFT } from "../template.mjs";

export const meta = {
  slug: "safety",
  title: "Safety",
  desc: "Kirra's safety model: three fleet postures, controlled decel-to-stop in Degraded, minimum-risk maneuvers, recovery hysteresis, telemetry watchdogs, and a safe-state specification written against ISO 26262.",
};

const stateSvg = `
<svg viewBox="0 0 960 300" role="img" aria-labelledby="ssT ssD">
  <title id="ssT">Fleet posture state machine</title>
  <desc id="ssD">Nominal transitions to Degraded on a sensor fault or confidence drop, and recovers automatically after five consecutive healthy reports inside a ten-second window. Nominal or Degraded transition to LockedOut on attestation failure, dependency cycle, or stale posture. LockedOut requires operator clearance to exit — recovery is never a timer.</desc>
  <g>
    <rect class="dg-box" x="30" y="100" width="220" height="90" rx="14" style="stroke:var(--ok)"/>
    <text class="dg-label" x="58" y="136">NOMINAL</text>
    <text class="dg-sub" x="58" y="158">all classified commands admitted</text>
  </g>
  <g>
    <rect class="dg-box" x="380" y="100" width="220" height="90" rx="14" style="stroke:var(--warn)"/>
    <text class="dg-label" x="408" y="136">DEGRADED</text>
    <text class="dg-sub" x="408" y="158">decel-to-stop-and-hold</text>
  </g>
  <g>
    <rect class="dg-box dg-box--deny" x="730" y="100" width="220" height="90" rx="14"/>
    <text class="dg-label" x="758" y="136">LOCKED OUT</text>
    <text class="dg-sub" x="758" y="158">every command denied</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M250 122 L 378 122"/>
  <text class="dg-mono" x="258" y="112">fault · low confidence</text>
  <path class="dg-edge dg-edge--flow" d="M378 172 L 250 172"/>
  <text class="dg-mono" x="256" y="192">5 healthy reports / 10 s window</text>
  <path class="dg-edge dg-edge--deny" d="M600 122 L 728 122"/>
  <text class="dg-mono" x="608" y="112">attestation fail · cycle · stale</text>
  <path class="dg-edge dg-edge--deny" d="M140 100 C 200 20, 700 20, 810 98"/>
  <text class="dg-mono" x="410" y="42">trust violation ⇒ direct lockout</text>
  <path class="dg-edge" d="M728 172 C 690 230, 660 230, 602 250 L 602 250" stroke-dasharray="4 6"/>
  <text class="dg-mono" x="620" y="250">operator clearance only — never a timer</text>
</svg>`;

export const body = `
${pageHero({
  eyebrow: "Safety model",
  title: "Fail closed.<br>Recover deliberately.",
  lede: "Kirra's safety model is three postures and a promise: when anything is unproven — a node unattested, a sensor silent, a cache stale, a command unclassified — the system moves toward stillness, and it never talks itself back into motion.",
})}

    <section aria-labelledby="h-states">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Posture state machine</p>
          <h2 id="h-states" data-reveal>Three states. Asymmetric exits.</h2>
          <p class="lede" data-reveal>Getting into a safe state is automatic and fast. Getting out is slow and
          evidence-gated: Degraded recovers only after five consecutive healthy reports inside a ten-second
          window, and LockedOut recovers only through a human. Down is easy; up requires proof.</p>
        </div>
        <div class="diagram-frame flow-anim" data-reveal>${stateSvg}</div>
        ${evRow("src/recovery_hysteresis.rs", "src/verifier.rs", "docs/safety/SAFE_STATE_SPECIFICATION.md")}
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-degraded">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>The Degraded contract</p>
          <h2 id="h-degraded" data-reveal>Degraded is not a crawl.<br>It is a controlled stop.</h2>
          <p class="lede" data-reveal>Motivated by the October 2023 Cruise incident — a post-collision pullover
          executed at ~3 m/s under a 5 m/s “crawl ceiling” — Kirra's Degraded posture admits a command only if it
          converges to a stop. The governor never authors re-acceleration.</p>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Three conditions, all required</h3>
            <p>(a) inside the minimum-risk decel envelope; (b) non-increasing speed — <code>|proposed| ≤ |current|</code>;
            (c) no re-initiation: once at rest (≤0.05 m/s), any command to move again is denied and the vehicle
            holds. A reversal through zero counts as re-initiation.</p>
            ${evRow("crates/kirra-core/src/kinematics_contract.rs", "docs/safety/SAFE_STATE_SPECIFICATION.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Enforced at four points</h3>
            <p>The same decel-to-stop gate runs at the HTTP actuator gateway, the fabric asset governor, the ROS 2
            trajectory validator, and the parko MRC profile — which also gates the independent angular-velocity
            channel for differential drive.</p>
            ${evRow("docs/adr/0011-degraded-http-actuator-503-vs-decel-gate.md", "tests/posture_gate_integration.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Machine-checked</h3>
            <p>Kani proofs K4 and K5 verify — over exhaustive <code>f64</code> case splits — that Degraded re-initiation
            and speed increases are denied. Not tested on samples: proved on the domain.</p>
            ${evRow("verification/kani/src/proofs_kinematics.rs:166")}
          </div>
        </div>
        <div class="compare-note" data-reveal style="margin-top:24px">
          <strong>Two meanings of “MRC,” kept deliberately distinct:</strong> Degraded's MRC is a decel-to-stop
          <em>envelope</em> that bounds a converging command; LockedOut's MRC is a safe-stop <em>maneuver</em> while all
          commands are denied. The safe-state specification (KIRRA-SSS-001) assigns each to a fault-severity class,
          with a 5.0 m/s MRC velocity ceiling.
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-watchdog">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Liveness</p>
          <h2 id="h-watchdog" data-reveal>Silence is a fault</h2>
          <p class="lede" data-reveal>A safety system that only reacts to bad messages misses the worst failure of
          all: no message. Kirra's watchdogs treat absence as evidence.</p>
        </div>
        <div class="table-wrap" data-reveal>
          <table class="tbl">
            <caption class="visually-hidden">Watchdog thresholds</caption>
            <thead><tr><th scope="col">Signal</th><th scope="col">Threshold</th><th scope="col">Consequence</th></tr></thead>
            <tbody>
              <tr><td>AV telemetry silence</td><td class="mono">1,000 ms warn · 2,000 ms fault</td><td>Node marked untrusted; posture recomputed (sweep every 100 ms)</td></tr>
              <tr><td>Posture cache age</td><td class="mono">&gt; 5,000 ms</td><td>Routing fails closed — treated as LockedOut</td></tr>
              <tr><td>Attestation nonce age</td><td class="mono">&gt; 30,000 ms</td><td>Challenge expires; proof must restart</td></tr>
              <tr><td>Federation report age</td><td class="mono">&gt; 5,000 ms</td><td>Report rejected (replay window)</td></tr>
              <tr><td>Armed-but-silent VRU channel</td><td class="mono">freshness budget</td><td>MRC-floor speed cap — the ego stops rather than drives blind</td></tr>
            </tbody>
          </table>
        </div>
        ${evRow("src/telemetry_watchdog.rs", "src/posture_cache.rs", "crates/kirra-trajectory/src/vru_channel.rs")}
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-envelope">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Per-class envelopes</p>
          <h2 id="h-envelope" data-reveal>No default vehicle class</h2>
          <p class="lede" data-reveal>A sidewalk courier and a robotaxi must not share an envelope. The vehicle class
          is mandatory configuration — unset, empty, or unknown aborts startup, because a wrong class would select
          another class's physics. Every number below is normative in the repository and marked validation-pending
          until signed off.</p>
        </div>
        <div class="table-wrap" data-reveal>
          <table class="tbl">
            <caption class="visually-hidden">Vehicle class contract profiles</caption>
            <thead><tr><th scope="col">Limit</th><th scope="col">Courier (sidewalk)</th><th scope="col">Delivery AV (road pod)</th><th scope="col">Robotaxi (frozen ref.)</th></tr></thead>
            <tbody>
              <tr><td>Max speed</td><td class="mono">3.0 m/s</td><td class="mono">12.0 m/s</td><td class="mono">35.0 m/s</td></tr>
              <tr><td>ODD speed cap</td><td class="mono">2.5 m/s</td><td class="mono">11.0 m/s</td><td class="mono">22.35 m/s</td></tr>
              <tr><td>Braking decel</td><td class="mono">3.0 m/s²</td><td class="mono">4.0 m/s²</td><td class="mono">4.5 m/s²</td></tr>
              <tr><td>Lateral accel</td><td class="mono">1.5 m/s²</td><td class="mono">2.5 m/s²</td><td class="mono">3.5 m/s²</td></tr>
              <tr><td>RSS lateral band</td><td class="mono">0.6 m</td><td class="mono">2.0 m</td><td class="mono">4.0 m</td></tr>
            </tbody>
          </table>
        </div>
        ${evRow("docs/CONTRACT_PROFILES.md", "src/gateway/contract_profiles.rs", "docs/adr/0028-sidewalk-courier-odd.md", "docs/adr/0001-occy-odd-speed-cap.md")}
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-invariants">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Invariants</p>
          <h2 id="h-invariants" data-reveal>Rules that have survived being attacked</h2>
          <p class="lede" data-reveal>The repository maintains a list of security invariants that submissions have
          repeatedly tried to weaken — and that are rejected outright. A few of the load-bearing ones:</p>
        </div>
        <div class="grid grid--2">
          <div class="card" data-reveal><h3>Envelope cap beats rate limits</h3><p>The governor clamps to the absolute hard boundary first, then applies rate-of-change limits. The envelope always wins.</p></div>
          <div class="card" data-reveal><h3>Unknown is denied everywhere</h3><p>The <code>Unknown</code> command class is denied in all postures — including Nominal. The early return is never removed.</p></div>
          <div class="card" data-reveal><h3>Attestation is never mocked</h3><p>Trust requires a per-node Ed25519 proof over a fresh challenge, checked against the registered key. No registered key, malformed proof, bad signature — rejected.</p></div>
          <div class="card" data-reveal><h3>Actuator topics never replay</h3><p>DDS actuator topics must use Volatile durability, so a reconnecting subscriber can never receive a stale command as if it were fresh.</p></div>
        </div>
        ${evRow("SECURITY.md", "docs/v1_security_invariants.md", "src/startup_sentinel.rs")}
      </div>
    </section>

${ctaBand("Read the safe-state specification.", "KIRRA-SSS-001 assigns a safe state to every fault-severity class — and the repository's tests hold the code to it.")}
`;
