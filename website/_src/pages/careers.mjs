import { SITE, ev, evRow, pageHero } from "../template.mjs";

export const meta = {
  slug: "careers",
  title: "Careers",
  desc: "How engineering works at Kirra Systems: fail-closed by default, evidence over assertion, proofs where they matter — and how to get involved through the open repository.",
};

export const body = `
${pageHero({
  eyebrow: "Careers",
  title: "Work where “trust me”<br>isn't an argument.",
  lede: "Kirra doesn't maintain a list of open roles on this site. What we can show you — verifiably — is how the engineering actually works here, because all of it is public. If that culture reads like home, the door is the repository.",
})}

    <section aria-labelledby="h-culture">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>How we work</p>
          <h2 id="h-culture" data-reveal>The culture is in the commit history</h2>
        </div>
        <div class="grid grid--2">
          <div class="card" data-reveal>
            <h3>Deny by default — in code review too</h3>
            <p>The security invariants exist because people tried to violate them and were reverted. A change that
            weakens attestation, bypasses the admin gate, or removes the Unknown-command denial is rejected outright,
            no matter how convenient. Guardrails are scripted, not vibes.</p>
            ${evRow("SECURITY.md", "ci/check_quality_guardrails.py")}
          </div>
          <div class="card" data-reveal>
            <h3>Evidence over assertion</h3>
            <p>“It works” means a test, a proof, a drill, or a measurement — with its scope stated. Draft artifacts
            say Draft. Host benchmarks say host. The README's certification claim includes the sentence most
            companies leave out.</p>
            ${evRow("README.md:117")}
          </div>
          <div class="card" data-reveal>
            <h3>Proofs where they pay</h3>
            <p>We reach for Kani, Loom, and Miri when the property deserves it, and say plainly where the proofs end
            (the trig path is excluded, and documented as such). Formal methods as a tool, not a religion.</p>
            ${evRow("verification/kani/", "crates/kirra-loom-models/src/lib.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Real hardware, real humility</h3>
            <p>First governed motion on a physical robot happened in July 2026 — and the hardware findings doc
            records exactly what doesn't work yet (steering + drive together awaits a vendor image). We write down
            the failures with the same care as the wins.</p>
            ${evRow("robot/install/README.md", "docs/hardware/HARDWARE_FINDINGS_R2X3.md")}
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-join">
      <div class="container container--narrow" style="text-align:center">
        <p class="eyebrow" data-reveal style="justify-content:center">Getting involved</p>
        <h2 id="h-join" data-reveal>The interview is a pull request</h2>
        <p class="lede" data-reveal style="margin:18px auto 0">The fastest way to work with us is to work with us: pick an
        issue, survive the 27 CI lanes, and defend your change against reviewers who will try to break it. Security
        researchers: the disclosure policy in SECURITY.md is the front door, and we respond within 48 hours.</p>
        <div class="hero__ctas" style="justify-content:center" data-reveal>
          <a class="btn btn--primary" href="${SITE.repo}/issues" target="_blank" rel="noopener">Open issues <span class="arrow" aria-hidden="true">→</span></a>
          <a class="btn btn--ghost" href="open-source.html">How contributions are gated</a>
        </div>
      </div>
    </section>
`;
