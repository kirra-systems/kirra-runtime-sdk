// ============================================================
// KIRRA — interaction + scroll choreography (GSAP)
// Drives the preloader, custom cursor, magnetic UI, scroll
// reveals, and the pinned posture section that pushes
// colour state into the WebGL lattice (window.KirraScene).
// ============================================================

const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

function boot() {
  const gsap = window.gsap;
  if (!gsap) { return void setTimeout(boot, 50); } // wait for CDN

  // Always resolve at the top — don't let the browser restore a scroll offset.
  if ('scrollRestoration' in history) history.scrollRestoration = 'manual';
  window.scrollTo(0, 0);

  const { ScrollTrigger } = window;
  if (window.ScrollTrigger) gsap.registerPlugin(window.ScrollTrigger);
  const ST = window.ScrollTrigger;

  // ---------- initial hidden states (set only once gsap exists) ----------
  if (!reduced) {
    gsap.set('[data-fade]', { y: 26, opacity: 0 });
    gsap.set('[data-reveal]', { y: 40, opacity: 0 });
    gsap.set('.hero__title .word', { yPercent: 115 });
  }

  // ---------- Preloader ----------
  const pre = document.getElementById('preloader');
  const fill = document.getElementById('preloaderFill');
  const pct = document.getElementById('preloaderPct');
  const status = document.getElementById('preloaderStatus');
  const statusLines = ['VERIFYING FLEET POSTURE', 'LOADING TRUST LATTICE', 'ATTESTING NODES', 'POSTURE: NOMINAL'];

  const counter = { v: 0 };
  const sceneReady = (window.KirraScene && window.KirraScene.whenReady) || Promise.resolve();
  let sceneDone = false;
  sceneReady.then(() => { sceneDone = true; });

  function finishPreloader() {
    window.scrollTo(0, 0);
    const tl = gsap.timeline();
    tl.to(pre, { yPercent: -100, duration: 1, ease: 'power4.inOut', onComplete: () => pre && (pre.style.display = 'none') })
      .add(introHero, '-=0.5');
  }

  // progress crawls to 90%, then completes when the scene is ready
  gsap.to(counter, {
    v: 90, duration: 1.4, ease: 'power1.inOut',
    onUpdate() {
      const p = Math.round(counter.v);
      if (fill) fill.style.width = p + '%';
      if (pct) pct.textContent = p + '%';
      if (status) status.textContent = statusLines[Math.min(statusLines.length - 1, Math.floor(p / 25))];
    },
    onComplete() {
      const wait = () => {
        if (sceneDone) {
          gsap.to(counter, {
            v: 100, duration: 0.5, ease: 'power2.out',
            onUpdate() { const p = Math.round(counter.v); if (fill) fill.style.width = p + '%'; if (pct) pct.textContent = p + '%'; },
            onComplete() { if (status) status.textContent = 'POSTURE: NOMINAL'; gsap.delayedCall(0.25, finishPreloader); },
          });
        } else { gsap.delayedCall(0.1, wait); }
      };
      wait();
    },
  });
  // hard safety net: never trap the user behind the preloader
  gsap.delayedCall(6, () => { if (pre && pre.style.display !== 'none') { pre.style.display = 'none'; introHero(); } });

  // ---------- Hero intro ----------
  let heroPlayed = false;
  function introHero() {
    if (heroPlayed) return; heroPlayed = true;
    if (reduced) return;
    gsap.timeline({ defaults: { ease: 'power4.out' } })
      .to('.hero__title .word', { yPercent: 0, duration: 1.1, stagger: 0.07 })
      .to('.hero__eyebrow', { y: 0, opacity: 1, duration: 0.7 }, 0.2)
      .to('.hero__lede', { y: 0, opacity: 1, duration: 0.8 }, '-=0.7')
      .to('.hero__actions', { y: 0, opacity: 1, duration: 0.8 }, '-=0.6')
      .to('.hero__flow', { y: 0, opacity: 1, duration: 0.8 }, '-=0.6');
  }

  // ---------- Custom cursor ----------
  const cursor = document.getElementById('cursor');
  const dot = document.getElementById('cursorDot');
  if (cursor && dot && window.matchMedia('(pointer:fine)').matches) {
    const xTo = gsap.quickTo(cursor, 'x', { duration: 0.45, ease: 'power3' });
    const yTo = gsap.quickTo(cursor, 'y', { duration: 0.45, ease: 'power3' });
    const dxTo = gsap.quickTo(dot, 'x', { duration: 0.12, ease: 'power3' });
    const dyTo = gsap.quickTo(dot, 'y', { duration: 0.12, ease: 'power3' });
    window.addEventListener('pointermove', (e) => { xTo(e.clientX); yTo(e.clientY); dxTo(e.clientX); dyTo(e.clientY); });
    document.querySelectorAll('a, button, [data-magnetic]').forEach((el) => {
      el.addEventListener('pointerenter', () => cursor.classList.add('is-hover'));
      el.addEventListener('pointerleave', () => cursor.classList.remove('is-hover'));
    });
  }

  // ---------- Magnetic elements ----------
  if (!reduced) {
    document.querySelectorAll('[data-magnetic]').forEach((el) => {
      const strength = 0.4;
      el.addEventListener('pointermove', (e) => {
        const r = el.getBoundingClientRect();
        const mx = e.clientX - (r.left + r.width / 2);
        const my = e.clientY - (r.top + r.height / 2);
        gsap.to(el, { x: mx * strength, y: my * strength, duration: 0.5, ease: 'power3.out' });
      });
      el.addEventListener('pointerleave', () => gsap.to(el, { x: 0, y: 0, duration: 0.6, ease: 'elastic.out(1,0.4)' }));
    });
  }

  // ---------- Nav stuck + scroll progress ----------
  const nav = document.getElementById('nav');
  const prog = document.getElementById('scrollProgress');
  function onScroll() {
    const y = window.scrollY;
    if (nav) nav.classList.toggle('is-stuck', y > 60);
    const max = document.documentElement.scrollHeight - window.innerHeight;
    if (prog) prog.style.width = (max > 0 ? (y / max) * 100 : 0) + '%';
  }
  window.addEventListener('scroll', onScroll, { passive: true });
  onScroll();

  if (reduced || !ST) {
    // no scroll animation — just make everything visible
    gsap.set('[data-fade], [data-reveal]', { clearProps: 'all' });
    revealManifesto(true); countStats(true);
    return;
  }

  // ---------- Scroll reveals ----------
  gsap.utils.toArray('[data-fade]').forEach((el) => {
    if (el.closest('.hero')) return; // hero handled by intro
    gsap.to(el, { y: 0, opacity: 1, duration: 0.9, ease: 'power3.out',
      scrollTrigger: { trigger: el, start: 'top 86%' } });
  });
  ST.batch('[data-reveal]', {
    start: 'top 85%',
    onEnter: (els) => gsap.to(els, { y: 0, opacity: 1, duration: 0.9, stagger: 0.09, ease: 'power3.out', overwrite: true }),
  });

  revealManifesto(false);
  countStats(false);
  setupPosture();

  ST.refresh();
}

// ---------- Manifesto word sweep ----------
function revealManifesto(instant) {
  const gsap = window.gsap;
  const p = document.querySelector('.manifesto__text');
  if (!p) return;
  // split top-level into .word spans, keeping existing .hl elements intact
  const frag = document.createDocumentFragment();
  Array.from(p.childNodes).forEach((node) => {
    if (node.nodeType === Node.TEXT_NODE) {
      node.textContent.split(/(\s+)/).forEach((tok) => {
        if (tok.trim() === '') { frag.appendChild(document.createTextNode(tok)); return; }
        const s = document.createElement('span'); s.className = 'word'; s.style.display = 'inline-block';
        s.textContent = tok; frag.appendChild(s);
      });
    } else {
      node.classList && node.classList.add('word');
      node.style && (node.style.display = 'inline-block');
      frag.appendChild(node);
    }
  });
  p.innerHTML = ''; p.appendChild(frag);
  const words = p.querySelectorAll('.word');
  if (instant || !window.ScrollTrigger) return;
  // Words stay legible at rest (0.55) and brighten to full as the section
  // scrolls through — fully revealed by the time it reaches mid-viewport.
  gsap.set(words, { opacity: 0.55 });
  gsap.to(words, {
    opacity: 1, stagger: 0.05, ease: 'none',
    scrollTrigger: { trigger: p, start: 'top 80%', end: 'center 60%', scrub: 1 },
  });
}

// ---------- Stat counters ----------
function countStats(instant) {
  const gsap = window.gsap;
  document.querySelectorAll('.stat__num[data-count]').forEach((el) => {
    const target = parseFloat(el.getAttribute('data-count'));
    const suffix = el.getAttribute('data-suffix') || '';
    // skip non-numeric placeholders (e.g. "ASIL D")
    if (isNaN(target) || !/^\d/.test(el.textContent.trim())) return;
    const obj = { v: 0 };
    const apply = () => { el.textContent = Math.round(obj.v) + suffix; };
    if (instant || !window.ScrollTrigger) { obj.v = target; apply(); return; }
    gsap.to(obj, { v: target, duration: 1.6, ease: 'power2.out', onUpdate: apply,
      scrollTrigger: { trigger: el, start: 'top 88%' } });
  });
}

// ---------- Pinned posture section → drives the lattice colour ----------
function setupPosture() {
  const gsap = window.gsap; const ST = window.ScrollTrigger;
  const section = document.querySelector('.posture');
  const heading = document.getElementById('postureHeading');
  const cards = gsap.utils.toArray('.pcard');
  if (!section || !heading) return;

  const states = [
    { name: 'Nominal',   color: '#34f5a6', p: 0.0, agit: 0.05 },
    { name: 'Degraded',  color: '#ffb627', p: 0.5, agit: 0.45 },
    { name: 'LockedOut', color: '#ff4d63', p: 1.0, agit: 0.9 },
  ];
  let current = -1;
  function setState(i) {
    if (i === current) return; current = i;
    const s = states[i];
    gsap.to(heading, { color: s.color, duration: 0.5 });
    heading.textContent = s.name;
    cards.forEach((c, idx) => c.classList.toggle('is-active', idx === i));
  }
  setState(0);

  ST.create({
    trigger: section, start: 'top top', end: 'bottom bottom', scrub: true,
    onUpdate(self) {
      const prog = self.progress;            // 0..1 across the 300vh section
      if (window.KirraScene) {
        window.KirraScene.setPosture01(prog);
        window.KirraScene.setAgitation(Math.min(1, prog * 1.0));
      }
      const i = prog < 0.34 ? 0 : prog < 0.67 ? 1 : 2;
      setState(i);
    },
    onLeave() { if (window.KirraScene) { window.KirraScene.setPosture01(0); window.KirraScene.setAgitation(0); } },
    onLeaveBack() { if (window.KirraScene) { window.KirraScene.setPosture01(0); window.KirraScene.setAgitation(0); } },
  });
}

// ---------- Copy button ----------
document.addEventListener('click', (e) => {
  const btn = e.target.closest('[data-copy]');
  if (!btn) return;
  const text = btn.getAttribute('data-copy');
  navigator.clipboard?.writeText(text).then(() => {
    const prev = btn.textContent; btn.textContent = 'Copied'; btn.classList.add('is-copied');
    setTimeout(() => { btn.textContent = prev; btn.classList.remove('is-copied'); }, 1600);
  });
});

if (document.readyState !== 'loading') boot();
else document.addEventListener('DOMContentLoaded', boot);
