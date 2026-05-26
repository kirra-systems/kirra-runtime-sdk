#!/bin/bash
# Register the Hiwonder ROSOrin fleet node graph in Aegis.
# Run once before starting the robot.
#
# Usage:
#   AEGIS_ADMIN_TOKEN=your-token bash scripts/setup_ros2_fleet.sh
#   or:
#   AEGIS_URL=http://192.168.1.100:8090 AEGIS_ADMIN_TOKEN=your-token bash scripts/setup_ros2_fleet.sh

set -e

AEGIS_URL="${AEGIS_URL:-http://localhost:8090}"
AEGIS_TOKEN="${AEGIS_ADMIN_TOKEN}"

if [ -z "$AEGIS_TOKEN" ]; then
    echo "ERROR: AEGIS_ADMIN_TOKEN environment variable is not set." >&2
    exit 1
fi

AUTH_HEADER="Authorization: Bearer $AEGIS_TOKEN"
CONTENT_TYPE="Content-Type: application/json"

echo "Registering Hiwonder ROSOrin fleet nodes in Aegis at $AEGIS_URL"

register_node() {
    local node_id="$1"
    local description="$2"
    echo "  Registering node: $node_id"
    curl -sf -X POST "$AEGIS_URL/attestation/register" \
        -H "$AUTH_HEADER" \
        -H "$CONTENT_TYPE" \
        -d "{
            \"node_id\": \"$node_id\",
            \"ak_public_pem\": \"PLACEHOLDER_PEM_REPLACE_WITH_REAL_KEY\",
            \"expected_pcr16_digest_hex\": \"0000000000000000000000000000000000000000000000000000000000000000\"
        }" > /dev/null
    echo "    OK $node_id registered"
}

register_dependency() {
    local from="$1"
    local to="$2"
    echo "  Dependency: $from -> $to"
    curl -sf -X POST "$AEGIS_URL/fleet/dependencies" \
        -H "$AUTH_HEADER" \
        -H "$CONTENT_TYPE" \
        -d "{\"node_id\": \"$to\", \"depends_on\": [\"$from\"]}" > /dev/null
    echo "    OK $from -> $to"
}

# --- Register leaf sensor nodes ---
echo ""
echo "Step 1: Registering sensor nodes..."
register_node "lidar_front" "RPLIDAR A2 -- primary obstacle detection"
register_node "depth_camera" "Intel RealSense D435 -- depth perception"
register_node "imu_primary" "MPU9250 -- inertial measurement"
register_node "wheel_encoders" "Mecanum wheel encoders -- odometry"

# --- Register fusion nodes ---
echo ""
echo "Step 2: Registering fusion nodes..."
register_node "perception_fusion" "LiDAR + camera sensor fusion"
register_node "odometry_fusion" "IMU + encoder odometry fusion"

# --- Register stack nodes ---
echo ""
echo "Step 3: Registering navigation stack nodes..."
register_node "navigation_stack" "Nav2 -- path planning and obstacle avoidance"
register_node "motor_controller" "Mecanum motor controller"

# --- Register dependency graph ---
echo ""
echo "Step 4: Registering dependency graph..."
# Perception fusion depends on lidar AND camera
register_dependency "lidar_front" "perception_fusion"
register_dependency "depth_camera" "perception_fusion"

# Odometry fusion depends on IMU AND encoders
register_dependency "imu_primary" "odometry_fusion"
register_dependency "wheel_encoders" "odometry_fusion"

# Navigation stack depends on both fusion nodes
register_dependency "perception_fusion" "navigation_stack"
register_dependency "odometry_fusion" "navigation_stack"

# Motor controller depends on nav stack
register_dependency "navigation_stack" "motor_controller"

echo ""
echo "Hiwonder ROSOrin fleet graph registered successfully."
echo ""
echo "Next steps:"
echo "  1. Replace PLACEHOLDER_PEM_REPLACE_WITH_REAL_KEY with actual TPM AK public keys"
echo "  2. Build and launch the interlock:"
echo "     cd ros2_ws && colcon build --packages-select aegis_safety"
echo "     source install/setup.bash"
echo "     ros2 launch aegis_safety aegis_with_robot.launch.py aegis_token:=\$AEGIS_ADMIN_TOKEN"
