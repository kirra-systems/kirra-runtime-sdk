# Kirra Console — Engineering Review & Redesign Record

Review-board pass (UX, frontend, human factors, fleet ops, SRE) over the Next.js
operator console, executed alongside the kirrasystems.com redesign so the
console is the **operational extension of the website's design system**. Every
change preserves existing functionality; the full audit inventory this work was
based on is summarized here.

## 1–4. Audit findings (UX · UI · architecture · workflows)

**What was already strong** (preserved deliberately):
- Clean App Router structure; strict TypeScript; a well-typed wire layer
  (`lib/api/types.ts`) mirroring the verifier's serde shapes.
- The server-side proxy (`app/api/kirra/[...path]/route.ts`): GET-only
  allow-list, admin token injected server-side, nothing backend-specific in the
  browser bundle. This is the correct security posture for a read-only console.
- Demo-mode-first design: every hook settles into typed demo snapshots, so the
  console always renders. 19 routes, dense information design, per-SVG
  aria-labels, zero TODO/console.log debt.

**Defects found and fixed in this pass:**

| Finding | Operator impact | Fix |
|---|---|---|
| Brand divergence: mint-green accent, off-brand bg, and the status hex quartet copy-pasted across **31 files** | Console read as a different company than the website; any palette change required touching every chart | All tokens now match `website/css/kirra.css`; every SVG/Recharts color reads `var(--c-*)` (verified: **zero hardcoded hex** outside one intentional constant) |
| Dark-only, hardcoded `class="dark"` | No high-ambient-light option (industrial environments) | Full light theme via `html.light` token overrides + persisted toggle; dark remains the control-room default regardless of OS |
| Overview header hardcoded "Fleet Nominal · E-Stop Armed · updated 2s ago" over simulated data | The flagship screen asserted safety state it never computed — the demo fleet actually contains a LockedOut node the header contradicted | `OverviewStatus` strip derives posture, nominal count, audit integrity, and freshness from the console's real data source; it now truthfully shows "FLEET LOCKED OUT · 6/8 nominal" on the demo dataset |
| Sidebar footer hardcoded "All systems nominal · v1.2.0" | A dead reassurance an operator could believe during an outage | Footer derives from `useHealth`: verifier connected / demo ("simulated fleet — not evidence") / offline |
| Fake identity "J. Looney · Operator · Admin" and env chip "PRODUCTION · us-fleet-1" | Misrepresents session privileges and environment | "Operator · read-only session" (the console *is* read-only) + env chip derived from the backend binding (LIVE / DEMO / OFFLINE) |
| `DemoBadge live={false}` hardcoded on 4 pages whose panels DO fetch live data (global, fleet/[id], runtime, reports) | Live data labeled simulated — the badge lied in both directions | `SourceBadge` driven by `useHealth`: "Live panels · simulated context" when bound |
| Decorative search input; bell button with hardcoded alert dot; fake env dropdown | Non-functional affordances erode trust in every real one | Search is now a keyboard-first quick-nav (routes + fleet units, combobox ARIA); bell links to `/events` without a fabricated badge; env chip is non-interactive |
| React hydration failures (#418) on /, /live, /runtime, /compliance | Full client re-render, console noise, flaky-feeling loads | Three root causes fixed: `Math.random()` in 4 mock modules → shared seeded PRNG (`lib/seeded.ts`); locale/TZ-dependent `toLocaleTimeString` → pinned UTC formatters (`lib/format.ts` — UTC is also the correct ops convention); render-time `Date.now()` aging → mounted clock with demo data anchored to a fixed epoch. Verified: zero hydration errors across all 18 routes, warm and cold |
| `next/font/google` build-time fetch; no lockfile; unused framer-motion dependency | Air-gapped builds fail; unpinned dependency resolution; dead weight | Fonts self-hosted from `website/fonts` via `next/font/local` (pixel-identical to the site); `package-lock.json` committed; framer-motion removed |
| Minor a11y gaps | Keyboard/AT operators | Skip-to-content link, `aria-current` on nav, combobox/listbox semantics on quick-nav, `aria-pressed` on the theme toggle |

## 5–9. IA, dashboard, diagnostics, telemetry, navigation

- **IA kept**: the three-group nav (Operations / Governance / Insight) with
  `preview` tags is sound; active states now use the brand accent (cyan) with
  status colors reserved for status.
- **Dashboard answers first**: the header strip answers "is the fleet safe,
  how many nodes are nominal, is the audit chain intact, how fresh is this" from
  real state before any simulated exec content renders.
- **Diagnostics/telemetry**: the live-backed surfaces (posture, audit verify,
  fabric telemetry, runtime snapshot, version adoption) are the real diagnostic
  set today; the roadmap below covers their expansion. No vanity metrics were
  added — screens that render simulated data remain explicitly labeled at three
  levels (nav tag, page badge, sidebar footer).

## 10–12. Accessibility, performance, security

- WCAG: token palette inherits the website's validated contrast work (light
  status colors are the AA-verified set); focus/keyboard paths added where
  interaction was mouse-only.
- Performance: build is static-prerendered (28 pages), first-load JS ~102 kB
  shared; the seeded-PRNG fix eliminates the hydrate-then-regenerate penalty on
  four routes; font self-hosting removes a third-party fetch.
- Security: unchanged by design — token stays server-side; proxy allow-list
  untouched; the console remains read-only (write paths live in
  `static/console.html` and `dashboard/`, per the deliberate split).

## 13–15. Verification & testing recommendations

Verified in this pass: `tsc --noEmit` clean; `next build` green;
all 18 routes screenshot-rendered in both themes; quick-nav keyboard flow
drives navigation; zero hydration errors cold and warm; zero non-503 console
errors (503s are the demo-mode proxy contract).

Recommended next (no test infrastructure exists yet):
1. Playwright smoke suite in CI: route renders + zero pageerrors + demo badges
   present (the checks scripted for this review are the seed).
2. Contract tests pinning `lib/api/types.ts` against the verifier's serde
   shapes (a mismatch currently surfaces only at runtime).
3. Hook unit tests for the demo-fallback state machine (`DemoMode` sentinel →
   settle → recover).

## 16. Fleet-scale roadmap (ranked)

1. ~~Consume the real SSE posture stream~~ **— DONE.** `useLiveFleet` attaches
   to `/system/posture/stream`; events push a coalesced snapshot refetch, the
   transport is shown in the UI, and it falls back to polling on any error.
   Verified end-to-end against a live verifier (see `PRODUCTION.md`).
2. **Alert center**: persist posture transitions + audit anomalies with
   acknowledge/history (groundwork: `useLiveFleet` already synthesizes
   transition events).
3. **Multi-verifier federation view**: the `/global` sites screen is the natural
   home; needs a verifier-list config and per-site health rollup.
4. **Merge the write-path consoles**: bring `static/console.html`'s clearance
   grant (WebCrypto Ed25519) into this console behind real per-operator
   sessions (#314 operator registry is the backend seam).
5. **Virtualized tables + persisted layouts** once fleets exceed ~100 nodes.
