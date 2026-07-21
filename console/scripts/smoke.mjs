// Self-managing production smoke test for the Kirra Console.
//
//   npm run build && npm run smoke
//
// Builds are not touched here: this script starts the production server on a
// scratch port, drives every route in headless Chromium, and fails on:
//   - non-200 document responses
//   - any page error (including React hydration failures)
//   - any console error other than the expected demo-mode 503s
//   - a missing <main id="console-main"> (shell failed to compose)
// It also exercises the quick-nav keyboard flow end-to-end.
//
// This is the CI gate distilled from the manual verification runs that caught
// the hydration regressions — codified so they can't return.

import { spawn } from 'node:child_process'
import { chromium } from 'playwright'

const PORT = 8899
const ROUTES = [
  '', 'live', 'global', 'fleet', 'fleet/r1', 'safety', 'runtime', 'telemetry',
  'missions', 'oversight', 'doer-bound', 'events', 'incidents', 'compliance',
  'analytics', 'explorer', 'reports', 'settings',
]

const server = spawn('npx', ['next', 'start', '-p', String(PORT)], { stdio: 'ignore' })
const kill = () => { try { server.kill('SIGTERM') } catch {} }
process.on('exit', kill)

// Wait for the server to accept connections.
let up = false
for (let i = 0; i < 60 && !up; i++) {
  try {
    const r = await fetch(`http://localhost:${PORT}/`, { redirect: 'manual' })
    up = r.status < 500
  } catch {
    await new Promise((r) => setTimeout(r, 500))
  }
}
if (!up) {
  console.error('SMOKE FAIL: server did not come up')
  process.exit(1)
}

// KIRRA_SMOKE_CHROMIUM overrides the browser binary (for sandboxes with a
// preinstalled Chromium); CI installs the matching build via
// `npx playwright install --with-deps chromium` and uses the default.
const browser = await chromium.launch({ executablePath: process.env.KIRRA_SMOKE_CHROMIUM || undefined })
const failures = []

for (const route of ROUTES) {
  const page = await browser.newPage({ viewport: { width: 1600, height: 950 } })
  const errs = []
  page.on('pageerror', (e) => errs.push(`pageerror: ${e.message.slice(0, 140)}`))
  page.on('console', (m) => {
    const t = m.text()
    if (m.type() === 'error' && !t.includes('503')) errs.push(`console: ${t.slice(0, 140)}`)
  })
  try {
    const resp = await page.goto(`http://localhost:${PORT}/${route}`, { waitUntil: 'networkidle', timeout: 30000 })
    if (!resp || resp.status() !== 200) errs.push(`status: ${resp?.status()}`)
    await page.waitForTimeout(1200)
    if (!(await page.$('#console-main'))) errs.push('missing #console-main')
  } catch (e) {
    errs.push(`nav: ${e.message.slice(0, 140)}`)
  }
  if (errs.length) failures.push({ route: route || '/', errs })
  console.log(`${errs.length ? '✗' : '✓'} /${route}`)
  await page.close()
}

// Quick-nav keyboard flow: type, Enter, land on the target route.
{
  const page = await browser.newPage({ viewport: { width: 1600, height: 950 } })
  await page.goto(`http://localhost:${PORT}/`, { waitUntil: 'networkidle', timeout: 30000 })
  await page.fill('input[role="combobox"]', 'incid')
  await page.keyboard.press('Enter')
  await page.waitForURL('**/incidents', { timeout: 10000 }).catch(() => failures.push({ route: 'quicknav', errs: ['did not navigate to /incidents'] }))
  console.log(`${failures.some((f) => f.route === 'quicknav') ? '✗' : '✓'} quick-nav`)
  await page.close()
}

await browser.close()
kill()

if (failures.length) {
  console.error('\nSMOKE FAIL:')
  for (const f of failures) console.error(` /${f.route}\n   ${f.errs.join('\n   ')}`)
  process.exit(1)
}
console.log(`\nSMOKE PASS: ${ROUTES.length} routes + quick-nav clean`)
process.exit(0)
