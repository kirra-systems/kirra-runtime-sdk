#!/usr/bin/env python3
"""Integration test across the REAL transcript-to-action boundary.

Unlike `repo_command_test.py` (which tests the bridge in isolation), this drives
the actual seams the voice loop uses:

  transcript → repo_command.handle()            (the deterministic matcher)
  transcript → skill_registry.plan_skills()     (Gemma's JSON contract)
             → skill_registry.execute_skill_decisions(..., repo_fn=…)

and, for one case, all the way to the REAL `scripts/robot-command.sh` against a
throwaway git repository with a local bare remote — no network, no GitHub, no
Gemma, no audio. That last case is what proves the spoken sentence is derived
from a real executor outcome rather than a mocked one.

Runs standalone (`python3 robot/repo_command_integration_test.py`); also
importable under pytest.
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import repo_command as rc  # noqa: E402
import skill_registry as sk  # noqa: E402

_FAILURES: list[str] = []


def check(cond, label):
    if cond:
        print(f"  ok   - {label}")
    else:
        print(f"  FAIL - {label}")
        _FAILURES.append(label)


def git(repo, *args, check_rc=True):
    p = subprocess.run(["git", "-C", str(repo), *args], shell=False,
                       capture_output=True, text=True)
    if check_rc and p.returncode != 0:
        raise RuntimeError(f"git {args} failed: {p.stderr}")
    return p.stdout.strip()


def make_repo(root):
    """A clone of a local bare remote, on a feature branch with one commit."""
    remote = Path(root) / "remote.git"
    seed = Path(root) / "seed"
    clone = Path(root) / "clone"
    subprocess.run(["git", "init", "--quiet", "--bare", "--initial-branch=main",
                    str(remote)], check=True)
    subprocess.run(["git", "init", "--quiet", "--initial-branch=main", str(seed)],
                   check=True)
    for k, v in (("user.name", "Test"), ("user.email", "t@e"),
                 ("commit.gpgsign", "false")):
        git(seed, "config", k, v)
    (seed / "base.txt").write_text("base\n", encoding="utf-8")
    git(seed, "add", "base.txt")
    git(seed, "commit", "--quiet", "-m", "base")
    git(seed, "remote", "add", "origin", str(remote))
    git(seed, "push", "--quiet", "-u", "origin", "main")
    subprocess.run(["git", "clone", "--quiet", str(remote), str(clone)], check=True)
    for k, v in (("user.name", "Test"), ("user.email", "t@e"),
                 ("commit.gpgsign", "false")):
        git(clone, "config", k, v)
    return remote, clone


# ── seam 1: the deterministic matcher (no Gemma at all) ──────────────────────
print("== transcript -> repo_command.handle (deterministic) ==")

os.environ["KIRRA_REPO_CMD_ENABLED"] = "1"


class Recorder:
    """A fake executor that records argvs and returns a canned exit code."""

    def __init__(self, code=0):
        self.code = code
        self.calls = []

    def __call__(self, argv):
        self.calls.append(list(argv))
        if argv[0] == "git":
            joined = " ".join(argv[1:])
            if "symbolic-ref" in joined:
                return 0, "main\n", ""
            if "--short" in joined:
                return 0, "abc1234\n", ""
            if "rev-parse" in joined:
                return 0, "abc1234def5678\n", ""
            return 0, "", ""
        return self.code, "", ""

    def executor_calls(self):
        return [c for c in self.calls if c and c[0] != "git"]


r = Recorder(0)
spoken = rc.handle("Hey Parker, sync to main.", wake_identity="parker", runner=r)
check(spoken is not None and "synchronized" in spoken.lower(),
      f"'Hey Parker, sync to main.' produces a success sentence: {spoken!r}")
check(r.executor_calls() == [["bash", str(rc.EXECUTOR), "Sync to main"]],
      "the deterministic matcher invoked the real executor phrase")

r = Recorder(0)
spoken = rc.handle("what do you see?", runner=r)
check(spoken is None and r.executor_calls() == [],
      "ordinary conversation falls through to the LLM (returns None), nothing runs")

r = Recorder(0)
spoken = rc.handle("sync to main and publish my work", runner=r)
check(spoken == rc.clarification_question() and r.executor_calls() == [],
      "an ambiguous utterance asks for clarification and does NOT fall through")

os.environ["KIRRA_REPO_CMD_ENABLED"] = "0"
r = Recorder(0)
check(rc.handle("sync to main", runner=r) is None and r.executor_calls() == [],
      "gate off -> handle() is inert (byte-identical router)")
os.environ["KIRRA_REPO_CMD_ENABLED"] = "1"


# ── seam 2: Gemma's JSON contract through the registry ───────────────────────
print("== Gemma JSON -> plan_skills -> execute_skill_decisions ==")

spoke, fenced = [], []
GEMMA_REPLY = ('{"say": "Getting the workspace onto main.", '
               '"skills": [{"name": "sync_to_main", "parameters": {}}]}')
say, decisions = sk.plan_skills(GEMMA_REPLY)
r = Recorder(0)


def repo_sink(intent):
    _res, sentence = rc.handle_intent(intent, runner=r)
    return sentence


counts = sk.execute_skill_decisions(
    decisions, fence_fn=lambda d: fenced.append(d) or "ok",
    speak_fn=lambda t: spoke.append(t), repo_fn=repo_sink)
check(say == "Getting the workspace onto main.", "Gemma's `say` survives the parse")
check(counts[sk.REPO_CMD] == 1 and fenced == [],
      "a repo skill executes via repo_fn and never touches the motion fence")
check(len(spoke) == 1 and "synchronized" in spoke[0].lower(),
      f"the executor's outcome is what gets spoken: {spoke!r}")
check(r.executor_calls() == [["bash", str(rc.EXECUTOR), "Sync to main"]],
      "the registry path reached the same allow-listed argv")

# Gemma emitting a shell-shaped skill name must reach nothing.
spoke, fenced = [], []
r = Recorder(0)
_say, decisions = sk.plan_skills(
    '{"say":"sure","skills":[{"name":"bash -c \'rm -rf /\'","parameters":{}}]}')
sk.execute_skill_decisions(decisions, fence_fn=lambda d: fenced.append(d) or "ok",
                           speak_fn=lambda t: spoke.append(t), repo_fn=repo_sink)
check(r.executor_calls() == [] and fenced == [],
      "a shell-shaped skill name executes NOTHING")
check(any("can't do that" in s.lower() for s in spoke),
      f"it is refused out loud: {spoke!r}")


# ── seam 3: the REAL executor against a throwaway repo ───────────────────────
print("== transcript -> REAL scripts/robot-command.sh -> spoken result ==")

with tempfile.TemporaryDirectory() as td:
    _remote, clone = make_repo(td)
    cwd = os.getcwd()
    try:
        os.chdir(clone)

        # (a) SUCCESS: on a clean clone, "sync to main" really synchronizes.
        spoken = rc.handle("Hey Rabbit, sync to main.", wake_identity="rabbit")
        head = git(clone, "rev-parse", "HEAD")
        branch = git(clone, "symbolic-ref", "--quiet", "--short", "HEAD")
        check(branch == "main", f"the real executor left us on main (got {branch})")
        check(spoken is not None and head[:7] in spoken and "synchronized" in spoken.lower(),
              f"the spoken sentence names the REAL commit: {spoken!r}")

        # (b) REFUSAL: publishing from main is refused even with a clean tree.
        #     The executor checks the branch BEFORE the working tree (the
        #     documented step order), so this must be exercised while still on
        #     main — which is also why (c) below needs a feature branch.
        spoken = rc.handle("publish this branch")
        check(spoken is not None and "on main" in spoken.lower(),
              f"publishing from main is refused: {spoken!r}")

        # (c) REFUSAL: a dirty tree, spoken as a refusal from the REAL exit code.
        git(clone, "switch", "--quiet", "--create", "feat/dirty-demo")
        (clone / "scratch.txt").write_text("uncommitted\n", encoding="utf-8")
        spoken = rc.handle("publish my work")
        check(spoken is not None and "didn't publish" in spoken.lower()
              and "uncommitted changes" in spoken.lower(),
              f"a real dirty tree is spoken as a refusal: {spoken!r}")
        check("scratch.txt" in spoken, "the refusal names the offending file")
        check("synchronized" not in spoken.lower()
              and "published feat" not in spoken.lower(),
              "the refusal does not also read as success")
        check((clone / "scratch.txt").exists(),
              "the operator's uncommitted file still exists (nothing discarded)")
        # And nothing was pushed for that branch.
        pushed = subprocess.run(
            ["git", "-C", str(_remote), "rev-parse", "--verify", "--quiet",
             "refs/heads/feat/dirty-demo"], shell=False, capture_output=True, text=True)
        check(pushed.returncode != 0,
              "a refused publish pushed nothing to the real remote")

        # Back to a clean state for (d).
        (clone / "scratch.txt").unlink()
        git(clone, "switch", "--quiet", "main")

        # (d) SUCCESS: on a real feature branch with a commit, publish works and
        #     the remote genuinely receives it.
        git(clone, "switch", "--quiet", "--create", "feat/voice-demo")
        (clone / "new.txt").write_text("work\n", encoding="utf-8")
        git(clone, "add", "new.txt")
        git(clone, "commit", "--quiet", "-m", "voice demo")
        local_head = git(clone, "rev-parse", "HEAD")
        spoken = rc.handle("Hey Parker, publish my work.", wake_identity="parker")
        remote_head = git(_remote, "rev-parse", "refs/heads/feat/voice-demo")
        check(remote_head == local_head,
              "the real remote received exactly the local HEAD")
        check(spoken is not None and "feat/voice-demo" in spoken
              and local_head[:7] in spoken,
              f"success names the real branch and commit: {spoken!r}")
    finally:
        os.chdir(cwd)

print()
if _FAILURES:
    print(f"== {len(_FAILURES)} FAILED ==")
    for f in _FAILURES:
        print(f"   - {f}")
    sys.exit(1)
print("== repo_command integration: all checks passed ==")


def test_repo_command_integration():
    """pytest entry point — the module body above is the suite."""
    assert not _FAILURES
