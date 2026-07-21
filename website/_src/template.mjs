// ============================================================
// KIRRA site — shared page template + helpers.
// Authoring-time only: `node website/_src/generate.mjs` writes
// static HTML into website/. The deployed site has no build step.
// ============================================================

export const SITE = {
  origin: "https://kirrasystems.com",
  name: "Kirra Systems",
  repo: "https://github.com/kirra-systems/kirra-runtime-sdk",
  version: "v1.1.2",
};

/** Evidence chip linking to the exact file (and line) in the repository. */
export const ev = (path, label) => {
  const [file, line] = path.split(":");
  const url = `${SITE.repo}/blob/main/${file}${line ? `#L${line}` : ""}`;
  return `<a class="evidence" href="${url}" target="_blank" rel="noopener">${label || path}</a>`;
};
export const evRow = (...paths) =>
  `<p class="evidence-row">${paths.map((p) => ev(p)).join("")}</p>`;

export const pill = (kind, label) => `<span class="pill pill--${kind}">${label}</span>`;
export const LIVE = pill("live", "LIVE");
export const GATED = pill("gated", "HARDWARE-GATED");
export const DRAFT = pill("gated", "DRAFT");
export const SPIKE = pill("rnd", "SPIKE");
export const PLANNED = pill("rnd", "PLANNED");

/** Adaptive constellation-K mark (theme-aware, matches the PNG logo). */
export const MARK = `<svg viewBox="0 0 32 32" fill="none" aria-hidden="true">
  <g stroke="currentColor" stroke-width="1.4" opacity="0.85">
    <path d="M7 4v24M7 17 25 4M11.5 13.7 25 28"/>
    <path d="M7 4l7 6.5M7 28l4.5-14.3M14 10.5 7 17" opacity="0.35"/>
  </g>
  <g fill="var(--accent, #3fd9c9)">
    <circle cx="7" cy="4" r="2"/><circle cx="7" cy="17" r="2.4"/><circle cx="7" cy="28" r="2"/>
    <circle cx="25" cy="4" r="2"/><circle cx="25" cy="28" r="2"/>
    <circle cx="14" cy="10.5" r="1.5"/><circle cx="11.5" cy="13.7" r="1.5"/>
  </g>
</svg>`;

const NAV_LINKS = [
  ["solutions.html", "Solutions"],
  ["vision.html", "Vision"],
  ["architecture.html", "Architecture"],
  ["safety.html", "Safety"],
  ["certification.html", "Certification"],
  ["field-notes.html", "Field Notes"],
  ["contact.html", "Contact"],
];

const SUBNAV = [
  ["solutions.html", "solutions"],
  ["industrial.html", "industrial / OT"],
  ["vision.html", "vision"],
  ["architecture.html", "architecture"],
  ["runtime.html", "runtime"],
  ["diagnostics.html", "diagnostics"],
  ["safety.html", "safety"],
  ["planning.html", "planning"],
  ["perception.html", "perception"],
  ["prediction.html", "prediction"],
  ["determinism.html", "determinism"],
  ["performance.html", "performance"],
  ["security.html", "security"],
  ["certification.html", "certification"],
  ["benchmarks.html", "benchmarks"],
  ["playground.html", "playground"],
  ["open-source.html", "open source"],
  ["documentation.html", "docs"],
  ["research.html", "research"],
];

const FOOTER = `
<footer class="footer">
  <div class="container">
    <div class="footer__grid">
      <div class="footer__brand">
        <a class="nav__brand" href="index.html">${MARK}<span>KIRRA</span></a>
        <p>The fail-closed runtime safety governor for AI-driven machines. The doer proposes; the checker bounds it.</p>
      </div>
      <nav aria-label="Platform">
        <h3>Platform</h3>
        <ul>
          <li><a href="solutions.html">Solutions</a></li>
          <li><a href="architecture.html">Architecture</a></li>
          <li><a href="runtime.html">Runtime</a></li>
          <li><a href="diagnostics.html">Diagnostics</a></li>
          <li><a href="determinism.html">Determinism</a></li>
          <li><a href="performance.html">Performance</a></li>
          <li><a href="security.html">Security</a></li>
        </ul>
      </nav>
      <nav aria-label="Safety">
        <h3>Safety</h3>
        <ul>
          <li><a href="safety.html">Safety model</a></li>
          <li><a href="planning.html">Planning</a></li>
          <li><a href="perception.html">Perception</a></li>
          <li><a href="prediction.html">Prediction</a></li>
          <li><a href="certification.html">Certification</a></li>
        </ul>
      </nav>
      <nav aria-label="Evidence">
        <h3>Evidence</h3>
        <ul>
          <li><a href="playground.html">Verdict Playground</a></li>
          <li><a href="benchmarks.html">Benchmarks</a></li>
          <li><a href="open-source.html">Open source</a></li>
          <li><a href="documentation.html">Documentation</a></li>
          <li><a href="research.html">Research</a></li>
        </ul>
      </nav>
      <nav aria-label="Company">
        <h3>Company</h3>
        <ul>
          <li><a href="company.html">About</a></li>
          <li><a href="vision.html">Vision</a></li>
          <li><a href="contact.html">Contact</a></li>
          <li><a href="field-notes.html">Field Notes</a></li>
          <li><a href="careers.html">Careers</a></li>
          <li><a href="${SITE.repo}" target="_blank" rel="noopener">GitHub ↗</a></li>
          <li><a href="${SITE.repo}/blob/main/SECURITY.md" target="_blank" rel="noopener">Security policy ↗</a></li>
        </ul>
      </nav>
    </div>
    <div class="footer__meta">
      <span>© <span data-year>2026</span> Kirra Systems</span>
      <span class="mono">kirra-verifier ${SITE.version} · Rust 2021 · MSRV 1.88</span>
      <span>Every claim on this site links to its source in the repository.</span>
      <a class="mono" data-lighthouse hidden href="${SITE.repo}/blob/main/scripts/lighthouse-bake.mjs" target="_blank" rel="noopener" style="text-decoration:none"></a>
    </div>
  </div>
</footer>`;

/**
 * meta: { slug, title, desc, section?, subnav?: bool, extraHead?, jsonld?, heroExtra? }
 * body: string of <main> inner HTML
 */
export function page(meta, body) {
  const path = meta.slug === "index" ? "" : `${meta.slug}.html`;
  const canonical = `${SITE.origin}/${path}`;
  const title = meta.slug === "index"
    ? "Kirra Systems — Fail-Closed Safety Governor for Autonomous Machines"
    : `${meta.title} — Kirra Systems`;
  const navLinks = NAV_LINKS.map(([href, label]) =>
    `<a href="${href}"${meta.slug + ".html" === href ? ' aria-current="page"' : ""}>${label}</a>`).join("\n        ");
  const subnav = meta.subnav === false ? "" : `
  <nav class="subnav" aria-label="Platform sections">
    <div class="subnav__inner">
      ${SUBNAV.map(([href, label]) =>
        `<a href="${href}"${meta.slug + ".html" === href ? ' aria-current="page"' : ""}>${label}</a>`).join("\n      ")}
    </div>
  </nav>`;
  const jsonld = meta.jsonld || {
    "@context": "https://schema.org",
    "@type": "WebPage",
    name: title,
    description: meta.desc,
    url: canonical,
    isPartOf: { "@type": "WebSite", name: SITE.name, url: SITE.origin },
  };

  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>${title}</title>
  <meta name="description" content="${meta.desc}">
  <link rel="canonical" href="${canonical}">
  <meta property="og:type" content="website">
  <meta property="og:site_name" content="Kirra Systems">
  <meta property="og:title" content="${title}">
  <meta property="og:description" content="${meta.desc}">
  <meta property="og:url" content="${canonical}">
  <meta property="og:image" content="${SITE.origin}/assets/og.png">
  <meta property="og:image:width" content="1200">
  <meta property="og:image:height" content="630">
  <meta name="twitter:card" content="summary_large_image">
  <meta name="twitter:title" content="${title}">
  <meta name="twitter:description" content="${meta.desc}">
  <meta name="twitter:image" content="${SITE.origin}/assets/og.png">
  <meta name="theme-color" content="#05070d">
  <link rel="icon" href="favicon.svg" type="image/svg+xml">
  <link rel="icon" href="assets/kirra-logo.png" type="image/png">
  <link rel="apple-touch-icon" href="assets/kirra-logo.png">
  <link rel="preload" href="fonts/space-grotesk-var-latin.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="preload" href="fonts/inter-var-latin.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="stylesheet" href="css/kirra.css">
  <script>/* theme init before first paint — prevents flash */
(function(){try{var t=localStorage.getItem("kirra-theme");if(!t)t=matchMedia("(prefers-color-scheme: light)").matches?"light":"dark";document.documentElement.dataset.theme=t;}catch(e){document.documentElement.dataset.theme="dark";}})();</script>
  <script type="application/ld+json">${JSON.stringify(jsonld)}</script>${meta.extraHead || ""}
</head>
<body>${meta.slug === "index" ? `
  <div class="preloader" id="preloader" hidden aria-hidden="true">
    <div class="preloader__inner">
      <div class="preloader__mark">${MARK}<span class="preloader__ring"></span></div>
      <div class="preloader__bar"><span id="preloaderFill"></span></div>
      <div class="preloader__meta">
        <span id="preloaderStatus">VERIFYING FLEET POSTURE</span>
        <span id="preloaderPct">0%</span>
      </div>
    </div>
  </div>
  <script>/* arm the preloader only when JS runs, motion is allowed, and it hasn't shown this session */
(function(){try{
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  if (sessionStorage.getItem("kirra-preloaded")) return;
  document.getElementById("preloader").hidden = false;
  document.documentElement.classList.add("is-preloading");
}catch(e){}})();</script>` : ""}
  <a class="skip-link" href="#main">Skip to content</a>

  <header class="nav" id="nav">
    <div class="nav__inner">
      <a href="index.html" class="nav__brand" aria-label="Kirra Systems home">${MARK}<span>KIRRA</span></a>
      <nav class="nav__links" id="navLinks" aria-label="Primary">
        ${navLinks}
      </nav>
      <button class="theme-toggle" id="themeToggle" aria-label="Toggle color theme">
        <svg class="icon-moon" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8Z"/></svg>
        <svg class="icon-sun" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M4.9 4.9l1.4 1.4m11.4 11.4 1.4 1.4M2 12h2m16 0h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>
      </button>
      <a href="${SITE.repo}" class="nav__cta" target="_blank" rel="noopener" aria-label="Kirra on GitHub (opens in new tab)"><span class="label">GitHub</span><span aria-hidden="true">↗</span></a>
      <button class="nav__burger" id="navBurger" aria-label="Open menu" aria-expanded="false" aria-controls="navLinks">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M3 6h18M3 12h18M3 18h18"/></svg>
      </button>
    </div>
  </header>${subnav}

  <main id="main">
${body}
  </main>
${FOOTER}
  <script src="js/kirra.js" defer></script>${meta.slug === "index" ? '\n  <script src="js/hero.js" defer></script>' : ""}
</body>
</html>`;
}

/** Standard sub-page hero. */
export const pageHero = ({ eyebrow, title, lede, extra = "" }) => `
    <section class="page-hero">
      <div class="container">
        <p class="eyebrow" data-reveal>${eyebrow}</p>
        <h1 data-reveal>${title}</h1>
        <p class="lede" data-reveal>${lede}</p>${extra}
      </div>
    </section>`;

/** Closing CTA band shared by sub-pages. */
export const ctaBand = (title, sub) => `
    <section class="cta-band">
      <div class="container">
        <p class="eyebrow" style="justify-content:center">READ THE SOURCE</p>
        <h2>${title}</h2>
        <p class="lede">${sub}</p>
        <div class="hero__ctas">
          <a class="btn btn--primary" href="${SITE.repo}" target="_blank" rel="noopener">View the repository <span class="arrow" aria-hidden="true">→</span></a>
          <a class="btn btn--ghost" href="documentation.html">Browse the documentation</a>
        </div>
      </div>
    </section>`;
