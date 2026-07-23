# Rabbit-mode — a conversational R2, and why it stays safe

> **Vision:** you talk to the R2 like Michael talks to Rabbit — free dialogue,
> personality, situational awareness, proactive commentary — and it talks back.
> **Thesis:** this is the *cleanest possible demonstration* of the doer/checker
> split. Rabbit is a **doer**: it can say anything, be charming, even be wrong. The
> only thing that ever reaches the wheels is a typed intent that survives
> `MickIntent::parse_llm_json` → Occy → the KIRRA checker. A conversational LLM
> is therefore **exactly as safe as a silent one** — the fence and the checker
> bound it identically. "It can be Rabbit and it *cannot* drive you into a wall."

This doc is the target architecture and an incremental path. It changes **nothing**
on the safety spine — every addition lives on the untrusted Mick/doer side of
`ci/check_mick_actuation_fence.py`.

---

## The one rule that makes this legal

A conversational agent has **two output channels, and they are not equal:**

```
                        ┌───────────────────────────────────────────────┐
  you speak ─► STT ─►   │  Rabbit dialogue agent (LLM + persona + memory)  │
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

| Rabbit capability | Status | Where |
|---|---|---|
| Voice in (STT) → text | ✅ built | `speech_shell` + `speech.rs` (whisper.cpp) |
| Text → **one** typed driving intent, fail-closed | ✅ built | `mick_service` `POST /intent` → `MickIntent::parse_llm_json` |
| Speaks the reason it stopped (narration) | ✅ built | #893 side-channel: verifier `GET /system/verdicts/last` → mick `GET /narration/last` → TTS |
| A named persona | ✅ partial | mick persona (chauffeur, gemma3:4b) — a label + system prompt, not yet a character |
| The fence that makes it all safe | ✅ built | `ci/check_mick_actuation_fence.py` |

So today's R2 is a **single-turn command taker that can explain a refusal**. Rabbit
is the same spine plus a **dialogue layer** and **proactive voice**.

---

## What "conversational Rabbit" adds (the gaps)

### 1. A dialogue manager (multi-turn + memory + persona)
Today `/intent` is one-shot: text → one intent or reject. Rabbit needs a turn loop
that holds **conversation context** and decides, per turn, which channel to use:

- *"Rabbit, what's around us?"* → **SPEAK** (answer from telemetry; no motion).
- *"Head for the kitchen."* → **ACT** (emit `go_to`/`route_to` through the door).
- *"Nice weather."* → **SPEAK** (banter; no intent at all).

Design: a new sidecar (`rabbit_service`, or an evolved `mick_service`) that wraps
the LLM with (a) a **Rabbit system prompt** (dry wit, protective, addresses you by
name), (b) a **rolling conversation buffer**, and (c) a **router** that classifies
each turn as chat / question / directive. A directive is handed to the EXISTING
`MickIntent` parse — the router never bypasses it. Chat/questions never touch it.

### 2. Situational grounding (so it answers *truthfully*, not plausibly)
Rabbit should answer "why did we stop?" with the *real* reason, not a guess. Feed
the agent **read-only** context each turn:

- **fleet posture / node posture** — the posture-exempt observability GETs
  (`GET /fleet/posture`), so "are we OK?" is grounded.
- **the last verdict + narration** — the #893 relay already wired
  (`GET /narration/last`), so "why did you stop?" speaks the checker's actual
  DenyCode sentence, phrased conversationally.
- **a perception summary** — the Taj corridor / nearest-obstacle snapshot
  (`robot/inspect_corridor.py` is exactly this data), so "what do you see?" is
  real. **Read-only** — grounding inputs are GETs; none is an actuation path.

The persona *phrases*; the numbers are ground truth. Rabbit never invents telemetry.

### 3. Proactive speech (Rabbit talks unprompted)
Rabbit comments without being asked — "Michael, I'm detecting an obstacle," "we've
arrived," "returning to a safe stop." This is an **event → speech** path (new):
the agent SUBSCRIBES to loop events (an MRC stop, goal reached, a posture change,
a perception cap) and renders a spoken line. Still channel A only — proactive
*speech*, never proactive *motion*.

### 4. Persona + a voice
A distinctive Piper voice + the Rabbit system prompt (the character). Cosmetic to
the architecture, load-bearing to the experience.

### 5. (Stretch) full-duplex / barge-in
Talk while it talks, interrupt it. Needs streaming STT + duplex audio — a real
audio-engineering effort, and the last mile, not the first.

---

## Incremental path (each stage ships + demos on its own)

| Stage | Capability | Build |
|---|---|---|
| **0** ✅ | Speak one command; it drives (bounded) or explains the refusal | *done* — `speech_shell` + PTT button |
| **1** ✅ | **Grounded Q&A** — "what's around us / why did we stop / are we OK" answered from real telemetry (SPEAK only) | *done* — `robot/rabbit_ask.py` (read-only telemetry reader + Rabbit prompt; no motion path) |
| **2** ✅ | **Multi-turn + persona** — conversation memory + Rabbit character; router splits chat vs directive; directive still goes through the ONE door | *done* — `robot/rabbit_converse.py` (memory + persona + fail-closed router → mick `POST /intent`) |
| **3** ✅ | **Proactive voice** — event transitions (posture / checker deny / cache-stale) → spoken lines | *done* — `robot/rabbit_watch.py` (read-only `/metrics` + `/narration/last` poll → transition-gated speech) |
| **4** | **Voice + polish** — the Rabbit Piper voice, listening chirp, arrival tones | audio assets |
| **5** | **Full-duplex / barge-in** | streaming STT + duplex audio |

Stage 1 is the natural next step and is **almost free**: the telemetry it needs is
already exposed (posture GETs, `/narration/last`, the corridor snapshot), and it's
pure SPEAK — it cannot affect safety, so it needs no new review of the spine.

### Stage 2 — how to run (`robot/rabbit_converse.py`)

```bash
./robot/rabbit_converse.py                 # interactive: one utterance per line (Ctrl-D quits)
echo "take us to the door" | ./robot/rabbit_converse.py --once
whisper-cli … | ./robot/rabbit_converse.py # voice: pipe the transcript per line (PTT + STT)
```

One dialogue, two channels, with memory + persona. Per turn Rabbit emits a JSON
`{say, directive}`; it speaks `say`, and **only if** `directive` is a clear
movement request does it hand that TEXT to `mick_service POST /intent` — the
same fail-closed door a human types into. **The door is the router:** Rabbit never
builds an intent; mick's `MickIntent::parse_llm_json` is the authority, occy +
the checker bound the result.

Fail-closed routing (unit-checked in `parse_reply`): anything Rabbit can't parse as
an unambiguous directive — prose, a `null`/`"none"`/empty directive, a question —
resolves to **no directive → SPEAK only → no motion**. A misheard or hallucinated
directive at worst becomes a checker-APPROVED bounded motion; an unparseable turn
drives nothing. The only actuation-adjacent call in the whole script is the
text-to-`/intent` POST — no publisher, no serial, no release token.

### Stage 3 — how to run (`robot/rabbit_watch.py`)

```bash
./robot/rabbit_watch.py    # run alongside the loop; Rabbit speaks on events (Ctrl-C quits)
```

Rabbit talks to *you*, unprompted. A read-only watcher polls two signals and speaks
only on a **state transition**:

- **posture** (`GET /metrics` `kirra_fleet_posture` 0/1/2 + cache-stale) →
  "I've dropped into a degraded mode," "I'm locking out and holding a safe stop,"
  "all systems nominal," "I've lost a fresh read on my safety state."
- **the checker's last verdict** (`GET /narration/last`, the #893 relay) → on a
  NEW deny, "I've had to refuse a command. \<the checker's real reason\>."

Discipline (unit-checked): announces ONLY on change (steady state is silent),
establishes a baseline on the first poll without speaking (no boot chatter),
rate-limits flaps, and stays silent for any signal that's unreachable (never a
false "recovered" from missing data). Channel A only — read-only GETs + TTS, no
`/intent`, no publisher, no serial: proactive **speech**, never proactive motion.

> **Natural extension (Stage 3b):** a perception-driven "obstacle ahead" warning
> — needs a light perception feed (a ROS subscribe to `/kirra/perception_health`
> or `/cmd_vel_raw`), vs. today's HTTP-only posture+verdict events.

### Stage 1 — how to run (`robot/rabbit_ask.py`)

```bash
./robot/rabbit_ask.py "what do you see?"       # answers from a live /scan → Taj
./robot/rabbit_ask.py "why did we stop?"       # answers from the #893 verdict relay
./robot/rabbit_ask.py "are we OK?"             # answers from /fleet/posture
echo "what's around us" | ./robot/rabbit_ask.py   # stdin form (e.g. from whisper-cli)
export KIRRA_TTS_CMD="./speak.sh"            # optional → Rabbit speaks the answer aloud
```

Structural safety (why Stage 1 needs no spine review): `rabbit_ask.py` makes only
**read-only GETs** (posture, narration) + a Taj `/perception` **analysis** POST +
an Ollama chat POST — there is **no `/intent` POST, no ROS publisher, no serial,
no release token**. It is Channel A by construction: it can talk about the robot,
never move it. Each telemetry source is fetched **fail-soft** — a missing source
becomes "unavailable" in Rabbit's answer (proven: with everything offline it says
"my voice module is offline, here is what I have" + the unavailable list, and
never invents a fact). Grounding: the LLM is instructed to answer ONLY from the
supplied telemetry; the persona phrases, the numbers are ground truth.

---

## Skills mode (opt-in) — named skills through the *same* door

`KIRRA_SKILLS_ENABLED=1` swaps the free-form `{say, directive}` router for a
**named-skill** contract: the LLM emits `{say, skills:[{name, parameters}]}` from
a REGISTERED vocabulary (`robot/skill_registry.py`). Default off → the free-form
router is byte-identical.

The registry is a **catalog, not a new door.** Its dispatcher (pure,
host-tested) turns each skill into exactly one of three decisions:

- **`FENCE`** — a *motion* skill (`navigate` / `cruise` / `turn` / `pull_over` /
  `stop`) compiles to a plain-words directive that goes through the **same**
  `offer_to_door` → mick `POST /intent` → `MickIntent` grounding → checker path a
  free-form directive already takes. Motion reaches the wheels ONLY via this
  decision, and it is just text handed to the existing door.
- **`SPEAK`** — a read-only skill (`speak`) narrates; no actuation.
- **`REFUSE`** — a cataloged-but-unimplemented skill (`dock`, `follow_person`,
  `search_area`, …) or an unknown name is refused, **never faked**. Fabricating a
  capability is the failure mode this guards against.

`execute_skill_decisions(decisions, offer_to_door, speak)` routes motion ONLY
through the injected fence sink — the single-door invariant, asserted directly in
`skill_registry_test.py`. So `ci/check_mick_actuation_fence.py` and the
one-door rule are untouched: a skill name buys the LLM a vocabulary, not
authority.

**Not yet default:** graduating the flag requires extending
`rabbit_model_smoketest.py` to gate the skills contract (the model-swap
discipline), and the mission Executive (the multi-step sequencer) is a later
slice. Metadata each skill advertises (permissions, interruptibility,
preconditions, failure modes) is carried now so that Executive can reason about
it without a redesign.

---

## World Model (opt-in) — a read projection, not a shared brain

`KIRRA_WORLD_MODEL_ENABLED=1` adds a deterministic **"situation report" / "sitrep"**
voice command (`robot/world_model.py`) that renders a single TTL'd view of the
live grounding — posture, perception, last stop reason, operator.

It is deliberately a **read projection, not an authority** (architecture ruling
§5.1). Rather than one shared mutable "brain" every subsystem depends on — a
single point of staleness that turns "is this fresh enough to act on?" into a
global question — each field carries its own `source` / `stamp_ms` / `ttl_ms`, and
a field read past its TTL comes back **`UNKNOWN`**. A stale or unavailable value
is **said to be unknown, never dressed as current**; a source that reports
"unavailable" leaves its field unset (`UNKNOWN`), never fabricated. The KIRRA
checker still reads its **own** inputs directly — the projection never gates
safety; it is Channel-A narration only.

The freshness core (fresh/stale/skew, snapshot, render, assemble) is host-tested;
the live gather is the thin seam. Fields without a producer yet (battery,
localization, nav state, known people) are simply absent → `UNKNOWN` — documented
projection slots, not fabricated readings.

---

## Missions (opt-in) — a multi-step Executive that the checker still bounds

`KIRRA_MISSIONS_ENABLED=1` adds a multi-step **Executive** (`robot/mission.py`):
the LLM emits `{say, mission:[{name, parameters}, …]}` — an ordered plan over the
**registered** skills — and the Executive runs it with sequencing, retry,
cancellation, and progress narration. It takes precedence over skills mode;
default off → the single-turn router is byte-identical.

The Executive is a **doer; the checker still owns every step:**

- **Motion only through the fence.** Each motion step is executed by handing its
  directive to the *same* `offer_to_door` → `/intent` → checker path a single
  directive already takes. `run_mission` routes motion *only* through the
  injected fence sink — asserted in `mission_test.py`.
- **Refuse before motion.** `validate_mission` dispatches every step up front; a
  mission with *any* unimplemented/unknown skill (e.g. `inspect`, `dock`) is
  **refused before a single step runs** — no partial mission with a bad step.
- **Halt, never skip.** A step the checker **refuses** halts the mission (motion
  never continues to the next step); a transient door error retries a bounded
  number of times, then halts. Fail-closed throughout.
- **Cancellable.** A barge-in (Slice 2 — a PTT press / `barge_in.py --signal`)
  cancels between or within steps; the ego stops (the checker's own MRC) and the
  Executive never authors re-acceleration.

So the mission layer buys the operator *sequencing*, not *authority*: the same
`ci/check_mick_actuation_fence.py` one-door rule holds, one step at a time. The
Executive core (plan / validate / step transitions / run) is pure and
host-tested; the LLM planning call is the thin seam. Missions are limited to the
registered skills — as unimplemented skills (`dock`, `search_area`, …) gain real,
fenced backings, missions get richer with no change to this safety story.

---

## Honest caveats

- **Latency.** A local LLM on the Orin (gemma3:4b) is a few seconds per turn.
  Fine for "take me to the kitchen"; a snappier model or a smaller router model
  for the chat/directive split helps the conversational feel.
- **The persona has no authority.** Say it in every prompt and every review: Rabbit
  *advises and narrates*; it does not decide safety. The checker does. A jailbreak
  that makes Rabbit *say* something reckless still produces, at most, a channel-B
  intent that the checker refuses.
- **Grounding must stay read-only.** The temptation will be to give Rabbit a "just
  do it" backdoor. There is no such door — every directive is a typed intent
  through `MickIntent::parse_llm_json`, and the fence CI fails the build if the
  conversational crate ever links an actuation symbol. That constraint is the
  feature, not a limitation.

## References
- `docs/hardware/R2_UNTETHERED_BRINGUP.md` — voice UX + the PTT button + e-stop
- `docs/testing/SPEECH_RABBIT_DEMO.md` — the built STT→intent→TTS loop
- `crates/kirra-sidecars/src/mick.rs` — `handle_text` → `LlmBrain` → the intent parse
- `ci/check_mick_actuation_fence.py` — why a conversational LLM still can't drive
- `robot/inspect_corridor.py` — the perception snapshot Rabbit answers "what do you see" from
- the #893 narration side-channel — how Rabbit speaks the checker's real reason
