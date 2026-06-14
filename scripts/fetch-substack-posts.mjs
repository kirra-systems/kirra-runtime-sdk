// Build-time fetch of the latest Substack posts → website/posts.json.
// Runs in the Pages deploy workflow (Node 20+, has global fetch). Pure stdlib,
// no dependencies. Resilient by design: any failure exits 0 and leaves the
// existing posts.json untouched, so the deploy never breaks.

import { writeFileSync } from 'node:fs';

const FEED = 'https://justinlooney.substack.com/feed';
const OUT = 'website/posts.json';
const MAX = 4;

function decodeEntities(s = '') {
  return s
    .replace(/<!\[CDATA\[([\s\S]*?)\]\]>/g, '$1')
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#0?39;/g, "'")
    .replace(/&#x27;/gi, "'")
    .replace(/&#x2019;/gi, '’')
    .trim();
}

function tag(block, name) {
  const m = block.match(new RegExp(`<${name}[^>]*>([\\s\\S]*?)</${name}>`, 'i'));
  return m ? decodeEntities(m[1]) : '';
}

function snippet(html, n = 150) {
  const text = decodeEntities(html).replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim();
  return text.length > n ? text.slice(0, n).replace(/\s+\S*$/, '') + '…' : text;
}

function firstImage(block) {
  // Substack item cover lives in <enclosure>, sometimes <media:content>,
  // otherwise the first <img> inside the CDATA description/content.
  let m = block.match(/<enclosure[^>]*url="([^"]+)"[^>]*type="image/i);
  if (m) return decodeEntities(m[1]);
  m = block.match(/<media:content[^>]*url="([^"]+)"/i);
  if (m && /\.(jpe?g|png|webp|gif|avif)/i.test(m[1])) return decodeEntities(m[1]);
  m = block.match(/<img[^>]*\bsrc="([^"]+)"/i);
  if (m) return decodeEntities(m[1]);
  return '';
}

try {
  const res = await fetch(FEED, {
    headers: {
      'User-Agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36',
      'Accept': 'application/rss+xml, application/xml;q=0.9, text/xml;q=0.8, */*;q=0.5',
    },
  });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const xml = await res.text();

  const posts = [...xml.matchAll(/<item>([\s\S]*?)<\/item>/g)]
    .slice(0, MAX)
    .map((m) => {
      const b = m[1];
      let date = '';
      const pub = tag(b, 'pubDate');
      if (pub) {
        const d = new Date(pub);
        if (!Number.isNaN(d.getTime())) {
          date = d.toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' });
        }
      }
      return {
        title: tag(b, 'title'),
        link: tag(b, 'link'),
        date,
        excerpt: snippet(tag(b, 'description')),
        image: firstImage(b),
      };
    })
    .filter((p) => p.title && /^https?:\/\//.test(p.link));

  writeFileSync(OUT, JSON.stringify(posts, null, 2) + '\n');
  console.log(`Substack: wrote ${posts.length} post(s) to ${OUT}`);
} catch (err) {
  console.warn(`Substack fetch skipped (${err.message}); keeping existing ${OUT}.`);
  process.exit(0); // never fail the deploy over an external feed
}
