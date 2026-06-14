// ============================================================
// KIRRA — live action_filter.py terminal
// Types real Kirra usage, then "runs" it: the Governor evaluating LLM
// actions against live posture and printing verdicts (ALLOW / CLAMP /
// DENY), with a blinking cursor. Loops. Respects prefers-reduced-motion.
// ============================================================

const LINES = [
  { t: '$ python action_filter.py', c: 't-prompt' },
  { t: '', c: '' },
  { t: 'import kirra', c: 't-code' },
  { t: 'gov = kirra.Governor("verifier.fleet.local")', c: 't-code' },
  { t: '', c: '' },
  { t: '# every action is judged against live fleet posture', c: 't-dim' },
  { t: 'gov.evaluate("robot-07", cmd_vel=1.2)', c: 't-code' },
  { t: '  ✓ ALLOW   Nominal · dispatched', c: 't-ok' },
  { t: 'gov.evaluate("robot-07", cmd_vel=999)', c: 't-code' },
  { t: '  ⊘ CLAMP   999 → 2.0 m/s · envelope cap', c: 't-warn' },
  { t: 'gov.evaluate("robot-07", "drive_to_moon")', c: 't-code' },
  { t: '  ✕ DENY    unknown action · blocked', c: 't-bad' },
  { t: 'gov.evaluate("robot-07", cmd_vel=1.2)   # Degraded', c: 't-code' },
  { t: '  ✕ DENY    kinetic write blocked', c: 't-bad' },
  { t: '', c: '' },
  { t: "# fail-closed — if trust isn't proven, nothing moves", c: 't-dim' },
];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function startTerminal() {
  const el = document.getElementById('termBody');
  if (!el) return;
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const cursor = document.createElement('span');
  cursor.className = 't-cursor';

  function renderStatic() {
    el.textContent = '';
    for (const line of LINES) {
      const div = document.createElement('div');
      div.className = 't-line ' + line.c;
      div.textContent = line.t || ' ';
      el.appendChild(div);
    }
  }

  async function run() {
    el.textContent = '';
    for (const line of LINES) {
      const div = document.createElement('div');
      div.className = 't-line ' + line.c;
      el.appendChild(div);
      div.appendChild(cursor);
      if (/t-(ok|warn|bad)/.test(line.c)) await sleep(360); // "executing…"
      for (const ch of line.t) {
        div.insertBefore(document.createTextNode(ch), cursor);
        await sleep(line.c === 't-ok' || line.c === 't-warn' || line.c === 't-bad' ? 12 : 17);
      }
      await sleep(line.t ? 70 : 40);
    }
    await sleep(2600);
    run();
  }

  if (reduced) { renderStatic(); return; }

  // start once the section scrolls into view
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
