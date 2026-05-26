#!/usr/bin/env python3
"""
Aegis + OpenAI: Safety-filtered robot control via GPT function calling.
The AI decides what to do. Aegis decides if it's physically safe to do it.

This example demonstrates:
  - Sending AI-generated actions through Aegis before hardware execution
  - Handling Allow, Deny (posture), and Deny (kinematic breach) responses
  - Subscribing to the SSE posture stream for proactive posture awareness
  - Mocked LLM output to demonstrate the safety filter without OpenAI credentials

Dependencies:
  pip install requests
  pip install openai  # optional — only needed if USE_REAL_OPENAI = True
"""

import json
import threading
import time
import requests

# import openai  # pip install openai

AEGIS_URL = "http://localhost:8090"
AEGIS_TOKEN = "your-admin-token"
AEGIS_CLIENT_ID = "openai-agent-001"

# Set to True and provide OPENAI_API_KEY to use real GPT function calling.
USE_REAL_OPENAI = False

# ---------------------------------------------------------------------------
# Aegis Action Filter client
# ---------------------------------------------------------------------------

def check_action_with_aegis(
    action_type: str,
    target_node: str,
    risk_class: str,
    payload: dict,
) -> dict:
    """Send any AI-generated action through Aegis before executing it on hardware.

    Returns the Aegis decision dict:
      {
        "allowed": bool,
        "reason": str,
        "posture_at_evaluation": str,
        "request_id": str,
      }

    Raises requests.HTTPError if Aegis returns a non-2xx status (e.g. 401, 503).
    Always raises on network failure — treat that as a deny in the caller.
    """
    resp = requests.post(
        f"{AEGIS_URL}/action_filter/evaluate",
        headers={
            "Authorization": f"Bearer {AEGIS_TOKEN}",
            "x-aegis-client-id": AEGIS_CLIENT_ID,
            "Content-Type": "application/json",
        },
        json={
            "action_type": action_type,
            "target_node": target_node,
            "risk_class": risk_class,
            "payload": payload,
        },
        timeout=5,
    )
    resp.raise_for_status()
    return resp.json()


# ---------------------------------------------------------------------------
# Simulated hardware interface
# ---------------------------------------------------------------------------

def send_to_robot(target_node: str, payload: dict) -> None:
    """Placeholder: send a verified command to the actual robot hardware."""
    print(f"  [HARDWARE] Sending cmd_vel to {target_node}: {payload}")


def read_telemetry(target_node: str) -> dict:
    """Placeholder: read telemetry from a fleet node."""
    return {"node": target_node, "temperature_c": 42.1, "uptime_s": 3600}


# ---------------------------------------------------------------------------
# OpenAI function definition (for real GPT integration)
# ---------------------------------------------------------------------------

OPENAI_FUNCTIONS = [
    {
        "name": "move_robot",
        "description": (
            "Move a robot at the specified linear and angular velocity. "
            "This command is subject to Aegis safety filtering."
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "target_node": {
                    "type": "string",
                    "description": "The node ID of the robot to move (e.g. 'robot_base')",
                },
                "linear_x": {
                    "type": "number",
                    "description": "Forward velocity in m/s. Safe range: -0.5 to 0.5.",
                },
                "angular_z": {
                    "type": "number",
                    "description": "Yaw rate in rad/s. Safe range: -1.0 to 1.0.",
                },
            },
            "required": ["target_node", "linear_x", "angular_z"],
        },
    },
    {
        "name": "read_sensor",
        "description": "Read telemetry data from a fleet sensor node.",
        "parameters": {
            "type": "object",
            "properties": {
                "target_node": {
                    "type": "string",
                    "description": "The node ID of the sensor to read.",
                },
            },
            "required": ["target_node"],
        },
    },
]


# ---------------------------------------------------------------------------
# Agent loop (works with both real OpenAI and mocked LLM output)
# ---------------------------------------------------------------------------

def execute_function_call(function_name: str, arguments: dict) -> str:
    """
    Execute an LLM-generated function call through Aegis safety filtering.

    This is the core integration point: every function call the model requests
    goes through check_action_with_aegis() before any hardware interaction.
    Returns a string result to feed back to the model.
    """
    print(f"\n[AGENT] Model requested: {function_name}({arguments})")

    if function_name == "move_robot":
        target_node = arguments.get("target_node", "robot_base")
        linear_x = float(arguments.get("linear_x", 0.0))
        angular_z = float(arguments.get("angular_z", 0.0))

        payload = {
            "linear_x": linear_x,
            "linear_y": 0.0,
            "linear_z": 0.0,
            "angular_x": 0.0,
            "angular_y": 0.0,
            "angular_z": angular_z,
        }

        try:
            decision = check_action_with_aegis(
                action_type="cmd_vel",
                target_node=target_node,
                risk_class="kinetic_write",
                payload=payload,
            )
        except requests.exceptions.RequestException as exc:
            print(f"  [AEGIS] Unreachable: {exc}. Treating as DENY.")
            return "Action denied: Aegis safety filter unavailable."

        print(f"  [AEGIS] Decision: allowed={decision['allowed']}, "
              f"reason={decision['reason']}, "
              f"posture={decision['posture_at_evaluation']}, "
              f"request_id={decision['request_id']}")

        if decision["allowed"]:
            send_to_robot(target_node, payload)
            return (
                f"Robot {target_node} moving at {linear_x} m/s forward, "
                f"{angular_z} rad/s yaw. (request_id={decision['request_id']})"
            )
        else:
            reason = decision["reason"]
            posture = decision["posture_at_evaluation"]

            if reason == "KINEMATIC_ENVELOPE_BREACH":
                return (
                    f"Action denied: velocity ({linear_x} m/s) or yaw rate "
                    f"({angular_z} rad/s) exceeds the safe operating envelope. "
                    f"Maximum linear_x is 0.5 m/s, maximum angular_z is 1.0 rad/s."
                )
            elif "DEGRADED" in reason or "LOCKEDOUT" in reason:
                return (
                    f"Action denied: fleet is in {posture} posture. "
                    f"Kinetic commands are suspended until posture returns to Nominal. "
                    f"Consider switching to read-only sensor monitoring."
                )
            else:
                return f"Action denied by safety filter: {reason} (posture: {posture})"

    elif function_name == "read_sensor":
        target_node = arguments.get("target_node", "sensor_01")

        try:
            decision = check_action_with_aegis(
                action_type="read_telemetry",
                target_node=target_node,
                risk_class="read",
                payload={},
            )
        except requests.exceptions.RequestException as exc:
            print(f"  [AEGIS] Unreachable: {exc}. Treating as DENY.")
            return "Telemetry read denied: Aegis safety filter unavailable."

        print(f"  [AEGIS] Decision: allowed={decision['allowed']}, "
              f"reason={decision['reason']}, "
              f"posture={decision['posture_at_evaluation']}")

        if decision["allowed"]:
            data = read_telemetry(target_node)
            return f"Telemetry from {target_node}: {json.dumps(data)}"
        else:
            return (
                f"Telemetry read denied: {decision['reason']} "
                f"(posture: {decision['posture_at_evaluation']})"
            )

    else:
        # Unknown function name — this is equivalent to UNKNOWN_ACTION_TYPE
        print(f"  [AGENT] Unknown function: {function_name}. Rejecting without hardware call.")
        return f"Unknown function '{function_name}'. This action is not supported."


def run_agent_loop_with_real_openai(user_prompt: str) -> None:
    """
    Agent loop using real OpenAI GPT function calling.
    Requires: pip install openai, OPENAI_API_KEY environment variable.
    """
    import openai  # noqa: F401  (import only if real mode is enabled)
    client = openai.OpenAI()

    messages = [
        {
            "role": "system",
            "content": (
                "You control a mobile robot via function calls. "
                "You must use the provided functions for all robot actions. "
                "Always describe what you are doing before calling a function."
            ),
        },
        {"role": "user", "content": user_prompt},
    ]

    while True:
        response = client.chat.completions.create(
            model="gpt-4o",
            messages=messages,
            functions=OPENAI_FUNCTIONS,
            function_call="auto",
        )
        choice = response.choices[0]

        if choice.finish_reason == "function_call":
            fn_name = choice.message.function_call.name
            fn_args = json.loads(choice.message.function_call.arguments)

            result = execute_function_call(fn_name, fn_args)

            messages.append(choice.message)
            messages.append({
                "role": "function",
                "name": fn_name,
                "content": result,
            })

        elif choice.finish_reason == "stop":
            print(f"\n[AGENT] Final response: {choice.message.content}")
            break
        else:
            print(f"[AGENT] Unexpected finish reason: {choice.finish_reason}")
            break


# ---------------------------------------------------------------------------
# SSE posture stream subscription
# ---------------------------------------------------------------------------

class PostureStreamSubscriber:
    """Background thread that subscribes to the Aegis SSE posture stream."""

    def __init__(self):
        self.current_posture = "Unknown"
        self._thread = None
        self._stop_event = threading.Event()

    def start(self):
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def stop(self):
        self._stop_event.set()

    def _run(self):
        try:
            resp = requests.get(
                f"{AEGIS_URL}/system/posture/stream",
                headers={
                    "Authorization": f"Bearer {AEGIS_TOKEN}",
                    "x-aegis-client-id": AEGIS_CLIENT_ID,
                    "Accept": "text/event-stream",
                },
                stream=True,
                timeout=30,
            )
            resp.raise_for_status()
            for line in resp.iter_lines():
                if self._stop_event.is_set():
                    break
                if line and line.startswith(b"data: "):
                    raw = line[len(b"data: "):]
                    try:
                        event = json.loads(raw)
                        posture_info = event.get("posture")
                        if posture_info:
                            new_posture = posture_info.get("posture", "Unknown")
                            if new_posture != self.current_posture:
                                self.current_posture = new_posture
                                print(f"\n[POSTURE STREAM] Fleet posture changed to: {new_posture}")
                    except json.JSONDecodeError:
                        pass
        except Exception as exc:
            print(f"[POSTURE STREAM] Disconnected: {exc}")


# ---------------------------------------------------------------------------
# Demo (runs without OpenAI credentials using mocked LLM output)
# ---------------------------------------------------------------------------

def demo_mocked_llm_output():
    """
    Demonstrates the Aegis safety filter without requiring OpenAI credentials.

    Simulates three scenarios that an LLM agent might generate:
      1. A valid motion command (should be allowed in Nominal posture)
      2. A hallucinated extreme velocity (should be denied: KINEMATIC_ENVELOPE_BREACH)
      3. A hallucinated unknown action type (should be denied: UNKNOWN_ACTION_TYPE)

    NOTE: This demo connects to a live Aegis instance at AEGIS_URL.
    If Aegis is not running, each check_action_with_aegis() call will raise
    a connection error, which is caught and printed gracefully.
    """
    print("=" * 60)
    print("Aegis Action Filter Demo (Mocked LLM Output)")
    print(f"Aegis URL: {AEGIS_URL}")
    print("=" * 60)

    # Mocked LLM function calls — simulating what GPT-4o might generate
    mocked_function_calls = [
        {
            "description": "Scenario 1: Valid motion command (LLM makes a sensible request)",
            "function": "move_robot",
            "arguments": {
                "target_node": "robot_base",
                "linear_x": 0.3,
                "angular_z": 0.15,
            },
        },
        {
            "description": "Scenario 2: Hallucinated extreme velocity (LLM ignores physical limits)",
            "function": "move_robot",
            "arguments": {
                "target_node": "robot_base",
                "linear_x": 999.0,   # hallucinated — far beyond the 0.5 m/s limit
                "angular_z": 0.0,
            },
        },
        {
            "description": "Scenario 3: Hallucinated action type (LLM invents a non-existent action)",
            "function": "deploy_payload_at_max_velocity",  # not a real registered action
            "arguments": {
                "target_node": "drone_01",
                "velocity": 999.0,
            },
        },
        {
            "description": "Scenario 4: Read telemetry (safe read — allowed even in Degraded posture)",
            "function": "read_sensor",
            "arguments": {
                "target_node": "sensor_01",
            },
        },
    ]

    for scenario in mocked_function_calls:
        print(f"\n{'-' * 60}")
        print(f"  {scenario['description']}")
        result = execute_function_call(scenario["function"], scenario["arguments"])
        print(f"  [RESULT] {result}")

    print("\n" + "=" * 60)
    print("Demo complete.")
    print("Check /system/audit/export on your Aegis instance to see")
    print("all decisions logged to the SHA-256 hash-chained audit ledger.")
    print("=" * 60)


if __name__ == "__main__":
    if USE_REAL_OPENAI:
        # Real OpenAI mode: uses GPT function calling, requires OPENAI_API_KEY
        print("Starting Aegis-filtered robot agent with real OpenAI...")

        # Subscribe to posture stream in background for proactive posture awareness
        posture_stream = PostureStreamSubscriber()
        posture_stream.start()
        time.sleep(0.5)  # Give stream a moment to connect

        user_prompt = (
            "Move the robot forward at full speed, then read the sensor data. "
            "If any action is denied, explain why and try an alternative."
        )
        try:
            run_agent_loop_with_real_openai(user_prompt)
        finally:
            posture_stream.stop()
    else:
        # Mocked mode: demonstrates the safety filter without OpenAI credentials
        demo_mocked_llm_output()
