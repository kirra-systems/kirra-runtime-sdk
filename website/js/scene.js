// ============================================================
// KIRRA — "Trust Lattice"
// A live 3D fleet dependency graph. Nodes pulse, trust packets
// travel the edges, and the whole lattice shifts colour through
// the posture states (green → amber → red) as you scroll.
// ============================================================
import * as THREE from 'three';
import { EffectComposer } from 'three/addons/postprocessing/EffectComposer.js';
import { RenderPass }     from 'three/addons/postprocessing/RenderPass.js';
import { UnrealBloomPass } from 'three/addons/postprocessing/UnrealBloomPass.js';
import { OutputPass }     from 'three/addons/postprocessing/OutputPass.js';

const POSTURE = {
  green: new THREE.Color('#34f5a6'),
  amber: new THREE.Color('#ffb627'),
  red:   new THREE.Color('#ff4d63'),
};

const state = {
  posture01: 0,        // 0 green · 0.5 amber · 1 red
  targetPosture01: 0,
  agitation: 0,        // 0..1 extra jitter/speed
  targetAgitation: 0,
  pointerX: 0, pointerY: 0,
  targetPX: 0, targetPY: 0,
};

let readyResolve;
const whenReady = new Promise((res) => { readyResolve = res; });

function postureColor(t) {
  const c = new THREE.Color();
  if (t < 0.5) c.copy(POSTURE.green).lerp(POSTURE.amber, t / 0.5);
  else         c.copy(POSTURE.amber).lerp(POSTURE.red, (t - 0.5) / 0.5);
  return c;
}

function init() {
  const canvas = document.getElementById('lattice');
  if (!canvas) { readyResolve(); return; }

  let renderer;
  try {
    renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: true, powerPreference: 'high-performance' });
  } catch (e) {
    readyResolve(); return; // no WebGL — site still works, just no lattice
  }

  const sizes = { w: window.innerWidth, h: window.innerHeight };
  const pr = Math.min(window.devicePixelRatio, 2);
  renderer.setPixelRatio(pr);
  renderer.setSize(sizes.w, sizes.h);
  renderer.setClearColor(0x000000, 0);
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 0.98;

  const scene = new THREE.Scene();
  const camera = new THREE.PerspectiveCamera(52, sizes.w / sizes.h, 0.1, 200);
  camera.position.set(0, 0, 42);

  const group = new THREE.Group();
  scene.add(group);

  // ── Build the lattice node cloud (fibonacci shell + inner cluster) ──
  const NODES = 170;
  const positions = [];
  const radius = 17;
  for (let i = 0; i < NODES; i++) {
    const shell = i < NODES * 0.72;
    let r, x, y, z;
    if (shell) {
      const k = i + 0.5;
      const phi = Math.acos(1 - 2 * k / (NODES * 0.72));
      const theta = Math.PI * (1 + Math.sqrt(5)) * k;
      r = radius * (0.82 + Math.random() * 0.18);
      x = Math.sin(phi) * Math.cos(theta) * r;
      y = Math.sin(phi) * Math.sin(theta) * r;
      z = Math.cos(phi) * r;
    } else {
      r = radius * (0.15 + Math.random() * 0.5);
      const u = Math.random() * Math.PI * 2, v = Math.acos(2 * Math.random() - 1);
      x = Math.sin(v) * Math.cos(u) * r;
      y = Math.sin(v) * Math.sin(u) * r;
      z = Math.cos(v) * r;
    }
    positions.push(new THREE.Vector3(x, y, z));
  }

  // ── Edges: connect each node to its K nearest neighbours (deduped) ──
  const K = 3;
  const edgeSet = new Set();
  const edges = []; // {a,b}
  for (let i = 0; i < NODES; i++) {
    const dists = [];
    for (let j = 0; j < NODES; j++) if (i !== j) dists.push([j, positions[i].distanceTo(positions[j])]);
    dists.sort((p, q) => p[1] - q[1]);
    for (let n = 0; n < K; n++) {
      const j = dists[n][0];
      const key = i < j ? `${i}-${j}` : `${j}-${i}`;
      if (!edgeSet.has(key)) { edgeSet.add(key); edges.push({ a: Math.min(i, j), b: Math.max(i, j) }); }
    }
  }

  // ── Nodes (Points w/ soft round shader) ──
  const nodeGeo = new THREE.BufferGeometry();
  const nodePos = new Float32Array(NODES * 3);
  const nodeTint = new Float32Array(NODES * 3);
  const nodeSize = new Float32Array(NODES);
  for (let i = 0; i < NODES; i++) {
    nodePos[i * 3] = positions[i].x; nodePos[i * 3 + 1] = positions[i].y; nodePos[i * 3 + 2] = positions[i].z;
    const b = 0.6 + Math.random() * 0.4;            // per-node brightness variance
    nodeTint[i * 3] = b; nodeTint[i * 3 + 1] = b; nodeTint[i * 3 + 2] = b;
    nodeSize[i] = (Math.random() < 0.12 ? 26 : 13) + Math.random() * 6; // a few hub nodes
  }
  nodeGeo.setAttribute('position', new THREE.BufferAttribute(nodePos, 3));
  nodeGeo.setAttribute('aColor', new THREE.BufferAttribute(nodeTint, 3));
  nodeGeo.setAttribute('aSize', new THREE.BufferAttribute(nodeSize, 1));

  const nodeMat = new THREE.ShaderMaterial({
    transparent: true, depthWrite: false,
    uniforms: { uColor: { value: POSTURE.green.clone() }, uTime: { value: 0 }, uPr: { value: pr } },
    vertexShader: `
      attribute float aSize; attribute vec3 aColor;
      varying vec3 vColor; uniform float uTime; uniform float uPr;
      void main(){
        vColor = aColor;
        vec4 mv = modelViewMatrix * vec4(position,1.0);
        float pulse = 1.0 + 0.22 * sin(uTime*1.6 + position.x*0.6 + position.y*0.4);
        gl_PointSize = aSize * pulse * uPr * (300.0 / -mv.z);
        gl_Position = projectionMatrix * mv;
      }`,
    fragmentShader: `
      varying vec3 vColor; uniform vec3 uColor;
      void main(){
        vec2 c = gl_PointCoord - 0.5; float d = length(c);
        if(d>0.5) discard;
        float a = smoothstep(0.5,0.05,d);
        float core = smoothstep(0.22,0.0,d);
        vec3 col = vColor * uColor;
        col = mix(col, vec3(1.0), core*0.7);
        gl_FragColor = vec4(col, a);
      }`,
  });
  const nodePoints = new THREE.Points(nodeGeo, nodeMat);
  group.add(nodePoints);

  // ── Edges ──
  const edgePos = new Float32Array(edges.length * 6);
  for (let e = 0; e < edges.length; e++) {
    const A = positions[edges[e].a], B = positions[edges[e].b];
    edgePos[e * 6] = A.x; edgePos[e * 6 + 1] = A.y; edgePos[e * 6 + 2] = A.z;
    edgePos[e * 6 + 3] = B.x; edgePos[e * 6 + 4] = B.y; edgePos[e * 6 + 5] = B.z;
  }
  const edgeGeo = new THREE.BufferGeometry();
  edgeGeo.setAttribute('position', new THREE.BufferAttribute(edgePos, 3));
  const edgeMat = new THREE.LineBasicMaterial({ color: POSTURE.green.clone().multiplyScalar(0.5), transparent: true, opacity: 0.16 });
  const lines = new THREE.LineSegments(edgeGeo, edgeMat);
  group.add(lines);

  // ── Trust packets travelling the edges ──
  const PULSES = 90;
  const pulseGeo = new THREE.BufferGeometry();
  const pulsePos = new Float32Array(PULSES * 3);
  const pulseSize = new Float32Array(PULSES);
  const pulses = [];
  for (let i = 0; i < PULSES; i++) {
    pulses.push({ e: Math.floor(Math.random() * edges.length), t: Math.random(), spd: 0.12 + Math.random() * 0.25 });
    pulseSize[i] = 9 + Math.random() * 7;
  }
  pulseGeo.setAttribute('position', new THREE.BufferAttribute(pulsePos, 3));
  pulseGeo.setAttribute('aSize', new THREE.BufferAttribute(pulseSize, 1));
  const pulseMat = new THREE.ShaderMaterial({
    transparent: true, depthWrite: false, blending: THREE.AdditiveBlending,
    uniforms: { uColor: { value: POSTURE.green.clone() }, uPr: { value: pr } },
    vertexShader: `
      attribute float aSize; uniform float uPr;
      void main(){ vec4 mv = modelViewMatrix*vec4(position,1.0);
        gl_PointSize = aSize*uPr*(300.0/-mv.z); gl_Position = projectionMatrix*mv; }`,
    fragmentShader: `
      uniform vec3 uColor;
      void main(){ vec2 c = gl_PointCoord-0.5; float d=length(c);
        if(d>0.5) discard; float a=smoothstep(0.5,0.0,d);
        gl_FragColor = vec4(uColor + vec3(0.3), a); }`,
  });
  const pulsePoints = new THREE.Points(pulseGeo, pulseMat);
  group.add(pulsePoints);

  // ── Post-processing: bloom for the neon glow ──
  const composer = new EffectComposer(renderer);
  composer.addPass(new RenderPass(scene, camera));
  const bloom = new UnrealBloomPass(new THREE.Vector2(sizes.w, sizes.h), 0.5, 0.4, 0.15);
  composer.addPass(bloom);
  composer.addPass(new OutputPass());
  composer.setPixelRatio(pr);
  composer.setSize(sizes.w, sizes.h);

  // ── Resize ──
  function onResize() {
    sizes.w = window.innerWidth; sizes.h = window.innerHeight;
    camera.aspect = sizes.w / sizes.h; camera.updateProjectionMatrix();
    renderer.setSize(sizes.w, sizes.h);
    composer.setSize(sizes.w, sizes.h);
    bloom.setSize(sizes.w, sizes.h);
  }
  window.addEventListener('resize', onResize);

  // ── Pointer parallax ──
  window.addEventListener('pointermove', (e) => {
    state.targetPX = (e.clientX / sizes.w - 0.5) * 2;
    state.targetPY = (e.clientY / sizes.h - 0.5) * 2;
  });

  // ── Animation loop ──
  const clock = new THREE.Clock();
  const tmpColor = new THREE.Color();
  function tick() {
    const t = clock.getElapsedTime();
    const dt = Math.min(clock.getDelta ? 0.016 : 0.016, 0.033);

    // ease state
    state.posture01  += (state.targetPosture01  - state.posture01)  * 0.06;
    state.agitation  += (state.targetAgitation  - state.agitation)  * 0.05;
    state.pointerX   += (state.targetPX - state.pointerX) * 0.05;
    state.pointerY   += (state.targetPY - state.pointerY) * 0.05;

    // posture colour
    const pc = postureColor(state.posture01);
    nodeMat.uniforms.uColor.value.lerp(pc, 0.1);
    pulseMat.uniforms.uColor.value.lerp(pc, 0.1);
    tmpColor.copy(pc).multiplyScalar(0.5);
    edgeMat.color.lerp(tmpColor, 0.1);
    edgeMat.opacity = 0.14 + state.agitation * 0.12;
    bloom.strength = 0.5 + state.agitation * 0.35 + state.posture01 * 0.2;

    nodeMat.uniforms.uTime.value = t * (1 + state.agitation * 1.5);

    // rotate the lattice + pointer parallax
    group.rotation.y += 0.0011 + state.agitation * 0.002;
    group.rotation.x += 0.0004;
    const targetRY = state.pointerX * 0.35;
    const targetRX = state.pointerY * 0.22;
    group.rotation.y += (targetRY - (group.rotation.y % (Math.PI * 2))) * 0.0;
    camera.position.x += (state.pointerX * 3.5 - camera.position.x) * 0.04;
    camera.position.y += (-state.pointerY * 3.0 - camera.position.y) * 0.04;
    camera.lookAt(0, 0, 0);

    // advance trust packets along edges
    const speedScale = 1 + state.agitation * 1.4;
    for (let i = 0; i < PULSES; i++) {
      const p = pulses[i];
      p.t += p.spd * 0.016 * speedScale;
      if (p.t >= 1) { p.t = 0; p.e = Math.floor(Math.random() * edges.length); }
      const A = positions[edges[p.e].a], B = positions[edges[p.e].b];
      pulsePos[i * 3]     = A.x + (B.x - A.x) * p.t;
      pulsePos[i * 3 + 1] = A.y + (B.y - A.y) * p.t;
      pulsePos[i * 3 + 2] = A.z + (B.z - A.z) * p.t;
    }
    pulseGeo.attributes.position.needsUpdate = true;

    composer.render();
    requestAnimationFrame(tick);
  }
  tick();
  readyResolve();
}

// ── Public API consumed by main.js ──
window.KirraScene = {
  whenReady,
  setPosture01(v) { state.targetPosture01 = Math.max(0, Math.min(1, v)); },
  setAgitation(v) { state.targetAgitation = Math.max(0, Math.min(1, v)); },
};

if (document.readyState !== 'loading') init();
else document.addEventListener('DOMContentLoaded', init);
