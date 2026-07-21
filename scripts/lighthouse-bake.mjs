// Deploy-time Lighthouse measurement → website/lighthouse.json.
// Runs in the Pages deploy workflow (same resilience pattern as
// fetch-substack-posts.mjs): ANY failure exits 0 and leaves the existing
// lighthouse.json untouched, so the deploy never breaks and the site keeps
// showing the last real measurement.
//
// Method: serve website/ locally, run Lighthouse (headless Chrome) on a
// sample of pages, and record the MINIMUM score per category across the
// sample — a worst-page number, not a cherry-picked one. The footer renders
// these values verbatim; if this script has never succeeded, the footer
// simply shows nothing. No score is ever invented.
//
// Local run:   node scripts/lighthouse-bake.mjs
//   (honors CHROME_PATH; on the dev sandbox point it at the Playwright
//    chromium, e.g. /opt/pw-browsers/chromium-1194/chrome-linux/chrome)

import { spawn, spawnSync } from "node:child_process";
import { writeFileSync, readFileSync, rmSync, mkdtempSync, existsSync } from "node:fs";
import { createServer } from "node:http";
import { gzipSync } from "node:zlib";
import { tmpdir } from "node:os";
import { join, extname, normalize } from "node:path";

const PAGES = ["index.html", "playground.html", "solutions.html"];
const PORT = 8823;
const OUT = "website/lighthouse.json";
const CATS = ["performance", "accessibility", "best-practices", "seo"];

// Serve website/ the way GitHub Pages does: gzip for text types, short cache.
// (Measuring through an uncompressed dev server unfairly penalizes LCP.)
const MIME = {
  ".html": "text/html; charset=utf-8", ".css": "text/css", ".js": "application/javascript",
  ".json": "application/json", ".svg": "image/svg+xml", ".xml": "application/xml",
  ".txt": "text/plain", ".png": "image/png", ".woff2": "font/woff2", ".wasm": "application/wasm",
};
const GZIP = new Set([".html", ".css", ".js", ".json", ".svg", ".xml", ".txt", ".wasm"]);

let server;
try {
  server = createServer((req, res) => {
    const path = normalize((req.url || "/").split("?")[0]).replace(/^\/+/, "") || "index.html";
    const file = join("website", path === "" ? "index.html" : path);
    if (!existsSync(file) || file.includes("..")) { res.writeHead(404); res.end(); return; }
    const ext = extname(file);
    const body = readFileSync(file);
    res.setHeader("content-type", MIME[ext] || "application/octet-stream");
    res.setHeader("cache-control", "max-age=600");
    if (GZIP.has(ext) && /gzip/.test(req.headers["accept-encoding"] || "")) {
      res.setHeader("content-encoding", "gzip");
      res.end(gzipSync(body));
    } else res.end(body);
  });
  server.on("error", (e) => {
    console.error("lighthouse bake skipped (server):", e.message);
    process.exit(0);
  });
  server.listen(PORT);
  await new Promise((r) => setTimeout(r, 500));

  const tmp = mkdtempSync(join(tmpdir(), "lh-"));
  const mins = Object.fromEntries(CATS.map((c) => [c, 1]));

  // Prefer an installed lighthouse binary; fall back to npx resolution.
  const haveBin = spawnSync("lighthouse", ["--version"], { stdio: "ignore" }).status === 0;
  const [cmd, prefix] = haveBin ? ["lighthouse", []] : ["npx", ["--yes", "lighthouse@12"]];

  // Lighthouse runs as an async child so the in-process server above can keep
  // answering Chrome's requests (a sync spawn would deadlock the event loop).
  const runLighthouse = (args) =>
    new Promise((resolve, reject) => {
      const child = spawn(cmd, args, { stdio: ["ignore", "ignore", "inherit"] });
      const timer = setTimeout(() => { child.kill("SIGKILL"); reject(new Error("lighthouse timeout")); }, 300000);
      child.on("error", (e) => { clearTimeout(timer); reject(e); });
      child.on("exit", (code) => {
        clearTimeout(timer);
        code === 0 ? resolve() : reject(new Error(`lighthouse exited ${code}`));
      });
    });

  // Median of 3 runs per page (lighthouse-ci's stability method), then the
  // MINIMUM across pages — a stable worst-page number.
  const RUNS = 3;
  const median = (a) => a.slice().sort((x, y) => x - y)[Math.floor(a.length / 2)];
  for (const page of PAGES) {
    const samples = Object.fromEntries(CATS.map((c) => [c, []]));
    for (let i = 0; i < RUNS; i++) {
      const outPath = join(tmp, `${page.replace(/\W/g, "_")}_${i}.json`);
      await runLighthouse([
        ...prefix,
        `http://localhost:${PORT}/${page}`,
        "--output=json",
        `--output-path=${outPath}`,
        `--only-categories=${CATS.join(",")}`,
        '--chrome-flags=--headless=new --no-sandbox --disable-gpu',
        "--quiet",
      ]);
      const report = JSON.parse(readFileSync(outPath, "utf8"));
      for (const c of CATS) {
        const s = report.categories[c]?.score;
        if (typeof s === "number") samples[c].push(s);
        else throw new Error(`no ${c} score for ${page}`);
      }
    }
    for (const c of CATS) mins[c] = Math.min(mins[c], median(samples[c]));
    console.log(
      `${page}: ` + CATS.map((c) => `${c}=${samples[c].map((s) => Math.round(s * 100)).join("/")}`).join(" "),
    );
  }

  const result = {
    performance: Math.round(mins["performance"] * 100),
    accessibility: Math.round(mins["accessibility"] * 100),
    bestPractices: Math.round(mins["best-practices"] * 100),
    seo: Math.round(mins["seo"] * 100),
    method: `median of ${RUNS} runs, minimum across ${PAGES.length} pages`,
    pages: PAGES,
    measuredAt: new Date().toISOString().slice(0, 10),
  };
  writeFileSync(OUT, JSON.stringify(result) + "\n");
  console.log("wrote", OUT, JSON.stringify(result));
  rmSync(tmp, { recursive: true, force: true });
} catch (err) {
  console.error("lighthouse bake skipped:", err.message || err);
} finally {
  server?.close();
}
process.exit(0);
