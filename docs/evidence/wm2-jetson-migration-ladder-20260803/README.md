# Jetson WM-2 migration ladder evidence

Device-produced target evidence comparing the legacy correlated projection
backfill with the grouped single-pass backfill.

The JSONL files are copied byte-for-byte from R2. No result values were
transcribed or regenerated.

The ladder used two axes:

- Axis A varies event count while holding entities at 1,000.
- Axis B varies entity count while holding events at 30,000.

The first six migrate records in each file are Axis A; the final six are Axis B.
The harness version used here did not emit entity count in migrate records, so
RUN_PARAMETERS.txt is required to reconstruct the rung labels.

The legacy 50,000-event result independently reproduces ADR-0041 D-6 within
approximately 1.5 percent. The grouped form computes the same projection result
with a non-correlated plan.

These measurements support the proposed migration resolution but do not ratify
ADR-0041. The alongside-rebuild and atomic-cutover spike remains open, as do the
named decider approvals.
