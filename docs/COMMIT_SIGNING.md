# Commit signing — identity, signature, and the `%G?` trap

**Author correctness and signature presence are different properties with
different remedies.** Conflating them cost this repository two rewritten commits
against history that was signed the entire time.

## The trap, stated once

For SSH signatures, `git log --format=%G?` returns **`N`** in two unrelated
situations:

| situation | `%G?` | actually signed? |
|---|---|---|
| no signature at all | `N` | no |
| signed, but `gpg.ssh.allowedSignersFile` is unset | **`N`** | **yes** |

Git cannot *verify* an SSH signature without an allowed-signers file, and it
reports that inability with the same character it uses for absence. Anything
reading `%G?` as a presence test will call signed commits unsigned.

**Presence is read from the object, not from `%G?`:**

```
git cat-file commit <sha> | grep -q '^gpgsig'   # signed?
git log --format=%G? -1 <sha>                   # verifiable HERE? (different question)
```

Whether GitHub shows a commit as *Verified* is a third question again — it
depends on the signing key being registered to the account, which no local check
can answer.

## What went wrong here

A stop hook read `%G?`, saw `N`, and prescribed
`git commit --amend --no-edit --reset-author`. That was run. It returned 0. The
new commit reported `N` as well, because it was signed and still unverifiable
locally. Two SHAs were burned (`5cb694b2` → `974d92e3`) with byte-identical
trees and no change in signature status.

Three independent faults combined:

1. **`%G?` used as a presence test.** The hook's own comment asserts that
   "signed-but-locally-unverifiable commits report B/U/E" — that is **false for
   SSH**; they report `N`.
2. **No preflight.** `commit.gpgsign=true` states a *requirement*; it says
   nothing about whether signing can *succeed*.
3. **No post-check.** `git commit` exiting 0 was treated as proof of a
   signature. It is not.

A misleading fourth signal made the wrong diagnosis attractive:
`user.signingkey` points at a **zero-byte** `.pub` with no private key beside
it — which looks decisive and is irrelevant. The key material comes from the
agent, and signing works fine.

## The preflight

```
scripts/check-commit-signing.sh              # preflight + range report
scripts/check-commit-signing.sh --preflight-only
scripts/check-commit-signing.sh --range <rev-range>
```

Exit: `0` signing proven usable · `1` configured but unusable · `2` not required
· `3` usage error.

It reports the configuration as **evidence** — format, key reference, whether
the file exists, whether it is empty, whether a private half is beside it or an
agent is present — and then ignores all of that in favour of the only thing that
settles the question: it signs a throwaway object with `git commit-tree -S` in a
`mktemp -d` repository and checks the result for a `gpgsig` header. Verifiability
is reported **separately** from presence, because an unverifiable signature and
an absent one are different defects.

The preflight never amends, commits, stages, checks out, pushes, or writes to
the working tree or index. Running it cannot churn a SHA — which is the damage
it exists to prevent. `scripts/test-check-commit-signing.sh` (19 checks) pins
that, including that the range listing never regresses to `%G?`.

**Provisioning is not assumed.** `user.signingkey` may be a path *or* a literal
key string, and the private half may live beside the `.pub` *or* in an agent.
Hard-coding either model is how a preflight produces a confident wrong answer.

## Which commits are checked

Default range is **unpushed only** (`origin/<branch>..HEAD`). That is deliberate:
published history must not be silently rewritten, and evidence-linked commits
must not be rewritten at all. Widen with `--range` only after deciding to rewrite
history.

There is **no written repository policy** on whether every branch commit must be
signed — this document is the first. Absent one, the preflight reports and does
not enforce.

## Remedies are not interchangeable

| diagnosis | remedy |
|---|---|
| wrong author, signing proven usable | amend is appropriate — for unpushed commits |
| correct author, signing unusable | **do not amend.** Provision a key first |
| signed but `%G?`=N | **do not amend.** Fix the checker |
| unsigned, pushed, evidence-linked | needs an explicit human policy decision |

## This branch

| commit | state | notes |
|---|---|---|
| `c3b04ee5` | **signed**, pushed | `code_commit` in the assist-2 Orin report. **Must not be rewritten** — the SHA is the provenance link to a live measurement. A byte-identical tree does **not** make two SHAs interchangeable for provenance. |
| `974d92e3` | **signed**, local | Current tip. Created by the failed signing remedy; tree-identical to `5cb694b2`. |
| `5cb694b2` | superseded | Replaced by `974d92e3`. Not the current tip. |

Both live commits carry `gpgsig` headers. Both report `%G?` = `N`. Both facts are
correct and not in tension.

## Open policy question — needs a human owner

`gpg.ssh.allowedSignersFile` is unset, so **no commit in this environment can be
locally verified**, only checked for signature presence. Whether to provision an
allowed-signers file, and whether unsigned or unverifiable commits should block
anything, is a repository-policy decision this document does not make.
