/* ============================================================
   KIRRA SYSTEMS — shared behaviors
   Zero dependencies. Everything is progressive enhancement:
   the site is fully readable with JS disabled.
   ============================================================ */
(() => {
  "use strict";
  const $ = (s, c = document) => c.querySelector(s);
  const $$ = (s, c = document) => [...c.querySelectorAll(s)];
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;

  /* ---------- Theme (init happens inline in <head>; this wires the toggle) */
  const toggle = $("#themeToggle");
  if (toggle) {
    toggle.addEventListener("click", () => {
      const cur = document.documentElement.dataset.theme === "light" ? "light" : "dark";
      const next = cur === "light" ? "dark" : "light";
      document.documentElement.dataset.theme = next;
      try { localStorage.setItem("kirra-theme", next); } catch {}
      toggle.setAttribute("aria-pressed", String(next === "light"));
      dispatchEvent(new CustomEvent("kirra:theme", { detail: next }));
    });
  }

  /* ---------- Preloader (armed by an inline script; drives + dismisses) */
  const pre = $("#preloader");
  if (pre && !pre.hidden) {
    const fill = $("#preloaderFill");
    const pct = $("#preloaderPct");
    const status = $("#preloaderStatus");
    const MSGS = ["CHALLENGING NODES", "VERIFYING ATTESTATION", "DERIVING FLEET POSTURE", "POSTURE: NOMINAL"];
    const DUR = 1300;
    const t0 = performance.now();
    const done = () => {
      pre.classList.add("is-done");
      document.documentElement.classList.remove("is-preloading");
      try { sessionStorage.setItem("kirra-preloaded", "1"); } catch {}
      setTimeout(() => pre.remove(), 600);
    };
    const tick = (t) => {
      const p = Math.min((t - t0) / DUR, 1);
      const eased = 1 - Math.pow(1 - p, 3);
      const n = Math.round(eased * 100);
      if (fill) fill.style.width = n + "%";
      if (pct) pct.textContent = n + "%";
      if (status) status.textContent = MSGS[Math.min(Math.floor(p * MSGS.length), MSGS.length - 1)];
      if (p < 1) requestAnimationFrame(tick); else setTimeout(done, 180);
    };
    requestAnimationFrame(tick);
    // hard safety: never hold the page longer than 3 s no matter what
    setTimeout(() => { if (document.documentElement.classList.contains("is-preloading")) done(); }, 3000);
  } else if (pre) pre.remove();

  /* ---------- Nav scroll state + mobile menu ---------- */
  const nav = $("#nav");
  if (nav) {
    const onScroll = () => nav.classList.toggle("is-scrolled", scrollY > 8);
    onScroll();
    addEventListener("scroll", onScroll, { passive: true });
  }
  const burger = $("#navBurger");
  const links = $("#navLinks");
  if (burger && links) {
    burger.addEventListener("click", () => {
      const open = links.classList.toggle("is-open");
      burger.setAttribute("aria-expanded", String(open));
    });
    links.addEventListener("click", (e) => {
      if (e.target.closest("a")) { links.classList.remove("is-open"); burger.setAttribute("aria-expanded", "false"); }
    });
  }

  /* ---------- Reveal on scroll ---------- */
  const revealables = $$("[data-reveal]");
  if (revealables.length && !reduced && "IntersectionObserver" in window) {
    // stagger siblings that share a parent
    const groups = new Map();
    revealables.forEach((el) => {
      const p = el.parentElement;
      if (!groups.has(p)) groups.set(p, 0);
      const i = groups.get(p);
      el.style.setProperty("--reveal-delay", `${Math.min(i * 0.08, 0.4)}s`);
      groups.set(p, i + 1);
    });
    const io = new IntersectionObserver((entries) => {
      for (const en of entries) {
        if (en.isIntersecting) { en.target.classList.add("is-visible"); io.unobserve(en.target); }
      }
    }, { threshold: 0.12, rootMargin: "0px 0px -8% 0px" });
    revealables.forEach((el) => io.observe(el));
  } else {
    revealables.forEach((el) => el.classList.add("is-visible"));
  }

  /* ---------- Animated counters ---------- */
  const counters = $$("[data-count]");
  if (counters.length) {
    const fmt = (n, dec) => n.toLocaleString("en-US", { minimumFractionDigits: dec, maximumFractionDigits: dec });
    const run = (el) => {
      const target = parseFloat(el.dataset.count);
      const dec = (el.dataset.count.split(".")[1] || "").length;
      if (reduced) { el.textContent = fmt(target, dec); return; }
      const dur = 1400; const t0 = performance.now();
      const tick = (t) => {
        const p = Math.min((t - t0) / dur, 1);
        const eased = 1 - Math.pow(1 - p, 4);
        el.textContent = fmt(target * eased, dec);
        if (p < 1) requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    };
    if ("IntersectionObserver" in window) {
      const io = new IntersectionObserver((entries) => {
        for (const en of entries) if (en.isIntersecting) { run(en.target); io.unobserve(en.target); }
      }, { threshold: 0.5 });
      counters.forEach((el) => io.observe(el));
    } else counters.forEach(run);
  }

  /* ---------- Bars animate in when visible ---------- */
  const bars = $$(".barlist__fill, .budget__fill");
  if (bars.length && "IntersectionObserver" in window && !reduced) {
    bars.forEach((b) => { b.dataset.w = b.style.getPropertyValue("--w") || b.getAttribute("data-width") || ""; b.style.setProperty("--w", "0%"); });
    const io = new IntersectionObserver((entries) => {
      for (const en of entries) if (en.isIntersecting) {
        en.target.style.setProperty("--w", en.target.dataset.w);
        io.unobserve(en.target);
      }
    }, { threshold: 0.4 });
    bars.forEach((b) => io.observe(b));
  }

  /* ---------- Tabs (ARIA) ---------- */
  $$("[data-tabs]").forEach((root) => {
    const tabs = $$("[role=tab]", root);
    const panels = $$("[role=tabpanel]", root);
    const select = (tab) => {
      tabs.forEach((t) => {
        const on = t === tab;
        t.setAttribute("aria-selected", String(on));
        t.tabIndex = on ? 0 : -1;
      });
      panels.forEach((p) => { p.hidden = p.id !== tab.getAttribute("aria-controls"); });
    };
    tabs.forEach((t, i) => {
      t.addEventListener("click", () => select(t));
      t.addEventListener("keydown", (e) => {
        let j = null;
        if (e.key === "ArrowRight") j = (i + 1) % tabs.length;
        if (e.key === "ArrowLeft") j = (i - 1 + tabs.length) % tabs.length;
        if (e.key === "Home") j = 0;
        if (e.key === "End") j = tabs.length - 1;
        if (j !== null) { e.preventDefault(); tabs[j].focus(); select(tabs[j]); }
      });
    });
  });

  /* ---------- Posture switch: flips fleet-posture driven UI ---------- */
  $$("[data-posture-demo]").forEach((root) => {
    const buttons = $$(".posture-switch button", root);
    const apply = (posture) => {
      buttons.forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.posture === posture)));
      root.dataset.postureState = posture;
      // verdict cells: data-v-nominal / data-v-degraded / data-v-lockedout hold "allow|gate|deny — label"
      $$("[data-v-nominal]", root).forEach((cell) => {
        const raw = cell.dataset["v" + posture.charAt(0).toUpperCase() + posture.slice(1)] || "";
        const [kind, ...rest] = raw.split("|");
        cell.className = "verdict verdict--" + kind;
        cell.textContent = rest.join("|");
      });
      // SVG elements marked with data-on="nominal degraded" show only in those states
      $$("[data-on]", root).forEach((el) => {
        el.style.opacity = el.dataset.on.includes(posture) ? "" : "0.12";
      });
      const status = $("[data-posture-status]", root);
      if (status) {
        const msgs = {
          nominal: "NOMINAL — all classified commands admitted. Unknown is always denied.",
          degraded: "DEGRADED — reads allowed; actuator motion deferred to the decel-to-stop gate; every other write denied (503).",
          lockedout: "LOCKED OUT — every command denied. Recovery requires operator clearance, not a timer.",
        };
        status.textContent = msgs[posture];
      }
    };
    buttons.forEach((b) => b.addEventListener("click", () => apply(b.dataset.posture)));
    apply(root.dataset.postureInitial || "nominal");
  });

  /* ---------- Copy buttons ---------- */
  $$("[data-copy]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const src = $(btn.dataset.copy);
      if (!src) return;
      try {
        await navigator.clipboard.writeText(src.textContent.trim());
        const old = btn.textContent; btn.textContent = "copied";
        setTimeout(() => (btn.textContent = old), 1400);
      } catch {}
    });
  });

  /* ---------- Field Notes feed (posts.json → RSS proxies fallback) ---------- */
  const notesGrid = $("[data-notes]");
  if (notesGrid) {
    const FEED = "https://justinlooney.substack.com/feed";
    const MAX = parseInt(notesGrid.dataset.notesMax || "4", 10);
    const fmtDate = (s) => {
      const d = new Date(s || "");
      return Number.isNaN(d.getTime()) ? "" : d.toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
    };
    const strip = (html, n = 150) => {
      const t = (html || "").replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim();
      return t.length > n ? t.slice(0, n).replace(/\s+\S*$/, "") + "…" : t;
    };
    const imgFrom = (html) => ((html || "").match(/<img[^>]*\bsrc="([^"]+)"/i) || [])[1] || "";
    const fromPostsJson = async () => {
      try {
        const r = await fetch(notesGrid.dataset.notesSrc || "posts.json", { cache: "no-cache" });
        if (!r.ok) return null;
        const a = await r.json();
        return Array.isArray(a) && a.length ? a.slice(0, MAX) : null;
      } catch { return null; }
    };
    const fromRss2Json = async () => {
      try {
        const r = await fetch("https://api.rss2json.com/v1/api.json?rss_url=" + encodeURIComponent(FEED));
        if (!r.ok) return null;
        const j = await r.json();
        if (j.status !== "ok" || !j.items?.length) return null;
        return j.items.slice(0, MAX).map((it) => ({
          title: it.title, link: it.link, date: fmtDate(it.pubDate),
          excerpt: strip(it.description || it.content),
          image: it.thumbnail || it.enclosure?.link || imgFrom(it.content || it.description),
        }));
      } catch { return null; }
    };
    const fromAllOrigins = async () => {
      try {
        const r = await fetch("https://api.allorigins.win/raw?url=" + encodeURIComponent(FEED));
        if (!r.ok) return null;
        const xml = new DOMParser().parseFromString(await r.text(), "text/xml");
        const items = [...xml.querySelectorAll("item")].slice(0, MAX);
        if (!items.length) return null;
        return items.map((it) => {
          const g = (n) => it.querySelector(n)?.textContent || "";
          const enc = it.querySelector("enclosure")?.getAttribute("url") || "";
          const desc = g("description");
          return { title: g("title"), link: g("link"), date: fmtDate(g("pubDate")), excerpt: strip(desc), image: enc || imgFrom(desc) };
        });
      } catch { return null; }
    };
    (async () => {
      const posts = (await fromPostsJson()) || (await fromRss2Json()) || (await fromAllOrigins());
      if (!posts) return; // subscribe CTA stays as fallback
      notesGrid.innerHTML = posts.map((p) => `
        <a class="card note-card" href="${p.link}" target="_blank" rel="noopener">
          ${p.image ? `<span class="note-card__img"><img src="${p.image}" alt="" loading="lazy" width="640" height="360"></span>` : ""}
          <time>${p.date || ""}</time>
          <h3>${p.title || ""}</h3>
          <p>${p.excerpt || ""}</p>
          <span class="card__more">Read on Substack <span aria-hidden="true">↗</span></span>
        </a>`).join("");
      const fallback = $("[data-notes-fallback]");
      if (fallback) fallback.hidden = true;
    })();
  }

  /* ---------- Current-year stamp ---------- */
  $$("[data-year]").forEach((el) => (el.textContent = new Date().getFullYear()));
})();
