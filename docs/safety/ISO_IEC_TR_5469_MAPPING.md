# Kirra — ISO/IEC TR 5469 AI-Functional-Safety Alignment Mapping

Document ID: KIRRA-TR5469-001
Version: 0.1
Status: Draft — alignment statement (NOT a certification claim)
References: ISO/IEC TR 5469:2024 (Artificial intelligence — Functional safety and AI systems); IEC 61508:2010; ISO 26262:2018; ISO/PAS 21448 (SOTIF); ISO/PAS 8800
Date: 2026-06-03

---

## 0. Honest framing — this is alignment, not certification

ISO/IEC TR 5469 is a **Technical Report**: informative **guidance**, not a
certifiable normative standard. Kirra therefore **aligns with and cites** TR 5469
as its AI-functional-safety *methodology and identity* — it is **not a
certification target**, and nothing in this document should be read as a
conformance or certification claim against TR 5469. The certifiable claims live
against the normative standards already in the matrix (IEC 61508 SIL 3 →
KIRRA-61508-001 / AEGIS-61508-001; ISO 26262 ASIL-D; ASTM F3269 → AEGIS-F3269-001).

What TR 5469 gives Kirra is the **conceptual anchor**: it is the standard that
describes Kirra's exact pattern, and it ties the cross-domain story together.

---

## 1. What TR 5469 is, and why it sits above the matrix

TR 5469 is **industry-agnostic**, with terminology anchored to **IEC 61508**. It
explicitly references IEC 61508, ISO 26262, IEC 62061, ISO 13849, and IEC 61511 —
i.e. it sits **above** the whole existing standards matrix (`STANDARDS_MATRIX.md`,
AEGIS-STD-001) and connects the per-vertical functional-safety entries into one
AI-safety narrative.

TR 5469 addresses several usage situations for AI in relation to safety functions,
including:

1. **AI used inside a safety function.**
2. **Non-AI safety-related functions used to ensure the safety of AI-controlled
   equipment.**
3. **AI used to develop safety functions.**

It classifies AI usage by levels/classes and discusses architectural measures to
mitigate AI insufficiency and how AI elements relate to IEC 61508 concepts.

---

## 2. The core mapping — Kirra is usage class 2

**Kirra is squarely usage situation (2): a deterministic, non-AI, independently
developed safety function that ensures the safety of AI-controlled equipment.**
The AI-controlled equipment is the Occy / Parko AI planner-controller; Kirra is
the safety function that **does not trust it** and enforces a hard envelope on its
outputs.

This is the architecturally **preferred** pattern TR 5469 endorses for AI in
safety-critical control. TR 5469 treats certifying a neural network to high
integrity as hard and limited; the recommended alternative is to **bound the AI's
outputs with a deterministic, certifiable safety channel**. Kirra IS that channel:

| TR 5469 concept | Kirra realization | Evidence |
|---|---|---|
| The "AI element" (insufficiency to be mitigated) | Occy / Parko AI planner + ML inference backends | (the governed item) |
| Non-AI safety function over AI-controlled equipment | The Kirra Governor — deterministic, independently developed | ADR-0003 (KIRRA-OCCY-ADR-003), Safety Architecture (AEGIS-SA-001) |
| Deterministic, bounded enforcement | `validate_vehicle_command()` — scalar kinematic-contract clamp, **WCET-bounded** verdict, no heap on the verdict path | SG-001 (velocity envelope), SG-002 (lateral accel), SG-004 (finiteness); `wcet_gate` |
| Fail-closed safe state on any insufficiency | Fail-closed → MRC (Minimal Risk Condition); unknown-command denial in all postures | SG-006, SG-008; OCCY SG4 (MRC publication), SG5 (liveness) |
| Architectural diversity measure | Comparator diversity — structurally/algorithmically diverse shadow | CERT-006 (KIRRA-CERT006-DIVERSITY-001) |
| Independence between AI element and safety function | Independent safety channel; integrity burden on the deterministic channel, not the AI | ADR-0003 two-tier; OCCY_DFA (KIRRA-OCCY-DFA-001) |

**The argument in one sentence:** the integrity burden sits on the certifiable
deterministic channel (Kirra), not on the AI — which is exactly the AI-safety
posture TR 5469 describes for usage class 2, and is Kirra's strongest AI-safety
claim.

---

## 3. Connection to the existing decomposition + architecture

TR 5469's usage-class-2 pattern is the conceptual umbrella over work Kirra already
has:

- **ASIL/integrity decomposition (OCCY_DFA, KIRRA-OCCY-DFA-001):** the deterministic
  Governor carries the high-integrity requirement; the AI planner is decomposed to
  a lower-integrity element whose insufficiency is mitigated by the Governor's hard
  envelope. TR 5469 is the AI-specific justification for *why* that decomposition
  is sound for an AI element.
- **Two-tier architecture (ADR-0003, KIRRA-OCCY-ARCH-001):** the base Governor is
  the non-AI safety function; the optional D1 add-on is an independent detection
  channel. TR 5469 frames both as architectural measures against AI insufficiency.
- **Comparator diversity (CERT-006, KIRRA-CERT006-DIVERSITY-001):** a diversity
  measure with honestly-stated limits — the kind of architectural mitigation TR
  5469 discusses, applied to the Governor itself.
- **IEC 61508 anchor (AEGIS-61508-001):** because TR 5469's terminology is anchored
  to 61508, Kirra's 61508 SIL-3 mapping is the normative spine; TR 5469 is the
  AI-context layer on top.
- **SOTIF (ISO 21448) / OCCY_SOTIF (KIRRA-OCCY-ODD-001):** SOTIF covers the AI
  planner's behavioral insufficiency (triggering conditions); TR 5469 + Kirra's
  enforcement convert that uncertainty into a bounded enforcement boundary. ISO/PAS
  8800 (matrix #25) is the automotive-specific tailoring of this same idea.

---

## 4. Relationship to ISO/PAS 8800 (automotive)

For the AV path, **ISO/PAS 8800** is the AI-safety layer *on top of* the existing
ISO 26262 (matrix #1) + SOTIF (matrix #2) entries — it tailors ISO 26262 for AI
and extends SOTIF for AI/ML non-determinism, and references TR 5469. TR 5469 is the
industry-agnostic methodology; ISO/PAS 8800 is its automotive specialization,
relevant when an AV stack is the certification target. The Kirra usage-class-2
argument above is identical in both; 8800 just maps it into 26262 process terms.

---

## 5. What this buys, and what it does not

- **Does buy:** the AI-safety *identity* — Kirra is the textbook TR 5469
  usage-class-2 safety function — and a single cross-domain narrative tying the
  61508 / 26262 / SOTIF / machinery entries into one AI-safety story.
- **Does not buy:** any certificate. TR 5469 is guidance; the certifiable claims
  remain IEC 61508 SIL 3 / ISO 26262 ASIL-D / ASTM F3269. This document is a
  reference mapping, not a conformance argument.

---

## 6. Document control

| Field | Value |
|---|---|
| Doc ID | KIRRA-TR5469-001 |
| Supersedes | — |
| Registered in | SAFETY_CASE_INDEX.md (AEGIS-SC-000) |
| Matrix entry | STANDARDS_MATRIX.md (AEGIS-STD-001) #24 |
| Cross-refs | ADR-0003, OCCY_DFA (KIRRA-OCCY-DFA-001), COMPARATOR_DIVERSITY (CERT-006), IEC_61508_MAPPING (AEGIS-61508-001), OCCY_SOTIF (KIRRA-OCCY-ODD-001) |
