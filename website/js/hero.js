/* ============================================================
   KIRRA — hero field. A governed signal lattice: a drifting node
   grid swept by a verification scanline. Hand-written WebGL 1,
   no libraries. Fallback: the CSS gradient behind the canvas.
   Pauses when offscreen or tab-hidden; disabled entirely under
   prefers-reduced-motion (static first frame is drawn instead).
   ============================================================ */
(() => {
  "use strict";
  const canvas = document.getElementById("heroField");
  if (!canvas) return;
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const gl = canvas.getContext("webgl", { alpha: true, antialias: false, powerPreference: "low-power" });
  if (!gl) return;

  const VERT = `
attribute vec2 p;
void main(){ gl_Position = vec4(p, 0.0, 1.0); }`;

  const FRAG = `
precision mediump float;
uniform vec2 uRes;
uniform float uTime;
uniform float uLight;   /* 0 = dark theme, 1 = light theme */
uniform vec2 uPointer;  /* -1..1 */

float hash(vec2 q){ return fract(sin(dot(q, vec2(127.1, 311.7))) * 43758.5453); }

void main(){
  vec2 uv = gl_FragCoord.xy / uRes;
  vec2 asp = vec2(uRes.x / uRes.y, 1.0);
  vec2 P = uv * asp;

  /* drifting node lattice */
  float cell = 26.0 / min(uRes.x, uRes.y) * 420.0 / 420.0;
  vec2 g = P * 16.0 + vec2(uTime * 0.06, uTime * 0.028);
  vec2 id = floor(g);
  vec2 f = fract(g) - 0.5;
  float rnd = hash(id);
  /* per-node jitter + slow breathing */
  vec2 jitter = 0.30 * vec2(sin(uTime * (0.4 + rnd) + rnd * 40.0), cos(uTime * (0.33 + rnd * 0.6) + rnd * 17.0));
  float d = length(f - jitter * 0.4);
  float node = smoothstep(0.055, 0.012, d) * (0.25 + 0.75 * rnd);

  /* verification scanline sweeping upward */
  float sweep = fract(uTime * 0.055);
  float band = exp(-pow((uv.y - sweep) * 26.0, 2.0));
  float trail = exp(-max(sweep - uv.y, 0.0) * 9.0) * step(uv.y, sweep) * 0.32;

  /* pointer halo */
  vec2 pv = (uPointer * 0.5 + 0.5);
  float halo = exp(-length((uv - pv) * asp) * 3.2) * 0.5;

  /* faint structural grid lines */
  vec2 gl2 = abs(fract(P * 8.0 + 0.5) - 0.5);
  float line = (1.0 - smoothstep(0.0, 0.02, min(gl2.x, gl2.y))) * 0.05;

  float glow = node * (0.32 + band * 1.6 + trail + halo) + line * (0.4 + band);

  /* occasional deny pulse: one cell per sweep flashes red-ish then is absorbed */
  float denySel = step(0.9985, hash(id + floor(uTime * 0.055)));
  float deny = denySel * node * band * 2.0;

  vec3 cyan  = mix(vec3(0.25, 0.85, 0.78), vec3(0.04, 0.45, 0.40), uLight);
  vec3 red   = mix(vec3(0.94, 0.40, 0.36), vec3(0.75, 0.22, 0.17), uLight);
  vec3 col = cyan * glow + red * deny;

  float alpha = clamp(glow * (1.0 - uLight * 0.55) + deny, 0.0, 1.0) * mix(0.9, 0.5, uLight);
  /* vignette so content stays readable */
  float vig = smoothstep(0.0, 0.45, uv.y) * (1.0 - 0.5 * smoothstep(0.55, 1.0, uv.x * (1.0 - uv.y)));
  gl_FragColor = vec4(col, alpha * (0.35 + 0.65 * vig));
}`;

  const sh = (type, src) => {
    const s = gl.createShader(type);
    gl.shaderSource(s, src); gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) { return null; }
    return s;
  };
  const vs = sh(gl.VERTEX_SHADER, VERT);
  const fs = sh(gl.FRAGMENT_SHADER, FRAG);
  if (!vs || !fs) return;
  const prog = gl.createProgram();
  gl.attachShader(prog, vs); gl.attachShader(prog, fs); gl.linkProgram(prog);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) return;
  gl.useProgram(prog);

  const buf = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buf);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
  const loc = gl.getAttribLocation(prog, "p");
  gl.enableVertexAttribArray(loc);
  gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
  gl.enable(gl.BLEND);
  gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);

  const uRes = gl.getUniformLocation(prog, "uRes");
  const uTime = gl.getUniformLocation(prog, "uTime");
  const uLight = gl.getUniformLocation(prog, "uLight");
  const uPointer = gl.getUniformLocation(prog, "uPointer");

  let light = document.documentElement.dataset.theme === "light" ? 1 : 0;
  addEventListener("kirra:theme", (e) => { light = e.detail === "light" ? 1 : 0; if (reduced) draw(4); });

  const DPR = Math.min(devicePixelRatio || 1, 1.5);
  const resize = () => {
    const r = canvas.getBoundingClientRect();
    const w = Math.max(1, Math.round(r.width * DPR));
    const h = Math.max(1, Math.round(r.height * DPR));
    if (canvas.width !== w || canvas.height !== h) {
      canvas.width = w; canvas.height = h;
      gl.viewport(0, 0, w, h);
    }
  };
  resize();
  addEventListener("resize", resize, { passive: true });

  let px = 0.35, py = 0.3, tx = px, ty = py;
  addEventListener("pointermove", (e) => {
    tx = (e.clientX / innerWidth) * 2 - 1;
    ty = -((e.clientY / innerHeight) * 2 - 1);
  }, { passive: true });

  const draw = (t) => {
    px += (tx - px) * 0.04; py += (ty - py) * 0.04;
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.uniform2f(uRes, canvas.width, canvas.height);
    gl.uniform1f(uTime, t);
    gl.uniform1f(uLight, light);
    gl.uniform2f(uPointer, px, py);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  };

  if (reduced) { draw(4); return; } // single static frame

  let running = true, visible = true;
  document.addEventListener("visibilitychange", () => { running = !document.hidden && visible; if (running) loop(performance.now()); });
  if ("IntersectionObserver" in window) {
    new IntersectionObserver(([en]) => {
      visible = en.isIntersecting;
      const was = running;
      running = visible && !document.hidden;
      if (running && !was) loop(performance.now());
    }).observe(canvas);
  }

  let raf = 0;
  const loop = (t) => {
    if (!running) return;
    cancelAnimationFrame(raf);
    draw(t / 1000);
    raf = requestAnimationFrame(loop);
  };
  loop(performance.now());
})();
