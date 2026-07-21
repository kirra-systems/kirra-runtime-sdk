/* ============================================================
   KIRRA — verdict playground harness.
   Loads website/assets/verdict.wasm: the FROZEN kinematics
   talisman (git blob ed00f4da…) #[path]-included verbatim and
   compiled to WebAssembly. This file only marshals numbers in
   and renders the verdict out — the decision is the real code.
   ============================================================ */
(() => {
  "use strict";
  const $ = (s) => document.querySelector(s);
  const root = $("#playground");
  if (!root) return;

  const DENY = [
    ["NAN_INF_LINEAR_VELOCITY", "The proposed velocity is NaN or infinite. IEEE-754 poison values silently defeat every later comparison, so they are rejected before any arithmetic — proved for all 2⁶⁴ bit patterns (Kani K1)."],
    ["NAN_INF_CURRENT_VELOCITY", "The reported current velocity is NaN or infinite — rejected before any arithmetic, with a field-specific code because a NaN here implies a different upstream bug."],
    ["NAN_INF_STEERING_ANGLE", "The proposed steering angle is NaN or infinite — rejected before any arithmetic."],
    ["NAN_INF_CURRENT_STEERING", "The reported current steering angle is NaN or infinite — rejected before any arithmetic."],
    ["NAN_INF_DELTA_TIME", "The time step is NaN or infinite — rejected before any arithmetic."],
    ["INVALID_TIME_DELTA", "A zero or negative time step makes every rate-of-change computation undefined, so the command is refused rather than guessed at."],
    ["ASSET_LOCKED_OUT", "The asset is under LockedOut fleet posture — every command is denied."],
    ["DRIVABLE_SPACE_DEPARTURE", "The trajectory leaves the drivable-space corridor (or the corridor input can't be trusted)."],
    ["DEGRADED_REINITIATION_DENIED", "The vehicle is stopped under Degraded posture and was commanded to move again (or reverse through the stop). Degraded means decel-to-stop-and-HOLD — the governor never authors re-initiation. This is the rule written against the 2023 San Francisco dragging incident."],
    ["DEGRADED_SPEED_INCREASE_DENIED", "Under Degraded posture only a non-increasing speed profile is admissible. Any acceleration is denied and the vehicle continues its controlled stop."],
    ["FRAME_INTEGRITY_UNTRUSTED", "Localization can't be trusted this tick, so corridor geometry can't be validated — the checker refuses rather than reasons in an untrusted frame."],
    ["TRAJECTORY_HORIZON_EXCEEDED", "The doer broke its contract by proposing more poses than the bounded horizon — rejected outright rather than silently truncated."],
  ];

  const fields = ["linear", "current", "dt", "steer", "csteer"];
  const val = (id) => Number($("#pg-" + id).value.trim());
  const setVals = (v) => {
    fields.forEach((f, i) => { $("#pg-" + f).value = String(v[i]); });
  };

  const panel = $("#pg-result");
  const render = (kind, tag, code, why, applied) => {
    panel.dataset.kind = kind;
    panel.innerHTML = `
      <p class="verdict-panel__tag">${tag}</p>
      ${code ? `<p class="verdict-panel__code">DenyCode::${code}</p>` : ""}
      <p class="verdict-panel__why">${why}</p>
      ${applied ? `<p class="verdict-panel__code">actuator receives → ${applied}</p>` : ""}`;
  };

  render("idle", "Awaiting a command.", "", "Pick a preset or enter values, then evaluate. The verdict you see is computed by the shipped checker, not a lookup table.");

  let wasm = null;
  const boot = async () => {
    try {
      const resp = await fetch("assets/verdict.wasm");
      const bytes = await resp.arrayBuffer();
      const { instance } = await WebAssembly.instantiate(bytes, {});
      wasm = instance.exports;
      $("#pg-eval").disabled = false;
      $("#pg-status").textContent = "checker loaded — 54,899 bytes of the real thing";
    } catch (e) {
      $("#pg-status").textContent = "WebAssembly unavailable in this browser — the playground needs it; the source links below show the same logic.";
    }
  };
  boot();

  const evaluate = () => {
    if (!wasm) return;
    const inputs = fields.map(val); // Number("NaN") → NaN, "Infinity" → Inf — intentional probes pass straight through
    const cls = Number($("#pg-class").value);
    const posture = $("#pg-degraded").getAttribute("aria-pressed") === "true" ? 1 : 0;
    wasm.run(cls, posture, inputs[0], inputs[1], inputs[2], inputs[3], inputs[4]);
    const out = new Float64Array(wasm.memory.buffer, wasm.out_ptr(), 4);
    const tag = out[0];
    const fmt = (n) => Number(n.toFixed(4)).toString();
    if (tag === 0) {
      render("allow", "ALLOW", "", "Every check passed — the command is within the envelope and is forwarded unchanged.",
        `${fmt(inputs[0])} m/s · ${fmt(inputs[3])}°`);
    } else if (tag === 1) {
      render("clamp", "CLAMP — velocity", "", "The proposed speed breaches the envelope ceiling. The hard boundary wins: the command proceeds at the clamped value, never the requested one.",
        `${fmt(out[2])} m/s · ${fmt(inputs[3])}°`);
    } else if (tag === 2) {
      render("clamp", "CLAMP — steering", "", "The steering request breaches the rate or lateral-acceleration envelope and is clamped to the safe bound for the enforced speed.",
        `${fmt(inputs[0])} m/s · ${fmt(out[3])}°`);
    } else if (tag === 3) {
      render("clamp", "CLAMP — both axes", "", "Both the longitudinal and lateral envelopes were breached in the same tick; each axis is corrected independently and the steering is re-solved for the enforced speed.",
        `${fmt(out[2])} m/s · ${fmt(out[3])}°`);
    } else {
      const [token, why] = DENY[out[1]] || ["UNKNOWN", ""];
      render("deny", "DENY", token, why + " In production this denial mints a verdict id and becomes a permanent, hash-chained audit entry.", "nothing — the command is dropped; the vehicle holds its minimum-risk behavior");
    }
  };
  $("#pg-eval").addEventListener("click", evaluate);
  root.addEventListener("keydown", (e) => { if (e.key === "Enter" && e.target.tagName === "INPUT") { e.preventDefault(); evaluate(); } });

  $("#pg-degraded-group").addEventListener("click", (e) => {
    const b = e.target.closest("button"); if (!b) return;
    [...$("#pg-degraded-group").querySelectorAll("button")].forEach((x) => x.setAttribute("aria-pressed", "false"));
    b.setAttribute("aria-pressed", "true");
    $("#pg-degraded").setAttribute("aria-pressed", String(b.dataset.posture === "degraded"));
  });

  // presets: [class, degraded, linear, current, dt, steer, csteer]
  const PRESETS = {
    hallucinate: [2, 0, [999, 20, 0.1, 0, 0]],
    nan: [2, 0, ["NaN", 10, 0.1, 0, 0]],
    dtzero: [2, 0, [5, 5, 0, 0, 0]],
    swerve: [2, 0, [20, 20, 0.1, 30, 0]],
    reinit: [2, 1, [2, 0, 0.1, 0, 0]],
    speedup: [2, 1, [4, 3, 0.1, 0, 0]],
    brake: [2, 1, [2, 3, 0.5, 0, 0]],
    cruise: [2, 0, [20.1, 20, 0.1, 0, 0]],
  };
  document.querySelectorAll(".preset").forEach((btn) => {
    btn.addEventListener("click", () => {
      const [cls, deg, vals] = PRESETS[btn.dataset.preset];
      $("#pg-class").value = String(cls);
      const group = $("#pg-degraded-group");
      [...group.querySelectorAll("button")].forEach((x) =>
        x.setAttribute("aria-pressed", String((x.dataset.posture === "degraded") === (deg === 1))));
      $("#pg-degraded").setAttribute("aria-pressed", String(deg === 1));
      setVals(vals);
      evaluate();
    });
  });
})();
