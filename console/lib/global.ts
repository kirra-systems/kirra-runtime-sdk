import type { Tone } from './types'

// Global Operations — the strategic, multi-site mission-control layer: fleet
// distribution across regions, activity/intervention heatmaps, regional risk
// overlays (weather / network / geofence), and cross-site alerts.

export type RiskLevel = 'low' | 'elevated' | 'high'

export interface GlobalSite {
  id: string
  name: string
  region: string
  x: number // 0..100 viewBox
  y: number // 0..50 viewBox
  assets: number
  active: number
  interventions: number // last 24h
  risk: RiskLevel
  tone: Tone
  weather: string
  network: string
  hub?: boolean
}

export const sites: GlobalSite[] = [
  { id: 'sf', name: 'San Francisco', region: 'US-West · HQ', x: 16, y: 22, assets: 142, active: 128, interventions: 9, risk: 'elevated', tone: 'warn', weather: 'High wind · fog', network: 'nominal', hub: true },
  { id: 'aus', name: 'Austin', region: 'US-Central', x: 26, y: 28, assets: 88, active: 84, interventions: 2, risk: 'low', tone: 'safe', weather: 'Clear', network: 'nominal' },
  { id: 'rot', name: 'Rotterdam', region: 'EU-West', x: 50, y: 17, assets: 64, active: 61, interventions: 1, risk: 'low', tone: 'safe', weather: 'Light rain', network: 'nominal' },
  { id: 'sin', name: 'Singapore', region: 'APAC', x: 76, y: 34, assets: 51, active: 47, interventions: 4, risk: 'elevated', tone: 'warn', weather: 'Monsoon', network: 'jitter elevated' },
  { id: 'tok', name: 'Tokyo', region: 'APAC-North', x: 86, y: 22, assets: 37, active: 36, interventions: 1, risk: 'low', tone: 'safe', weather: 'Clear', network: 'nominal' },
]

// Soft weather systems drawn as radial blobs over a region.
export interface WeatherZone { id: string; label: string; x: number; y: number; r: number; tone: Tone }
export const weatherZones: WeatherZone[] = [
  { id: 'w1', label: 'Pacific wind system', x: 13, y: 22, r: 13, tone: 'warn' },
  { id: 'w2', label: 'SE-Asia monsoon', x: 76, y: 34, r: 11, tone: 'ice' },
]

// Geofence corridors / keep-in zones at sites.
export interface GeoZone { id: string; label: string; x: number; y: number; w: number; h: number; tone: Tone }
export const geofences: GeoZone[] = [
  { id: 'g1', label: 'SF yard corridor', x: 11, y: 17, w: 11, h: 10, tone: 'safe' },
  { id: 'g2', label: 'SIN dock geofence', x: 71, y: 29, w: 11, h: 10, tone: 'warn' },
]

export interface CrossSiteAlert { id: string; ts: string; region: string; message: string; tone: Tone }
export const crossSiteAlerts: CrossSiteAlert[] = [
  { id: 'a1', ts: '23:01', region: 'US-West', message: 'High-wind advisory — 3 SF assets throttled to Degraded envelope', tone: 'warn' },
  { id: 'a2', ts: '22:54', region: 'APAC', message: 'DDS jitter elevated on Singapore↔HQ federation link (24 ms)', tone: 'warn' },
  { id: 'a3', ts: '22:40', region: 'APAC', message: 'Monsoon cell entering Singapore yard — geofence watch active', tone: 'ice' },
  { id: 'a4', ts: '22:18', region: 'EU-West', message: 'Rotterdam geofence breach auto-cleared — asset re-contained', tone: 'safe' },
  { id: 'a5', ts: '21:50', region: 'Global', message: 'Federation reconciliation complete · gen 4471 · all peers in sync', tone: 'safe' },
]

export interface RegionRisk { region: string; weather: Tone; network: Tone; geofence: Tone; note: string }
export const regionRisk: RegionRisk[] = [
  { region: 'US-West', weather: 'warn', network: 'safe', geofence: 'safe', note: 'wind advisory active' },
  { region: 'US-Central', weather: 'safe', network: 'safe', geofence: 'safe', note: 'nominal' },
  { region: 'EU-West', weather: 'safe', network: 'safe', geofence: 'safe', note: 'nominal' },
  { region: 'APAC', weather: 'ice', network: 'warn', geofence: 'warn', note: 'monsoon + link jitter' },
  { region: 'APAC-North', weather: 'safe', network: 'safe', geofence: 'safe', note: 'nominal' },
]

export const totals = {
  assets: sites.reduce((a, s) => a + s.assets, 0),
  active: sites.reduce((a, s) => a + s.active, 0),
  interventions: sites.reduce((a, s) => a + s.interventions, 0),
  sites: sites.length,
}
