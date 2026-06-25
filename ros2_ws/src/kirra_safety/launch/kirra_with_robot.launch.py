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
from launch.actions import DeclareLaunchArgument, IncludeLaunchDescription
from launch.launch_description_sources import PythonLaunchDescriptionSource
from launch.substitutions import LaunchConfiguration, PathJoinSubstitution
from launch_ros.actions import Node
from launch_ros.substitutions import FindPackageShare


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

    kirra_url = LaunchConfiguration('kirra_url')
    kirra_token = LaunchConfiguration('kirra_token')
    use_sim_time = LaunchConfiguration('use_sim_time')
    use_perception_cap = LaunchConfiguration('use_perception_cap')
    taj_url = LaunchConfiguration('taj_url')

    params_file = PathJoinSubstitution([
        FindPackageShare('kirra_safety'), 'config', 'kirra_params.yaml'
    ])

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
