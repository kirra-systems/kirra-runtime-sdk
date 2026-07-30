# Robot Command Language

Spoken workflow phrases mapped to safe, auditable Git state transitions.

```bash
./scripts/robot-command.sh "Sync to main"
./scripts/robot-command.sh "Publish my work"
```

Matching is case-insensitive and tolerates surrounding and internal whitespace.

| Phrase | Resulting state |
|---|---|
| `Sync to main` | Clean local `main` exactly synchronized with `origin/main` |
| `Publish my work` | Current feature branch pushed and verified on `origin` |

## Governing principles

1. **Intent over implementation.** A phrase names a destination state, not a
   script. "Sync to main" means *"put me on a clean main that matches the
   remote"* — the operator should not have to know which primitive gets there.
2. **Never destroy local work.** No command stashes, resets, cleans,
   force-pushes, rewrites history, or deletes a file.
3. **Stop when a precondition is not satisfied.** The first unmet precondition
   is a refusal with an actionable message — never a "best effort" that
   half-completes.
4. **Report the resulting repository state.** Every success prints the branch,
   the resolved commit, and the verified relationship to the remote.
5. **Commands are idempotent where practical.** Running `Sync to main` when
   already synchronized succeeds and says so.
6. **Spoken commands are state transitions, not shell aliases.** They validate,
   act, then *verify the postcondition* — an alias does only the middle step.

### Why refusing beats helping

The unsafe routes to "sync to main" — `git stash`, `git reset --hard`,
`git checkout -B main origin/main`, or a merge commit — all reach a state that
*looks* correct while local work is gone or history is rewritten. A stash is the
subtlest: nothing is technically lost, but the work is now invisible in a place
the operator did not choose. So a dirty tree is a **stop**, with the paths
listed, and the operator decides.

---

## `Sync to main`

**Intent** — leave me on a clean local `main` that is exactly `origin/main`,
ready to branch new work from.

**Preconditions**

1. Inside a Git worktree.
2. An `origin` remote exists.
3. The working tree is clean, **including untracked files**.
4. `origin/main` exists after fetching.
5. If local `main` exists, it is an ancestor of `origin/main`
   (fast-forward is possible).

**Exact behavior**

1. Verify worktree, verify `origin`.
2. Verify the working tree is clean — *before* any network or branch operation,
   so a dirty tree costs nothing and changes nothing.
3. `git fetch --prune origin`.
4. Verify `refs/remotes/origin/main` exists.
5. If local `main` exists, verify
   `git merge-base --is-ancestor main origin/main`, then switch to it.
   Otherwise create it: `git switch --create main --track origin/main`.
6. If `HEAD` already equals `origin/main`, do nothing further.
   Else `git merge --ff-only refs/remotes/origin/main`.
7. Verify the postcondition: branch is `main`, `HEAD` equals `origin/main`, and
   the working tree is still clean.

**Postconditions** — on `main`; `HEAD == origin/main`; working tree clean.

**Refusal conditions**

| Condition | Exit |
|---|---|
| Not inside a Git worktree | 3 |
| No `origin` remote, or the fetch failed | 4 |
| Working tree dirty or has untracked files | 5 |
| `origin/main` missing after fetch | 6 |
| Local `main` diverged (fast-forward impossible) | 7 |
| Postcondition verification failed | 12 |

**Never performs** — no stash, no `reset --hard`, no `clean`, no rebase, no
merge commit, no branch deletion, no commit creation, no push.

> The divergence check runs **before** the branch switch, so a diverged local
> `main` leaves you exactly where you started. This is deliberately stricter
> than `merge --ff-only` alone, which would refuse only after switching.

**Example invocation**

```bash
./scripts/robot-command.sh "Sync to main"
```

**Example success output**

```
== Sync to main ==
  fetching origin (with prune)...
  switching to 'main'...
Switched to branch 'main'
Your branch is up to date with 'origin/main'.
  already at origin/main — nothing to fast-forward.

State: synchronized.
  branch                 main
  commit                 87b73e4e32b26bca7a2f40be83d7848d28ed8040
  commit (short)         87b73e4
  upstream               origin/main
  working tree           clean (no changes, no untracked files)
  ready for new work     yes
```

Git's own `switch` / `merge` progress lines are left un-suppressed on purpose:
these commands change repository state, and hiding the underlying operation
would make the report harder to audit, not easier to read. The abbreviated SHA
is whatever length Git chooses for the repository.

Run it again and it is idempotent — the switch is skipped because the branch is
already correct:

```
== Sync to main ==
  fetching origin (with prune)...
  already at origin/main — nothing to fast-forward.

State: synchronized.
  ...
```

**Example failure output** (dirty tree, exit 5)

```
REFUSED: the working tree is not clean.
  Changed or untracked paths:
     M crates/kirra-sidecars/src/taj.rs
    ?? notes.md
  Commit them, or move them aside yourself, then re-run.
  This command will not stash, reset, or discard anything.
```

**Example failure output** (diverged `main`, exit 7)

```
REFUSED: local 'main' has diverged from 'origin/main' — fast-forward is impossible.
  local  main:  6f1c2ab...
  remote origin/main: ccef923...
  Local 'main' holds commits the remote does not.
  Resolve it yourself (rebase or merge deliberately, or move them to a branch).
  This command will not rebase, merge-commit, or reset.
```

---

## `Publish my work`

**Intent** — make my current feature branch exist on `origin` at exactly my
local `HEAD`, and prove it.

**Preconditions**

1. Inside a Git worktree.
2. An `origin` remote exists.
3. `HEAD` is on a branch (not detached).
4. That branch is **not** `main`.
5. The working tree is clean, **including untracked files**.
6. The branch has at least one commit.

**Exact behavior**

1. Verify worktree, verify `origin`.
2. Resolve the branch with `git symbolic-ref --quiet --short HEAD`; empty means
   detached → refuse.
3. Refuse if the branch is `main`.
4. Verify the working tree is clean.
5. Verify `HEAD` resolves (the branch is not unborn).
6. Read the upstream with
   `git for-each-ref --format='%(upstream:short)'`.
   - No upstream → `git push --set-upstream origin HEAD`.
   - Upstream present → `git push` (honours the configured upstream under
     `push.default=simple`).
7. Verify the postcondition: `refs/remotes/<upstream>` resolves to exactly the
   local `HEAD`.

**Postconditions** — the branch exists on `origin` at the local `HEAD`; an
upstream is configured; the working tree is untouched.

**Refusal conditions**

| Condition | Exit |
|---|---|
| Not inside a Git worktree | 3 |
| No `origin` remote | 4 |
| Working tree dirty or has untracked files | 5 |
| Detached `HEAD` | 8 |
| Current branch is `main` | 9 |
| Branch has no commits | 10 |
| The push failed (e.g. not a fast-forward) | 11 |
| Remote-tracking ref does not match `HEAD` | 12 |

**Never performs** — no commit creation, no pull request, no merge, no branch
switch, no tag push, no force-push (neither `--force` nor
`--force-with-lease`), no stash, no discard.

> A non-fast-forward push **fails** (exit 11) and remote history is left
> untouched. There is no escalation path: reconciling a diverged branch is a
> deliberate act the operator performs themselves.

**Example invocation**

```bash
./scripts/robot-command.sh "Publish my work"
```

**Example success output**

```
== Publish my work ==
  no upstream configured — creating origin/feat/demo-branch...
To /path/to/remote.git
 * [new branch]      HEAD -> feat/demo-branch
branch 'feat/demo-branch' set up to track 'origin/feat/demo-branch'.

State: published.
  branch                 feat/demo-branch
  commit                 4e5d21e0a928f1228d6f09b1c57bd43dfb4e2845
  commit (short)         4e5d21e
  remote tracking        origin/feat/demo-branch
  upstream created       yes
  fully published        yes (remote matches local HEAD)
```

On a branch that already has an upstream the first line reads
`upstream is 'origin/<branch>' — pushing...` and `upstream created` reports `no`.

**Example failure output** (on `main`, exit 9)

```
REFUSED: refusing to publish directly from 'main'.
  Move your work to a feature branch:  git switch --create <branch>
```

**Example failure output** (non-fast-forward, exit 11)

```
REFUSED: push to 'origin/feature/race' failed (it is not a fast-forward, or the remote rejected it).
  Nothing local was changed and remote history was not rewritten.
  This command never force-pushes; reconcile the branch deliberately.
```

---

## Exit statuses

| Code | Meaning |
|---|---|
| 0 | Success |
| 2 | Unknown phrase or bad invocation (no repository changes) |
| 3 | Not inside a Git worktree |
| 4 | No `origin` remote, or fetch failed |
| 5 | Working tree not clean |
| 6 | `origin/main` missing |
| 7 | Fast-forward impossible (diverged) |
| 8 | Detached `HEAD` |
| 9 | Publish refused from `main` |
| 10 | Branch has no commits |
| 11 | Push failed |
| 12 | Postcondition verification failed |

## Unknown phrases

An unrecognized phrase exits **2**, makes no repository changes, and prints the
supported phrases. The dispatcher uses a `case` statement over a normalized
string — the phrase selects a function and is never executed. **No `eval`.**

## Prohibited operations

By construction, `scripts/robot-command.sh` contains no:

- `git reset --hard`
- `git clean`
- `git stash`
- `git push --force` / `--force-with-lease`
- automatic commit creation
- automatic conflict resolution

`scripts/test-robot-command.sh` asserts their textual absence (after stripping
comments), so an edit that introduces one reds the suite. It also asserts the
guarantee behaviourally: a non-fast-forward push must fail and leave remote
history unchanged.

## Testing

```bash
bash scripts/test-robot-command.sh
```

25 cases over throwaway repositories with **local bare remotes** — no network,
no GitHub, no dependence on this checkout's state. Each case asserts both the
exit status and the resulting repository state (branch, `HEAD`, remote refs,
file survival). `shellcheck -S error` covers both scripts via the
`static-analysis` CI job, which lints every tracked `*.sh`.

---

# Voice control (Gemma 3 4B)

> **Gemma interprets and explains. The deterministic command executor authorizes
> and acts.**

Say it out loud:

```
Hey Parker, sync to main.
Hey Rabbit, publish my work.
```

Opt-in: `KIRRA_REPO_CMD_ENABLED=1`. Off (default) the router is byte-identical.

## The authority boundary

```
mic → wake_word.py (RMS gate → whisper.cpp tiny → pure token matcher)
        ↓ ONE newline on stdout — the trigger contract, carries NO identity
rabbit_voice.sh → bounded clip → whisper.cpp → transcript
        ↓
rabbit_converse.py :: route()
   ├─ repo_command.handle()  ← DETERMINISTIC matcher, runs BEFORE the model
   └─ Gemma 3 4B (Ollama, KIRRA_RABBIT_MODEL) → {say, skills:[{name}]}
        ↓ skill_registry.plan_skills()  → allow-listed by NAME
        ↓ Decision(REPO_CMD, "<intent>") ← payload is a NAME, never a command
        ↓ execute_skill_decisions(..., repo_fn=_repo_sink)
        ↓
repo_command.run_intent()  → argv ["bash", robot-command.sh, "<allow-listed phrase>"]
        ↓                     shell=False
scripts/robot-command.sh   ← THE AUTHORITY: every precondition, every refusal
        ↓ exit code → structured result
repo_command.speak_result() → rabbit_persona.speak() → piper TTS
```

The deterministic matcher runs **before** Gemma on purpose: a canonical phrase
must not depend on inference being available, or on the model choosing correctly.
Gemma is the fallback for wording the matcher doesn't cover, and the explainer
for results.

## Wake phrases

“Hey Rabbit” and “Hey Parker” (plus `hello`/`yo` forms) are **two names for the
same assistant**. Only the `rabbit` forms are in `DEFAULT_PHRASES`; **“Hey
Parker” wakes only when it is configured**, by listing it in
`KIRRA_WAKE_PHRASES`. A bring-up that expects the second name must set that
variable — `test_a_configured_parker_phrase_wakes` in `wake_word_test.py` pins
both halves of that fact.

Note the two layers do not have to agree. The wake listener decides what *starts*
a turn; once a turn is running, the transcript parsers strip either name, so
“Hey Parker, sync to main.” resolves identically to the Rabbit form whether or
not `parker` is a configured wake phrase (`3c` in `repo_command_test.py`).

🔴 The wake phrase **activates**; it never authorizes. The trigger contract is a
single newline on stdout, which carries no identity, so the name used cannot
select a different persona, policy, or permission set. There is no
per-wake-phrase privilege to preserve or subvert.

## Typed intents

| Intent | Registry kind | Effect |
|---|---|---|
| `sync_to_main` | `REPO` | `Sync to main` |
| `publish_my_work` | `REPO` | `Publish my work` |

Registered in the existing `robot/skill_registry.py` catalog — no parallel
assistant stack. They are the first `REPO`-kind skills: they touch the git
repository, never the wheels, and they take **no parameters**.

## Paraphrases

Canonical: “Sync to main.” · “Publish my work.”

| Sync | Publish |
|---|---|
| Get us onto the latest main | Push my current work |
| Update the workspace to main | Make sure this branch is on origin |
| Get the latest origin main | Publish this branch |
| Prepare the repository for new work | Push the current feature branch |

Case and harmless punctuation are ignored; a wake prefix is stripped before
matching. The vocabulary is **closed** — “rebase onto main”, “merge main”,
“reset to origin”, “delete the branch” match *nothing* and become “I didn't
recognize that as an approved repository command.” There is no general Git
grammar here.

## The allow-list boundary — why Gemma cannot reach a shell

Four independent barriers, each with a test:

1. **Name-only selection.** Gemma emits a skill *name*. `plan_skills` looks it up
   in `REGISTRY`; anything unregistered becomes `REFUSE`. A reply naming
   `bash -c 'rm -rf /'` is an unknown skill.
2. **Parameters discarded.** `dispatch` for a `REPO` skill returns
   `Decision(REPO_CMD, name)` and throws `params` away — there is no field
   through which model text can travel.
3. **Allow-list at the executor.** `run_intent` raises `KeyError` for any intent
   outside the two-key `INTENT_PHRASE` dict, *before* a process starts.
4. **Fixed argv, no shell.** The argv is always exactly
   `["bash", <resolved script path>, <phrase from the dict>]`, run with
   `shell=False`. Only two argvs are constructible; the test asserts exact
   membership rather than scanning a denylist.

The bridge also performs no git mutation of its own — its only git calls are
`symbolic-ref`, `rev-parse`, and `status` (read-only), asserted by test.

## Structured results

Success:

```json
{"command": "sync_to_main", "status": "success", "reason": "ok", "exit_code": 0,
 "branch": "main", "commit_sha": "3c57b74c…", "commit_short": "3c57b74c",
 "message": "Workspace is synchronized and ready for new work."}
```

Refusal — a normal result, not a crash:

```json
{"command": "publish_my_work", "status": "refused",
 "reason": "working_tree_dirty", "exit_code": 5,
 "changed_files": ["src/example.rs"]}
```

Spoken outcomes, deliberately distinct so the operator knows what to do next:

| Case | Spoken |
|---|---|
| Success | “Main is synchronized with origin at commit 3c57b74c, ready for new work.” |
| Success (publish) | “Published feat/x to origin at commit 3c57b74c.” |
| Policy refusal | “I didn't publish the branch because the working tree has uncommitted changes: src/example.rs.” |
| Execution failure | “The fetch failed, so the repository was not changed.” |
| Unknown | “I didn't recognize that as an approved repository command.” |
| Ambiguous | “Do you want me to sync to main or publish the current branch?” |

`speak_result` reads `status`; **success wording is unreachable unless the
executor exited 0**, and an exit code not in `EXIT_MAP` becomes an `error`.

## Confirmation policy

These two need no extra confirmation when the intent is clear, because the
executor itself refuses unsafe state: `Sync to main` refuses a dirty or diverged
tree, and `Publish my work` refuses unsafe state and never force-pushes. The
worst outcome of a misheard command is a refusal.

🔴 **Do not generalize this.** Higher-impact future actions — merge, deploy,
reset, delete, mission execution, any actuator command — are *not* self-limiting
in the same way and may require explicit spoken confirmation or separate
authorization. That judgement belongs with the command, not with the voice layer.

## Audit

Opt-in via `KIRRA_REPO_CMD_AUDIT_PATH` (JSONL, one record per request):
timestamp, wake identity, transcript, intent, parser decision, whether execution
was attempted, status, reason, exit code, branch, commit SHA, changed files.

Records are written for **non-executed** requests too (unknown and ambiguous),
which is what makes “why didn't it do anything?” answerable.

Never logged: environment contents, tokens, credentials, or remote URLs (a URL
can embed a token). A failed append warns on stderr and returns `False` — a full
disk must not take the assistant down. The wake listener's privacy rule is
unaffected: it still logs no continuous audio, and only the post-wake command
utterance is recorded.

## Adding a future approved command safely

1. Implement the state transition in `scripts/robot-command.sh` with explicit
   preconditions, refusals, and postcondition verification. **The executor is the
   authority** — never put the logic in the voice layer.
2. Document it here: intent, preconditions, behaviour, postconditions, refusals,
   what it never does, exit code.
3. Add the phrase to `INTENT_PHRASE` in `robot/repo_command.py` and a row to
   `EXIT_MAP` for any new exit code.
4. Add conservative paraphrases to the closed pattern list. Resist generality.
5. Register the skill with kind `REPO` in `robot/skill_registry.py` and mention
   it in `skills_prompt_fragment`.
6. Add a refusal sentence to `_REFUSAL_SENTENCE` / `_ERROR_SENTENCE`.
7. **Decide whether it needs spoken confirmation** — if its worst misfire is
   worse than a refusal, it does.
8. Extend `repo_command_test.py` and the integration test.

## Voice limitations

- No conversational state is carried between wake interactions for repo commands:
  each utterance is resolved independently, so a clarification question must be
  answered with a full phrase (“sync to main”), not “the first one”.
- Wake identity is recorded only when the caller passes it. The wake trigger
  carries no identity over its stdout contract, so in the shipped pipeline the
  field is `"unknown"` unless a future trigger protocol conveys it.
- The paraphrase list is hand-curated, not learned; unusual phrasing falls
  through to Gemma, and if Gemma doesn't pick an intent the operator is told it
  wasn't recognized.

## Possible future work

Not implemented, and deliberately not described above as if they were:
`Open a PR`, `Checkpoint`, `Merge and sync`. Each would need its own
precondition and refusal analysis before earning a phrase — in particular,
anything that creates commits or merges would be the first command in this
language permitted to change history, which is exactly the property the current
two are designed to avoid.
