import { Pill } from '@/components/ui/primitives'

// Read-only data-provenance badge for page headers.
//
// The Mission Console is an observe-only (QM-domain) surface with no actuator
// authority. Most screens render bundled mock data; only a few read the real
// verifier. This badge makes the data source explicit at a glance — important
// for grant / acquirer demos where confident-looking numbers must not be
// mistaken for live readings.
//
// `live` is the page's PRIMARY data source:
//   - true  → the page reads the real verifier (green "LIVE" pill). Pages that
//             track a runtime `source` should pass `source === 'live'`, so the
//             badge flips to "SIMULATED DATA" when the backend is unconfigured
//             and the page has fallen back to demo data.
//   - false → the page renders bundled mock data (amber "SIMULATED DATA" pill).
//
// Purely presentational (no hooks / no handlers) so it renders in both server
// and client page components.
export function DemoBadge({ live }: { live: boolean }) {
  return live ? <Pill tone="safe">Live</Pill> : <Pill tone="warn">Simulated data</Pill>
}
