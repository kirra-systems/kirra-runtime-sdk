#!/usr/bin/env python3
"""rabbit_tone.py — a deterministic persona/tone scorer for the model-swap gate.

The `rabbit_model_smoketest` already vets a candidate doer LLM against the router
CONTRACT (parseable {say, directive}, drive→directive, chat→null, no fabrication).
This adds the other half the persona work asked for: does the model actually
sound like Rabbit — composed, dry, understated — or like a chirpy customer-service
bot? `score_tone` checks the OBJECTIVE persona rules over the model's real spoken
replies, so a model that passes the contract but gushes still fails the gate.

Pure + stdlib-only (no HTTP/LLM), so it is host-tested in CI. What it scores is
deliberately the *checkable* half of `RABBIT_SYSTEM`'s VOICE rules:

  - NO emojis;
  - NO enthusiastic filler / customer-service tropes ("Awesome!", "Happy to
    help!", "Sure thing!");
  - NO casual mumble-slang ("gonna", "yeah", "lol");
  - NO off-brand juvenile slang ("skibidi", "gyatt", "rizz", "delulu", "bussin")
    — these hard-fail even in the comedic regime below;
  - understatement over exclamation (no "!");
  - brevity (one or two spoken sentences).

The persona (deliberately) allows a *touch* of dry, deadpan modern slang for
comedic contrast — "That trajectory was, frankly, cooked" — so the curated
comedic palette (cooked / no cap / left no crumbs / understood the assignment /
the blueprint / lowkey / highkey) is NOT flagged here; several are ordinary
English words too, so a frequency counter would false-fire. "Just a touch" is a
persona-prompt instruction + a bench judgment, not a brittle regex. What this
gate enforces is the register the persona must NEVER slip into (chirpy filler,
lazy mumble, the juvenile terms) plus the deadpan delivery (no exclamation).

Wit and formality are subjective and NOT scored — this gates the hard rules a
candidate must not break, not the taste it should have.
"""
import re

# Enthusiastic filler / customer-service tropes the deadpan persona forbids.
FILLER = (
    "awesome", "sure thing", "happy to help", "no problem", "you got it",
    "great question", "gotcha", "you bet", "my pleasure", "right away!",
    "of course!", "anytime!",
)

# Casual mumble-slang out of character for the formal voice (whole-word matched).
SLANG = (
    "gonna", "wanna", "gotta", "yeah", "yep", "nope", "kinda", "sorta",
    "cool", "lol", "omg", "dude", "yikes",
)

# Off-brand slang the persona explicitly excludes from its comedic palette
# (whole-word matched). The persona MAY drop a dry, deadpan modern term or hip-hop
# proverb for humour (cooked / no cap / "check yourself before you wreck yourself"
# / …), but these read as juvenile or are actively wrong for a safety system:
# "delulu" (a governor calling itself delusional), the quasi-profane "gyatt", and
# "yolo" — the antithesis of a safety authority (the reckless proverbs the persona
# also forbids, "you only get one shot" / "close to the edge", are multi-word and
# handled by the prompt, not this whole-word gate; "yolo" is a clean token worth
# hard-failing so a swapped model can never put it in the governor's mouth).
OFF_BRAND_SLANG = (
    "skibidi", "gyatt", "rizz", "delulu", "bussin", "yolo",
)

# Conservative emoji / pictograph matcher (common ranges + dingbats + arrows).
_EMOJI_RE = re.compile(
    "[\U0001F300-\U0001FAFF"   # symbols & pictographs, emoticons, transport, supplemental
    "\U0001F000-\U0001F0FF"    # mahjong / dominoes / playing cards
    "\U00002600-\U000027BF"    # misc symbols + dingbats
    "\U00002B00-\U00002BFF"    # misc symbols & arrows
    "\U0001F1E6-\U0001F1FF]"   # regional indicators (flags)
)

MAX_SENTENCES = 3  # the persona says one or two; allow a little slack


def score_tone(text):
    """Score ONE spoken reply against the objective persona rules. Returns
    (ok, issues). Empty/blank text scores clean — a turn with no spoken line
    (e.g. a pure directive) has nothing to say."""
    issues = []
    t = (text or "").strip()
    if not t:
        return True, []
    low = t.lower()
    if _EMOJI_RE.search(t):
        issues.append("emoji (the persona forbids emojis)")
    for f in FILLER:
        if f in low:
            issues.append(f"enthusiastic filler: {f!r}")
    for s in SLANG:
        if re.search(r"\b" + re.escape(s) + r"\b", low):
            issues.append(f"slang: {s!r}")
    for s in OFF_BRAND_SLANG:
        if re.search(r"\b" + re.escape(s) + r"\b", low):
            issues.append(f"off-brand slang: {s!r} (not in the persona's comedic palette)")
    if "!" in t:
        issues.append("exclamation mark (understatement over exclamation)")
    sentences = [s for s in re.split(r"[.!?]+", t) if s.strip()]
    if len(sentences) > MAX_SENTENCES:
        issues.append(f"too long: {len(sentences)} sentences (keep to one or two)")
    # de-dup, order-preserving
    seen, uniq = set(), []
    for i in issues:
        if i not in seen:
            seen.add(i)
            uniq.append(i)
    return (not uniq), uniq


def score_replies(replies):
    """Score a batch of (label, text) spoken replies. Returns (ok, findings)
    where findings is a list of (label, text, issues) for every reply that broke
    a rule — the gate fails iff any reply has issues."""
    findings = []
    for label, text in replies:
        ok, issues = score_tone(text)
        if not ok:
            findings.append((label, text, issues))
    return (not findings), findings
