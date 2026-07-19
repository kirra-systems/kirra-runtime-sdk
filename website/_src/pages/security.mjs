import { ev, evRow, pageHero, ctaBand, LIVE, GATED } from "../template.mjs";

export const meta = {
  slug: "security",
  title: "Security",
  desc: "Kirra's security architecture: per-node Ed25519 attestation with TPM quotes, scoped RBAC, constant-time token comparison, SHA-256 hash-chained audit with WORM shipping, signed federation, and a pinned supply chain.",
};

const attestSvg = `
<svg viewBox="0 0 960 240" role="img" aria-labelledby="atT atD">
  <title id="atT">Attestation challenge–response</title>
  <desc id="atD">The verifier issues a random nonce with a 30-second TTL. The node signs node id plus nonce with its attestation key; optionally a TPM quote binds PCR16 to the same nonce. The verifier checks the signature against the registered public key and the PCR digest, then marks the node trusted. Any failure rejects.</desc>
  <g>
    <rect class="dg-box dg-box--accent" x="20" y="70" width="230" height="100" rx="12"/>
    <text class="dg-label" x="40" y="100">Verifier</text>
    <text class="dg-mono" x="40" y="124">nonce · 30 s TTL · volatile</text>
    <text class="dg-sub" x="40" y="146">never persisted</text>
  </g>
  <g>
    <rect class="dg-box" x="700" y="70" width="240" height="100" rx="12"/>
    <text class="dg-label" x="720" y="100">Node</text>
    <text class="dg-mono" x="720" y="124">Ed25519(node_id ‖ nonce)</text>
    <text class="dg-sub" x="720" y="146">+ TPM2 quote over PCR16 (policy)</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M250 95 L 698 95"/>
  <text class="dg-mono" x="380" y="84">1 · challenge nonce</text>
  <path class="dg-edge dg-edge--flow" d="M698 145 L 250 145"/>
  <text class="dg-mono" x="380" y="168">2 · signed proof (+ quote)</text>
  <text class="dg-mono" x="20" y="216">3 · verify against registered AK · PCR16 digest must match ⇒ Trusted — anything else ⇒ reject, nonce preserved for retry</text>
</svg>`;

export const body = `
${pageHero({
  eyebrow: "Security",
  title: "Identity is proven.<br>Everything else is denied.",
  lede: "Kirra treats its own fleet as a hostile network: every node proves who it is cryptographically, every carrier is untrusted, every privileged route demands a scope, and every decision lands in a ledger that can't be quietly edited.",
})}

    <section aria-labelledby="h-attest">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Node attestation</p>
          <h2 id="h-attest" data-reveal>Challenge–response, rooted toward hardware</h2>
          <p class="lede" data-reveal>Trust is never asserted by an admin — it's proven per node: an Ed25519 signature
          over a fresh 30-second nonce, checked against the node's registered key. Nodes under a
          <code>require_tpm_quote</code> policy must additionally present a TPM2 quote whose PCR16 digest matches
          their registered measured-boot value, bound to the same nonce.</p>
        </div>
        <div class="diagram-frame flow-anim" data-reveal>${attestSvg}</div>
        ${evRow("crates/kirra-safety-authority/src/attestation.rs", "src/tpm_quote.rs", "SECURITY.md")}
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-authz">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Authorization</p>
          <h2 id="h-authz" data-reveal>Scoped roles, constant-time everywhere</h2>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Fail-closed RBAC</h3>
            <p>Four roles — admin, integrator, auditor, operator — with per-scope gates on every privileged route.
            The authorization core is a pure function: no env reads, no store access, no way to accidentally
            default-allow. A missing admin token means 503, never 200.</p>
            ${evRow("src/authz.rs", "docs/safety/PRINCIPAL_TOKENS.md")}
          </div>
          <div class="card" data-reveal>
            <h3>No timing oracles</h3>
            <p>Every security-critical byte comparison goes through <code>constant_time_compare</code> — standard
            equality on secrets is forbidden by invariant. Token plaintexts are never stored; only SHA-256 hashes.</p>
            ${evRow("src/security.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>mTLS with expiry teeth</h3>
            <p>Client certificates are CA-verified at the TLS layer and pinned by SHA-256 fingerprint to a role.
            A cert at or past its expiry fail-closes exactly like a revocation; a half-configured TLS listener
            aborts startup rather than falling back to plaintext.</p>
            ${evRow("docs/safety/TRANSPORT_SECURITY.md")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-audit">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Audit</p>
          <h2 id="h-audit" data-reveal>A memory that can't be edited</h2>
          <p class="lede" data-reveal>Every trust decision, config digest, campaign mutation, and denied command is a
          SHA-256 hash-chained ledger entry. The chain survives SIGKILL mid-append as a valid prefix — proven by a
          power-loss drill in CI — ships off-box to WORM storage, and re-verifies independently with no access to
          the source database.</p>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Explainable denials</h3>
            <p>A denied actuator command mints a verdict id and becomes a signed, human-readable artifact: machine
            deny-code, operator explanation, the recorded inputs, and the chain fields — served to auditors at
            <code>GET /verdicts/{id}</code>.</p>
            ${evRow("src/verdicts.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Federation without trust</h3>
            <p>Cross-controller trust reports are Ed25519-signed, freshness-windowed (5 s), and replay-proof — nonces
            burn atomically with the report commit. Generation ordering reconciles conflicting reports
            deterministically.</p>
            ${evRow("src/federation.rs", "src/federation_reconciliation.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Release tokens</h3>
            <p>The governor signs the digest of the exact validated contract bytes; the actuator verifies “the
            governor approved exactly these bytes” before anything moves. Key provisioning is fail-closed: no
            configured source, no key — a non-production dev key never loads by default.</p>
            ${evRow("crates/kirra-release-token/src/lib.rs", "docs/safety/GOVERNOR_KEY_PROVISIONING.md")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-supply">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Supply chain</p>
          <h2 id="h-supply" data-reveal>Pinned, audited, signed</h2>
        </div>
        <div class="table-wrap" data-reveal>
          <table class="tbl">
            <caption class="visually-hidden">Supply chain controls</caption>
            <thead><tr><th scope="col">Control</th><th scope="col">Mechanism</th><th scope="col">Gate</th></tr></thead>
            <tbody>
              <tr><td>Dependency policy</td><td>cargo-deny: license allowlist, wildcard ban, crates.io-only sources</td><td class="mono">blocking CI lane</td></tr>
              <tr><td>Vulnerability audit</td><td>cargo-audit on every push</td><td class="mono">blocking (pipefail)</td></tr>
              <tr><td>Actions pinning</td><td>every GitHub Action pinned to a 40-hex commit SHA, checked by script</td><td class="mono">blocking CI lane</td></tr>
              <tr><td>Container provenance</td><td>digest-pinned base images; keyless cosign signatures via OIDC</td><td class="mono">docker workflow</td></tr>
              <tr><td>Release integrity</td><td>tag ↔ version ↔ changelog preflight; safety-case evidence bundle attached to releases</td><td class="mono">release workflow</td></tr>
            </tbody>
          </table>
        </div>
        ${evRow("deny.toml", "scripts/pin-actions.sh", ".github/workflows/docker.yml", ".github/workflows/release.yml")}
        <p class="evidence-note">Coordinated disclosure: see <a class="evidence" href="https://github.com/kirra-systems/kirra-runtime-sdk/blob/main/SECURITY.md" target="_blank" rel="noopener">SECURITY.md</a> — 48-hour first response.</p>
      </div>
    </section>

${ctaBand("Attack the design, not the marketing.", "The invariants, the threat analysis, and the audit chain implementation are all public — we prefer adversarial readers.")}
`;
