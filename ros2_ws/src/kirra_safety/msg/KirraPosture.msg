string posture          # Nominal | Degraded | LockedOut
string[] blocked_nodes  # Nodes causing degradation or lockout
int64 timestamp_ms      # Unix timestamp (milliseconds)
int64 generation        # Monotonic posture generation counter
