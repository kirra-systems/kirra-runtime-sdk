#!/usr/bin/env python3
"""
Full stack launch for Hiwonder ROSOrin with Kirra safety interlock.

Topic remapping:
  nav2 publishes to /cmd_vel_raw
  kirra_safety subscribes to /cmd_vel_raw and publishes to /cmd_vel
  motor controllers subscribe to /cmd_vel

This ensures: nav2 -> Kirra -> motors (not nav2 -> motors directly)
"""

import os
from launch import LaunchDescription
from launch.actions import DeclareLaunchArgument, ExecuteProcess, IncludeLaunchDescription
from launch.conditions import IfCondition
from launch.launch_description_sources import PythonLaunchDescriptionSource
from launch.substitutions import LaunchConfiguration, PathJoinSubstitution, PythonExpression
from launch_ros.actions import Node
from launch_ros.substitutions import FindPackageShare

# Where the Rust sidecar binaries (planner_service, taj_service) live. Built by
# `cargo build --release -p kirra-mick --example {planner_service,taj_service}` (or
# scripts/orin_bringup.sh). Overridable via KIRRA_SIDECAR_DIR or the sidecar_dir arg.
DEFAULT_SIDECAR_DIR = os.environ.get(
    'KIRRA_SIDECAR_DIR',
    os.path.join(os.path.expanduser('~'), 'kirra-runtime-sdk', 'target', 'release', 'examples'),
)


def generate_launch_description():
    kirra_url_arg = DeclareLaunchArgument(
        'kirra_url',
        default_value='http://localhost:8090',
        description='Kirra verifier URL',
    )
    kirra_token_arg = DeclareLaunchArgument(
        'kirra_token',
        default_value=os.environ.get('KIRRA_ADMIN_TOKEN', ''),
        description='Kirra admin token',
    )
    use_sim_time_arg = DeclareLaunchArgument(
        'use_sim_time',
        default_value='false',
        description='Use simulation time (for Gazebo/Isaac Sim)',
    )
    use_perception_cap_arg = DeclareLaunchArgument(
        'use_perception_cap',
        default_value='false',
        description='Enable the Taj corridor speed derate on the cmd_vel path '
                    '(requires the taj_service sidecar + perception_governor node)',
    )
    taj_url_arg = DeclareLaunchArgument(
        'taj_url',
        default_value='http://localhost:8101',
        description='Taj perception sidecar URL (kirra-mick example taj_service)',
    )
    # --- Rust sidecars folded into this launch (single-box: everything on the Orin) ---
    start_sidecars_arg = DeclareLaunchArgument(
        'start_sidecars',
        default_value='true',
        description='Start the Rust sidecars (Occy planner + Taj perception) from this launch. '
                    'Set false if you start them separately (e.g. scripts/orin_bringup.sh --serve).',
    )
    start_planner_service_arg = DeclareLaunchArgument(
        'start_planner_service',
        default_value='true',
        description='Start the Occy planner sidecar (planner_service, POST /plan).',
    )
    sidecar_dir_arg = DeclareLaunchArgument(
        'sidecar_dir',
        default_value=DEFAULT_SIDECAR_DIR,
        description='Directory holding the planner_service + taj_service release binaries.',
    )
    planner_addr_arg = DeclareLaunchArgument(
        'planner_addr',
        default_value='127.0.0.1:8100',
        description='Bind address for the Occy planner sidecar (KIRRA_PLANNER_ADDR).',
    )
    taj_addr_arg = DeclareLaunchArgument(
        'taj_addr',
        default_value='127.0.0.1:8101',
        description='Bind address for the Taj perception sidecar (KIRRA_TAJ_ADDR); '
                    'must match taj_url.',
    )

    kirra_url = LaunchConfiguration('kirra_url')
    kirra_token = LaunchConfiguration('kirra_token')
    use_sim_time = LaunchConfiguration('use_sim_time')
    use_perception_cap = LaunchConfiguration('use_perception_cap')
    taj_url = LaunchConfiguration('taj_url')
    start_sidecars = LaunchConfiguration('start_sidecars')
    start_planner_service = LaunchConfiguration('start_planner_service')
    sidecar_dir = LaunchConfiguration('sidecar_dir')
    planner_addr = LaunchConfiguration('planner_addr')
    taj_addr = LaunchConfiguration('taj_addr')

    # Conditions: the planner runs when sidecars are on AND planner is wanted; the Taj
    # service runs when sidecars are on AND the perception cap is enabled (no point
    # otherwise). The perception_governor node mirrors the Taj-service condition.
    planner_cond = IfCondition(PythonExpression(
        ["'", start_sidecars, "' == 'true' and '", start_planner_service, "' == 'true'"]))
    taj_cond = IfCondition(PythonExpression(
        ["'", start_sidecars, "' == 'true' and '", use_perception_cap, "' == 'true'"]))

    params_file = PathJoinSubstitution([
        FindPackageShare('kirra_safety'), 'config', 'kirra_params.yaml'
    ])

    # --- Rust sidecars (ExecuteProcess) --------------------------------------------------
    # The Occy planner endpoint (POST /plan). respawn so a transient crash recovers; the
    # interceptor fails closed meanwhile.
    planner_service = ExecuteProcess(
        name='planner_service',
        cmd=[PathJoinSubstitution([sidecar_dir, 'planner_service'])],
        additional_env={'KIRRA_PLANNER_ADDR': planner_addr},
        condition=planner_cond,
        respawn=True,
        respawn_delay=2.0,
        output='screen',
    )
    # The Taj perception sidecar (POST /perception). Only started when the cmd_vel
    # perception cap is enabled — the perception_governor below POSTs /scan to it.
    taj_service = ExecuteProcess(
        name='taj_service',
        cmd=[PathJoinSubstitution([sidecar_dir, 'taj_service'])],
        additional_env={'KIRRA_TAJ_ADDR': taj_addr},
        condition=taj_cond,
        respawn=True,
        respawn_delay=2.0,
        output='screen',
    )

    # Kirra safety nodes -- intercept /cmd_vel_raw (from nav2), output to /cmd_vel (to motors)
    cmd_vel_interceptor = Node(
        package='kirra_safety',
        executable='cmd_vel_interceptor',
        name='cmd_vel_interceptor',
        parameters=[
            params_file,
            {
                'kirra_url': kirra_url,
                'kirra_token': kirra_token,
                'input_topic': '/cmd_vel_raw',
                'output_topic': '/cmd_vel',
                'use_sim_time': use_sim_time,
                'use_perception_cap': use_perception_cap,
            },
        ],
        output='screen',
    )

    # Taj corridor -> assured-clear-distance speed cap on the cmd_vel path. Subscribes /scan,
    # POSTs to the taj_service sidecar, publishes /kirra/perception_speed_cap. The interceptor
    # applies it (opt-in via use_perception_cap) BEFORE the governor — Taj tightens, KIRRA bounds.
    perception_governor = Node(
        package='kirra_safety',
        executable='perception_governor',
        name='perception_governor',
        condition=IfCondition(use_perception_cap),
        parameters=[
            params_file,
            {'taj_url': taj_url, 'use_sim_time': use_sim_time},
        ],
        output='screen',
    )

    sensor_monitor = Node(
        package='kirra_safety',
        executable='sensor_monitor',
        name='sensor_monitor',
        parameters=[
            params_file,
            {'kirra_url': kirra_url, 'kirra_token': kirra_token, 'use_sim_time': use_sim_time},
        ],
        output='screen',
    )

    posture_subscriber = Node(
        package='kirra_safety',
        executable='posture_subscriber',
        name='posture_subscriber',
        parameters=[
            params_file,
            {'kirra_url': kirra_url, 'kirra_token': kirra_token, 'use_sim_time': use_sim_time},
        ],
        output='screen',
    )

    return LaunchDescription([
        kirra_url_arg,
        kirra_token_arg,
        use_sim_time_arg,
        use_perception_cap_arg,
        taj_url_arg,
        start_sidecars_arg,
        start_planner_service_arg,
        sidecar_dir_arg,
        planner_addr_arg,
        taj_addr_arg,
        planner_service,
        taj_service,
        cmd_vel_interceptor,
        perception_governor,
        sensor_monitor,
        posture_subscriber,
        # NOTE: Add nav2_bringup and robot_description includes here.
        # Example:
        #   IncludeLaunchDescription(
        #       PythonLaunchDescriptionSource([
        #           PathJoinSubstitution([FindPackageShare('nav2_bringup'), 'launch', 'navigation_launch.py'])
        #       ]),
        #       launch_arguments={'cmd_vel_topic': '/cmd_vel_raw'}.items(),
        #   ),
        # Uncomment and configure based on your robot's nav2 package.
    ])
