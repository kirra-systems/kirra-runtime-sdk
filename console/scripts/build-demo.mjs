// Build the static, hostable DEMO of the console for GitHub Pages.
//
//   node scripts/build-demo.mjs   →  console/out/  (served at /console/)
//
// The console normally runs as a Next server (with the verifier proxy). A
// static export can't include a route handler, and the public demo has no
// backend anyway — so this script temporarily removes the proxy route, builds
// with `output: export` + basePath /console + NEXT_PUBLIC_KIRRA_STATIC_DEMO=1
// (which makes the client short-circuit straight to demo data), then restores
// the route. The result is a pure static site: no server, demo data only,
// labeled as such at every level.

import { execFileSync } from 'node:child_process'
import { renameSync, existsSync, rmSync } from 'node:fs'

// Routes that can't be statically exported (a dynamic route handler, and the
// catch-all placeholder which would need on-demand rendering). They're stashed
// for the demo build and always restored. The proxy has no meaning without a
// backend; unknown routes fall through to the static 404 page.
const STASH = [
  ['app/api', '.stash-api'],
  ['app/[...slug]', '.stash-slug'],
]

function restore() {
  for (const [dir, stash] of STASH) if (existsSync(stash)) renameSync(stash, dir)
}

// Always restore the proxy route, even on failure — it must never be lost.
process.on('exit', restore)
process.on('SIGINT', () => { restore(); process.exit(1) })

try {
  rmSync('out', { recursive: true, force: true })
  for (const [dir, stash] of STASH) if (existsSync(dir)) renameSync(dir, stash)
  execFileSync('npx', ['next', 'build'], {
    stdio: 'inherit',
    env: { ...process.env, KIRRA_STATIC_DEMO: '1' },
  })
  console.log('\n✓ static demo built → console/out (serve under /console)')
} catch (e) {
  console.error('demo build failed:', e.message)
  restore()
  process.exit(1)
}
