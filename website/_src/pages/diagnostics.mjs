import { ev, evRow, pageHero, ctaBand, LIVE } from "../template.mjs";

export const meta = {
  slug: "diagnostics",
  title: "Diagnostics",
  desc: "Kirra's observability surface: fleet posture APIs and live event streams, explainable denial verdicts, Prometheus metrics that survive lockout, tamper-evident audit export, and three operator consoles.",
};

export const body = `
${pageHero({
  eyebrow: "Diagnostics &amp; observability",
  title: "You can't govern<br>what you can't see.",
  lede: "A safety governor that goes dark under failure is worse than none — operators must be able to tell “locked out” from “down” at 2 AM. Kirra's observability surface is engineered for exactly that moment: reads survive lockout, denials explain themselves, and the audit trail proves its own integrity.",
})}

    <section aria-labelledby="h-surface">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>The operator surface</p>
          <h2 id="h-surface" data-reveal>Observability that survives the bad day</h2>
          <p class="lede" data-reveal>Observability GETs are deliberately posture-exempt: a read cannot actuate,
          and blocking it would remove fleet visibility exactly when an operator needs it most. Writes on the
          same prefixes stay fully gated.</p>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Fleet posture, live</h3>
            <p>Point-in-time posture per fleet and per node, time-series history, flap detection, and a
            server-sent-events stream of every posture transition — each event carrying its generation number,
            so consumers can detect reordering and staleness.</p>
            ${evRow("src/posture_engine_v2.rs", "src/verifier.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Denials that explain themselves</h3>
            <p>Every denied actuator command mints a verdict id and becomes a signed, human-readable artifact:
            the machine deny-code, an operator-language explanation, the exact recorded inputs, and the audit-chain
            fields — served to the auditor role at <code>GET /verdicts/{id}</code>.</p>
            ${evRow("src/verdicts.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>An audit trail that proves itself</h3>
            <p>Chain verification and export endpoints let an operator — or a regulator — confirm ledger integrity
            on demand, and the shipped off-box stream re-verifies independently with no access to the source
            database.</p>
            ${evRow("src/audit_chain.rs", "src/audit_shipper.rs")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-metrics">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Metrics</p>
          <h2 id="h-metrics" data-reveal>A scrape that survives lockout</h2>
          <p class="lede" data-reveal>The Prometheus endpoint is posture-exempt so monitoring keeps working while
          the fleet is locked out — the moment dashboards matter most. Fleet-safety series sit alongside rollout
          and credential-lifecycle series.</p>
        </div>
        <div class="table-wrap" data-reveal>
          <table class="tbl">
            <caption class="visually-hidden">Selected Prometheus series</caption>
            <thead><tr><th scope="col">Series</th><th scope="col">What it tells an operator</th></tr></thead>
            <tbody>
              <tr><td class="mono">kirra_ota_campaigns_total{state}</td><td>Rollout campaigns by lifecycle state — a halted campaign is visible fleet-wide</td></tr>
              <tr><td class="mono">kirra_ota_campaign_rollout_percent</td><td>Staged rollout progress per campaign</td></tr>
              <tr><td class="mono">kirra_ota_campaign_applied_nodes</td><td>Adoption numerator — which nodes actually run the artifact</td></tr>
              <tr><td class="mono">kirra_cert_principals{state}</td><td>mTLS credential census: active, revoked, expired, expiring soon</td></tr>
            </tbody>
          </table>
        </div>
        ${evRow("src/metrics.rs", "src/health.rs")}
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-consoles">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Operator tooling</p>
          <h2 id="h-consoles" data-reveal>Three consoles, one honesty rule</h2>
          <p class="lede" data-reveal>Operator UIs never overstate what happened. The air-gapped console renders a
          clearance grant as “GRANT RECORDED — DELIVERY PENDING” rather than implying a vehicle was released —
          the fail-closed philosophy applied to pixels.</p>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Fleet console</h3>
            <p>The full operator application: posture trends, envelope plots, fleet topology and position maps,
            version-adoption views, incident replay maps, and audit evidence browsing.</p>
            ${evRow("console/", "docs/CONSOLE_RUNBOOK.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Air-gapped console</h3>
            <p>A single self-contained HTML file — inline styles and scripts, no CDN, no build — for secured
            environments where the recovery surface must work with nothing but a browser.</p>
            ${evRow("static/console.html")}
          </div>
          <div class="card" data-reveal>
            <h3>Ops dashboard</h3>
            <p>A lightweight dashboard shipped alongside the verifier in Docker Compose, with its own isolated
            request pool so an API flood can't starve the operator's view — and vice versa.</p>
            ${evRow("dashboard/", "docker-compose.yml")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-drift">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Health &amp; drift</p>
          <h2 id="h-drift" data-reveal>The quiet failures, made loud</h2>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Config drift detection</h3>
            <p>Every boot commits a SHA-256 digest of the effective configuration to the audit chain, and unknown
            <code>KIRRA_*</code> variables warn at startup — a typo'd setting that silently isn't taking effect
            becomes visible instead of latent.</p>
            ${evRow("src/env_config.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Credential lifecycle</h3>
            <p>An hourly census warns on expiring and lapsed mTLS principals with hash-chained audit entries —
            pure observability, because the auth path already fail-closes expiry on its own.</p>
            ${evRow("src/cert_expiry_monitor.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Liveness watchdogs</h3>
            <p>Telemetry silence, stale posture caches, and silent redundancy channels all surface as posture
            changes within their sweep budgets — absence of data is itself a monitored signal.</p>
            ${evRow("src/telemetry_watchdog.rs")}
          </div>
        </div>
      </div>
    </section>

${ctaBand("See the whole system's state in one place.", "Bring up the verifier and dashboard with Docker Compose, watch the posture stream, and pull a verdict explanation yourself.")}
`;
