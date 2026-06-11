# CMakeLists.txt — QNX RTM harness (#271, EPIC #270).
#
# Builds the Rust JUDGE as a no_std staticlib via rustc DIRECTLY (no cargo —
# dependency-free, and mirrors the QNX cross-compile shape), then links it into
# the C++ SHIM + harness/demo. The harness is wired as a ctest so a WRONG VERDICT
# fails the build.
#
# QNX cross-compile hook (real work = #274): for a QNX target one sets the
# toolchain file (qcc/q++) and points `RUSTC_TARGET` at the QNX tuple
# (e.g. `x86_64-pc-nto-qnx8_0_0` / `aarch64-unknown-nto-qnx8_0_0`) with
# `--no-default-features`-equivalent core-only flags. Host build below uses the
# native rustc/g++; see docs/adr/KIRRA_QNX_CROSSCOMPILE.md.

cmake_minimum_required(VERSION 3.16)
project(kirra_qnx_rtm_harness CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Strict, exception-free, RTTI-free — the integration-boundary discipline.
add_compile_options(-Wall -Wextra -Werror -fno-exceptions -fno-rtti)

# --- Rust judge: rustc -> libkirra_judge.a (no_std staticlib, panic=abort) -----
find_program(RUSTC rustc REQUIRED)

set(KIRRA_JUDGE_SRC "${CMAKE_CURRENT_SOURCE_DIR}/kirra_judge.rs")
set(KIRRA_JUDGE_LIB "${CMAKE_CURRENT_BINARY_DIR}/libkirra_judge.a")

add_custom_command(
    OUTPUT "${KIRRA_JUDGE_LIB}"
    COMMAND "${RUSTC}"
            --edition 2021
            --crate-type staticlib
            --crate-name kirra_judge
            -C panic=abort
            -C opt-level=2
            -C debuginfo=0
            -o "${KIRRA_JUDGE_LIB}"
            "${KIRRA_JUDGE_SRC}"
    DEPENDS "${KIRRA_JUDGE_SRC}"
    COMMENT "rustc: building no_std staticlib libkirra_judge.a (panic=abort)"
    VERBATIM
)
add_custom_target(kirra_judge_lib DEPENDS "${KIRRA_JUDGE_LIB}")

# Imported target for the static judge library.
add_library(kirra_judge STATIC IMPORTED GLOBAL)
set_target_properties(kirra_judge PROPERTIES IMPORTED_LOCATION "${KIRRA_JUDGE_LIB}")
add_dependencies(kirra_judge kirra_judge_lib)

# A no_std rustc staticlib still references a few platform symbols on link
# (pthread / dl / math / unwind-less libgcc). Link them defensively.
set(KIRRA_JUDGE_LINK kirra_judge pthread dl m)

# --- C++ shim (driver) ---------------------------------------------------------
add_library(kirra_shim STATIC kirra_shim.cpp)
target_include_directories(kirra_shim PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}")

# --- Harness + demo ------------------------------------------------------------
add_executable(rtm_harness rtm_harness.cpp)
target_link_libraries(rtm_harness PRIVATE kirra_shim ${KIRRA_JUDGE_LINK})

add_executable(kirra_demo kirra_demo.cpp)
target_link_libraries(kirra_demo PRIVATE kirra_shim ${KIRRA_JUDGE_LINK})

# --- ctest: a wrong verdict (non-zero exit) FAILS the build/test ---------------
enable_testing()
add_test(NAME rtm_harness COMMAND rtm_harness)
add_test(NAME kirra_demo COMMAND kirra_demo)
