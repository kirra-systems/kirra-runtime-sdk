from setuptools import find_packages, setup
import os
from glob import glob

package_name = 'kirra_safety'

setup(
    name=package_name,
    version='1.0.0',
    packages=find_packages(exclude=['test']),
    data_files=[
        ('share/ament_index/resource_index/packages',
            ['resource/' + package_name]),
        ('share/' + package_name, ['package.xml']),
        (os.path.join('share', package_name, 'launch'),
            glob(os.path.join('launch', '*launch.[pxy][yma]*'))),
        (os.path.join('share', package_name, 'config'),
            glob(os.path.join('config', '*.yaml'))),
        (os.path.join('share', package_name, 'worlds'),
            glob(os.path.join('worlds', '*.world'))),
        (os.path.join('share', package_name, 'urdf'),
            glob(os.path.join('urdf', '*.urdf'))),
    ],
    install_requires=['setuptools'],
    zip_safe=True,
    maintainer='Kirra Safety',
    maintainer_email='safety@kirra.systems',
    description='Kirra safety interlock for ROS2',
    license='MIT',
    tests_require=['pytest'],
    entry_points={
        'console_scripts': [
            'cmd_vel_interceptor = kirra_safety.cmd_vel_interceptor:main',
            'sensor_monitor = kirra_safety.sensor_monitor:main',
            'posture_subscriber = kirra_safety.posture_subscriber:main',
            'perception_governor = kirra_safety.perception_governor:main',
            'doer_commander = kirra_safety.doer_commander:main',
            'occy_doer = kirra_safety.occy_doer:main',
        ],
    },
)
