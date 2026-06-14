// ============================================================
// KIRRA — logo background knockout
// mix-blend-mode can't reach the lattice behind the fixed nav, so the PNG's
// dark square shows as a hard box. Instead, chroma-key the near-black pixels
// to transparent at runtime (soft alpha by luminance) and reuse the result
// for every logo. Same-origin image → canvas stays untainted.
// ============================================================

function maskLogo(src) {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.crossOrigin = 'anonymous';
    img.onload = () => {
      try {
        const c = document.createElement('canvas');
        c.width = img.naturalWidth;
        c.height = img.naturalHeight;
        const ctx = c.getContext('2d');
        ctx.drawImage(img, 0, 0);
        const d = ctx.getImageData(0, 0, c.width, c.height);
        const p = d.data;
        for (let i = 0; i < p.length; i += 4) {
          const lum = Math.max(p[i], p[i + 1], p[i + 2]); // brightest channel
          let a = (lum - 10) * 3;                          // dark → transparent, soft edge
          p[i + 3] = a < 0 ? 0 : a > 255 ? 255 : a;
        }
        ctx.putImageData(d, 0, 0);
        resolve(c.toDataURL('image/png'));
      } catch (e) { reject(e); }
    };
    img.onerror = reject;
    img.src = src;
  });
}

async function applyMask() {
  const slots = document.querySelectorAll('.nav__logo, .preloader__logo');
  if (!slots.length) return;
  const src = slots[0].getAttribute('src');
  if (!src) return;
  try {
    const url = await maskLogo(src);
    slots.forEach((el) => { el.src = url; });
  } catch { /* leave the original PNG on failure */ }
}

if (document.readyState !== 'loading') applyMask();
else document.addEventListener('DOMContentLoaded', applyMask);
