import { SITE, ev, evRow, pageHero, ctaBand } from "../template.mjs";

export const meta = {
  slug: "company",
  title: "Company",
  desc: "Kirra Systems is a safety-governance company based in Titusville, Florida, founded by Justin Looney — a career grounded in hardware verification, network security, military communications, and real safety-critical control.",
  jsonld: {
    "@context": "https://schema.org",
    "@type": "Organization",
    name: "Kirra Systems",
    url: "https://kirrasystems.com",
    logo: "https://kirrasystems.com/assets/kirra-logo.png",
    sameAs: ["https://github.com/kirra-systems/kirra-runtime-sdk"],
    address: {
      "@type": "PostalAddress",
      addressLocality: "Titusville",
      addressRegion: "FL",
      addressCountry: "US",
    },
    founder: {
      "@type": "Person",
      name: "Justin Looney",
      jobTitle: "Founder",
    },
  },
};

export const body = `
${pageHero({
  eyebrow: "Company",
  title: "Built by people who have<br>been downstream of a bad command.",
  lede: "Kirra Systems builds the fail-closed layer between AI intent and physical action. It is grounded in a simple conviction, earned across enterprise hardware verification, federal network security, military communications, and the operation of real public-safety infrastructure: complex systems must fail closed, in the real world, with real consequences downstream.",
})}

    <section aria-labelledby="h-founder">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>The founder</p>
          <h2 id="h-founder" data-reveal>Justin Looney</h2>
        </div>
        <div class="grid grid--2" style="align-items:start;gap:48px">
          <div data-reveal>
            <div style="display:flex;align-items:center;gap:18px;margin-bottom:22px">
              <div aria-hidden="true" style="flex:none;width:72px;height:72px;border-radius:16px;display:grid;place-items:center;background:var(--accent-soft);border:1px solid var(--accent-line);font-family:var(--font-display);font-weight:700;font-size:1.5rem;color:var(--accent);letter-spacing:0.02em">JL</div>
              <div>
                <p class="mono" style="font-size:0.8rem;color:var(--ink)">Founder</p>
                <p class="mono dim" style="font-size:0.78rem">Titusville, Florida</p>
              </div>
            </div>
            <p class="lede" style="font-size:1.08rem">Justin Looney founded Kirra Systems and architected its core —
            the planner-independent checker, the fail-closed actuation model, and the cryptographic attestation that
            makes every safety decision auditable. His career has centered on the exact disciplines a safety
            governor demands: rigorous hardware and systems verification, network security, and safety-critical
            infrastructure.</p>
          </div>
          <div style="display:grid;gap:16px">
            <div class="card" data-reveal>
              <h3>Enterprise hardware verification</h3>
              <p>Hardware and Diagnostics Test Engineer and technical lead on IBM's <strong>xSeries</strong>
              enterprise server line; test lead at <strong>Emulex</strong> for CNA/FCoE — Converged Network
              Adapter / Fibre Channel over Ethernet, high-performance data-center networking hardware. This is
              work defined by one discipline: proving complex systems behave correctly and fail predictably under
              fault — the discipline at the heart of Kirra.</p>
            </div>
            <div class="card" data-reveal>
              <h3>Federal network &amp; security programs</h3>
              <p>U.S. EPA Technical Point of Contact (GS-2210-13) on an enterprise network and security task order —
              technical authority over enterprise wireless design and deployment, a data-center network
              modernization from Cisco Catalyst to Nexus, and a WAN security upgrade to <strong>MTIPS</strong>, the
              federal Managed Trusted Internet Protocol Service.</p>
            </div>
            <div class="card" data-reveal>
              <h3>Career military communications</h3>
              <p>Retired U.S. Army Signal Corps <strong>Master Sergeant (E-8)</strong>, a Telecommunications
              Operations Chief (25W) — two decades leading mission-critical military communications, where systems
              fail safely or not at all.</p>
            </div>
            <div class="card" data-reveal>
              <h3>Real safety-critical control</h3>
              <p>Licensed operation of public water-treatment infrastructure — direct, hands-on responsibility for
              the automated control systems that govern a real public utility, where a wrong command has physical
              consequences and auditability is not optional.</p>
            </div>
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-shows">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Experience, in the codebase</p>
          <h2 id="h-shows" data-reveal>Where that background shows up</h2>
          <p class="lede" data-reveal>The founder's story isn't a résumé beside the product — it's visible in the
          engineering choices. Each discipline maps to a mechanism you can read.</p>
        </div>
        <div class="grid grid--2">
          <div class="card" data-reveal>
            <h3>Verification instinct → proofs, not promises</h3>
            <p>A career spent proving hardware fails predictably is why the checker core is machine-checked over all
            inputs, kill-tested for durability, and gated by 27 CI lanes — not asserted in a datasheet.</p>
            ${evRow("verification/kani/", "tests/audit_chain_prefix_on_kill.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Network security → attestation &amp; audit</h3>
            <p>Federal network-security program leadership is why trust is proven per node with Ed25519
            challenge-response, and why every decision lands in a hash-chained ledger — a governor you audit, not
            one you trust.</p>
            ${evRow("crates/kirra-safety-authority/src/attestation.rs", "src/audit_chain.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Military comms → fail safe or not at all</h3>
            <p>Two decades where systems fail safely or not at all is the origin of the fail-closed default:
            unproven, stale, malformed, or unclassified — the answer is deny.</p>
            ${evRow("src/posture_cache.rs", "docs/safety/SAFE_STATE_SPECIFICATION.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Water-plant control → deny on physics</h3>
            <p>Hands-on responsibility for utility control systems is why Kirra's industrial adapters decode the
            actual setpoint on the wire and deny on magnitude regardless of credentials — the exact class of the
            2021 Oldsmar water-plant incident.</p>
            ${evRow("crates/kirra-industrial", "docs/protocol_adapters.md")}
          </div>
        </div>
        <p class="evidence-note" style="margin-top:20px">The founder biography above is provided by Kirra Systems; the
        repository links show the mechanisms each discipline informed.</p>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-where">
      <div class="container container--narrow" style="text-align:center">
        <p class="eyebrow" data-reveal style="justify-content:center">Where we are</p>
        <h2 id="h-where" data-reveal>Titusville, Florida</h2>
        <p class="lede" data-reveal style="margin:18px auto 0">On Florida's Space Coast — a region built around systems
        that cannot be allowed to fail unsafely. Kirra Systems develops entirely in the open; the fastest way to
        know the company is to read its repository.</p>
        <div class="hero__ctas" style="justify-content:center" data-reveal>
          <a class="btn btn--primary" href="contact.html">Get in touch <span class="arrow" aria-hidden="true">→</span></a>
          <a class="btn btn--ghost" href="careers.html">How we work</a>
          <a class="btn btn--ghost" href="${SITE.repo}" target="_blank" rel="noopener">The repository ↗</a>
        </div>
      </div>
    </section>
`;
