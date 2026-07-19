import { ev, evRow, pageHero, ctaBand, LIVE } from "../template.mjs";

export const meta = {
  slug: "runtime",
  title: "Runtime",
  desc: "Inside the Kirra verifier runtime: the coalescing posture engine, epoch-fenced HA failover, disk-before-memory persistence, OTA campaigns with Uptane verification, and a declarative execution manager.",
};

const haSvg = `
<svg viewBox="0 0 960 250" role="img" aria-labelledby="haT haD">
  <title id="haT">HA failover with epoch fencing</title>
  <desc id="haD">A primary verifier holds a durable epoch and writes heartbeats. A standby promotes by claiming the next epoch through a compare-and-swap. A revived old primary is fenced: its stale epoch is refused, so there is exactly one writer at a time.</desc>
  <g>
    <rect class="dg-box dg-box--accent" x="20" y="30" width="240" height="90" rx="12"/>
    <text class="dg-label" x="40" y="60">Primary — Active</text>
    <text class="dg-mono" x="40" y="84">holds epoch N · heartbeats 2 s</text>
    <text class="dg-sub" x="40" y="104">lease renew at half-life (EP-03)</text>
  </g>
  <g>
    <rect class="dg-box" x="20" y="150" width="240" height="80" rx="12"/>
    <text class="dg-label" x="40" y="180">Standby — Passive</text>
    <text class="dg-mono" x="40" y="204">watches heartbeat + lease</text>
  </g>
  <g>
    <rect class="dg-box" x="380" y="80" width="220" height="90" rx="12"/>
    <text class="dg-label" x="400" y="110">Durable store</text>
    <text class="dg-mono" x="400" y="134">try_claim_epoch — CAS</text>
    <text class="dg-sub" x="400" y="154">single source of authority</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M260 75 C 320 75, 330 100, 378 110"/>
  <path class="dg-edge dg-edge--flow" d="M260 190 C 320 190, 330 160, 378 145"/>
  <g>
    <rect class="dg-box dg-box--deny" x="700" y="80" width="240" height="90" rx="12"/>
    <text class="dg-label" x="720" y="110">Revived old primary</text>
    <text class="dg-mono" x="720" y="134">epoch N &lt; N+1 ⇒ FENCED</text>
    <text class="dg-sub" x="720" y="154">stale re-claim refused</text>
  </g>
  <path class="dg-edge dg-edge--deny" d="M700 125 L 602 125"/>
</svg>`;

export const body = `
${pageHero({
  eyebrow: "Runtime",
  title: "A control plane that<br>refuses to guess.",
  lede: "The verifier service is the fleet's source of truth: who is attested, what posture the fleet is in, which artifact every node should be running. Every one of those answers is derived, persisted, and fenced — never assumed.",
})}

    <section aria-labelledby="h-engine">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Posture engine</p>
          <h2 id="h-engine" data-reveal>One worker, coalesced bursts, monotonic generations</h2>
          <p class="lede" data-reveal>Posture recalculation is serialized through a single worker fed by a typed
          trigger channel. When five sensors fault at once, the worker drains all buffered triggers and recomputes
          once — no thundering herd, no torn intermediate states.</p>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Cycle-safe DAG traversal</h3>
            <p>Fleet posture derives from a gray/black two-set traversal over the dependency graph. A cycle locks the
            fleet out with <code>CYCLE_DETECTED</code>; a depth blowout locks out with <code>MAX_DEPTH_EXCEEDED</code>.
            A locked-out dependency propagates LockedOut upward — never merely Degraded.</p>
            ${evRow("crates/kirra-safety-authority/src/dag.rs", "src/verifier.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Generations survive restarts</h3>
            <p>The posture generation counter persists to SQLite and is restored at boot, so generation numbers never
            travel backward across restarts — federation peers depend on that monotonicity, and a conditional-upsert
            keeps it race-safe on both SQLite and Postgres.</p>
            ${evRow("src/posture_engine.rs", "crates/kirra-persistence")}
          </div>
          <div class="card" data-reveal>
            <h3>Structured lockout reasons</h3>
            <p>Fail-closed decisions carry typed reasons — <code>DagLockedOut</code>, <code>PostureCacheStale</code> — not
            strings. The cache snapshot is atomic: posture, generation, timestamp, and TTL move together.</p>
            ${evRow("src/posture_engine_v2.rs", "src/posture_cache.rs")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-ha">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>High availability</p>
          <h2 id="h-ha" data-reveal>Failover without split brain</h2>
          <p class="lede" data-reveal>Heartbeats detect failure; they never grant authority. Authority is a durable
          epoch claimed by compare-and-swap, and a revived old primary is fenced — its writes refused — the moment a
          standby has claimed the next epoch. The whole drill runs as a deterministic test in CI, plus a real
          two-process drill.</p>
        </div>
        <div class="diagram-frame flow-anim" data-reveal>${haSvg}</div>
        <p class="diagram-caption">
          <span>Lease-based failover (≤5 s, EP-03) is opt-in behind <code>KIRRA_HA_LEASE_ENABLED</code>; the epoch CAS remains the sole takeover authority either way.</span>
        </p>
        ${evRow("tests/ha_failover.rs", "tests/ha_two_process_drill.rs", "src/lease.rs", "src/standby_monitor.rs", "verification/kani/src/proofs_lease.rs")}
        <div class="compare-note" data-reveal style="margin-top:24px">
          The lease timing algebra — renew at half-life, promote only after holder expiry, clock-skew fails safe —
          is machine-checked by Kani proofs L1–L4 over <em>all</em> 64-bit inputs, not sampled ones.
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-persist">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Persistence</p>
          <h2 id="h-persist" data-reveal>Disk before memory, always</h2>
          <p class="lede" data-reveal>WAL-mode SQLite with a strict ordering invariant: state is saved to disk before
          it is inserted into memory, so a crash can lose an in-flight request but never invent one. Schema
          migrations are versioned and refuse to open a database written by a newer binary.</p>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Kill-tested durability</h3>
            <p>A CI drill SIGKILLs a child process mid-append and asserts the audit chain reopens as a valid,
            intact prefix — an abrupt power loss can never leave a torn or forked chain.</p>
            ${evRow("tests/audit_chain_prefix_on_kill.rs", "tests/audit_row_durability_on_abort.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Backend-agnostic contracts</h3>
            <p>Storage is a family of traits — epoch fence, node store, federation, posture state, OTA campaigns —
            and the <em>same</em> conformance suites run against SQLite, in-memory, and a live Postgres 16 service
            container in CI. Corrupt rows decode fail-closed on every backend.</p>
            ${evRow("crates/kirra-verifier-pg", ".github/workflows/ci.yml:503")}
          </div>
          <div class="card" data-reveal>
            <h3>Config that fails at boot</h3>
            <p>Every <code>KIRRA_*</code> variable lives in one canonical registry; unknown vars warn at startup, and a
            malformed value in a migrated var aborts boot rather than silently defaulting at use. The effective
            config's SHA-256 digest is committed to the audit chain, so drift is detectable.</p>
            ${evRow("src/env_config.rs")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-ota">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Fleet rollout</p>
          <h2 id="h-ota" data-reveal>OTA that halts before it hurts</h2>
          <p class="lede" data-reveal>Governor artifacts roll out through staged campaigns with a hard rule:
          <code>advance</code> is fail-closed on fleet posture. If the fleet is anything but Nominal, a campaign
          halts — it never rolls. A background sweep auto-halts active campaigns on a confirmed regression
          between advances.</p>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Dual-slot A/B installs</h3>
            <p>Node-side, a device-agnostic installer stages, trials, and health-gates every artifact — automatic
            rollback on failed probes — with both app-level slots and the Jetson bootloader's native A/B slots via
            <code>nvbootctrl</code>. Verified end-to-end by a two-node rollout harness in CI.</p>
            ${evRow("crates/kirra-ota-installer", "tests/two_node_rollout.rs", "crates/kirra-ota-installer/src/nvbootctrl.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Uptane, end to end</h3>
            <p>An anchored node verifies the full signed metadata set — root-keyed role signatures, freshness, rollback
            floors — before it downloads anything. A re-served older set (rollback attack) or a stripped set
            (downgrade-by-omission) is refused. The verifier is just an untrusted carrier.</p>
            ${evRow("crates/kirra-ota-installer", "docs/ota/UPTANE_ROLES.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Attested adoption</h3>
            <p>Nodes report the digest they're actually running; reports can be Ed25519-signed against the node's
            registered attestation key, making adoption counts unforgeable. Campaign lifecycle mutations are
            audit-chained.</p>
            ${evRow("crates/kirra-ota-campaign", "src/campaign_monitor.rs")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-exec">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Execution model</p>
          <h2 id="h-exec" data-reveal>Supervised loops, declared as data</h2>
          <p class="lede" data-reveal>The runtime's seven supervised loops — watchdogs, monitors, shippers — are
          declared in a task manifest with dependencies, criticality, and deadlines. Startup order is a topological
          sort that fails closed on a cycle, a missing dependency, or a duplicate.</p>
        </div>
        <div class="table-wrap" data-reveal>
          <table class="tbl">
            <caption class="visually-hidden">Key supervised loops and their cadence</caption>
            <thead><tr><th scope="col">Loop</th><th scope="col">Cadence</th><th scope="col">Fail-closed behavior</th></tr></thead>
            <tbody>
              <tr><td>Telemetry watchdog</td><td class="mono">100 ms sweep</td><td>2 s of sensor silence ⇒ node faulted, posture recomputed</td></tr>
              <tr><td>Campaign monitor</td><td class="mono">1 s sweep</td><td>Confirmed posture regression ⇒ active campaigns auto-halt</td></tr>
              <tr><td>Heartbeat writer</td><td class="mono">2 s</td><td>Primary silence ⇒ standby promotes via epoch CAS</td></tr>
              <tr><td>Audit shipper</td><td class="mono">5 s</td><td>Ship-then-advance cursor; at-least-once to WORM sink</td></tr>
              <tr><td>Cert-expiry monitor</td><td class="mono">1 h census</td><td>Lapsed mTLS principal ⇒ warn + audit (auth already fail-closes)</td></tr>
            </tbody>
          </table>
        </div>
        ${evRow("src/execution_manager.rs", "src/telemetry_watchdog.rs", "src/campaign_monitor.rs", "src/audit_shipper.rs", "src/cert_expiry_monitor.rs")}
      </div>
    </section>

${ctaBand("Run it yourself in five minutes.", "docker compose up brings up the verifier, dashboard, and an optional HA standby — then kill the primary and watch the fence hold.")}
`;
