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

## Possible future work

Not implemented, and deliberately not described above as if they were:
`Open a PR`, `Checkpoint`, `Merge and sync`. Each would need its own
precondition and refusal analysis before earning a phrase — in particular,
anything that creates commits or merges would be the first command in this
language permitted to change history, which is exactly the property the current
two are designed to avoid.
