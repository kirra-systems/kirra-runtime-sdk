# Active Work

> Maximum 3 tasks in flight at once (WIP limit). Matches In Progress column on
> the project board. Pull from Ready when a slot opens; move to done.md on merge.
>
> **Authority model reminder (all active tasks):**
> - **LockedOut** → hard stop, 0.0 m/s. No motion. Requires human intervention.
> - **Degraded** → MRC fallback cap, 5.0 m/s ceiling. Velocity cap, not a veto.
> - **Nominal** → nominal profile, 35.0 m/s ceiling, stricter accel rate-limit.
> - **Governor unreachable** → Degraded semantics locally (MRC cap + log event).
> - **RSS unsafe** → Degraded semantics (MRC cap + log event).
> LockedOut and Degraded are **separate code paths** — never shared.
> Synchronous path: `planned_cmd → governor → final_cmd`.
>
> **Test count reminder:** parko-core has ~30–40 tests (NOT 333). kirra-runtime-sdk
> holds ~333 tests. Do not conflate the two.

---

