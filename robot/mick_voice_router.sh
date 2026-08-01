#!/usr/bin/env bash
# mick_voice_router.sh — the HYBRID wake-driven voice pipeline:
#
#   wake_word.py | voice_transcribe.sh | voice_turn_router.py
#      (trigger)     (record + STT)        (deterministic route)
#                                            ├─ conversation → /chat/stream → piper
#                                            └─ explicit motion → POST /intent
#                                               (Mick → Occy → checker → release
#                                                → consumer, unchanged)
#
# The sibling of rabbit_voice.sh (which keeps the legacy LLM-routed Rabbit
# dialogue for rollback). Only ONE of the two voice services may run —
# mick-voice-router.service and rabbit-voice.service declare mutual
# Conflicts= so they can never compete for the microphone.
#
# 🔴 The router NEVER bypasses the governed motion path: motion is TEXT
# POSTed to mick /intent (exactly what a human typing hits); ambiguity fails
# closed to a fixed clarification with no endpoint contacted; conversation
# talks only to the chat sidecar. See voice_turn_router.py.
#
# ROS is deliberately NOT sourced here: the router has no perception
# grounding (chat has no live robot state by design), so the pipeline stays
# lean. The legacy rabbit_voice.sh keeps its ROS sourcing for
# rabbit_converse's "what do you see".
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Shared env (STT/RECORD/TTS/chat+intent URLs — robot/install/rabbit.env.example)
if [ -f /etc/kirra/robot.env ]; then set -a; . /etc/kirra/robot.env; set +a; fi

echo "mick_voice_router: wake phrase → follow-up turn → route (chat | governed /intent). Ctrl-C quits." >&2
python3 "$HERE/wake_word.py" | "$HERE/voice_transcribe.sh" | python3 "$HERE/voice_turn_router.py"
