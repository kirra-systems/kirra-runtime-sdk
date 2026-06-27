# CMakeLists.txt — QNX RTM harness (#271, EPIC #270).
#
# Builds the Rust JUDGE as a no_std staticlib via rustc DIRECTLY (no cargo —
# dependency-free, and mirrors the QNX cross-compile shape), then links it into
# the C++ SHIM + harness/demo. The harness is wired as a ctest so a WRONG VERDICT
# fails the build.
#
# QNX cross-compile (real work = #274). Default OFF → this file behaves EXACTLY as
# before (native rustc/g++, ctest runs). With -DKIRRA_QNX_TARGET=ON plus
# qnx.toolchain.cmake (qcc/q++) it cross-builds the shim/harness/demo AND the
# wcet_measure tool for a QNX nto target, and does NOT register ctest (the nto
# binaries run on the QNX target, not the build host). The no_std judge staticlib
# is supplied prebuilt for the target via -DKIRRA_JUDGE_LIB_PREBUILT (built by
# run_qnx_fdit.sh, which handles the Rust-for-QNX toolchain), or built here by a
# native/nto-capable rustc with -DKIRRA_RUSTC_TARGET. See WCET_QNX_BRINGUP.md.

cmake_minimum_required(VERSION 3.16)
project(kirra_qnx_rtm_harness CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

option(KIRRA_QNX_TARGET "Cross-build for a QNX target (adds wcet_measure; no host ctest)" OFF)
set(KIRRA_RUSTC_TARGET "" CACHE STRING
    "rustc nto tuple for the judge (e.g. x86_64-pc-nto-qnx800); empty = host rustc")
set(KIRRA_JUDGE_LIB_PREBUILT "" CACHE FILEPATH
    "Prebuilt libkirra_judge.a for the target (built by run_qnx_fdit.sh); empty = build via rustc here")

# Strict, exception-free, RTTI-free — the integration-boundary discipline.
add_compile_options(-Wall -Wextra -Werror -fno-exceptions -fno-rtti)

set(KIRRA_JUDGE_SRC "${CMAKE_CURRENT_SOURCE_DIR}/kirra_judge.rs")

if(KIRRA_JUDGE_LIB_PREBUILT)
    # The judge staticlib was cross-built out-of-band for the target (the Rust
    # toolchain detection lives in run_qnx_fdit.sh). Just import it.
    set(KIRRA_JUDGE_LIB "${KIRRA_JUDGE_LIB_PREBUILT}")
    add_custom_target(kirra_judge_lib)  # no-op: nothing to build here
    message(STATUS "kirra_judge: using prebuilt ${KIRRA_JUDGE_LIB}")
else()
    # --- Rust judge: rustc -> libkirra_judge.a (no_std staticlib, panic=abort) --
    find_program(RUSTC rustc REQUIRED)
    set(KIRRA_JUDGE_LIB "${CMAKE_CURRENT_BINARY_DIR}/libkirra_judge.a")

    # Host build, or an nto-capable rustc with a prebuilt `core`: append --target.
    set(KIRRA_JUDGE_RUSTC_FLAGS
        --edition 2021 --crate-type staticlib --crate-name kirra_judge
        -C panic=abort -C opt-level=2 -C debuginfo=0)
    if(KIRRA_RUSTC_TARGET)
        list(APPEND KIRRA_JUDGE_RUSTC_FLAGS --target "${KIRRA_RUSTC_TARGET}")
    endif()

    add_custom_command(
        OUTPUT "${KIRRA_JUDGE_LIB}"
        COMMAND "${RUSTC}" ${KIRRA_JUDGE_RUSTC_FLAGS}
                -o "${KIRRA_JUDGE_LIB}" "${KIRRA_JUDGE_SRC}"
        DEPENDS "${KIRRA_JUDGE_SRC}"
        COMMENT "rustc: building no_std staticlib libkirra_judge.a (panic=abort)"
        VERBATIM
    )
    add_custom_target(kirra_judge_lib DEPENDS "${KIRRA_JUDGE_LIB}")
endif()

# Imported target for the static judge library.
add_library(kirra_judge STATIC IMPORTED GLOBAL)
set_target_properties(kirra_judge PROPERTIES IMPORTED_LOCATION "${KIRRA_JUDGE_LIB}")
add_dependencies(kirra_judge kirra_judge_lib)

# A no_std rustc staticlib references a few platform symbols on link. On Linux
# those are separate libs (pthread/dl/m); on QNX they live in libc, and an
# explicit -ldl/-lpthread/-lm may not resolve — so link only the judge there and
# let qcc pull libc. Add back per `qcc -V` if a symbol is missing.
if(KIRRA_QNX_TARGET)
    set(KIRRA_JUDGE_LINK kirra_judge)
else()
    set(KIRRA_JUDGE_LINK kirra_judge pthread dl m)
endif()

# --- C++ shim (driver) ---------------------------------------------------------
add_library(kirra_shim STATIC kirra_shim.cpp)
target_include_directories(kirra_shim PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}")

# --- Harness + demo ------------------------------------------------------------
add_executable(rtm_harness rtm_harness.cpp)
target_link_libraries(rtm_harness PRIVATE kirra_shim ${KIRRA_JUDGE_LINK})

add_executable(kirra_demo kirra_demo.cpp)
target_link_libraries(kirra_demo PRIVATE kirra_shim ${KIRRA_JUDGE_LINK})

# --- WCET measurement (QNX target only) ----------------------------------------
# Built only for the QNX cross-target: a host number is INDICATIVE, never a WCET
# claim — keeping it out of the host build enforces that discipline structurally.
if(KIRRA_QNX_TARGET)
    add_executable(wcet_measure wcet_measure.cpp)
    target_link_libraries(wcet_measure PRIVATE kirra_shim ${KIRRA_JUDGE_LINK})
endif()

# --- ctest: a wrong verdict (non-zero exit) FAILS the build/test ---------------
# Only on a HOST build — nto binaries cannot execute on the Ubuntu build host;
# run_qnx_fdit.sh copies them to the QNX target and runs them there.
enable_testing()
if(NOT KIRRA_QNX_TARGET)
    add_test(NAME rtm_harness COMMAND rtm_harness)
    add_test(NAME kirra_demo COMMAND kirra_demo)
endif()
