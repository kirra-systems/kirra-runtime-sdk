"""voice — composes the existing kirra_voice_doctor.sh (STT/TTS engines, model
files, and the ALSA plughw drift check) as one module. The shell doctor stays
independently runnable; this wrap only maps its exit + --quiet line into the
framework schema.
"""
import os

from doctor.core import detail, run_cmd

NAME = "voice"
DESCRIPTION = "voice/audio config (wraps kirra_voice_doctor.sh)"
DEFAULT, HEAVY, TIMEOUT_S = True, False, 25


def run(ctx):
    script = os.path.join(ctx["here"], "kirra_voice_doctor.sh")
    if not os.path.isfile(script):
        return {"details": [detail("kirra_voice_doctor.sh", "UNKNOWN",
                                   f"not found at {script}")]}
    rc, out, err = run_cmd(["bash", script, "--quiet"], timeout_s=20)
    line = (out.strip().splitlines() or [err.strip() or "no output"])[-1]
    if rc == 0:
        return {"details": [detail("voice doctor", "PASS", line)]}
    return {"details": [detail("voice doctor", "FAIL", line,
                               fix="run robot/kirra_voice_doctor.sh for the full ✔/❌ report")],
            "recommended_action": "run robot/kirra_voice_doctor.sh"}
