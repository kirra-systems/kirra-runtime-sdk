import { pageHero } from "../template.mjs";

export const meta = {
  slug: "field-notes",
  title: "Field Notes",
  desc: "Engineering dispatches from the Kirra build: governed hardware bring-ups, proof engineering, fail-closed design, and what it takes to put a safety governor on real machines.",
};

export const body = `
${pageHero({
  eyebrow: "Field Notes",
  title: "Dispatches from the build.",
  lede: "Long-form engineering notes written while the work happens — governed bring-ups on real hardware, proof engineering, incident replay, and the unglamorous discipline of failing closed. Published on Substack; the latest notes load below.",
})}

    <section aria-label="Latest notes">
      <div class="container">
        <div class="notes-grid" data-notes data-notes-max="12" data-reveal></div>
        <div class="card" data-notes-fallback data-reveal style="text-align:center;margin-top:8px">
          <h3>Read and subscribe on Substack</h3>
          <p style="margin:10px auto 0">The feed couldn't be loaded right now — the notes live at
          justinlooney.substack.com and new ones arrive as the work ships.</p>
          <a class="btn btn--primary" style="margin-top:18px" href="https://justinlooney.substack.com" target="_blank" rel="noopener">Open Substack <span class="arrow" aria-hidden="true">↗</span></a>
        </div>
      </div>
    </section>
`;
