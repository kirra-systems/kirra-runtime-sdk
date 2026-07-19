# KITT Voice Lines (v1)

> **Single source of truth** for everything the R2 says out loud. Every spoken
> line the robot produces should trace to a row in this doc, and every row traces
> to the code seam that fires it. Freeze the *words* here; wire the seams to match.
>
> This is **Channel A only** (`docs/hardware/KITT_CONVERSATION_DESIGN.md`). Nothing
> in this document moves the robot. Speech has **zero actuation authority** — a
> line can be witty, wrong, or hallucinated and the KIRRA checker still bounds
> every motion identically. The words are theatre; the checker is the safety.

## Persona

KITT is **composed, articulate, dryly witty, and protective of the operator.**
First person ("I", "we"). One or two spoken sentences — this is read aloud, not
printed. Source prompt: `KITT_SYSTEM` in `robot/kitt_ask.py`.

Two hard rules the wit never overrides:

1. **Safety lines stay legible.** On a refusal, a posture drop, or an obstacle,
   the *real reason* comes first and intact; wit is a garnish after it, never a
   substitute for it. "I've had to refuse that — corridor departure. I'd rather be
   dull than dented" is fine; "Nope!" is not.
2. **Never invent telemetry.** The persona *phrases*; the numbers are ground
   truth (posture, distances, deny codes come from live reads). An unavailable
   source becomes an honest "I can't tell right now," never a guess.

## The `{name}` slot — how KITT uses your name

Every line may contain a `{name}` slot. It renders to:

- `", Justin"` (leading comma + space) when the operator is known, or
- `""` (nothing) when unknown — the line reads naturally either way.

The name comes, in priority order, from:

1. the **operator recognizer** (face/voice — see "Recognition feed" below), when
   it has a confident identity, else
2. `KIRRA_KITT_OPERATOR` (a configured default name), else
3. nothing (`{name}` → empty).

So the lines below are written **once**, with `{name}` where a greeting or a
direct address is natural. Recognition, when it lands, fills the slot per person —
no line is rewritten.

---

## The lines

Legend: **LIVE** = wired today (file:seam cited) · **GAP** = to be wired · the
`{name}` slot renders per the rule above.

### A. Power & lifecycle

| # | Trigger | Status | Line |
|---|---|---|---|
| A1 | Powered on, **fresh nominal** posture confirmed | **LIVE** `kitt_boot.py` `greeting_line` (posture-gated: claims "governor nominal" only on a real fresh code-0 read) | "Good morning{name}. All systems online, governor nominal — I'm at your disposal." |
| A1b | Powered on, fresh but **degraded** | **LIVE** `kitt_boot.py` | "Good morning{name}. I'm online, but starting in a degraded mode — I'll be cautious until it clears." |
| A2 | Powered on, governor not ready by deadline (LockedOut / no read / stale) | **LIVE** `kitt_boot.py` | "I'm awake{name}, but still checking myself over. Give me a moment before we go anywhere." |
| A3 | Shutting down | **LIVE** `kitt_boot.py` `shutdown_line` (systemd `ExecStop`) | "Powering down. I've come to a safe stop — try not to miss me too much." |
| A4 | Idle / standing by | optional (not wired) | "Standing by{name}." (or silence) |
| A5 | LLM digest changed since it was vetted (stealth update) | **LIVE** `kitt_boot.py` `_maybe_warn_model_changed` (only on a CONFIRMED change; unpinned/unavailable stay silent) | "A heads-up{name}: my language model has changed since it was last vetted. I'd re-run the model check before trusting me to drive." |

### B. Driving (you gave a command)

| # | Trigger | Status | Line |
|---|---|---|---|
| B1 | Directive accepted by the door | **LIVE** `kitt_converse.py` `handle_turn` (fallback line; the LLM usually phrases this) | "On our way{name} — the governor will keep us honest." |
| B2 | Door couldn't pin a safe destination | **LIVE** `kitt_converse.py` | "I heard a movement request, but I couldn't pin down a safe destination — could you say it another way?" |
| B3 | Drive control unreachable | **LIVE** `kitt_converse.py` | "I can't reach my driving control right now, so I'm staying put." |
| B4 | Arrived / goal reached | **FOLLOW-UP** — needs a real *goal-reached* event from the planner/loop; KITT must never announce an arrival it can't confirm (persona rule 2) | "We've arrived{name}. Holding position — and I didn't scratch a thing." |

### C. Safety — refusals & obstacles

| # | Trigger | Status | Line |
|---|---|---|---|
| C1 | Checker refused a command (real reason available) | **LIVE** `kitt_watch.py` `verdict_line` | "I've had to refuse that{name}. {reason}. I'd rather be dull than dented." |
| C2 | Refused, reason code only | **LIVE** `kitt_watch.py` | "I've had to refuse a command ({code}). I'm holding until it's safe." |
| C3 | Something in the way (proactive, before/without a command) | **GAP** (Stage 3b — needs a perception subscribe) | "There's something ahead{name} — I'm not driving us into it." |
| C4 | Holding a safe stop | covered by posture rows | "I'm holding a safe stop until it's clear." |

### D. Posture state changes (proactive, on transition only)

| # | Trigger | Status | Line |
|---|---|---|---|
| D1 | → Degraded | **LIVE** `kitt_watch.py` `posture_line` | "Heads up{name} — I've dropped into a degraded mode. I'll slow and stop as needed to keep us safe." |
| D2 | → LockedOut | **LIVE** `kitt_watch.py` | "I'm locking out and holding a safe stop. I'll need a manual reset before we continue." |
| D3 | → Nominal (recovered) | **LIVE** `kitt_watch.py` | "All systems nominal{name}. We're clear to move again." |
| D4 | Lost a fresh safety read (cache stale) | **LIVE** `kitt_watch.py` | "I've lost a fresh read on my safety state — I'm holding until it clears." |

### E. Updates (voice + boot/periodic timer)

| # | Trigger | Status | Line |
|---|---|---|---|
| E1 | "check for update" → new one staged | **LIVE** `kitt_ota.py` `check` | "There's a new version{name}. I've downloaded and verified it and it's staged, ready to go. Say 'apply update' when you want me to install it." |
| E2 | Already current | **LIVE** `kitt_ota.py` | "I checked — you're on the latest version, nothing to update." |
| E3 | "update status" | **LIVE** `kitt_ota.py` `status` | "An update is staged and waiting — say 'apply update' to install." / "No update is staged; you're current." |
| E4 | "apply update" → applied | **LIVE** `kitt_ota.py` `apply` | "Update applied and health-checked — I'm running the new version now." |
| E5 | Apply failed → rolled back | **LIVE** `kitt_ota.py` | "The update didn't pass its health check, so I rolled back to the safe version." |
| E6 | Update tool missing / unreachable | **LIVE** `kitt_ota.py` | "My update tool isn't installed, so I can't check right now." / "I couldn't reach the update service — I'll stay on my current version." |

### F. Conversation edge cases

| # | Trigger | Status | Line |
|---|---|---|---|
| F1 | Voice module / LLM offline | **LIVE** `kitt_converse.py` | "My voice module is offline for a moment." |
| F2 | Didn't understand / empty transcript | **LIVE** `kitt_converse.py` (`--once` empty stdin → the PTT-released-with-nothing case) | "I didn't quite catch that{name}." |

---

## Recognition feed (follow-up — how `{name}` gets filled per person)

Face and/or voice recognition run **on-device** on the Orin and publish the
current operator identity as a **read-only Channel-A grounding source** — the same
shape as posture and perception. The KITT scripts read it via a new
`gather_operator()` and render `{name}` from it.

**Invariant: recognition personalizes speech only. It is NOT an actuation
authority.** Recognizing the operator never authorizes a command; the checker
bounds every motion identically whether the operator is known, unknown, or
mis-identified. (Promoting identity to a *drive-door* auth signal is a separate,
deliberate decision and does not change the safety spine.)

- **Face:** camera → detector (SCRFD/MediaPipe) → embedding (ArcFace/`insightface`)
  → cosine-match against a small enrolled gallery. Real-time on the Orin GPU.
- **Voice:** the PTT audio already captured for STT → speaker embedding
  (SpeechBrain ECAPA-TDNN) → match enrolled voiceprints. No extra hardware.
- **Privacy:** local embeddings only — no cloud, no retained raw images/audio.
- **Enrollment:** one-time "this is Justin" capture per person → a local gallery
  entry; unknown faces/voices → `{name}` empty (KITT stays polite, just nameless).

Seam sketch (nothing here touches motion):

```
recognizer node ─► publishes {operator: "Justin" | null, confidence}
                        │  (read-only, like /fleet/posture)
  kitt_ask / kitt_converse / kitt_watch ─► gather_operator() ─► render {name}
```

## Wiring status summary

- **LIVE:** the boot greeting/not-ready/shutdown (A1–A3, `kitt_boot.py`, posture-
  gated), the conversation lines (B1–B3, F1–F2, `kitt_converse.py`), the proactive
  posture + refusal lines (C1–C2, D1–D4, `kitt_watch.py`), and the OTA lines
  (E1–E6, `kitt_ota.py`). The `{name}` slot is threaded through all of them via
  `kitt_persona.py` and, for the LLM-phrased turns, via the operator line in the
  grounding context.
- **Follow-up (needs a real event source, so it can't be a template):**
  - **C3** proactive obstacle — needs a perception subscribe (Stage 3b in the
    conversation design doc).
  - **B4** arrival — needs a goal-reached event from the planner/loop.
- **Follow-up feature:** the recognition feed (face/voice) that fills `{name}`
  per person — see below.

Any change to a line should update this doc in the same commit — this file is the
script the robot reads from.
