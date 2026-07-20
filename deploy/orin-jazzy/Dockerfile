# Orin Jazzy runtime (ADR-0036, Step 3) — the robot-side KIRRA stack on ROS 2
# Jazzy, built to run INSIDE a container on a JetPack-6 / Ubuntu-22.04 / Humble
# Orin host. This decouples the stack from L4T: the host stays on whatever
# JetPack ships (Humble), while the checker/adapter/governed-consumer speak Jazzy
# in the container. They meet the host's Humble ROS graph (and the isolated
# Autoware doer) only over DDS — same cross-distro wire story as the Autoware
# isolation, inverted (here Jazzy is the guest).
#
# SCOPE — the CPU-only safety spine: the ros2 adapter (checker), the ADR-0033
# governed motor consumer + its verify-core FFI, the curated autoware_*_msgs, and
# the robot Channel-A layer. GPU doers (parko TensorRT/camera perception) are NOT
# here — they need an nvcr.io/nvidia/l4t-jetpack-based Jazzy image and are a
# follow-up; the safety path needs no CUDA, so a plain ros:jazzy base runs on the
# Orin's aarch64 today.
#
# ros:jazzy-ros-base is multi-arch (arm64 present) → builds/runs natively on Orin.
# Digest-pinned by Dependabot (`docker` ecosystem); tag kept for readability.
FROM ros:jazzy-ros-base

SHELL ["/bin/bash", "-c"]

# Toolchain: rust (r2r's bindgen needs libclang), colcon, rosdep, build-essential.
# rustup installs the repo-pinned toolchain from rust-toolchain.toml at build.
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential clang libclang-dev curl git pkg-config \
      python3-colcon-common-extensions python3-rosdep python3-pip python3-serial \
    && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /opt/kirra/src
COPY . .
# Honor the repo toolchain pin (rust-toolchain.toml) for a reproducible build.
RUN rustup show

# 1. Build + install the curated autoware_*_msgs + kirra_safety into a ROS overlay.
#    rosdep resolves kirra_safety's deps; the 4 curated msgs build from .msg alone.
RUN source /opt/ros/jazzy/setup.bash \
    && (rosdep update || true) \
    && cd ros2_ws \
    && (rosdep install --from-paths src --ignore-src -r -y || \
        echo '[warn] rosdep partial — curated msgs still build from .msg') \
    && colcon build --packages-up-to \
         autoware_planning_msgs autoware_perception_msgs \
         autoware_map_msgs autoware_control_msgs kirra_safety \
    && cp -a install /opt/kirra/ros2_ws_install

# 2. Build the ros2 adapter (checker), the governed-consumer verify core (cdylib),
#    and the dev release-token minter — against the sourced curated types.
RUN source /opt/ros/jazzy/setup.bash \
    && source /opt/kirra/ros2_ws_install/setup.bash \
    && cargo build --locked --release \
         -p kirra-ros2-adapter --features ros2 \
    && cargo build --locked --release \
         -p kirra-consumer-ffi -p kirra-release-token --bin kirra_ros_release_mint
RUN install -d /opt/kirra/bin /opt/kirra/lib /opt/kirra/robot \
    && install -m0755 target/release/kirra_ros2_adapter_node /opt/kirra/bin/ \
    && install -m0755 target/release/kirra_ros_release_mint  /opt/kirra/bin/ \
    && install -m0644 target/release/libkirra_consumer_ffi.so /opt/kirra/lib/

# 3. Stage the robot-side Python layer + the distro-agnostic ROS resolver.
RUN for f in kirra_motor_consumer.py kirra_ffi.py r2_drive.py kirra_release_publisher.py \
             ros_env.sh rabbit_persona.py rabbit_ask.py rabbit_converse.py rabbit_watch.py; do \
      [ -f "robot/$f" ] && install -m0755 "robot/$f" "/opt/kirra/robot/$f" || true; \
    done
RUN install -m0755 deploy/orin-jazzy/entrypoint.sh /opt/kirra/entrypoint.sh

ENV KIRRA_CONSUMER_LIB=/opt/kirra/lib/libkirra_consumer_ffi.so \
    KIRRA_ROS_WS_SETUP=/opt/kirra/ros2_ws_install/setup.bash \
    ROS_DOMAIN_ID=28

WORKDIR /opt/kirra
ENTRYPOINT ["/opt/kirra/entrypoint.sh"]
# Default role = the checker/adapter node. Override with `command: [consumer]` etc.
CMD ["adapter"]
