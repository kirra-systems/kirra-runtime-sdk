# CMake toolchain file for the ROSMASTER R2 target MCU.
#
# Target: STM32F103RCT6 — Cortex-M3, 72 MHz, 256 KiB flash, 48 KiB SRAM, no FPU
# (see docs/HARDWARE_REFERENCE.md and hal/board_manifest.hpp
# kExpectedSharedBoardMcu). Use with:
#
#   cmake -S . -B build-target \
#     -DCMAKE_TOOLCHAIN_FILE=cmake/arm-none-eabi-cortex-m3.cmake
#
# This slice cross-compiles the portable r2_platform_core static library
# (freestanding, no host libc/OS) to prove the safety core carries no host
# dependency. The linker script, startup/vector table and the target
# application image are follow-up slices of #968; a static-library build needs
# no linker script, so CMAKE_TRY_COMPILE_TARGET_TYPE is set accordingly to keep
# the compiler-detection probe from trying to link a bare-metal executable.

set(CMAKE_SYSTEM_NAME Generic)
set(CMAKE_SYSTEM_PROCESSOR arm)

# The compiler-check must not attempt a full link (no startup/linker script yet).
set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)

set(CMAKE_C_COMPILER arm-none-eabi-gcc)
set(CMAKE_CXX_COMPILER arm-none-eabi-g++)
set(CMAKE_ASM_COMPILER arm-none-eabi-gcc)

# Cortex-M3: Thumb-2, soft-float (no FPU on the F1). We build against newlib
# (the arm-none-eabi libc), so this is a *hosted* freestanding target — NOT
# -ffreestanding: that would set _GLIBCXX_HOSTED=0, and newlib's <cmath> →
# <bits/specfun.h> (C++17 special math functions) then references
# std::__throw_domain_error, which the non-hosted <bits/functexcept.h> omits.
# The section flags let a later link garbage-collect unused code/data to fit the
# 256 KiB flash.
set(_r2_target_arch_flags
    "-mcpu=cortex-m3 -mthumb -mfloat-abi=soft -ffunction-sections -fdata-sections")

set(CMAKE_C_FLAGS_INIT "${_r2_target_arch_flags}")
set(CMAKE_CXX_FLAGS_INIT "${_r2_target_arch_flags} -fno-exceptions -fno-rtti -fno-unwind-tables")
set(CMAKE_ASM_FLAGS_INIT "${_r2_target_arch_flags}")

# Never search the host for programs; only the cross sysroot for libs/headers.
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
