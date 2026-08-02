# Vision — Kirra OS

## What Kirra OS is

**Kirra OS is a governed cognitive operating system for robots.**

Kirra sits between probabilistic autonomy and physical actuation. It provides
the architecture that interprets, grounds, bounds, verifies, authorizes, and
audits robot behaviour.

> **On the words "operating system."** This is the platform framing for the
> governed intelligence and interaction layer *above* robotics middleware.
> Kirra is not a kernel, not a certified operating system, and not a
> replacement for Linux or ROS 2. It runs on top of them, and on the QNX
> partition path it runs beside them. See
> [`ARCHITECTURE.md`](ARCHITECTURE.md) for where each piece actually sits.

---

## The problem

Robotics has become good at generating plausible behaviour and has not become
correspondingly good at refusing bad behaviour.

A language model can emit a fluent, well-formed, entirely wrong motion
command. A learned planner can produce a trajectory that is smooth,
in-distribution, and headed into an occluded junction. A geometric planner can
be correct about geometry and wrong about a stale sensor. In all three cases
the output *looks* right, and looking right is exactly what the generating
component optimizes for.

The common industry response is to improve the generator: better prompts,
better training data, more evaluation. That reduces the frequency of bad
proposals. It cannot bound their consequence, because the thing being asked to
judge the output is the thing that produced it.

Kirra takes the other approach. The generator stays fallible, and an
independent architecture decides what reaches the actuator.

**Intelligence proposes. Architecture decides.**

---

## Governed autonomy

Four roles, with authority deliberately concentrated in one of them:

| Role | Name | Trust |
|---|---|---|
| Human-facing conversation and explanation | **Mick**, the Robot Companion | No actuation authority |
| Proposing a plan or trajectory | **Occy**, or another swappable doer | Untrusted |
| Independently checking, bounding, and authorizing | **Kirra Governor / Verifier** | The trusted checker |
| Executing an authorized command | Verifying consumer → hardware | Verifies before acting |

**Mick communicates. Occy proposes. Kirra decides.**

The doer may be geometric, learned, or language-model-driven. It is never
trusted merely because it is well formed. The checker evaluates the proposal
against envelopes, containment, safety distance, freshness, and posture, then
accepts, clamps, or falls back to a minimum-risk response.

This is what makes the model swappable. The safety argument does not rest on
which planner produced the proposal, so replacing the planner does not
invalidate it — a property recorded in
[`docs/adr/0020-doer-invariant-safety-case.md`](docs/adr/0020-doer-invariant-safety-case.md).

---

## Local-first operation

The conversational and planning layers run on the robot. A robot that needs a
datacentre to answer "what are you doing?" is not a companion; it is a thin
client with a microphone.

Local operation also removes a class of failure that no amount of cloud
engineering fixes: the network being unavailable at exactly the moment an
operator needs to understand what the machine is about to do.

Remote services remain possible and are opt-in.
→ [`docs/hardware/RABBIT_CONVERSATION_DESIGN.md`](docs/hardware/RABBIT_CONVERSATION_DESIGN.md)

---

## Hardware independence

Kinematic limits differ per platform, so they are configuration rather than
code. A deployment selects a vehicle class and gets that class's envelope;
selecting nothing is a startup failure rather than a silent default, because a
wrong envelope is worse than no robot.
→ [`docs/CONTRACT_PROFILES.md`](docs/CONTRACT_PROFILES.md)

The reference integration is a Rosmaster R2 on a Jetson Orin NX
([`docs/adr/0014-rosmaster-r2-orin-nx-kirra-integration.md`](docs/adr/0014-rosmaster-r2-orin-nx-kirra-integration.md)),
and the checker core is deliberately middleware-agnostic so the same argument
survives a change of chassis.

---

## Model independence

The doer LLM is swappable with no safety re-review, because the checker never
consults it. What a swap *does* require is a doer-quality check, so a new model
is not silently worse at its job.
→ [`robot/rabbit_model_smoketest.py`](robot/rabbit_model_smoketest.py),
[`robot/mick_chat_model_smoketest.py`](robot/mick_chat_model_smoketest.py)

> **Models, planners, and robot hardware are replaceable.
> The governed safety boundary remains stable.**

---

## The Robot Companion

Most robots communicate in status codes, blinking lights, and log files. The
operator is left inferring intent from behaviour, which works until the moment
it matters.

Mick is the human-facing layer: what the robot knows, what it can do, why it
made a decision, and when it is uncertain. Mick explains refusals rather than
hiding them, because a refusal an operator understands is a system they can
work with.

Mick has no direct actuation authority. That is enforced structurally, not by
instruction.
→ [`COMPANION.md`](COMPANION.md)

**Every robot deserves a trusted companion.**

---

## Long-term platform vision

| Product | Description | Status |
|---|---|---|
| **Kirra OS** | Governed cognitive operating system for robots | Active development |
| **Mick** | The Robot Companion | Active development |
| **Occy** | The untrusted planning doer | Active development |
| **Kirra Governor / Verifier** | The trusted checker and authorization boundary | Active development |
| **Kirra Studio** | Developer and operations tooling | **Planned / conceptual** |
| **Kirra Fleet** | Fleet deployment and monitoring tooling | **Planned / conceptual** |

Kirra Studio and Kirra Fleet are names for intended future products. They are
not shipped, and nothing in this repository should be read as delivering them.

Direction over time: more platforms, richer world state, and a companion that
handles longer workflows — all without moving authority out of the checker.
→ [`ROADMAP.md`](ROADMAP.md)

---

## Product boundaries

What Kirra does **not** claim:

- **Kirra does not make AI safe.** It bounds what a proposal may do to a
  specific machine, under a documented configuration, within stated
  assumptions of use. System-level safety depends on the ODD, the hardware,
  the sensors, the maps, the configuration, the integration, and the
  verification chain actually deployed.
  → [`docs/safety/ASSUMPTIONS_OF_USE.md`](docs/safety/ASSUMPTIONS_OF_USE.md)
- **Kirra is not a certified product.** Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508 SIL 3 requirements. Independent third-party assessment has not yet been performed. → [`SAFETY.md`](SAFETY.md)
- **Kirra is not a planner.** It does not make the robot capable; it bounds
  the capability something else provides.
- **Kirra is not a perception system.** It consumes a perception contract and
  fails closed when that contract is unmet.
- **Kirra is not a replacement for ROS 2, Autoware, or a middleware stack.**
  On the AV line it explicitly keeps Autoware as the doer.
  → [`docs/adr/0036-autoware-distro-migration-occy-gap.md`](docs/adr/0036-autoware-distro-migration-occy-gap.md)

**Trust the architecture, not the model.**
