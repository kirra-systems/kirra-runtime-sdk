# R2 — "check for update" (startup timer + voice), stage-and-ask

Two ways to check for a governed software update, both built on the existing OTA
machinery (`kirra-ota-ctl` + the verifier campaign plane). **Policy: stage and
ask** — a check only ever STAGES a verified update; applying is always a
deliberate, health-gated step. Nothing auto-installs, ever.

## What a "check" actually does

`kirra-ota-ctl pull` → GET the node's assignment from the verifier
(`/fleet/campaigns/assignment/{node_id}`) → if a new governor artifact is
assigned, **download + verify** (SHA-256 + Uptane signed metadata: root-keyed
role signatures, freshness, rollback floors) → **stage** it into the inactive A/B
slot. A staged slot is inert until applied.

**Security:** a check only TRIGGERS this. The artifact is cryptographically
verified and health-gated by `kirra-ota-ctl` regardless of who asked — a voice
command (or a timer) is a convenience, never an authorization bypass. An
unsigned/rolled-back/unauthorized artifact is refused at the verify step.

## 1. On startup + periodically (unattended)

```bash
sudo cp robot/install/systemd/kirra-ota-check.{service,timer} /etc/systemd/system/
sudo sed -i "s/__KIRRA_ROBOT_USER__/$USER/" /etc/systemd/system/kirra-ota-check.service
sudo systemctl daemon-reload
sudo systemctl enable --now kirra-ota-check.timer   # ~2 min after boot, then every 6 h
```
`Persistent=true` catches a missed window. Stage-only → it can never change the
running governor unattended.

## 2. By voice ("check for update")

Runs through the Rabbit conversational router (`robot/rabbit_converse.py`), matched
**deterministically** by keyword in `robot/rabbit_ota.py` — NOT the LLM, and NOT
the fenced Mick `/intent` movement door (an OTA check is a system command, not a
movement; the two can never be confused).

| you say | it does | it says |
|---|---|---|
| "check for update" / "any updates?" | `pull` (stage-only) | "…staged, ready — say 'apply update' to install" / "you're up to date" |
| "update status" | `status` | whether an update is staged |
| "apply update" / "install update" | `probe` (HEALTH-GATED commit-or-rollback) | "applied and health-checked" / "didn't pass — rolled back" |

"apply update" is the **ask**: a distinct, deliberate command. It runs the
health-gated probe — an unhealthy new artifact **auto-rolls-back** to the safe
version; it is never a blind commit.

Standalone (for the timer log, or testing):
```bash
python3 robot/rabbit_ota.py check     # or status | apply
```

## Caveats
- **Needs an upstream campaign.** A check only finds something if a release is
  published on the verifier/fleet this node points at. Standalone bench robot,
  no campaign → "you're up to date" (the plumbing is ready for when you push one).
- **Governs the safety-critical governor artifact** — the OTA-managed binary,
  with the full verify/rollback discipline. App-level bits (Rabbit scripts,
  planner) are a separate `git pull` + rebuild if you want those too.
- **Config:** `kirra-ota-ctl` reads node id / verifier URL / slot paths from its
  env — set them in `/etc/kirra/robot.env`. `KIRRA_OTA_CTL` overrides the binary
  path (default `/opt/kirra/bin/kirra-ota-ctl`).

## Follow-up (bench-validated)
The voice "apply" runs `probe` (health-gates a trial). Wiring the full A/B trial
lifecycle — restart the governor into the staged slot, then probe — is the
reboot-spanning flow in `docs/ota/ORIN_APP_LEVEL_AB_DRILL.md`; validate that at
the bench before relying on voice-apply for a live governor swap.
