#!/usr/bin/env python3
"""Launch all three Kirra safety nodes."""

import os
from launch import LaunchDescription
from launch.actions import DeclareLaunchArgument
from launch.substitutions import LaunchConfiguration, PathJoinSubstitution
from launch_ros.actions import Node
from launch_ros.substitutions import FindPackageShare


def generate_launch_description():
    kirra_url_arg = DeclareLaunchArgument(
        'kirra_url',
        default_value='http://localhost:8090',
        description='Base URL for the Kirra verifier service',
    )
    kirra_token_arg = DeclareLaunchArgument(
        'kirra_token',
        default_value=os.environ.get('KIRRA_ADMIN_TOKEN', ''),
        description='Kirra admin bearer token',
    )
    params_file_arg = DeclareLaunchArgument(
        'params_file',
        default_value=PathJoinSubstitution([
            FindPackageShare('kirra_safety'), 'config', 'kirra_params.yaml'
        ]),
        description='Path to kirra_params.yaml',
    )

    kirra_url = LaunchConfiguration('kirra_url')
    kirra_token = LaunchConfiguration('kirra_token')
    params_file = LaunchConfiguration('params_file')

    cmd_vel_interceptor = Node(
        package='kirra_safety',
        executable='cmd_vel_interceptor',
        name='cmd_vel_interceptor',
        parameters=[
            params_file,
            {'kirra_url': kirra_url, 'kirra_token': kirra_token},
        ],
        output='screen',
    )

    sensor_monitor = Node(
        package='kirra_safety',
        executable='sensor_monitor',
        name='sensor_monitor',
        parameters=[
            params_file,
            {'kirra_url': kirra_url, 'kirra_token': kirra_token},
        ],
        output='screen',
    )

    posture_subscriber = Node(
        package='kirra_safety',
        executable='posture_subscriber',
        name='posture_subscriber',
        parameters=[
            params_file,
            {'kirra_url': kirra_url, 'kirra_token': kirra_token},
        ],
        output='screen',
    )

    return LaunchDescription([
        kirra_url_arg,
        kirra_token_arg,
        params_file_arg,
        cmd_vel_interceptor,
        sensor_monitor,
        posture_subscriber,
    ])
