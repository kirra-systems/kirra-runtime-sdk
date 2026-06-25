#!/usr/bin/env python3
"""
Watch Taj + KIRRA stop a robot in Gazebo (single-box, runs on the Orin).

Brings up, in one command:
  - Gazebo Classic with a short corridor + an end wall (kirra_corridor.world),
  - a differential-drive robot with a forward 180-degree lidar (kirra_bot.urdf),
  - the Kirra safety stack (cmd_vel_interceptor + perception_governor + the Rust
    planner/Taj sidecars), via the shared kirra_with_robot.launch.py,
  - the "doer" (doer_commander) naively commanding the robot forward,
  - optionally the KIRRA verifier itself (start_verifier).

The robot drives toward the wall; Taj's corridor sees the wall as a shrinking clear
distance and publishes an assured-clear-distance speed cap; the interceptor applies it
before the governor (Taj tightens, KIRRA bounds); the robot brakes to a controlled stop
before the wall — even though the doer keeps commanding forward.

Run (verifier secrets in the env):
  export KIRRA_ADMIN_TOKEN=... KIRRA_SUPERVISOR_RESET_KEY=...
  ros2 launch kirra_safety kirra_gazebo_demo.launch.py

Prereqs on the Orin: ROS 2 + gazebo_ros_pkgs (Gazebo Classic), and the Rust sidecars
+ verifier built once (cargo build --release ... / scripts/orin_bringup.sh).
"""

import os
from launch import LaunchDescription
from launch.actions import (
    DeclareLaunchArgument, ExecuteProcess, IncludeLaunchDescription,
)
from launch.conditions import IfCondition
from launch.launch_description_sources import PythonLaunchDescriptionSource
from launch.substitutions import Command, LaunchConfiguration, PathJoinSubstitution
from launch_ros.actions import Node
from launch_ros.parameter_descriptions import ParameterValue
from launch_ros.substitutions import FindPackageShare

HOME = os.path.expanduser('~')
DEFAULT_BUILD_DIR = os.environ.get(
    'KIRRA_BUILD_DIR', os.path.join(HOME, 'kirra-runtime-sdk', 'target', 'release'))


def generate_launch_description():
    pkg = FindPackageShare('kirra_safety')

    kirra_token_arg = DeclareLaunchArgument(
        'kirra_token', default_value=os.environ.get('KIRRA_ADMIN_TOKEN', ''),
        description='Kirra admin token (defaults to $KIRRA_ADMIN_TOKEN).')
    world_arg = DeclareLaunchArgument(
        'world', default_value=PathJoinSubstitution([pkg, 'worlds', 'kirra_corridor.world']),
        description='Gazebo world file.')
    forward_speed_arg = DeclareLaunchArgument(
        'forward_speed_mps', default_value='1.2',
        description="The doer's naive constant forward command (m/s).")
    start_verifier_arg = DeclareLaunchArgument(
        'start_verifier', default_value='true',
        description='Start the KIRRA verifier from this launch (needs KIRRA_ADMIN_TOKEN + '
                    'KIRRA_SUPERVISOR_RESET_KEY in the env). Set false if it is already up.')
    verifier_bin_arg = DeclareLaunchArgument(
        'verifier_bin', default_value=os.path.join(DEFAULT_BUILD_DIR, 'kirra_verifier_service'),
        description='Path to the kirra_verifier_service binary.')
    gui_arg = DeclareLaunchArgument(
        'gui', default_value='true', description='Run the Gazebo client GUI (false = headless).')

    kirra_token = LaunchConfiguration('kirra_token')
    world = LaunchConfiguration('world')
    forward_speed = LaunchConfiguration('forward_speed_mps')
    start_verifier = LaunchConfiguration('start_verifier')
    verifier_bin = LaunchConfiguration('verifier_bin')
    gui = LaunchConfiguration('gui')

    robot_urdf = PathJoinSubstitution([pkg, 'urdf', 'kirra_bot.urdf'])
    robot_description = ParameterValue(Command(['cat ', robot_urdf]), value_type=str)

    # KIRRA verifier (optional) — the governance plane the interceptor calls. Inherits the
    # KIRRA_* secrets from the launching shell; respawns on a transient crash.
    verifier = ExecuteProcess(
        name='kirra_verifier_service',
        cmd=[verifier_bin],
        condition=IfCondition(start_verifier),
        respawn=True, respawn_delay=2.0, output='screen',
    )

    # Gazebo Classic with the ROS factory + init plugins (so spawn_entity works).
    gzserver = ExecuteProcess(
        name='gzserver',
        cmd=['gzserver', '--verbose', '-s', 'libgazebo_ros_init.so',
             '-s', 'libgazebo_ros_factory.so', world],
        output='screen',
    )
    gzclient = ExecuteProcess(
        name='gzclient', cmd=['gzclient'], condition=IfCondition(gui), output='screen')

    # Robot state publisher (sim time) + spawn the robot into Gazebo.
    rsp = Node(
        package='robot_state_publisher', executable='robot_state_publisher',
        parameters=[{'robot_description': robot_description, 'use_sim_time': True}],
        output='screen',
    )
    spawn = Node(
        package='gazebo_ros', executable='spawn_entity.py',
        arguments=['-topic', 'robot_description', '-entity', 'kirra_bot',
                   '-x', '0', '-y', '0', '-z', '0.1'],
        output='screen',
    )

    # The Kirra safety stack + Rust sidecars (interceptor, perception_governor, planner,
    # taj_service, sensor_monitor, posture_subscriber) — reuse the shared launch with the
    # Taj cap ON and sim time ON.
    safety_stack = IncludeLaunchDescription(
        PythonLaunchDescriptionSource(
            [PathJoinSubstitution([pkg, 'launch', 'kirra_with_robot.launch.py'])]),
        launch_arguments={
            'kirra_token': kirra_token,
            'use_sim_time': 'true',
            'use_perception_cap': 'true',
            'start_sidecars': 'true',
        }.items(),
    )

    # The untrusted doer: command the robot forward forever; KIRRA + Taj stop it.
    doer = Node(
        package='kirra_safety', executable='doer_commander', name='doer_commander',
        parameters=[{'cmd_topic': '/cmd_vel_raw', 'forward_speed_mps': forward_speed,
                     'use_sim_time': True}],
        output='screen',
    )

    return LaunchDescription([
        kirra_token_arg, world_arg, forward_speed_arg, start_verifier_arg,
        verifier_bin_arg, gui_arg,
        verifier, gzserver, gzclient, rsp, spawn, safety_stack, doer,
    ])
