# qnx.toolchain.cmake — QNX SDP 8.0 cross-toolchain for the RTM harness (#274).
#
# Selects qcc/q++ as the C/C++ compilers and points the find-root at the QNX
# target sysroot. x86_64 is the default (the Phase-I now-path); switch the variant
# for an aarch64 board. Source `qnxsdp-env.sh` BEFORE configuring so QNX_HOST /
# QNX_TARGET (and qcc) are on PATH.
#
# Usage (normally via run_qnx_fdit.sh, which sets the prebuilt judge lib too):
#   source ~/qnx800/qnxsdp-env.sh
#   cmake -S tools/qnx-rtm-harness -B build-qnx \
#         -DCMAKE_TOOLCHAIN_FILE=tools/qnx-rtm-harness/qnx.toolchain.cmake \
#         -DKIRRA_QNX_TARGET=ON -DKIRRA_QNX_QCC_VARIANT=gcc_ntox86_64 \
#         -DKIRRA_JUDGE_LIB_PREBUILT=/abs/path/libkirra_judge.a

set(CMAKE_SYSTEM_NAME QNX)
set(CMAKE_SYSTEM_VERSION 8.0.0)

if("$ENV{QNX_HOST}" STREQUAL "" OR "$ENV{QNX_TARGET}" STREQUAL "")
    message(FATAL_ERROR
        "QNX_HOST/QNX_TARGET are unset — source qnxsdp-env.sh first "
        "(e.g. `source ~/qnx800/qnxsdp-env.sh`).")
endif()

# The qcc/q++ COMPILATION VARIANT (`qcc -V` lists what your install provides).
#   x86_64  → gcc_ntox86_64
#   aarch64 → gcc_ntoaarch64le
# Override with -DKIRRA_QNX_QCC_VARIANT=... if your SDP names it differently
# (older SDP 7 used a `_cxx` suffix for the C++ variant).
if(NOT DEFINED KIRRA_QNX_QCC_VARIANT)
    set(KIRRA_QNX_QCC_VARIANT "gcc_ntox86_64")
endif()

if(KIRRA_QNX_QCC_VARIANT MATCHES "aarch64")
    set(CMAKE_SYSTEM_PROCESSOR aarch64)
else()
    set(CMAKE_SYSTEM_PROCESSOR x86_64)
endif()

set(CMAKE_C_COMPILER   qcc)
set(CMAKE_CXX_COMPILER q++)

# qcc/q++ pick the cross target from the -V variant; inject it on every compile
# AND link (the compiler check, the objects, and the final link all need it).
set(CMAKE_C_FLAGS_INIT          "-V${KIRRA_QNX_QCC_VARIANT}")
set(CMAKE_CXX_FLAGS_INIT        "-V${KIRRA_QNX_QCC_VARIANT}")
set(CMAKE_EXE_LINKER_FLAGS_INIT "-V${KIRRA_QNX_QCC_VARIANT}")

# Resolve libs/headers in the QNX sysroot, never the host's.
set(CMAKE_FIND_ROOT_PATH "$ENV{QNX_TARGET}")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
