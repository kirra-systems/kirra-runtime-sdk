import { SITE, ev, evRow, pageHero, ctaBand } from "../template.mjs";

export const meta = {
  slug: "documentation",
  title: "Documentation",
  desc: "The Kirra documentation map: architecture decision records, safety artifacts, hardware runbooks, protocol specs, and operator guides — all versioned with the code.",
};

const docLink = (path, title, desc) => `
          <a class="card" href="${SITE.repo}/blob/main/${path}" target="_blank" rel="noopener" data-reveal>
            <h3>${title}</h3>
            <p>${desc}</p>
            <span class="card__more mono" style="font-size:0.78rem">${path} <span aria-hidden="true">↗</span></span>
          </a>`;

export const body = `
${pageHero({
  eyebrow: "Documentation",
  title: "Versioned with the code.<br>Honest like the code.",
  lede: "Kirra's documentation is not a wiki that drifts — it lives in the repository, gets reviewed like code, and carries explicit status labels. Start anywhere below; every link lands on the source of truth.",
})}

    <section aria-labelledby="h-start-docs">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Start here</p>
          <h2 id="h-start-docs" data-reveal>The core reading path</h2>
        </div>
        <div class="grid grid--3">
          ${docLink("README.md", "README", "The system thesis: doers propose, Kirra bounds — with the component map and safety-goal table.")}
          ${docLink("CLAUDE.md", "Engineering context", "The deep operational reference: every module, invariant, constant, and route — kept current because the tooling depends on it.")}
          ${docLink("INSTALL.md", "Install guide", "From cargo test to a running governed fleet, on Linux, Docker, Kubernetes, or a Jetson.")}
          ${docLink("docs/adr", "36 decision records", "Every consequential choice — transports, postures, deployment platform, planner seams — argued and numbered.")}
          ${docLink("docs/safety", "Safety artifacts (65)", "HARA, SOTIF, TARA, UL 4600 GSN, safe-state spec, WCET methodology — each labeled with its real maturity.")}
          ${docLink("docs/hardware", "Hardware runbooks", "ROSMASTER R2 bring-up, e-stop spec, closed-loop tuning, golden builds — the record of putting this on metal.")}
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-guides">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Deep dives</p>
          <h2 id="h-guides" data-reveal>Specifications &amp; guides</h2>
        </div>
        <div class="grid grid--3">
          ${docLink("docs/REPLAY_INCIDENT_RECONSTRUCTION.md", "Incident replay", "How bit-identical verdict reconstruction works, and what it refuses to guess about.")}
          ${docLink("docs/CONTRACT_PROFILES.md", "Vehicle class profiles", "The normative envelope numbers for courier, delivery-AV, and robotaxi classes.")}
          ${docLink("docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md", "Partition boundary spec", "The frozen contract layout and the 7-step seqlock trust chain across the hypervisor.")}
          ${docLink("docs/ota", "OTA & Uptane", "Dual-slot installs, rollout campaigns, Uptane roles, and the Orin A/B drills.")}
          ${docLink("docs/llm_integration_guide.md", "LLM integration", "How to put a language model in the loop without ever putting it in charge.")}
          ${docLink("docs/CONSOLE_RUNBOOK.md", "Operator console", "Running the fleet console, clearance flows, and the air-gapped fallback console.")}
        </div>
        <p class="evidence-note" style="margin-top:24px">API reference: <code>cargo doc --open</code> builds the full rustdoc locally — CI enforces
        deny-warnings on it, so it's always current. ${ev(".github/workflows/ci.yml:323", "docs-and-sdk lane")}</p>
      </div>
    </section>

${ctaBand("If it isn't written down, it didn't happen.", "Decision records, drafts, and gap reports included — documentation that admits what it doesn't know yet.")}
`;
