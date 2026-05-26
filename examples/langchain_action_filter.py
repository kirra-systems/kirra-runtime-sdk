#!/usr/bin/env python3
"""
Aegis + LangChain: Safety-filtered robot control via LangChain tools.
The AI decides what to do. Aegis decides if it's physically safe to do it.

This example demonstrates:
  - Wrapping robot actions as LangChain tools with Aegis safety filtering
  - Handling Allow, Deny (posture), and Deny (kinematic breach) responses
  - Mocked agent output to demonstrate the safety filter without LangChain/OpenAI

Dependencies:
  pip install requests
  pip install langchain langchain-openai  # optional — for real agent

The key architectural point: the @tool function body calls Aegis BEFORE
sending any command to hardware. The LangChain agent sees only the tool's
return value — it never has direct hardware access.
"""

import json
import requests

AEGIS_URL = "http://localhost:8090"
AEGIS_TOKEN = "your-admin-token"
AEGIS_CLIENT_ID = "langchain-agent-001"

# ---------------------------------------------------------------------------
# Aegis client helper
# ---------------------------------------------------------------------------

def _aegis_evaluate(
    action_type: str,
    target_node: str,
    risk_class: str,
    payload: dict,
) -> dict:
    """Send an action claim to Aegis for posture-aware safety evaluation.

    Returns the Aegis decision dict. Raises requests.HTTPError on HTTP errors.
    Raises requests.ConnectionError / requests.Timeout on network failures.
    Callers must treat any exception from this function as a DENY.
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
# LangChain tool definitions with Aegis safety filtering
#
# Each @tool function:
#   1. Calls Aegis to get a safety decision
#   2. Only proceeds with hardware interaction if allowed=True
#   3. Returns a human-readable result (which the agent sees as tool output)
#
# If LangChain is not installed, we provide stub decorators so the module
# can still be imported and demonstrated without the full LangChain stack.
# ---------------------------------------------------------------------------

try:
    from langchain.tools import tool as langchain_tool
    LANGCHAIN_AVAILABLE = True
except ImportError:
    LANGCHAIN_AVAILABLE = False

    # Minimal stub decorator so the module runs without langchain installed
    def langchain_tool(func):
        func._is_tool = True
        return func


@langchain_tool
def move_robot(target_node: str, linear_x: float, angular_z: float) -> str:
    """
    Move a robot at the specified linear and angular velocity.

    Args:
        target_node: The fleet node ID of the robot (e.g. 'robot_base').
        linear_x: Forward velocity in m/s. Safe range: -0.5 to 0.5.
        angular_z: Yaw rate in rad/s. Safe range: -1.0 to 1.0.

    Returns:
        A string describing the outcome (allowed or denied, with reason).
    """
    payload = {
        "linear_x": float(linear_x),
        "linear_y": 0.0,
        "linear_z": 0.0,
        "angular_x": 0.0,
        "angular_y": 0.0,
        "angular_z": float(angular_z),
    }

    print(f"\n[TOOL] move_robot called: target={target_node}, "
          f"linear_x={linear_x}, angular_z={angular_z}")

    try:
        decision = _aegis_evaluate(
            action_type="cmd_vel",
            target_node=target_node,
            risk_class="kinetic_write",
            payload=payload,
        )
    except requests.exceptions.RequestException as exc:
        print(f"  [AEGIS] Unreachable: {exc}. Halting action.")
        return (
            "Motion command denied: Aegis safety filter is unreachable. "
            "No hardware command was sent."
        )

    print(f"  [AEGIS] allowed={decision['allowed']}, "
          f"reason={decision['reason']}, "
          f"posture={decision['posture_at_evaluation']}, "
          f"request_id={decision['request_id']}")

    if decision["allowed"]:
        # Only here do we interact with hardware
        _send_cmd_vel_to_hardware(target_node, payload)
        return (
            f"Robot {target_node} is now moving at {linear_x} m/s forward "
            f"and {angular_z} rad/s yaw. "
            f"(Safety check passed, request_id={decision['request_id']})"
        )

    reason = decision["reason"]
    posture = decision["posture_at_evaluation"]

    if reason == "KINEMATIC_ENVELOPE_BREACH":
        return (
            f"Motion denied: the requested velocity ({linear_x} m/s) or yaw rate "
            f"({angular_z} rad/s) exceeds the robot's safe operating envelope. "
            f"Maximum values are 0.5 m/s and 1.0 rad/s. Please reduce the values."
        )
    elif "LOCKEDOUT" in reason:
        return (
            f"Motion denied: fleet is in LockedOut posture. "
            f"All commands are blocked until the fleet recovers to Nominal. "
            f"Use read_sensor to monitor fleet state."
        )
    elif "DEGRADED" in reason:
        return (
            f"Motion denied: fleet is in Degraded posture ({posture}). "
            f"Kinetic commands are suspended. "
            f"You can still use read_sensor to observe the system."
        )
    else:
        return (
            f"Motion denied by Aegis safety filter. "
            f"Reason: {reason}. Fleet posture: {posture}."
        )


@langchain_tool
def read_sensor(target_node: str) -> str:
    """
    Read telemetry data from a fleet sensor node.

    This is a read-only operation and is permitted even when the fleet is in
    Degraded posture. It is blocked only in LockedOut posture.

    Args:
        target_node: The fleet node ID of the sensor (e.g. 'sensor_01').

    Returns:
        JSON telemetry data or a denial message.
    """
    print(f"\n[TOOL] read_sensor called: target={target_node}")

    try:
        decision = _aegis_evaluate(
            action_type="read_telemetry",
            target_node=target_node,
            risk_class="read",
            payload={},
        )
    except requests.exceptions.RequestException as exc:
        print(f"  [AEGIS] Unreachable: {exc}. Halting read.")
        return "Telemetry read denied: Aegis safety filter is unreachable."

    print(f"  [AEGIS] allowed={decision['allowed']}, "
          f"reason={decision['reason']}, "
          f"posture={decision['posture_at_evaluation']}")

    if decision["allowed"]:
        data = _read_hardware_telemetry(target_node)
        return f"Telemetry from {target_node}: {json.dumps(data)}"
    else:
        return (
            f"Telemetry read denied: {decision['reason']} "
            f"(posture: {decision['posture_at_evaluation']})"
        )


@langchain_tool
def get_fleet_health() -> str:
    """
    Query the current fleet posture from Aegis (public, unauthenticated endpoint).

    Returns a summary of the current fleet-wide posture state.
    This does not go through the action filter — it reads public posture state.
    """
    print("\n[TOOL] get_fleet_health called")
    try:
        resp = requests.get(f"{AEGIS_URL}/fleet/posture", timeout=5)
        resp.raise_for_status()
        data = resp.json()
        fleet = data.get("fleet", [])
        if not fleet:
            return "Fleet posture: no nodes registered."
        summary = []
        for node in fleet:
            node_id = node.get("node_id", "unknown")
            posture = node.get("posture", "Unknown")
            summary.append(f"{node_id}: {posture}")
        return "Fleet health:\n" + "\n".join(summary)
    except requests.exceptions.RequestException as exc:
        return f"Could not retrieve fleet health: {exc}"


# ---------------------------------------------------------------------------
# Simulated hardware interfaces
# ---------------------------------------------------------------------------

def _send_cmd_vel_to_hardware(target_node: str, payload: dict) -> None:
    """Placeholder: send a verified velocity command to the robot hardware."""
    print(f"  [HARDWARE] cmd_vel sent to {target_node}: {payload}")


def _read_hardware_telemetry(target_node: str) -> dict:
    """Placeholder: read live telemetry from a fleet node."""
    return {
        "node": target_node,
        "temperature_c": 41.5,
        "uptime_s": 7200,
        "battery_pct": 87.3,
        "status": "nominal",
    }


# ---------------------------------------------------------------------------
# Real LangChain agent loop (requires langchain, langchain-openai, OPENAI_API_KEY)
# ---------------------------------------------------------------------------

def run_langchain_agent(user_prompt: str) -> None:
    """
    Run a LangChain ReAct agent with Aegis-filtered robot tools.
    Requires: pip install langchain langchain-openai, and OPENAI_API_KEY env var.
    """
    if not LANGCHAIN_AVAILABLE:
        print("LangChain not installed. Run: pip install langchain langchain-openai")
        return

    from langchain_openai import ChatOpenAI  # type: ignore
    from langchain.agents import AgentExecutor, create_react_agent  # type: ignore
    from langchain.prompts import PromptTemplate  # type: ignore

    tools = [move_robot, read_sensor, get_fleet_health]

    llm = ChatOpenAI(model="gpt-4o", temperature=0)

    # ReAct prompt template
    template = """You are a robot fleet operator. Use the provided tools to control robots.
Always check fleet health before issuing motion commands.
If a motion command is denied, explain why and try an alternative approach.

Tools:
{tools}

Tool names: {tool_names}

Use this format:
Thought: I need to...
Action: tool_name
Action Input: {{"param": "value"}}
Observation: (result)
... (repeat if needed)
Final Answer: (your summary)

Question: {input}
{agent_scratchpad}"""

    prompt = PromptTemplate(
        input_variables=["input", "tools", "tool_names", "agent_scratchpad"],
        template=template,
    )

    agent = create_react_agent(llm=llm, tools=tools, prompt=prompt)
    executor = AgentExecutor(agent=agent, tools=tools, verbose=True, max_iterations=6)

    result = executor.invoke({"input": user_prompt})
    print(f"\n[AGENT] Final answer: {result['output']}")


# ---------------------------------------------------------------------------
# Demo (runs without LangChain or OpenAI credentials using mocked agent output)
# ---------------------------------------------------------------------------

def demo_mocked_agent_output():
    """
    Demonstrates Aegis safety filtering with mocked agent tool invocations.

    Simulates four scenarios that a LangChain agent might generate:
      1. Valid motion command in nominal posture
      2. Out-of-envelope velocity (kinematic breach)
      3. Read telemetry (allowed even in degraded posture)
      4. A scenario where the agent checks fleet health before moving

    NOTE: This demo connects to a live Aegis instance at AEGIS_URL.
    If Aegis is not running, each call will raise a connection error
    which is caught and printed as a graceful denial.
    """
    print("=" * 60)
    print("Aegis Action Filter Demo — LangChain-style Tool Invocations")
    print(f"Aegis URL: {AEGIS_URL}")
    print("=" * 60)

    scenarios = [
        {
            "description": "Agent calls move_robot with safe parameters",
            "tool": "move_robot",
            "kwargs": {"target_node": "robot_base", "linear_x": 0.3, "angular_z": 0.15},
        },
        {
            "description": "Agent calls move_robot with hallucinated extreme velocity",
            "tool": "move_robot",
            "kwargs": {"target_node": "robot_base", "linear_x": 999.0, "angular_z": 0.0},
        },
        {
            "description": "Agent calls read_sensor (safe even in Degraded posture)",
            "tool": "read_sensor",
            "kwargs": {"target_node": "sensor_01"},
        },
        {
            "description": "Agent checks fleet health (public endpoint, no filter)",
            "tool": "get_fleet_health",
            "kwargs": {},
        },
        {
            "description": "Agent calls move_robot with valid max forward speed",
            "tool": "move_robot",
            "kwargs": {"target_node": "robot_base", "linear_x": 0.5, "angular_z": 0.0},
        },
    ]

    tool_map = {
        "move_robot": move_robot,
        "read_sensor": read_sensor,
        "get_fleet_health": get_fleet_health,
    }

    for scenario in scenarios:
        print(f"\n{'-' * 60}")
        print(f"  {scenario['description']}")
        tool_fn = tool_map[scenario["tool"]]
        if scenario["kwargs"]:
            result = tool_fn(**scenario["kwargs"])
        else:
            result = tool_fn()
        print(f"  [RESULT] {result}")

    print("\n" + "=" * 60)
    print("Demo complete.")
    print("In a real LangChain agent, these tool calls are generated by the LLM.")
    print("Aegis evaluates each one against live fleet posture before any")
    print("hardware interaction occurs. The agent only sees the tool's return value.")
    print("=" * 60)


if __name__ == "__main__":
    import sys

    if len(sys.argv) > 1 and sys.argv[1] == "--real":
        # Real LangChain mode
        if not LANGCHAIN_AVAILABLE:
            print("ERROR: LangChain not installed.")
            print("Run: pip install langchain langchain-openai")
            sys.exit(1)
        prompt = " ".join(sys.argv[2:]) if len(sys.argv) > 2 else (
            "Check the fleet health, then move robot_base forward at 0.3 m/s. "
            "If denied, explain why and read sensor data instead."
        )
        print(f"Running LangChain agent with prompt: {prompt!r}")
        run_langchain_agent(prompt)
    else:
        # Default: mocked demo that works without any API credentials
        demo_mocked_agent_output()
