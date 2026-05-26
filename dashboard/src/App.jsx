import { useState, useEffect, useCallback, useRef, Component } from "react";

export class ErrorBoundary extends Component {
  constructor(props) { super(props); this.state = { error: null }; }
  static getDerivedStateFromError(error) { return { error }; }
  render() {
    if (this.state.error) {
      return (
        <div style={{ background: "#0a0c0f", color: "#e2e8f0", padding: 40, fontFamily: "monospace", minHeight: "100vh" }}>
          <div style={{ color: "#ef4444", fontSize: 18, marginBottom: 16 }}>Dashboard Error</div>
          <pre style={{ color: "#fca5a5", background: "#111418", padding: 16, borderRadius: 6, whiteSpace: "pre-wrap", wordBreak: "break-all" }}>
            {this.state.error.message}
          </pre>
          <button onClick={() => this.setState({ error: null })}
            style={{ marginTop: 16, padding: "8px 16px", background: "#3b82f6", color: "white", border: "none", borderRadius: 4, cursor: "pointer" }}>
            Retry
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}

// ─── Design tokens ───────────────────────────────────────────────────────────
const CSS = `
  @import url('https://fonts.googleapis.com/css2?family=Space+Mono:wght@400;700&family=DM+Sans:wght@300;400;500;600&display=swap');

  *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }

  :root {
    --bg:        #0a0c0f;
    --surface:   #111418;
    --surface2:  #181c22;
    --border:    #1e2530;
    --border2:   #2a3340;
    --text:      #e2e8f0;
    --muted:     #64748b;
    --nominal:   #00d4a0;
    --degraded:  #f59e0b;
    --locked:    #ef4444;
    --trusted:   #00d4a0;
    --untrusted: #ef4444;
    --unknown:   #64748b;
    --accent:    #3b82f6;
    --mono:      'Space Mono', monospace;
    --sans:      'DM Sans', sans-serif;
  }

  body {
    background: var(--bg);
    color: var(--text);
    font-family: var(--sans);
    font-size: 14px;
    line-height: 1.5;
    min-height: 100vh;
  }

  ::-webkit-scrollbar { width: 4px; height: 4px; }
  ::-webkit-scrollbar-track { background: var(--surface); }
  ::-webkit-scrollbar-thumb { background: var(--border2); border-radius: 2px; }

  .app { display: flex; flex-direction: column; min-height: 100vh; }

  /* Header */
  .header {
    display: flex; align-items: center; justify-content: space-between;
    padding: 0 24px; height: 56px;
    background: var(--surface); border-bottom: 1px solid var(--border);
    position: sticky; top: 0; z-index: 100;
  }
  .header-left { display: flex; align-items: center; gap: 12px; }
  .logo { font-family: var(--mono); font-size: 16px; font-weight: 700; letter-spacing: 0.05em; }
  .logo span { color: var(--nominal); }
  .version { font-family: var(--mono); font-size: 11px; color: var(--muted);
    background: var(--surface2); padding: 2px 8px; border-radius: 3px; border: 1px solid var(--border); }
  .conn-badge {
    display: flex; align-items: center; gap: 6px;
    font-family: var(--mono); font-size: 11px;
    padding: 4px 10px; border-radius: 4px; border: 1px solid var(--border);
    background: var(--surface2);
  }
  .conn-dot { width: 6px; height: 6px; border-radius: 50%; }
  .conn-dot.connected { background: var(--nominal); box-shadow: 0 0 6px var(--nominal); }
  .conn-dot.disconnected { background: var(--locked); }

  /* Layout */
  .layout { display: grid; grid-template-columns: 220px 1fr; flex: 1; }
  .sidebar {
    background: var(--surface); border-right: 1px solid var(--border);
    padding: 16px 0; display: flex; flex-direction: column; gap: 4px;
  }
  .nav-item {
    display: flex; align-items: center; gap: 10px;
    padding: 9px 20px; cursor: pointer; font-size: 13px; font-weight: 500;
    color: var(--muted); transition: all 0.15s; border-left: 2px solid transparent;
    user-select: none;
  }
  .nav-item:hover { color: var(--text); background: var(--surface2); }
  .nav-item.active { color: var(--text); background: var(--surface2); border-left-color: var(--accent); }
  .nav-icon { font-size: 15px; width: 18px; text-align: center; }
  .nav-section { font-family: var(--mono); font-size: 10px; color: var(--muted);
    padding: 12px 20px 4px; letter-spacing: 0.1em; text-transform: uppercase; }

  .main { padding: 24px; overflow-y: auto; }

  /* Connect screen */
  .connect-screen {
    display: flex; align-items: center; justify-content: center;
    min-height: calc(100vh - 56px); padding: 24px;
  }
  .connect-card {
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 8px; padding: 40px; width: 100%; max-width: 480px;
  }
  .connect-title { font-family: var(--mono); font-size: 20px; margin-bottom: 8px; }
  .connect-sub { color: var(--muted); font-size: 13px; margin-bottom: 32px; }
  .field { margin-bottom: 20px; }
  .field label { display: block; font-size: 12px; font-weight: 500; color: var(--muted);
    margin-bottom: 6px; text-transform: uppercase; letter-spacing: 0.05em; font-family: var(--mono); }
  .field input {
    width: 100%; padding: 10px 12px; background: var(--surface2);
    border: 1px solid var(--border2); border-radius: 5px; color: var(--text);
    font-family: var(--mono); font-size: 13px; outline: none; transition: border-color 0.15s;
  }
  .field input:focus { border-color: var(--accent); }
  .btn {
    display: inline-flex; align-items: center; gap: 8px;
    padding: 10px 20px; border-radius: 5px; font-size: 13px; font-weight: 600;
    cursor: pointer; border: none; transition: all 0.15s; font-family: var(--sans);
  }
  .btn-primary { background: var(--accent); color: white; }
  .btn-primary:hover { background: #2563eb; }
  .btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn-ghost { background: transparent; color: var(--muted); border: 1px solid var(--border2); }
  .btn-ghost:hover { color: var(--text); border-color: var(--border2); background: var(--surface2); }
  .btn-danger { background: transparent; color: var(--locked); border: 1px solid var(--locked)44; }
  .btn-danger:hover { background: var(--locked)11; }
  .btn-sm { padding: 6px 12px; font-size: 12px; }
  .btn-full { width: 100%; justify-content: center; }

  /* Page titles */
  .page-header { margin-bottom: 24px; }
  .page-title { font-family: var(--mono); font-size: 20px; font-weight: 700; margin-bottom: 4px; }
  .page-sub { color: var(--muted); font-size: 13px; }

  /* Posture banner */
  .posture-banner {
    border-radius: 8px; padding: 20px 24px; margin-bottom: 24px;
    display: flex; align-items: center; justify-content: space-between;
    border: 1px solid; transition: all 0.3s;
  }
  .posture-banner.nominal { background: #00d4a011; border-color: #00d4a033; }
  .posture-banner.degraded { background: #f59e0b11; border-color: #f59e0b33; }
  .posture-banner.lockedout { background: #ef444411; border-color: #ef444433; }
  .posture-label { font-family: var(--mono); font-size: 28px; font-weight: 700; }
  .posture-label.nominal { color: var(--nominal); }
  .posture-label.degraded { color: var(--degraded); }
  .posture-label.lockedout { color: var(--locked); }
  .posture-meta { font-size: 12px; color: var(--muted); margin-top: 4px; }

  /* Stats row */
  .stats-row { display: grid; grid-template-columns: repeat(4, 1fr); gap: 16px; margin-bottom: 24px; }
  .stat-card {
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 8px; padding: 16px 20px;
  }
  .stat-label { font-size: 11px; color: var(--muted); text-transform: uppercase;
    letter-spacing: 0.08em; font-family: var(--mono); margin-bottom: 8px; }
  .stat-value { font-family: var(--mono); font-size: 28px; font-weight: 700; }
  .stat-value.green { color: var(--nominal); }
  .stat-value.yellow { color: var(--degraded); }
  .stat-value.red { color: var(--locked); }
  .stat-value.blue { color: var(--accent); }

  /* Cards */
  .card {
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 8px; overflow: hidden; margin-bottom: 16px;
  }
  .card-header {
    padding: 14px 20px; border-bottom: 1px solid var(--border);
    display: flex; align-items: center; justify-content: space-between;
  }
  .card-title { font-family: var(--mono); font-size: 13px; font-weight: 700; letter-spacing: 0.05em; }
  .card-body { padding: 20px; }

  /* Node grid */
  .node-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 16px; }
  .node-card {
    background: var(--surface2); border: 1px solid var(--border);
    border-radius: 8px; padding: 16px; transition: border-color 0.2s;
  }
  .node-card:hover { border-color: var(--border2); }
  .node-card.trusted { border-left: 3px solid var(--trusted); }
  .node-card.untrusted { border-left: 3px solid var(--untrusted); }
  .node-card.unknown { border-left: 3px solid var(--unknown); }
  .node-header { display: flex; align-items: flex-start; justify-content: space-between; margin-bottom: 12px; }
  .node-id { font-family: var(--mono); font-size: 13px; font-weight: 700; }
  .node-type { font-size: 11px; color: var(--muted); margin-top: 2px; }
  .trust-badge {
    font-family: var(--mono); font-size: 10px; font-weight: 700;
    padding: 3px 8px; border-radius: 3px; text-transform: uppercase; letter-spacing: 0.08em;
  }
  .trust-badge.trusted { background: #00d4a022; color: var(--trusted); }
  .trust-badge.untrusted { background: #ef444422; color: var(--untrusted); }
  .trust-badge.unknown { background: #64748b22; color: var(--unknown); }
  .node-meta { font-size: 11px; color: var(--muted); font-family: var(--mono); }
  .node-posture { margin-top: 8px; }
  .posture-pill {
    display: inline-flex; align-items: center; gap: 4px;
    font-family: var(--mono); font-size: 11px; padding: 3px 8px; border-radius: 3px;
  }
  .posture-pill.nominal { background: #00d4a011; color: var(--nominal); }
  .posture-pill.degraded { background: #f59e0b11; color: var(--degraded); }
  .posture-pill.lockedout { background: #ef444411; color: var(--locked); }
  .posture-dot { width: 5px; height: 5px; border-radius: 50%; background: currentColor; }

  /* Event log */
  .event-log { font-family: var(--mono); font-size: 12px; }
  .event-entry {
    display: grid; grid-template-columns: 140px 120px 1fr;
    padding: 8px 0; border-bottom: 1px solid var(--border);
    align-items: start; gap: 12px;
  }
  .event-entry:last-child { border-bottom: none; }
  .event-time { color: var(--muted); }
  .event-type {
    font-size: 10px; font-weight: 700; padding: 2px 6px; border-radius: 3px;
    text-transform: uppercase; letter-spacing: 0.06em; white-space: nowrap;
    display: inline-block;
  }
  .event-type.transition { background: #3b82f622; color: var(--accent); }
  .event-type.fault { background: #ef444422; color: var(--locked); }
  .event-type.recovery { background: #00d4a022; color: var(--nominal); }
  .event-type.info { background: #64748b22; color: var(--muted); }
  .event-msg { color: var(--text); word-break: break-all; }

  /* Forms */
  .form-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
  .form-grid-3 { display: grid; grid-template-columns: 1fr 1fr 1fr; gap: 16px; }
  .form-actions { display: flex; gap: 10px; margin-top: 20px; }

  /* Table */
  .table { width: 100%; border-collapse: collapse; }
  .table th {
    text-align: left; padding: 10px 16px; font-size: 11px; font-weight: 700;
    color: var(--muted); text-transform: uppercase; letter-spacing: 0.08em;
    font-family: var(--mono); border-bottom: 1px solid var(--border);
    background: var(--surface2);
  }
  .table td { padding: 12px 16px; border-bottom: 1px solid var(--border); font-size: 13px; }
  .table tr:last-child td { border-bottom: none; }
  .table tr:hover td { background: var(--surface2); }
  .mono { font-family: var(--mono); font-size: 12px; }

  /* Alert */
  .alert {
    padding: 12px 16px; border-radius: 6px; font-size: 13px; margin-bottom: 16px;
    border: 1px solid;
  }
  .alert.error { background: #ef444411; border-color: #ef444433; color: #fca5a5; }
  .alert.success { background: #00d4a011; border-color: #00d4a033; color: var(--nominal); }
  .alert.warning { background: #f59e0b11; border-color: #f59e0b33; color: #fcd34d; }

  /* Loading */
  .loading { display: flex; align-items: center; justify-content: center; padding: 40px; color: var(--muted); }
  .spin { animation: spin 1s linear infinite; display: inline-block; }
  @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

  /* Empty state */
  .empty { text-align: center; padding: 48px; color: var(--muted); }
  .empty-icon { font-size: 32px; margin-bottom: 12px; }
  .empty-text { font-size: 13px; }

  /* Pulse animation for live indicator */
  @keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.4; } }
  .live-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--nominal);
    animation: pulse 2s ease-in-out infinite; display: inline-block; margin-right: 6px; }

  /* Responsive */
  @media (max-width: 900px) {
    .layout { grid-template-columns: 1fr; }
    .sidebar { display: none; }
    .stats-row { grid-template-columns: 1fr 1fr; }
  }
  @media (max-width: 600px) {
    .stats-row { grid-template-columns: 1fr; }
    .form-grid, .form-grid-3 { grid-template-columns: 1fr; }
  }

  select {
    width: 100%; padding: 10px 12px; background: var(--surface2);
    border: 1px solid var(--border2); border-radius: 5px; color: var(--text);
    font-family: var(--mono); font-size: 13px; outline: none;
  }
  select:focus { border-color: var(--accent); }
  textarea {
    width: 100%; padding: 10px 12px; background: var(--surface2);
    border: 1px solid var(--border2); border-radius: 5px; color: var(--text);
    font-family: var(--mono); font-size: 12px; outline: none; resize: vertical; min-height: 80px;
  }
  textarea:focus { border-color: var(--accent); }

  .tabs { display: flex; gap: 2px; margin-bottom: 20px; background: var(--surface2);
    padding: 4px; border-radius: 6px; border: 1px solid var(--border); width: fit-content; }
  .tab { padding: 7px 16px; border-radius: 4px; cursor: pointer; font-size: 13px;
    font-weight: 500; color: var(--muted); transition: all 0.15s; }
  .tab.active { background: var(--surface); color: var(--text); box-shadow: 0 1px 3px #0004; }
  .tab:hover:not(.active) { color: var(--text); }

  .divider { height: 1px; background: var(--border); margin: 20px 0; }

  .tag { display: inline-block; font-family: var(--mono); font-size: 10px;
    padding: 2px 6px; border-radius: 3px; background: var(--surface2);
    border: 1px solid var(--border2); color: var(--muted); margin: 2px; }
`;

// ─── API client ──────────────────────────────────────────────────────────────
function makeApi(baseUrl, token) {
  const headers = { "Content-Type": "application/json", "Authorization": `Bearer ${token}` };
  const call = async (method, path, body) => {
    const res = await fetch(`${baseUrl}${path}`, {
      method, headers,
      body: body ? JSON.stringify(body) : undefined,
    });
    if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
    return res.json().catch(() => ({}));
  };
  return {
    health:        () => fetch(`${baseUrl}/health`).then(r => r.json()),
    fleetPosture:  () => call("GET", "/fleet/posture"),
    nodePosture:   (id) => call("GET", `/fleet/posture/${id}`),
    nodeHistory:   (id) => call("GET", `/fleet/history/${id}`),
    auditVerify:   () => call("GET", "/system/audit/verify"),
    registerNode:  (b) => call("POST", "/attestation/register", b),
    registerDeps:  (b) => call("POST", "/fleet/dependencies", b),
    issueChallenge:(id) => call("POST", `/attestation/challenge/${id}`),
    sensorReport:  (b) => call("POST", "/fleet/diagnostics/report", b),
    registerAV:    (b) => call("POST", "/fleet/assets/register", b),
  };
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

// Rust serde serializes NodeTrustState as:
//   "Trusted" | "Unknown" (unit variants → strings)
//   {"Untrusted": "reason"} (tuple variant → object)
function normalizeTrustState(t) {
  if (!t) return "unknown";
  if (typeof t === "object") return t.Untrusted !== undefined ? "untrusted" : "unknown";
  return t.toLowerCase();
}

function formatTrustState(t) {
  if (!t) return "Unknown";
  if (typeof t === "object") {
    if (t.Untrusted !== undefined) return `Untrusted: ${t.Untrusted}`;
    return JSON.stringify(t);
  }
  return String(t);
}

function postureClass(p) {
  if (!p) return "unknown";
  if (typeof p === "object") return "unknown";
  const s = p.toLowerCase();
  if (s === "nominal") return "nominal";
  if (s === "degraded") return "degraded";
  if (s.includes("locked")) return "lockedout";
  return "unknown";
}

function trustClass(t) {
  const s = normalizeTrustState(t);
  if (s === "trusted") return "trusted";
  if (s.includes("untrusted")) return "untrusted";
  return "unknown";
}

function timeAgo(ms) {
  if (!ms) return "never";
  const diff = Date.now() - ms;
  if (diff < 60000) return `${Math.floor(diff / 1000)}s ago`;
  if (diff < 3600000) return `${Math.floor(diff / 60000)}m ago`;
  return `${Math.floor(diff / 3600000)}h ago`;
}

function fmtTime(ms) {
  return new Date(ms).toLocaleTimeString();
}

// ─── Main App ─────────────────────────────────────────────────────────────────
export default function AegisDashboard() {
  const [config, setConfig]       = useState({ url: "http://localhost:8090", token: "" });
  const [connected, setConnected] = useState(false);
  const [page, setPage]           = useState("overview");
  const [fleet, setFleet]         = useState(null);
  const [events, setEvents]       = useState([]);
  const [audit, setAudit]         = useState(null);
  const [fabricState, setFabricState] = useState(null);
  const [loading, setLoading]     = useState(false);
  const [error, setError]         = useState(null);
  const [toast, setToast]         = useState(null);
  const apiRef = useRef(null);
  const refreshRef = useRef(null);

  const showToast = (msg, type = "success") => {
    setToast({ msg, type });
    setTimeout(() => setToast(null), 3500);
  };

  const connect = async () => {
    setLoading(true); setError(null);
    try {
      const api = makeApi(config.url.replace(/\/$/, ""), config.token);
      await api.health();
      apiRef.current = api;
      setConnected(true);
      loadFleet(api);
    } catch (e) {
      setError(`Connection failed: ${e.message}. Check URL and token.`);
    } finally { setLoading(false); }
  };

  const loadFleet = useCallback(async (api) => {
    try {
      const data = await (api || apiRef.current).fleetPosture();
      setFleet(data);
    } catch (e) { /* silent refresh fail */ }
  }, []);

  const loadAudit = useCallback(async () => {
    try {
      const data = await apiRef.current.auditVerify();
      setAudit(data);
    } catch (e) { setAudit({ error: e.message }); }
  }, []);

  const loadFabricState = useCallback(async () => {
    try {
      const baseUrl = config.url.replace(/\/$/, "");
      const res = await fetch(`${baseUrl}/fabric/state`, {
        headers: { "Content-Type": "application/json", "Authorization": `Bearer ${config.token}` },
      });
      if (res.ok) {
        const data = await res.json();
        setFabricState(data);
      }
    } catch (e) { /* silent refresh fail */ }
  }, [config.url, config.token]);

  // Auto-refresh fleet every 5s when connected
  useEffect(() => {
    if (!connected) return;
    refreshRef.current = setInterval(() => loadFleet(), 5000);
    return () => clearInterval(refreshRef.current);
  }, [connected, loadFleet]);

  // Auto-refresh fabric state every 5s when connected
  useEffect(() => {
    if (!connected) return;
    loadFabricState();
    const interval = setInterval(() => loadFabricState(), 5000);
    return () => clearInterval(interval);
  }, [connected, loadFabricState]);

  // SSE posture stream (best-effort; requires x-aegis-client-id header via server config)
  useEffect(() => {
    if (!connected) return;
    const url = `${config.url.replace(/\/$/, "")}/system/posture/stream`;
    let es;
    try {
      es = new EventSource(url);
      es.onmessage = (e) => {
        try {
          const data = JSON.parse(e.data);
          setEvents(prev => [{ ...data, receivedAt: Date.now() }, ...prev].slice(0, 100));
        } catch {}
      };
      es.onerror = () => { es.close(); };
    } catch {}
    return () => { try { es?.close(); } catch {} };
  }, [connected, config.url]);

  if (!connected) {
    return (
      <>
        <style>{CSS}</style>
        <div className="app">
          <div className="header">
            <div className="header-left">
              <div className="logo">AEG<span>IS</span></div>
              <div className="version">Safety Kernel</div>
            </div>
          </div>
          <div className="connect-screen">
            <div className="connect-card">
              <div className="connect-title">Connect to Aegis</div>
              <div className="connect-sub">Enter your verifier URL and admin token to get started.</div>
              {error && <div className="alert error">{error}</div>}
              <div className="field">
                <label>Verifier URL</label>
                <input value={config.url} onChange={e => setConfig(c => ({ ...c, url: e.target.value }))}
                  placeholder="http://192.168.1.100:8090" />
              </div>
              <div className="field">
                <label>Admin Token</label>
                <input type="password" value={config.token}
                  onChange={e => setConfig(c => ({ ...c, token: e.target.value }))}
                  placeholder="Bearer token from /etc/aegis/aegis.env"
                  onKeyDown={e => e.key === "Enter" && connect()} />
              </div>
              <button className="btn btn-primary btn-full" onClick={connect} disabled={loading}>
                {loading ? "Connecting…" : "Connect"}
              </button>
            </div>
          </div>
        </div>
      </>
    );
  }

  const nodes = fleet?.fleet || [];
  const trustedCount   = nodes.filter(n => trustClass(n.local_status || n.trust_state) === "trusted").length;
  const untrustedCount = nodes.filter(n => trustClass(n.local_status || n.trust_state) === "untrusted").length;
  const overallPosture = fleet?.posture || (nodes.length ? "Nominal" : "—");

  const navItems = [
    { id: "overview",  icon: "◉", label: "Overview" },
    { id: "fleet",     icon: "⬡", label: "Fleet Nodes" },
    { id: "stream",    icon: "▸", label: "Event Stream" },
    { id: "audit",     icon: "⎗", label: "Audit Chain" },
    { id: "fabric",    icon: "⬙", label: "Fabric" },
    { id: "register",  icon: "+", label: "Register Node" },
    { id: "deps",      icon: "⇄", label: "Dependencies" },
    { id: "sensor",    icon: "⚡", label: "Sensor Report" },
    { id: "avmeta",    icon: "◈", label: "AV Metadata" },
  ];

  return (
    <>
      <style>{CSS}</style>
      <div className="app">
        {/* Header */}
        <div className="header">
          <div className="header-left">
            <div className="logo">AEG<span>IS</span></div>
            <div className="version">Safety Kernel</div>
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
            <div className="conn-badge">
              <div className="conn-dot connected" />
              {config.url.replace(/^https?:\/\//, "").replace(/\/$/, "")}
            </div>
            <button className="btn btn-ghost btn-sm" onClick={() => { setConnected(false); setFleet(null); setEvents([]); }}>
              Disconnect
            </button>
          </div>
        </div>

        <div className="layout">
          {/* Sidebar */}
          <div className="sidebar">
            <div className="nav-section">Monitor</div>
            {navItems.slice(0, 5).map(n => (
              <div key={n.id} className={`nav-item ${page === n.id ? "active" : ""}`}
                onClick={() => { setPage(n.id); if (n.id === "audit") loadAudit(); }}>
                <span className="nav-icon">{n.icon}</span>{n.label}
              </div>
            ))}
            <div className="nav-section">Manage</div>
            {navItems.slice(5).map(n => (
              <div key={n.id} className={`nav-item ${page === n.id ? "active" : ""}`}
                onClick={() => setPage(n.id)}>
                <span className="nav-icon">{n.icon}</span>{n.label}
              </div>
            ))}
          </div>

          {/* Main content */}
          <div className="main">
            {toast && (
              <div className={`alert ${toast.type}`} style={{ position: "fixed", top: 70, right: 24, zIndex: 999, maxWidth: 360 }}>
                {toast.msg}
              </div>
            )}

            {page === "overview" && <OverviewPage nodes={nodes} overallPosture={overallPosture} events={events} fleet={fleet} />}
            {page === "fleet"    && <FleetPage nodes={nodes} onRefresh={() => loadFleet()} />}
            {page === "stream"   && <StreamPage events={events} />}
            {page === "audit"    && <AuditPage audit={audit} onRefresh={loadAudit} />}
            {page === "fabric"   && <FabricPage fabricState={fabricState} onRefresh={loadFabricState} />}
            {page === "register" && <RegisterPage api={apiRef.current} onSuccess={(m) => { showToast(m); loadFleet(); }} />}
            {page === "deps"     && <DepsPage api={apiRef.current} nodes={nodes} onSuccess={(m) => showToast(m)} />}
            {page === "sensor"   && <SensorPage api={apiRef.current} nodes={nodes} onSuccess={(m) => { showToast(m); loadFleet(); }} />}
            {page === "avmeta"   && <AVMetaPage api={apiRef.current} nodes={nodes} onSuccess={(m) => showToast(m)} />}
          </div>
        </div>
      </div>
    </>
  );
}

// ─── Overview ────────────────────────────────────────────────────────────────
function OverviewPage({ nodes, overallPosture, events, fleet }) {
  const pc = postureClass(overallPosture);
  const trusted   = nodes.filter(n => trustClass(n.local_status || n.trust_state) === "trusted").length;
  const untrusted = nodes.filter(n => trustClass(n.local_status || n.trust_state) === "untrusted").length;

  return (
    <div>
      <div className="page-header">
        <div className="page-title">Overview</div>
        <div className="page-sub">Fleet safety posture and real-time status</div>
      </div>

      <div className={`posture-banner ${pc}`}>
        <div>
          <div style={{ fontSize: 12, color: "var(--muted)", fontFamily: "var(--mono)", marginBottom: 6, textTransform: "uppercase", letterSpacing: "0.1em" }}>
            Fleet Posture
          </div>
          <div className={`posture-label ${pc}`}>{overallPosture || "UNKNOWN"}</div>
          <div className="posture-meta">{nodes.length} nodes registered · {trusted} trusted · {untrusted} untrusted</div>
        </div>
        <div style={{ textAlign: "right" }}>
          <div style={{ fontSize: 11, color: "var(--muted)", fontFamily: "var(--mono)" }}>
            <span className="live-dot" />LIVE
          </div>
          <div style={{ fontSize: 11, color: "var(--muted)", marginTop: 4 }}>
            {new Date().toLocaleTimeString()}
          </div>
        </div>
      </div>

      <div className="stats-row">
        <div className="stat-card">
          <div className="stat-label">Total Nodes</div>
          <div className="stat-value blue">{nodes.length}</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Trusted</div>
          <div className="stat-value green">{trusted}</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Untrusted</div>
          <div className="stat-value red">{untrusted}</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Events</div>
          <div className="stat-value blue">{events.length}</div>
        </div>
      </div>

      {nodes.length > 0 && (
        <div className="card">
          <div className="card-header"><div className="card-title">NODE STATUS</div></div>
          <div className="card-body">
            <div className="node-grid">
              {nodes.slice(0, 6).map((n, i) => <NodeCard key={i} node={n} />)}
            </div>
          </div>
        </div>
      )}

      {events.length > 0 && (
        <div className="card">
          <div className="card-header"><div className="card-title">RECENT EVENTS</div></div>
          <div className="card-body">
            <EventLog events={events.slice(0, 5)} />
          </div>
        </div>
      )}

      {nodes.length === 0 && (
        <div className="card">
          <div className="card-body">
            <div className="empty">
              <div className="empty-icon">⬡</div>
              <div className="empty-text">No nodes registered yet.<br />Use the Register Node panel to add devices to the fleet.</div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

// ─── Fleet ───────────────────────────────────────────────────────────────────
function FleetPage({ nodes, onRefresh }) {
  return (
    <div>
      <div className="page-header" style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start" }}>
        <div>
          <div className="page-title">Fleet Nodes</div>
          <div className="page-sub">{nodes.length} registered nodes</div>
        </div>
        <button className="btn btn-ghost btn-sm" onClick={onRefresh}>↻ Refresh</button>
      </div>

      {nodes.length === 0 ? (
        <div className="empty"><div className="empty-icon">⬡</div><div className="empty-text">No nodes registered</div></div>
      ) : (
        <div className="node-grid">
          {nodes.map((n, i) => <NodeCard key={i} node={n} detailed />)}
        </div>
      )}
    </div>
  );
}

function NodeCard({ node, detailed }) {
  const trust = trustClass(node.local_status || node.trust_state || node.status);
  const posture = postureClass(node.propagated_status || node.posture);
  const nodeId = node.node_id || node.id || "unknown";
  const trustRaw = formatTrustState(node.local_status || node.trust_state || node.status);

  return (
    <div className={`node-card ${trust}`}>
      <div className="node-header">
        <div>
          <div className="node-id">{nodeId}</div>
          {node.subsystem_class && <div className="node-type">{node.subsystem_class}</div>}
        </div>
        <div className={`trust-badge ${trust}`}>{trust}</div>
      </div>
      {detailed && (
        <div className="node-meta" style={{ marginBottom: 8 }}>
          Trust: {trustRaw}
        </div>
      )}
      {(node.propagated_status || node.posture) && (
        <div className="node-posture">
          <div className={`posture-pill ${posture}`}>
            <div className="posture-dot" />
            {node.propagated_status || node.posture}
          </div>
        </div>
      )}
      {node.blocked_by?.length > 0 && (
        <div style={{ marginTop: 8 }}>
          {node.blocked_by.map(b => <span key={b} className="tag">{b}</span>)}
        </div>
      )}
    </div>
  );
}

// ─── Stream ──────────────────────────────────────────────────────────────────
function StreamPage({ events }) {
  return (
    <div>
      <div className="page-header">
        <div className="page-title">Event Stream</div>
        <div className="page-sub"><span className="live-dot" />Live SSE posture events — {events.length} received</div>
      </div>
      <div className="card">
        <div className="card-body">
          {events.length === 0 ? (
            <div className="empty"><div className="empty-icon">▸</div><div className="empty-text">Waiting for events…</div></div>
          ) : (
            <EventLog events={events} />
          )}
        </div>
      </div>
    </div>
  );
}

function EventLog({ events }) {
  return (
    <div className="event-log">
      {events.map((e, i) => {
        const et = (e.event_type || "INFO").toLowerCase();
        const cls = et.includes("transition") ? "transition" : et.includes("fault") || et.includes("violation") ? "fault" : et.includes("recov") ? "recovery" : "info";
        return (
          <div key={i} className="event-entry">
            <div className="event-time">{fmtTime(e.receivedAt || e.emitted_at_ms || Date.now())}</div>
            <div><span className={`event-type ${cls}`}>{e.event_type || "EVENT"}</span></div>
            <div className="event-msg">
              {e.node_id && <span style={{ color: "var(--accent)" }}>{e.node_id} </span>}
              {e.posture && <span style={{ color: "var(--nominal)" }}>{e.posture?.propagated_status || JSON.stringify(e.posture)}</span>}
            </div>
          </div>
        );
      })}
    </div>
  );
}

// ─── Audit ───────────────────────────────────────────────────────────────────
function AuditPage({ audit, onRefresh }) {
  return (
    <div>
      <div className="page-header" style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start" }}>
        <div>
          <div className="page-title">Audit Chain</div>
          <div className="page-sub">Tamper-evident SHA-256 hash chain</div>
        </div>
        <button className="btn btn-ghost btn-sm" onClick={onRefresh}>↻ Verify</button>
      </div>
      {!audit ? (
        <div className="loading"><span className="spin">↻</span>&nbsp;Loading…</div>
      ) : audit.error ? (
        <div className="alert error">{audit.error}</div>
      ) : (
        <div className="card">
          <div className="card-body">
            <table className="table">
              <tbody>
                <tr><td className="mono" style={{ color: "var(--muted)", width: 200 }}>Chain Intact</td>
                  <td><span className={`trust-badge ${audit.chain_intact ? "trusted" : "untrusted"}`}>{audit.chain_intact ? "VERIFIED" : "BROKEN"}</span></td></tr>
                <tr><td className="mono" style={{ color: "var(--muted)" }}>Total Entries</td><td className="mono">{audit.total_entries}</td></tr>
                <tr><td className="mono" style={{ color: "var(--muted)" }}>Latest Hash</td><td className="mono" style={{ wordBreak: "break-all", fontSize: 11 }}>{audit.latest_hash || "—"}</td></tr>
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}

// ─── Register Node ───────────────────────────────────────────────────────────
function RegisterPage({ api, onSuccess }) {
  const [form, setForm] = useState({ node_id: "", ak_pem: "", pcr16: "" });
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(null);
  const set = (k, v) => setForm(f => ({ ...f, [k]: v }));

  const submit = async () => {
    if (!form.node_id.trim()) { setError("Node ID is required"); return; }
    setLoading(true); setError(null);
    try {
      await api.registerNode(form);
      onSuccess(`Node "${form.node_id}" registered successfully`);
      setForm({ node_id: "", ak_pem: "", pcr16: "" });
    } catch (e) { setError(e.message); }
    finally { setLoading(false); }
  };

  return (
    <div>
      <div className="page-header">
        <div className="page-title">Register Node</div>
        <div className="page-sub">Add a new device to the fleet registry</div>
      </div>
      <div className="card">
        <div className="card-body">
          {error && <div className="alert error">{error}</div>}
          <div className="field">
            <label>Node ID *</label>
            <input value={form.node_id} onChange={e => set("node_id", e.target.value)}
              placeholder="lidar_front, camera_left, gps_primary…" />
          </div>
          <div className="field">
            <label>AK Public Key PEM (optional)</label>
            <textarea value={form.ak_pem} onChange={e => set("ak_pem", e.target.value)}
              placeholder="-----BEGIN PUBLIC KEY-----&#10;…" />
          </div>
          <div className="field">
            <label>PCR16 Value (optional)</label>
            <input value={form.pcr16} onChange={e => set("pcr16", e.target.value)}
              placeholder="Hex-encoded PCR16 measurement" />
          </div>
          <div className="form-actions">
            <button className="btn btn-primary" onClick={submit} disabled={loading}>
              {loading ? "Registering…" : "Register Node"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

// ─── Dependencies ────────────────────────────────────────────────────────────
function DepsPage({ api, nodes, onSuccess }) {
  const [nodeId, setNodeId] = useState("");
  const [deps, setDeps]     = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError]   = useState(null);

  const submit = async () => {
    if (!nodeId.trim()) { setError("Node ID is required"); return; }
    const depends_on = deps.split(",").map(s => s.trim()).filter(Boolean);
    if (!depends_on.length) { setError("At least one dependency is required"); return; }
    setLoading(true); setError(null);
    try {
      await api.registerDeps({ node_id: nodeId, depends_on });
      onSuccess(`Dependencies registered for "${nodeId}"`);
      setNodeId(""); setDeps("");
    } catch (e) { setError(e.message); }
    finally { setLoading(false); }
  };

  return (
    <div>
      <div className="page-header">
        <div className="page-title">Dependencies</div>
        <div className="page-sub">Define the trust dependency graph between nodes</div>
      </div>
      <div className="card">
        <div className="card-body">
          {error && <div className="alert error">{error}</div>}
          <div className="field">
            <label>Node ID</label>
            {nodes.length > 0 ? (
              <select value={nodeId} onChange={e => setNodeId(e.target.value)}>
                <option value="">Select a node…</option>
                {nodes.map((n, i) => <option key={i} value={n.node_id || n.id}>{n.node_id || n.id}</option>)}
              </select>
            ) : (
              <input value={nodeId} onChange={e => setNodeId(e.target.value)} placeholder="perception_fusion" />
            )}
          </div>
          <div className="field">
            <label>Depends On (comma-separated)</label>
            <input value={deps} onChange={e => setDeps(e.target.value)}
              placeholder="lidar_front, camera_front, imu_primary" />
          </div>
          <div style={{ padding: "12px 16px", background: "var(--surface2)", borderRadius: 6, marginBottom: 16, fontSize: 12, color: "var(--muted)", fontFamily: "var(--mono)" }}>
            Example: perception_fusion depends on lidar_front, camera_front<br />
            If lidar_front becomes Untrusted → perception_fusion → Degraded
          </div>
          <button className="btn btn-primary" onClick={submit} disabled={loading}>
            {loading ? "Registering…" : "Register Dependencies"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ─── Sensor Report ───────────────────────────────────────────────────────────
function SensorPage({ api, nodes, onSuccess }) {
  const [form, setForm] = useState({ source_node_id: "", confidence_score: "0.95", hardware_fault_detected: false });
  const [loading, setLoading] = useState(false);
  const [error, setError]     = useState(null);
  const set = (k, v) => setForm(f => ({ ...f, [k]: v }));

  const submit = async () => {
    if (!form.source_node_id) { setError("Node ID is required"); return; }
    setLoading(true); setError(null);
    try {
      await api.sensorReport({ ...form, confidence_score: parseFloat(form.confidence_score) });
      onSuccess(`Sensor report submitted for "${form.source_node_id}"`);
    } catch (e) { setError(e.message); }
    finally { setLoading(false); }
  };

  return (
    <div>
      <div className="page-header">
        <div className="page-title">Sensor Report</div>
        <div className="page-sub">Submit a health report for a sensor node</div>
      </div>
      <div className="card">
        <div className="card-body">
          {error && <div className="alert error">{error}</div>}
          <div className="field">
            <label>Node ID</label>
            {nodes.length > 0 ? (
              <select value={form.source_node_id} onChange={e => set("source_node_id", e.target.value)}>
                <option value="">Select a node…</option>
                {nodes.map((n, i) => <option key={i} value={n.node_id || n.id}>{n.node_id || n.id}</option>)}
              </select>
            ) : (
              <input value={form.source_node_id} onChange={e => set("source_node_id", e.target.value)} placeholder="lidar_front" />
            )}
          </div>
          <div className="form-grid">
            <div className="field">
              <label>Confidence Score (0.0 – 1.0)</label>
              <input type="number" min="0" max="1" step="0.01"
                value={form.confidence_score} onChange={e => set("confidence_score", e.target.value)} />
            </div>
            <div className="field">
              <label>Hardware Fault</label>
              <select value={form.hardware_fault_detected ? "true" : "false"}
                onChange={e => set("hardware_fault_detected", e.target.value === "true")}>
                <option value="false">No fault detected</option>
                <option value="true">Hardware fault detected</option>
              </select>
            </div>
          </div>
          <div style={{ padding: "12px 16px", background: "var(--surface2)", borderRadius: 6, marginBottom: 16, fontSize: 12, color: "var(--muted)" }}>
            Confidence &lt; 0.70 or hardware fault → node marked Untrusted → posture recalculated<br />
            5 consecutive healthy reports required to restore trust (hysteresis)
          </div>
          <button className="btn btn-primary" onClick={submit} disabled={loading}>
            {loading ? "Submitting…" : "Submit Report"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ─── Fabric ──────────────────────────────────────────────────────────────────
function postureColor(posture) {
  if (!posture) return "#64748b";
  const s = typeof posture === "string" ? posture.toLowerCase() : JSON.stringify(posture).toLowerCase();
  if (s === "nominal") return "#22c55e";
  if (s === "degraded") return "#f59e0b";
  if (s.includes("locked")) return "#ef4444";
  return "#64748b";
}

function FabricPage({ fabricState, onRefresh }) {
  const assets = fabricState?.assets || [];
  const nominalCount    = fabricState?.nominal_count    ?? 0;
  const degradedCount   = fabricState?.degraded_count   ?? 0;
  const lockedOutCount  = fabricState?.locked_out_count ?? 0;
  const totalAssets     = fabricState?.total_assets     ?? 0;

  return (
    <div>
      <div className="page-header" style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start" }}>
        <div>
          <div className="page-title">Fabric</div>
          <div className="page-sub">Multi-Asset Safety Fabric — cross-asset trust posture</div>
        </div>
        <button className="btn btn-ghost btn-sm" onClick={onRefresh}>↻ Refresh</button>
      </div>

      {!fabricState ? (
        <div className="loading"><span className="spin">↻</span>&nbsp;Loading fabric state…</div>
      ) : (
        <>
          <div className="stats-row">
            <div className="stat-card">
              <div className="stat-label">Total Assets</div>
              <div className="stat-value blue">{totalAssets}</div>
            </div>
            <div className="stat-card">
              <div className="stat-label">Nominal</div>
              <div className="stat-value green">{nominalCount}</div>
            </div>
            <div className="stat-card">
              <div className="stat-label">Degraded</div>
              <div className="stat-value yellow">{degradedCount}</div>
            </div>
            <div className="stat-card">
              <div className="stat-label">Locked Out</div>
              <div className="stat-value red">{lockedOutCount}</div>
            </div>
          </div>

          {assets.length === 0 ? (
            <div className="card">
              <div className="card-body">
                <div className="empty">
                  <div className="empty-icon">⬙</div>
                  <div className="empty-text">No fabric assets registered yet.<br />Use POST /fabric/assets/register to add assets.</div>
                </div>
              </div>
            </div>
          ) : (
            <div className="card">
              <div className="card-header">
                <div className="card-title">FABRIC ASSETS</div>
                <div style={{ fontSize: 11, color: "var(--muted)", fontFamily: "var(--mono)" }}>
                  gen {fabricState.fabric_generation}
                </div>
              </div>
              <div className="card-body">
                <div className="node-grid">
                  {assets.map((a, i) => {
                    const color = postureColor(a.posture);
                    const postureStr = typeof a.posture === "string" ? a.posture : JSON.stringify(a.posture);
                    return (
                      <div key={i} className="node-card" style={{ borderLeft: `3px solid ${color}` }}>
                        <div className="node-header">
                          <div>
                            <div className="node-id">{a.asset_id}</div>
                            <div className="node-type">{a.asset_type || ""}</div>
                          </div>
                          <div style={{
                            fontFamily: "var(--mono)", fontSize: 10, fontWeight: 700,
                            padding: "3px 8px", borderRadius: 3, textTransform: "uppercase",
                            letterSpacing: "0.08em", background: `${color}22`, color,
                          }}>
                            {postureStr}
                          </div>
                        </div>
                        {a.blocked_by?.length > 0 && (
                          <div style={{ marginTop: 8 }}>
                            {a.blocked_by.map(b => <span key={b} className="tag">{b}</span>)}
                          </div>
                        )}
                        <div className="node-meta" style={{ marginTop: 8 }}>
                          gen {a.generation}
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>
            </div>
          )}
        </>
      )}
    </div>
  );
}

// ─── AV Metadata ─────────────────────────────────────────────────────────────
function AVMetaPage({ api, nodes, onSuccess }) {
  const [form, setForm] = useState({ node_id: "", subsystem_type: "Perception", hardware_id: "", confidence_floor: "0.70" });
  const [loading, setLoading] = useState(false);
  const [error, setError]     = useState(null);
  const set = (k, v) => setForm(f => ({ ...f, [k]: v }));

  const submit = async () => {
    if (!form.node_id) { setError("Node ID is required"); return; }
    setLoading(true); setError(null);
    try {
      await api.registerAV({ ...form, confidence_floor: parseFloat(form.confidence_floor) });
      onSuccess(`AV metadata registered for "${form.node_id}"`);
    } catch (e) { setError(e.message); }
    finally { setLoading(false); }
  };

  return (
    <div>
      <div className="page-header">
        <div className="page-title">AV Metadata</div>
        <div className="page-sub">Register autonomous vehicle subsystem classification</div>
      </div>
      <div className="card">
        <div className="card-body">
          {error && <div className="alert error">{error}</div>}
          <div className="field">
            <label>Node ID</label>
            {nodes.length > 0 ? (
              <select value={form.node_id} onChange={e => set("node_id", e.target.value)}>
                <option value="">Select a node…</option>
                {nodes.map((n, i) => <option key={i} value={n.node_id || n.id}>{n.node_id || n.id}</option>)}
              </select>
            ) : (
              <input value={form.node_id} onChange={e => set("node_id", e.target.value)} placeholder="lidar_front" />
            )}
          </div>
          <div className="form-grid">
            <div className="field">
              <label>Subsystem Type</label>
              <select value={form.subsystem_type} onChange={e => set("subsystem_type", e.target.value)}>
                <option>Perception</option>
                <option>Planning</option>
                <option>Actuation</option>
                <option>Positioning</option>
              </select>
            </div>
            <div className="field">
              <label>Hardware ID / Serial</label>
              <input value={form.hardware_id} onChange={e => set("hardware_id", e.target.value)} placeholder="LIDAR-SN-001" />
            </div>
          </div>
          <div className="field">
            <label>Confidence Floor (default 0.70)</label>
            <input type="number" min="0" max="1" step="0.01"
              value={form.confidence_floor} onChange={e => set("confidence_floor", e.target.value)} />
          </div>
          <button className="btn btn-primary" onClick={submit} disabled={loading}>
            {loading ? "Registering…" : "Register AV Metadata"}
          </button>
        </div>
      </div>
    </div>
  );
}
