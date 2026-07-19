# kirrasystems.com

The Kirra Systems website — a build-free static site served by GitHub Pages
(`.github/workflows/deploy-pages.yml` uploads this directory verbatim on pushes to
`main`; `CNAME` pins the custom domain).

**Every engineering claim on the site links to its evidence in this repository.**
See `REDESIGN.md` for the full audit, strategy, and design-decision record.

## Layout

```
website/
├── _src/                 # authoring sources
│   ├── template.mjs      # shared head/nav/footer + helpers (evidence chips, pills)
│   ├── pages/*.mjs       # one module per page: { meta, body }
│   └── generate.mjs      # writes the static HTML + sitemap + robots + 404
├── css/kirra.css         # design system (dark/light themes, AA+ contrast)
├── js/kirra.js           # behaviors — zero dependencies, progressive enhancement
├── js/hero.js            # WebGL hero field (hand-written shader, no libraries)
├── fonts/                # self-hosted variable fonts (latin subset, ~102 KB)
├── assets/               # logo, generated og.png
├── *.html                # the committed, generated output GitHub Pages serves
├── posts.json            # Field Notes cache, baked by scripts/fetch-substack-posts.mjs
└── sitemap.xml, robots.txt, favicon.svg, 404.html, CNAME
```

## Editing a page

1. Edit the page module in `_src/pages/` (or `_src/template.mjs` for shared chrome).
2. Regenerate: `node website/_src/generate.mjs`
3. Commit **both** the source and the regenerated HTML.

The deploy has no build step on purpose — what's committed is what ships, the site has
no external runtime dependencies (no CDNs, no font services), and rollback is a plain
`git revert`.

## Rules of the site

- No claim without a repository citation (the `ev()` helper renders evidence chips).
- Experimental things say so: `SPIKE`, `DRAFT`, `HARDWARE-GATED` pills mirror the
  repository's own labels.
- Host-measured timing is always labeled indicative — certified WCET language is
  reserved until QNX-target numbers exist.
- Never state or imply achieved certification. The line of record is `README.md:117`.

## Field Notes feed

`js/kirra.js` renders the latest Substack posts from `posts.json` (baked at deploy time),
falling back to rss2json, then allorigins, then a static subscribe CTA — preserved from
the previous site because Substack 403s datacenter IPs.
