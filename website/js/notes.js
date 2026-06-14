// ============================================================
// KIRRA — Field Notes feed
// Renders the latest Substack posts as cards. Substack blocks datacenter
// IPs (so the build-time fetch in CI 403s), therefore we fetch CLIENT-SIDE
// from the visitor's browser through CORS-friendly reader proxies, trying:
//   1. posts.json   (baked at build time — used if CI ever succeeds)
//   2. rss2json     (parsed JSON + thumbnails)
//   3. allorigins   (raw XML, parsed here)
// If all fail, the section's subscribe CTA remains as the fallback.
// ============================================================

const FEED = 'https://justinlooney.substack.com/feed';
const MAX = 4;

function fmtDate(s) {
  if (!s) return '';
  const d = new Date(s);
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' });
}
function strip(html, n = 160) {
  const t = (html || '').replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim();
  return t.length > n ? t.slice(0, n).replace(/\s+\S*$/, '') + '…' : t;
}
function imgFrom(html) {
  const m = (html || '').match(/<img[^>]*\bsrc="([^"]+)"/i);
  return m ? m[1] : '';
}

async function fromPostsJson() {
  try {
    const r = await fetch('posts.json', { cache: 'no-cache' });
    if (!r.ok) return null;
    const a = await r.json();
    return Array.isArray(a) && a.length ? a.slice(0, MAX) : null;
  } catch { return null; }
}

async function fromRss2Json() {
  try {
    const r = await fetch('https://api.rss2json.com/v1/api.json?rss_url=' + encodeURIComponent(FEED));
    if (!r.ok) return null;
    const j = await r.json();
    if (j.status !== 'ok' || !Array.isArray(j.items) || !j.items.length) return null;
    return j.items.slice(0, MAX).map((it) => ({
      title: it.title,
      link: it.link,
      date: fmtDate(it.pubDate),
      excerpt: strip(it.description || it.content),
      image: it.thumbnail || (it.enclosure && it.enclosure.link) || imgFrom(it.content || it.description),
    }));
  } catch { return null; }
}

async function fromAllOrigins() {
  try {
    const r = await fetch('https://api.allorigins.win/raw?url=' + encodeURIComponent(FEED));
    if (!r.ok) return null;
    const xml = new DOMParser().parseFromString(await r.text(), 'text/xml');
    const items = [...xml.querySelectorAll('item')].slice(0, MAX);
    if (!items.length) return null;
    return items.map((it) => {
      const get = (sel) => (it.querySelector(sel) ? it.querySelector(sel).textContent : '') || '';
      const desc = get('description') || get('encoded');
      const enc = it.querySelector('enclosure') ? it.querySelector('enclosure').getAttribute('url') : '';
      return { title: get('title'), link: get('link'), date: fmtDate(get('pubDate')), excerpt: strip(desc), image: enc || imgFrom(desc) };
    });
  } catch { return null; }
}

function render(grid, posts) {
  const clean = posts.filter((p) => p && typeof p.link === 'string' && /^https?:\/\//.test(p.link) && p.title);
  if (!clean.length) return false;
  grid.textContent = '';
  for (const p of clean) {
    const card = document.createElement('a');
    card.className = 'note-card';
    card.href = p.link; card.target = '_blank'; card.rel = 'noopener';

    if (p.image && /^https?:\/\//.test(p.image)) {
      const img = document.createElement('img');
      img.className = 'note-card__img';
      img.src = p.image; img.alt = ''; img.loading = 'lazy';
      img.addEventListener('error', () => img.remove());
      card.appendChild(img);
    }
    const body = document.createElement('div');
    body.className = 'note-card__body';
    if (p.date) { const d = document.createElement('span'); d.className = 'note-card__date'; d.textContent = p.date; body.appendChild(d); }
    const t = document.createElement('span'); t.className = 'note-card__title'; t.textContent = p.title; body.appendChild(t);
    if (p.excerpt) { const e = document.createElement('span'); e.className = 'note-card__excerpt'; e.textContent = p.excerpt; body.appendChild(e); }
    const m = document.createElement('span'); m.className = 'note-card__more'; m.textContent = 'Read on Substack ↗'; body.appendChild(m);
    card.appendChild(body);
    grid.appendChild(card);
  }
  grid.classList.add('is-loaded');
  if (window.ScrollTrigger) window.ScrollTrigger.refresh();
  return true;
}

async function loadNotes() {
  const grid = document.getElementById('notesGrid');
  if (!grid) return;
  for (const src of [fromPostsJson, fromRss2Json, fromAllOrigins]) {
    try {
      const posts = await src();
      if (posts && render(grid, posts)) return;
    } catch { /* try next source */ }
  }
  // all sources failed — keep the subscribe CTA as the fallback
}

if (document.readyState !== 'loading') loadNotes();
else document.addEventListener('DOMContentLoaded', loadNotes);
