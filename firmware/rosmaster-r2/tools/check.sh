#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build="${R2_BUILD_DIR:-/tmp/kirra-r2-firmware-build}"

cmake -S "$root" -B "$build" \
  -DCMAKE_BUILD_TYPE=RelWithDebInfo \
  -DR2_ENABLE_SANITIZERS=ON \
  -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
cmake --build "$build" --parallel
ctest --test-dir "$build" --output-on-failure
"$build/r2_deterministic_sim"

if command -v clang-tidy >/dev/null 2>&1; then
  while IFS= read -r source; do
    clang-tidy -p "$build" "$source"
  done < <(printf '%s\n' \
    "$root/kinematics/src/ackermann.cpp" \
    "$root/control/src/motion_controller.cpp" \
    "$root/protocol/src/wire.cpp" \
    "$root/safety/src/safety_manager.cpp" \
    "$root/firmware/src/configuration.cpp")
else
  printf 'warning: clang-tidy is not installed; static analysis skipped\n' >&2
fi

if command -v cppcheck >/dev/null 2>&1; then
  cppcheck --enable=warning,style,performance,portability \
    --error-exitcode=1 --inline-suppr --std=c++17 \
    -I "$root/hal/include" \
    -I "$root/kinematics/include" \
    -I "$root/control/include" \
    -I "$root/protocol/include" \
    -I "$root/diagnostics/include" \
    -I "$root/safety/include" \
    -I "$root/firmware/include" \
    -I "$root/bootloader/include" \
    "$root/hal" "$root/kinematics" "$root/control" "$root/protocol" \
    "$root/diagnostics" "$root/safety" "$root/firmware" "$root/bootloader"
else
  printf 'warning: cppcheck is not installed; static analysis skipped\n' >&2
fi
