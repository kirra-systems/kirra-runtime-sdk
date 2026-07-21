import { SITE, ev, evRow, pageHero } from "../template.mjs";

export const meta = {
  slug: "contact",
  title: "Contact",
  desc: "Talk to Kirra Systems: commercial and partnership inquiries, technical evaluation and due diligence, coordinated security disclosure, and open-source collaboration.",
};

export const body = `
${pageHero({
  eyebrow: "Contact",
  title: "Talk to the people<br>who wrote the code.",
  lede: "Kirra Systems doesn't route you through a sales layer — inquiries go to the engineering that builds the governor. Pick the door that matches your question; each one tells you exactly what to expect.",
})}

    <section aria-labelledby="h-doors">
      <div class="container">
        <h2 id="h-doors" class="visually-hidden">Contact channels</h2>
        <div class="grid grid--2">
          <div class="card" data-reveal>
            <h3>Commercial &amp; partnerships</h3>
            <p>Deploying Kirra in a vehicle program, robotics fleet, or industrial platform — or exploring a
            strategic partnership or platform integration. Tell us your machine class and what proposes commands
            to it; we'll tell you honestly whether the governor fits today or belongs on the roadmap.</p>
            <a class="btn btn--primary" style="margin-top:18px" href="mailto:justin.looney@kirrasystems.com?subject=Commercial%20inquiry%20%E2%80%94%20Kirra">justin.looney@kirrasystems.com</a>
          </div>
          <div class="card" data-reveal>
            <h3>Technical evaluation &amp; due diligence</h3>
            <p>Evaluating the platform seriously? Start where a diligence team should: the repository, the 27
            blocking CI lanes, and the safety-case evidence bundle attached to every tagged release. Then bring
            us your hardest questions — we prefer evaluators who arrive skeptical.</p>
            <div class="hero__ctas" style="margin-top:18px">
              <a class="btn btn--ghost" href="${SITE.repo}" target="_blank" rel="noopener">The repository ↗</a>
              <a class="btn btn--ghost" href="mailto:justin.looney@kirrasystems.com?subject=Technical%20evaluation%20%E2%80%94%20Kirra">Ask us directly</a>
            </div>
            <p class="evidence-note" style="margin-top:16px">Or touch it now — two live demos, no install:
            <a class="evidence" href="playground.html">the Verdict Playground</a> runs the real frozen checker in your
            browser; <a class="evidence" href="/console/">the operator console demo</a> runs the full Next.js console
            on a simulated fleet.</p>
            ${evRow(".github/workflows/release.yml", "ci/build_safety_case.py")}
          </div>
          <div class="card" data-reveal>
            <h3>Security disclosure</h3>
            <p>Found a vulnerability — or got an unsafe command past the checker? Coordinated disclosure with a
            documented 48-hour first response. If you can break the envelope, we want the writeup more than
            you do.</p>
            <a class="btn btn--ghost" style="margin-top:18px" href="${SITE.repo}/blob/main/SECURITY.md" target="_blank" rel="noopener">Read the disclosure policy ↗</a>
            ${evRow("SECURITY.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Open source &amp; community</h3>
            <p>Bug reports, feature discussions, and contributions run through GitHub in the open — the same
            gates apply to everyone, including us. Long-form engineering notes ship on Field Notes.</p>
            <div class="hero__ctas" style="margin-top:18px">
              <a class="btn btn--ghost" href="${SITE.repo}/issues" target="_blank" rel="noopener">Open an issue ↗</a>
              <a class="btn btn--ghost" href="field-notes.html">Field Notes</a>
            </div>
          </div>
        </div>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-expect">
      <div class="container container--narrow" style="text-align:center">
        <p class="eyebrow" data-reveal style="justify-content:center">What to expect</p>
        <h2 id="h-expect" data-reveal>The same honesty you see on this site</h2>
        <p class="lede" data-reveal style="margin:18px auto 0">If a capability is experimental, we'll say so before you
        commit anything to it — the repository labels its spikes and drafts, and so do we. Security reports get a
        first response within 48 hours; everything else gets a direct answer from the people building the system,
        not a qualification funnel.</p>
      </div>
    </section>
`;
