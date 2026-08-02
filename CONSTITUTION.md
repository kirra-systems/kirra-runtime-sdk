# The Kirra Constitution

Fifteen non-negotiable principles. Everything else in this repository is an
implementation detail that may change; these are the commitments that decide
whether a change is acceptable at all.

A principle here is only worth the evidence behind it, so each links to where
it is actually enforced.

---

### 1. Safety is architectural, not prompt-based

No system prompt, instruction, guardrail phrase, or fine-tune is a safety
control. Safety comes from an independent component that evaluates a proposal
after the model has finished producing it.
→ [`docs/adr/0020-doer-invariant-safety-case.md`](docs/adr/0020-doer-invariant-safety-case.md)

### 2. Doers are never trusted

A planner is not trusted because it is well engineered, well tested, or
well formed. Geometric, learned, and language-model planners are all treated
as fallible proposers.
→ [`docs/ARCHITECTURE_STACK.md`](docs/ARCHITECTURE_STACK.md) §2

### 3. Intelligence proposes; architecture decides

The component that generates a behaviour is never the component that
authorizes it.
→ [`crates/kirra-trajectory/src/validation.rs`](crates/kirra-trajectory/src/validation.rs)

### 4. Missing, stale, malformed, unauthenticated, or non-finite evidence fails closed

Absence of evidence is never evidence of safety. A silent sensor, an expired
credential, an unparseable payload, and a `NaN` all resolve to refusal or a
minimum-risk response — never to a permissive default.
→ [`docs/safety/SAFE_STATE_SPECIFICATION.md`](docs/safety/SAFE_STATE_SPECIFICATION.md),
[`src/telemetry_watchdog.rs`](src/telemetry_watchdog.rs)

### 5. Physical action requires independent verification and authorization

Reaching an actuator requires a verdict from a component that did not produce
the command, and a release authorization the consumer verifies before acting.
→ [`docs/adr/0031-release-token-on-the-actuation-path.md`](docs/adr/0031-release-token-on-the-actuation-path.md),
[`crates/kirra-inline-governor/`](crates/kirra-inline-governor/)

### 6. The robot never fabricates live state

If the system was not told a fact and cannot source it, it says so. An
invented sensor reading is worse than an admitted gap, because it is
indistinguishable from a real one.
→ [`robot/mick_chat_contract.py`](robot/mick_chat_contract.py)

### 7. The world model represents sourced physical-world facts, not model imagination

Coordinates, poses, object positions, and map features come from perception or
a trusted registry. Language selects *which* thing is meant; it never supplies
the geometry.
→ [`crates/kirra-sidecars/src/destination.rs`](crates/kirra-sidecars/src/destination.rs)

### 8. Conversation memory and world state are separate

What was said is not what is true. Dialogue history must never become an
input the safety path reads as fact.
→ [`crates/kirra-sidecars/tests/mick_chat_separation.rs`](crates/kirra-sidecars/tests/mick_chat_separation.rs)

### 9. Local operation and privacy are defaults

The conversational and planning layers run on the robot. Remote services are
opt-in, not assumed.
→ [`docs/hardware/RABBIT_CONVERSATION_DESIGN.md`](docs/hardware/RABBIT_CONVERSATION_DESIGN.md)

### 10. Hardware and model implementations are replaceable

Swapping a model, a planner, or a chassis must not require re-reasoning about
the safety boundary. If it does, the boundary was in the wrong place.
→ [`docs/CONTRACT_PROFILES.md`](docs/CONTRACT_PROFILES.md)

### 11. Safety claims must reflect actual evidence and assessment status

Designed-in-alignment is not compliance. A draft mapping is not an assessment.
A passing test is not a certificate. See the claim rules in
[`SAFETY.md`](SAFETY.md#public-claim-rules).

### 12. Mick is a Robot Companion, not an actuator

The conversational layer has no motion authority, and that is enforced
structurally rather than by policy.
→ [`ci/check_mick_actuation_fence.py`](ci/check_mick_actuation_fence.py)

### 13. A robot companion should reduce cognitive load

The operator should finish an interaction knowing more and worrying less.
Explanations exist to be understood, not to demonstrate sophistication.

### 14. Calm before clever

Predictable, quiet, and correct beats impressive. A companion that is
occasionally delightful and frequently confusing is a bad companion.

### 15. Architecture changes require updated evidence and traceability

A change to a safety mechanism is incomplete until its requirement, test, and
traceability records move with it.
→ [`docs/safety/REQUIREMENTS_TRACEABILITY.md`](docs/safety/REQUIREMENTS_TRACEABILITY.md),
[`docs/safety/TRACEABILITY.md`](docs/safety/TRACEABILITY.md)

---

## Applying this

When a change is proposed, the questions are: which principle does it touch,
what evidence moves with it, and does any claim in the change describe more
assurance than the repository actually holds?

Principle 11 is the one most easily broken by accident — usually in a README,
a slide, or a release note rather than in code.
