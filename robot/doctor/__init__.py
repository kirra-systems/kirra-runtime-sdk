"""doctor — the R2 diagnostics framework (read-only observability layer).

Composes the existing per-concern checks (kirra_voice_doctor.sh,
preflight_autostart.sh, lint_robot_env.sh, the verifier's /health//ready//metrics)
into one deterministic, modular runner. See docs/diagnostics.md.

🔴 SECURITY MODEL: diagnostics are OBSERVABILITY, never a safety authority.
Every module is read-only (no writes, no GPIO actuation, no firmware, no motion,
no /intent). A diagnostics verdict never gates the planner, the governor, RSS,
the checker, or the fence — those fail closed on their own, independently.
"""
