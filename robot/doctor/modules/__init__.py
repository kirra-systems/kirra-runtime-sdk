"""doctor.modules — one file per diagnostic. Each exposes:

  NAME         stable module id (sorted, addressable via --module NAME)
  DESCRIPTION  one line for reports
  DEFAULT      True → runs in the default set; False → opt-in via --module/--all
               (heavy, sudo-prompting, or environment-dependent checks)
  HEAVY        advisory flag surfaced in metadata
  TIMEOUT_S    per-module isolation timeout
  run(ctx)     -> {"details": [core.detail(...)], "recommended_action": str|None,
                   "metadata": {...}}   (status derived = worst of details)

Modules must be READ-ONLY and must never raise for an *expected* failure —
report it as a detail. Unexpected exceptions are caught by the runner and
become an UNKNOWN for this module only.
"""
