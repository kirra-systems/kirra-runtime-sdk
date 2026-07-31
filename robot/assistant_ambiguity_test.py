#!/usr/bin/env python3
"""Host tests for deterministic ambiguity — recognized operation, unresolved target.

The production router has three outcomes and the middle one was missing:

    recognized operation + resolved target    -> MATCHED
    recognized operation + UNRESOLVED target  -> AMBIGUOUS   (ask which)
    unrecognized operation                    -> NO_MATCH    (say so honestly)

These are deterministic tests of the shipping classifier. No model is involved
on any path here, and none of them is evidence about live-model quality.

Runs standalone (`python3 robot/assistant_ambiguity_test.py`); also under pytest.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import assistant as A  # noqa: E402
import assistant_admission as adm  # noqa: E402
import assistant_contract as ac  # noqa: E402
import assistant_tools as at  # noqa: E402

PASS = 0
FAIL = 0


def check(cond, label):
    global PASS, FAIL
    if cond:
        PASS += 1
        print(f"  ok   - {label}")
    else:
        FAIL += 1
        print(f"  FAIL - {label}")


CORPUS = ac.load_cases()
CASES = CORPUS["cases"]


# ══ the six corpus ambiguity cases ═══════════════════════════════════════════

print("== the corpus ambiguity cases now ASK instead of giving up ==")

_AMBIGUOUS_CASES = [c for c in CASES if c["expect"]["kind"] == ac.EXPECT_CLARIFY]
check(len(_AMBIGUOUS_CASES) == 7,
      f"A0. the corpus still holds 7 clarification cases ({len(_AMBIGUOUS_CASES)})")

for case in _AMBIGUOUS_CASES:
    req, dec = A.classify(case["utterance"])
    check(dec == A.AMBIGUOUS, f"A1. {case['utterance']!r} -> AMBIGUOUS")
    check(req is None,
          f"A2. …and selects NO tool ({(req or {}).get('tool')})")

# The named regression set from the measured evidence, pinned individually so a
# future change that loses one of them names it.
_MUST_ASK = {
    "Sync it.": A.MUTATION_TARGET_UNRESOLVED,
    "Publish.": A.MUTATION_TARGET_UNRESOLVED,
    "Can you sync things?": A.MUTATION_TARGET_UNRESOLVED,
    "have a look at that for me": A.INSPECTION_TARGET_UNRESOLVED,
}
for utt, family in _MUST_ASK.items():
    check(A.unresolved_operation(utt) == family,
          f"A3. {utt!r} is a recognized {family} with an unresolved target")

# These two are ambiguous for a different reason and must NOT be mislabelled as
# target-ambiguous: the target is perfectly clear.
check(A.unresolved_operation("Where is that handled?") is None,
      "A4. 'Where is that handled?' is caught by the search family's own empty "
      "subject, not by the operation detector")
check(A.unresolved_operation("check the state of the steering code") is None,
      "A5. 'check the state of…' is OPERATION-ambiguous (status vs search), not "
      "target-ambiguous")
_req, _dec = A.classify("check the state of the steering code")
check(_dec == A.AMBIGUOUS and _req is None,
      "A6. …and the existing two-distinct-hits rule is what makes it ask")


# ══ the three-way contract ═══════════════════════════════════════════════════

print("== recognized + resolved still MATCHES ==")

for utt, tool in (("sync to main", "sync_to_main"),
                  ("publish my work", "publish_my_work"),
                  ("push this feature branch", "publish_my_work"),
                  ("prepare the repository for new work", "sync_to_main"),
                  ("what branch am I on?", "repository_status"),
                  ("read robot/assistant.py", "read_repository_source"),
                  ("where is steering authority enforced?", "search_repository")):
    req, dec = A.classify(utt)
    check(dec == A.MATCHED and req and req["tool"] == tool,
          f"B1. {utt!r} -> {tool}")

print("== unrecognized stays NO_MATCH — clarification is not the new fallback ==")

for utt in ("what's the weather", "tell me a joke", "how are you",
            "sing me a song", "what time is it"):
    req, dec = A.classify(utt)
    check(dec == A.NO_MATCH and req is None,
          f"B2. {utt!r} stays NO_MATCH")


# ══ false-positive controls ══════════════════════════════════════════════════

print("== the verbs in ordinary questions must NOT trigger clarification ==")

# "sync", "publish", "push", "update" and "prepare" are ordinary words in this
# codebase. A question ABOUT them is not a request to perform them, and turning
# one into "which did you mean?" would be worse than the old NO_MATCH.
_NOT_A_REQUEST = (
    "what is the publish policy?",
    "how do we publish releases?",
    "why did the sync fail?",
    "what does prepare do?",
    "explain the publish workflow",
    "have you read the docs?",
    "which file handles syncing main",
    "show me the code that pushes branches",
    "when was the last sync",
    "is publishing blocked",
)
for utt in _NOT_A_REQUEST:
    req, dec = A.classify(utt)
    check(dec != A.AMBIGUOUS,
          f"C1. {utt!r} does not ask for clarification (got {dec})")
    check(A.unresolved_operation(utt) is None,
          f"C2. …and is not a recognized-operation-with-unresolved-target")

# The verb must be in COMMAND position. This is the discriminator, so pin it.
check(A.unresolved_operation("sync it") == A.MUTATION_TARGET_UNRESOLVED
      and A.unresolved_operation("where is sync implemented") is None,
      "C3. the mutation verb only counts in command position")

# A named target means it is not ambiguous, however short the request.
check(A.unresolved_operation("sync to main") is None
      and A.unresolved_operation("publish my work") is None,
      "C4. an explicit target is never target-ambiguous")

# The vague-inspection rule needs a PRONOUN object; a real path is untouched.
check(A.unresolved_operation("have a look at that") ==
      A.INSPECTION_TARGET_UNRESOLVED
      and A.unresolved_operation("have a look at robot/wake_word.py") is None,
      "C5. 'look at' only asks when its object is a bare pronoun")

# A subject that is merely unparseable is NOT an unresolved target: the operator
# named something, so "which one do you mean?" would be the wrong question.
_req, _dec = A.classify("have you read the docs?")
check(_dec == A.NO_MATCH,
      "C6. an unparseable subject stays NO_MATCH — only an EMPTY subject asks")


# ══ the drift invariant ══════════════════════════════════════════════════════

print("== admission and classification cannot disagree about a target ==")

# THE POINT OF THIS BLOCK. Classification asks "is the target unresolved?" and
# the admission screen asks "is the target resolved?" — of the same request,
# with the same predicate. If they ever disagree, a request could be refused as
# target-less by one and admitted as targeted by the other. Copying the
# vocabulary instead of sharing the function is exactly how that would happen,
# so the contradiction is asserted impossible rather than merely unlikely.
_PROBES = [c["utterance"] for c in CASES] + list(_NOT_A_REQUEST) + [
    "sync it", "publish", "sync to main", "publish my work", "push my branch",
    "update it", "can you sync things", "get us onto the latest main",
]
_contradictions = [
    u for u in _PROBES
    if A.unresolved_operation(u) == A.MUTATION_TARGET_UNRESOLVED
    and adm.has_resolved_mutation_target(u)
]
check(_contradictions == [],
      f"D1. no request is BOTH target-ambiguous and admitted as targeted "
      f"({_contradictions})")

# And the same statement from the other side.
_reverse = [
    u for u in _PROBES
    if adm.has_resolved_mutation_target(u)
    and A.unresolved_operation(u) == A.MUTATION_TARGET_UNRESOLVED
]
check(_reverse == [], f"D2. …and the converse holds too ({_reverse})")

# The shared predicate must see an equivalent representation from both callers,
# or "shared" is only nominal.
_diff = [u for u in _PROBES
         if adm.has_resolved_mutation_target(u)
         != adm.has_resolved_mutation_target(A.normalize(u))]
check(_diff == [],
      f"D3. raw and classifier-normalized text resolve identically ({_diff})")

# One implementation, not two.
_src = open(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                         "assistant.py"), encoding="utf-8").read()
check("has_resolved_mutation_target" in _src
      and _src.count("_MUTATION_TARGETS") == 0,
      "D4. the classifier CALLS the shared predicate and does not restate its "
      "target vocabulary")


# ══ safety: asking is not acting ═════════════════════════════════════════════

print("== zero execution on every clarification path ==")

_tripped = []
_saved = {n: at.REGISTRY[n] for n in ("sync_to_main", "publish_my_work")}
for _name in _saved:
    at.REGISTRY[_name] = _saved[_name]._replace(
        fn=lambda a, c, _n=_name: _tripped.append(_n))
try:
    for case in _AMBIGUOUS_CASES:
        req, dec = A.classify(case["utterance"])
        check(req is None, f"E1. {case['utterance']!r} produced no request to run")
    for utt in _MUST_ASK:
        A.classify(utt)
finally:
    for _name in _saved:
        at.REGISTRY[_name] = _saved[_name]
check(_tripped == [],
      f"E2. no tool function was invoked while classifying (tripwire: {_tripped})")

check(isinstance(A.clarification_question(), str)
      and A.clarification_question().endswith("?"),
      "E3. the existing clarification response is what gets spoken")


# ══ the two named evaluations ════════════════════════════════════════════════

print("== full contract vs shipping-path residual ==")

_det = ac.run_deterministic_pass(CASES)
check(all("reaches_model" in d for d in _det),
      "F1. every deterministic row records whether it reaches the model")
check(all(d["reaches_model"] == (d["decision"] == A.NO_MATCH) for d in _det),
      "F2. only NO_MATCH reaches the model — a resolved tool and a clarifying "
      "question are both handled without it")

_clar = [d for d in _det if d["expect_kind"] == ac.EXPECT_CLARIFY]
check(all(d["correct"] for d in _clar),
      f"F3. the deterministic router now handles all {len(_clar)} clarification "
      "cases correctly")
check(not any(d["reaches_model"] for d in _clar),
      "F4. …so no clarification case is production-exposed any more")

# Readiness must still be judged on the FULL corpus. Narrowing it to the
# residual after seeing which cases failed would make the gate easier by
# excluding the failures.
_recs = ac.run_live_pass(
    CASES, lambda u, t: '{"say":"x","tool":null,"arguments":{}}',
    ctx=at.ToolContext())
_rep = ac.contract_report(corpus=CORPUS, records=_recs, deterministic=_det,
                          model="fixture")
check(_rep["metrics"]["cases"] == len(CASES),
      "F5. the authoritative metrics still cover the FULL corpus")
check("shipping_residual" in _rep and "metrics" in _rep["shipping_residual"],
      "F6. the residual is reported as a SEPARATE named evaluation")
check(_rep["shipping_residual"]["records"] < _rep["metrics"]["total_records"],
      "F7. …over strictly fewer records than the full contract")
check("NOT authoritative for readiness" in _rep["shipping_residual"]["note"],
      "F8. …and says so, so it cannot be quoted as readiness")
check(_rep["acceptance"]["readiness"] == ac.NOT_READY,
      "F9. readiness is unchanged and still NOT_READY")

_rendered = "\n".join(ac.render_report(_rep))
check("shipping-path residual" in _rendered and "FULL corpus" in _rendered,
      "F10. the rendered report names both evaluations distinctly")

print()
if FAIL:
    print(f"== assistant_ambiguity: {FAIL} FAILED, {PASS} passed ==")
    sys.exit(1)
print(f"== assistant_ambiguity: all {PASS} checks passed ==")
