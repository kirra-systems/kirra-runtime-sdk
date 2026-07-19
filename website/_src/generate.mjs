// ============================================================
// KIRRA site generator — authoring-time only.
//   node website/_src/generate.mjs
// Reads page modules from _src/pages/, writes static HTML into
// website/, plus sitemap.xml, robots.txt and 404.html. The
// committed output is what GitHub Pages serves — no deploy build.
// ============================================================
import { readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { page, SITE } from "./template.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "..");
const pagesDir = join(here, "pages");

const slugs = [];
for (const f of readdirSync(pagesDir).filter((f) => f.endsWith(".mjs")).sort()) {
  const mod = await import(pathToFileURL(join(pagesDir, f)).href);
  const html = page(mod.meta, mod.body);
  const file = `${mod.meta.slug}.html`;
  writeFileSync(join(out, file), html);
  slugs.push(mod.meta.slug);
  console.log(`✓ ${file} (${(html.length / 1024).toFixed(1)} KB)`);
}

// sitemap.xml
const today = new Date().toISOString().slice(0, 10);
const urls = slugs
  .map((s) => (s === "index" ? "" : `${s}.html`))
  .map((p) => `  <url><loc>${SITE.origin}/${p}</loc><lastmod>${today}</lastmod></url>`)
  .join("\n");
writeFileSync(join(out, "sitemap.xml"),
`<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${urls}
</urlset>
`);
console.log("✓ sitemap.xml");

// robots.txt
writeFileSync(join(out, "robots.txt"),
`User-agent: *
Allow: /
Sitemap: ${SITE.origin}/sitemap.xml
`);
console.log("✓ robots.txt");

// 404
writeFileSync(join(out, "404.html"), page(
  {
    slug: "404",
    title: "Page not found",
    desc: "This page does not exist. Fail-closed: we deny by default.",
    subnav: false,
  },
  `
    <section class="page-hero" style="min-height:70svh;display:grid;align-content:center">
      <div class="container" style="text-align:center">
        <p class="eyebrow" style="justify-content:center">HTTP 404 · VERDICT: DENY</p>
        <h1>Unknown route.<br><span style="color:var(--accent)">Denied by default.</span></h1>
        <p class="lede" style="margin:20px auto 0">Unclassified requests are refused in every posture — that
        includes this URL. The page you're looking for doesn't exist or has moved.</p>
        <div class="hero__ctas" style="justify-content:center">
          <a class="btn btn--primary" href="index.html">Return home <span class="arrow" aria-hidden="true">→</span></a>
          <a class="btn btn--ghost" href="documentation.html">Documentation</a>
        </div>
      </div>
    </section>`
));
console.log("✓ 404.html");
