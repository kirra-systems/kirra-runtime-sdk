# shellcheck shell=bash
# robot/ros_env.sh — distro-agnostic ROS 2 environment resolver (ADR-0036).
#
# Sourceable helper. The KIRRA checker + adapter are distro-independent (pure
# Rust / r2r 0.9.5, which supports Humble/Jazzy/Kilted); only these robot-side
# shell scripts hardcoded `/opt/ros/humble`. This resolves the right setup.bash
# so the robot layer runs on Jazzy (24.04) OR Humble (22.04) unchanged — part of
# moving everything except the isolated Autoware doer to 24.04/Jazzy.
#
# Resolution order (first match wins):
#   1. $KIRRA_ROS_SETUP           explicit full path to a setup.bash (override)
#   2. $ROS_DISTRO already set     the caller's shell is already sourced
#   3. probe $KIRRA_ROS_DISTRO_PREF (default "jazzy humble") under /opt/ros
#
# Usage:
#   source "$(dirname "${BASH_SOURCE[0]}")/ros_env.sh"
#   kirra_source_ros            # source it; returns 1 if none found (caller decides fatal)
#   p="$(kirra_ros_setup_path)" # just resolve the path, no sourcing

# Print the resolved setup.bash path; empty output + return 1 if none is found.
kirra_ros_setup_path() {
  if [ -n "${KIRRA_ROS_SETUP:-}" ] && [ -f "${KIRRA_ROS_SETUP}" ]; then
    printf '%s\n' "${KIRRA_ROS_SETUP}"
    return 0
  fi
  local root="${KIRRA_ROS_ROOT:-/opt/ros}"
  if [ -n "${ROS_DISTRO:-}" ] && [ -f "${root}/${ROS_DISTRO}/setup.bash" ]; then
    printf '%s\n' "${root}/${ROS_DISTRO}/setup.bash"
    return 0
  fi
  local d
  for d in ${KIRRA_ROS_DISTRO_PREF:-jazzy humble}; do
    if [ -f "${root}/${d}/setup.bash" ]; then
      printf '%s\n' "${root}/${d}/setup.bash"
      return 0
    fi
  done
  return 1
}

# Print just the resolved distro name (e.g. "jazzy"), or nothing + return 1.
kirra_ros_distro() {
  local setup distro root="${KIRRA_ROS_ROOT:-/opt/ros}"
  setup="$(kirra_ros_setup_path)" || return 1
  distro="${setup#"${root}"/}"     # "<root>/<distro>/setup.bash" → "<distro>/setup.bash"
  distro="${distro%%/*}"
  printf '%s\n' "${distro}"
}

# Source the resolved ROS env. If $ROS_DISTRO is already set the caller's shell
# is assumed already sourced (matches the prior `[ -z "$ROS_DISTRO" ]` guards).
# ROS setup.bash references unset vars, so nounset is relaxed across the source
# and restored afterwards. Returns 1 if nothing could be sourced.
kirra_source_ros() {
  if [ -n "${ROS_DISTRO:-}" ]; then
    return 0    # already sourced upstream
  fi
  local setup
  setup="$(kirra_ros_setup_path)" || return 1
  local had_u=0
  case $- in *u*) had_u=1 ;; esac
  set +u
  # shellcheck disable=SC1090
  . "${setup}"
  [ "${had_u}" = 1 ] && set -u
  return 0
}
