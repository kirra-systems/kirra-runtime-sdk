# KITT-mode — a conversational R2, and why it stays safe

> **Vision:** you talk to the R2 like Michael talks to KITT — free dialogue,
> personality, situational awareness, proactive commentary — and it talks back.
> **Thesis:** this is the *cleanest possible demonstration* of the doer/checker
> split. KITT is a **doer**: it can say anything, be charming, even be wrong. The
> only thing that ever reaches the wheels is a typed intent that survives
> `MickIntent::parse_llm_json` → Occy → the KIRRA checker. A conversational LLM
> is therefore **exactly as safe as a silent one** — the fence and the checker
> bound it identically. "It can be KITT and it *cannot* drive you into a wall."

This doc is the target architecture and an incremental path. It changes **nothing**
on the safety spine — every addition lives on the untrusted Mick/doer side of
`ci/check_mick_actuation_fence.py`.

---

## The one rule that makes this legal

A conversational agent has **two output channels, and they are not equal:**

```
                        ┌───────────────────────────────────────────────┐
  you speak ─► STT ─►   │  KITT dialogue agent (LLM + persona + memory)  │
                        └───────────────┬───────────────────┬───────────┘
                                        │                    │
                        (A) SPEAK       │                    │  (B) ACT
                        anything ───────┘                    └─── a typed intent ONLY,
                        → TTS → audio out                        through the EXISTING door:
                        (say it; it never moves)                 POST /intent →
                                                                 MickIntent::parse_llm_json →
                                                                 Occy plan → KIRRA checker → wheels
```

- **Channel A (SPEAK)** is unrestricted: banter, answers, warnings, status, jokes
  — it renders to TTS and **can never move the robot**. Words are not commands.
- **Channel B (ACT)** is the *only* path to motion, and it is the **same
  fail-closed door typed text already uses**. The agent cannot emit a
  `Twist`, a release token, or a serial byte — it is structurally fenced
  (`ci/check_mick_actuation_fence.py` covers the whole Mick side, speech
  included). A hallucinated "sure, flooring it!" in channel A is just *sound*;
  unless it also produces a well-formed intent in channel B that the **checker**
  then admits, nothing happens.

This is the whole safety story: **make the personality as rich as you like — it
has zero actuation authority.** The checker doesn't care how eloquent the doer is.

---

## What's already built (you're further than it looks)

| KITT capability | Status | Where |
|---|---|---|
| Voice in (STT) → text | ✅ built | `speech_shell` + `speech.rs` (whisper.cpp) |
| Text → **one** typed driving intent, fail-closed | ✅ built | `mick_service` `POST /intent` → `MickIntent::parse_llm_json` |
| Speaks the reason it stopped (narration) | ✅ built | #893 side-channel: verifier `GET /system/verdicts/last` → mick `GET /narration/last` → TTS |
| A named persona | ✅ partial | mick persona (chauffeur, gemma3:4b) — a label + system prompt, not yet a character |
| The fence that makes it all safe | ✅ built | `ci/check_mick_actuation_fence.py` |

So today's R2 is a **single-turn command taker that can explain a refusal**. KITT
is the same spine plus a **dialogue layer** and **proactive voice**.

---

## What "conversational KITT" adds (the gaps)

### 1. A dialogue manager (multi-turn + memory + persona)
Today `/intent` is one-shot: text → one intent or reject. KITT needs a turn loop
that holds **conversation context** and decides, per turn, which channel to use:

- *"KITT, what's around us?"* → **SPEAK** (answer from telemetry; no motion).
- *"Head for the kitchen."* → **ACT** (emit `go_to`/`route_to` through the door).
- *"Nice weather."* → **SPEAK** (banter; no intent at all).

Design: a new sidecar (`kitt_service`, or an evolved `mick_service`) that wraps
the LLM with (a) a **KITT system prompt** (dry wit, protective, addresses you by
name), (b) a **rolling conversation buffer**, and (c) a **router** that classifies
each turn as chat / question / directive. A directive is handed to the EXISTING
`MickIntent` parse — the router never bypasses it. Chat/questions never touch it.

### 2. Situational grounding (so it answers *truthfully*, not plausibly)
KITT should answer "why did we stop?" with the *real* reason, not a guess. Feed
the agent **read-only** context each turn:

- **fleet posture / node posture** — the posture-exempt observability GETs
  (`GET /fleet/posture`), so "are we OK?" is grounded.
- **the last verdict + narration** — the #893 relay already wired
  (`GET /narration/last`), so "why did you stop?" speaks the checker's actual
  DenyCode sentence, phrased conversationally.
- **a perception summary** — the Taj corridor / nearest-obstacle snapshot
  (`robot/inspect_corridor.py` is exactly this data), so "what do you see?" is
  real. **Read-only** — grounding inputs are GETs; none is an actuation path.

The persona *phrases*; the numbers are ground truth. KITT never invents telemetry.

### 3. Proactive speech (KITT talks unprompted)
KITT comments without being asked — "Michael, I'm detecting an obstacle," "we've
arrived," "returning to a safe stop." This is an **event → speech** path (new):
the agent SUBSCRIBES to loop events (an MRC stop, goal reached, a posture change,
a perception cap) and renders a spoken line. Still channel A only — proactive
*speech*, never proactive *motion*.

### 4. Persona + a voice
A distinctive Piper voice + the KITT system prompt (the character). Cosmetic to
the architecture, load-bearing to the experience.

### 5. (Stretch) full-duplex / barge-in
Talk while it talks, interrupt it. Needs streaming STT + duplex audio — a real
audio-engineering effort, and the last mile, not the first.

---

## Incremental path (each stage ships + demos on its own)

| Stage | Capability | Build |
|---|---|---|
| **0** ✅ | Speak one command; it drives (bounded) or explains the refusal | *done* — `speech_shell` + PTT button |
| **1** | **Grounded Q&A** — "what's around us / why did we stop / are we OK" answered from real telemetry (SPEAK only) | read-only telemetry reader + a Q&A prompt; no motion path touched |
| **2** | **Multi-turn + persona** — conversation memory + KITT character; router splits chat vs directive; directive still goes through the ONE door | `kitt_service` dialogue manager |
| **3** | **Proactive voice** — event subscriptions (MRC/arrival/posture) → spoken lines | event→speech bridge |
| **4** | **Voice + polish** — the KITT Piper voice, listening chirp, arrival tones | audio assets |
| **5** | **Full-duplex / barge-in** | streaming STT + duplex audio |

Stage 1 is the natural next step and is **almost free**: the telemetry it needs is
already exposed (posture GETs, `/narration/last`, the corridor snapshot), and it's
pure SPEAK — it cannot affect safety, so it needs no new review of the spine.

---

## Honest caveats

- **Latency.** A local LLM on the Orin (gemma3:4b) is a few seconds per turn.
  Fine for "take me to the kitchen"; a snappier model or a smaller router model
  for the chat/directive split helps the conversational feel.
- **The persona has no authority.** Say it in every prompt and every review: KITT
  *advises and narrates*; it does not decide safety. The checker does. A jailbreak
  that makes KITT *say* something reckless still produces, at most, a channel-B
  intent that the checker refuses.
- **Grounding must stay read-only.** The temptation will be to give KITT a "just
  do it" backdoor. There is no such door — every directive is a typed intent
  through `MickIntent::parse_llm_json`, and the fence CI fails the build if the
  conversational crate ever links an actuation symbol. That constraint is the
  feature, not a limitation.

## References
- `docs/hardware/R2_UNTETHERED_BRINGUP.md` — voice UX + the PTT button + e-stop
- `docs/testing/SPEECH_KITT_DEMO.md` — the built STT→intent→TTS loop
- `crates/kirra-sidecars/src/mick.rs` — `handle_text` → `LlmBrain` → the intent parse
- `ci/check_mick_actuation_fence.py` — why a conversational LLM still can't drive
- `robot/inspect_corridor.py` — the perception snapshot KITT answers "what do you see" from
- the #893 narration side-channel — how KITT speaks the checker's real reason
