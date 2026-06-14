// ============================================================
// KIRRA — live action_filter.py terminal
// Types real Kirra usage (syntax-highlighted) then "runs" it: the Governor
// evaluating LLM actions against live posture, printing ALLOW / CLAMP / DENY
// with a blinking cursor. Loops, starts on scroll, reduced-motion safe.
// Code tokens reuse the site's c-kw / c-fn / c-str / c-num / c-cm colours.
// ============================================================

// line = { c?: lineColourClass, text?: string }  — single colour
//      | { segs: [ [text, tokenClass], ... ] }   — syntax-coloured code
const LINES = [
  { c: 't-prompt', text: '$ cargo run --example action_filter' },
  { text: '' },
  { segs: [['use ', 'c-kw'], ['kirra::action_filter::', ''], ['evaluate_action_claim', 'c-fn'], [';', '']] },
  { segs: [['use ', 'c-kw'], ['kirra::verifier::FleetPosture', ''], [';', '']] },
  { text: '' },
  { c: 'c-cm', text: '// every agent action is judged against live posture' },
  { segs: [['let ', 'c-kw'], ['d = ', ''], ['evaluate_action_claim', 'c-fn'], ['(claim, ', ''], ['FleetPosture::Nominal', 'c-num'], [')', '']] },
  { c: 't-ok', text: '  ✓ allowed=true    NOMINAL_VALID_KINEMATICS' },
  { c: 'c-cm', text: '// claim.payload = { "linear": 999.0 }' },
  { c: 't-bad', text: '  ✕ allowed=false   KINEMATIC_ENVELOPE_BREACH' },
  { c: 'c-cm', text: '// claim.action_type = "drive_to_moon"' },
  { c: 't-bad', text: '  ✕ allowed=false   UNKNOWN_ACTION_TYPE' },
  { c: 'c-cm', text: '// posture = FleetPosture::Degraded' },
  { c: 't-bad', text: '  ✕ allowed=false   DEGRADED_POSTURE_KINETIC_DENIED' },
  { c: 'c-cm', text: '// claim.action_type = "read_telemetry"' },
  { c: 't-ok', text: '  ✓ allowed=true    DEGRADED_READ_ONLY_PERMITTED' },
  { text: '' },
  { c: 'c-cm', text: "// fail-closed — if trust isn't proven, nothing moves" },
];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function startTerminal() {
  const el = document.getElementById('termBody');
  if (!el) return;
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const cursor = document.createElement('span');
  cursor.className = 't-cursor';

  function lineSegs(line) { return line.segs || [[line.text || '', '']]; }

  function renderStatic() {
    el.textContent = '';
    for (const line of LINES) {
      const div = document.createElement('div');
      div.className = 't-line ' + (line.c || '');
      for (const [text, cls] of lineSegs(line)) {
        if (cls) { const s = document.createElement('span'); s.className = cls; s.textContent = text; div.appendChild(s); }
        else if (text) div.appendChild(document.createTextNode(text));
      }
      el.appendChild(div);
    }
  }

  async function typeLine(line) {
    const div = document.createElement('div');
    div.className = 't-line ' + (line.c || '');
    el.appendChild(div);
    div.appendChild(cursor);
    const isOut = /t-(ok|warn|bad)/.test(line.c || '');
    if (isOut) await sleep(360); // "executing…"
    for (const [text, cls] of lineSegs(line)) {
      let span = null;
      if (cls) { span = document.createElement('span'); span.className = cls; div.insertBefore(span, cursor); }
      for (const ch of text) {
        const node = document.createTextNode(ch);
        if (span) span.appendChild(node);
        else div.insertBefore(node, cursor);
        await sleep(isOut ? 12 : 18);
      }
    }
    await sleep(80);
  }

  async function run() {
    el.textContent = '';
    for (const line of LINES) await typeLine(line);
    await sleep(2600);
    run();
  }

  if (reduced) { renderStatic(); return; }

  let started = false;
  const io = new IntersectionObserver((entries) => {
    if (entries.some((e) => e.isIntersecting) && !started) {
      started = true;
      io.disconnect();
      run();
    }
  }, { threshold: 0.2 });
  io.observe(el);
}

if (document.readyState !== 'loading') startTerminal();
else document.addEventListener('DOMContentLoaded', startTerminal);
