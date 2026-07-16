cmake_minimum_required(VERSION 3.21)
project(kirra_rosmaster_r2_firmware VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

option(R2_BUILD_TESTS "Build host-based unit and integration tests" ON)
option(R2_BUILD_SIMULATION "Build the deterministic host simulation" ON)
option(R2_ENABLE_SANITIZERS "Enable address and undefined-behavior sanitizers" OFF)
option(R2_ENABLE_COVERAGE "Enable GCC/Clang coverage instrumentation" OFF)
option(R2_BUILD_FUZZERS "Build the libFuzzer decoder target (requires a Clang toolchain)" OFF)

# When cross-compiling for the MCU target (see cmake/arm-none-eabi-cortex-m3.cmake),
# only the portable r2_platform_core static library is built — the host tests,
# deterministic simulation and libFuzzer targets are host-only. This slice of
# #968 proves the safety core is freestanding-clean; the linker script, startup
# and target image are follow-up slices.
if(CMAKE_CROSSCOMPILING)
    set(R2_BUILD_TESTS OFF)
    set(R2_BUILD_SIMULATION OFF)
    set(R2_BUILD_FUZZERS OFF)
    set(R2_ENABLE_SANITIZERS OFF)
    set(R2_ENABLE_COVERAGE OFF)
    message(STATUS "R2: cross-compiling r2_platform_core for ${CMAKE_SYSTEM_PROCESSOR} (host targets disabled)")
endif()

add_library(r2_platform_core STATIC
    kinematics/src/ackermann.cpp
    control/src/motion_controller.cpp
    protocol/src/wire.cpp
    safety/src/safety_manager.cpp
    firmware/src/configuration.cpp
    firmware/src/control_loop.cpp
    firmware/src/runner.cpp
    firmware/src/safe_hal.cpp
)

target_include_directories(r2_platform_core PUBLIC
    hal/include
    kinematics/include
    control/include
    protocol/include
    diagnostics/include
    safety/include
    firmware/include
    bootloader/include
)

target_compile_features(r2_platform_core PUBLIC cxx_std_17)
target_compile_options(r2_platform_core PRIVATE
    -Wall -Wextra -Wpedantic -Werror -Wconversion -Wsign-conversion
    -Wshadow -Wdouble-promotion -Wformat=2 -Wundef
    -fno-exceptions -fno-rtti
)

if(R2_ENABLE_SANITIZERS AND CMAKE_CXX_COMPILER_ID MATCHES "Clang|GNU")
    target_compile_options(r2_platform_core PUBLIC -fsanitize=address,undefined -fno-omit-frame-pointer)
    target_link_options(r2_platform_core PUBLIC -fsanitize=address,undefined)
endif()

if(R2_ENABLE_COVERAGE AND CMAKE_CXX_COMPILER_ID MATCHES "Clang|GNU")
    target_compile_options(r2_platform_core PUBLIC --coverage -O0 -g)
    target_link_options(r2_platform_core PUBLIC --coverage)
endif()

# Flashable target image (cross-compile only). Links the whole safety core plus
# the startup/vector table and the SafeHal safe-boot entry against the slot-A
# linker script, producing r2_firmware_image.elf + .bin. Build/link + size
# verification only (the target-build CI lane size-checks it against the 104 KiB
# slot); it is not run on silicon here. Host builds skip this entirely.
if(CMAKE_CROSSCOMPILING)
    set(R2_TARGET_LINKER_SCRIPT
        "${CMAKE_CURRENT_SOURCE_DIR}/firmware/link/stm32f103rc_app_a.ld")
    add_executable(r2_firmware_image
        firmware/src/startup_stm32f103.cpp
        firmware/src/main_target.cpp
    )
    target_link_libraries(r2_firmware_image PRIVATE r2_platform_core)
    target_compile_options(r2_firmware_image PRIVATE
        -Wall -Wextra -Wpedantic -Werror -Wconversion -Wsign-conversion
        -Wshadow -Wdouble-promotion -Wformat=2 -Wundef
        -fno-exceptions -fno-rtti
        # Single-core MCU, no hosted atexit: drop the thread-safe-static guards
        # and the __cxa_atexit/__dso_handle destructor registration (main never
        # returns, so local-static destructors never run anyway).
        -fno-threadsafe-statics -fno-use-cxa-atexit
    )
    target_link_options(r2_firmware_image PRIVATE
        "-T${R2_TARGET_LINKER_SCRIPT}"
        -nostartfiles
        --specs=nano.specs
        --specs=nosys.specs
        -Wl,--gc-sections
        -Wl,-Map=r2_firmware_image.map
    )
    set_target_properties(r2_firmware_image PROPERTIES
        SUFFIX ".elf"
        LINK_DEPENDS "${R2_TARGET_LINKER_SCRIPT}")

    # Emit a raw .bin next to the .elf for flashing/inspection and report size.
    find_program(R2_OBJCOPY arm-none-eabi-objcopy)
    find_program(R2_SIZE arm-none-eabi-size)
    if(R2_OBJCOPY)
        add_custom_command(TARGET r2_firmware_image POST_BUILD
            COMMAND ${R2_OBJCOPY} -O binary
                    $<TARGET_FILE:r2_firmware_image> r2_firmware_image.bin
            VERBATIM)
    endif()
    if(R2_SIZE)
        add_custom_command(TARGET r2_firmware_image POST_BUILD
            COMMAND ${R2_SIZE} $<TARGET_FILE:r2_firmware_image>
            VERBATIM)
    endif()

    # Standalone bring-up image: the same startup + SafeHal entry linked at the
    # reset base (0x08000000, whole flash) so it boots on bare silicon with no
    # bootloader. For first hardware bring-up only (flash + halt/inspect); see
    # docs/MCU_BRINGUP_DRILL.md.
    set(R2_BRINGUP_LINKER_SCRIPT
        "${CMAKE_CURRENT_SOURCE_DIR}/firmware/link/stm32f103rc_bringup.ld")
    add_executable(r2_firmware_bringup
        firmware/src/startup_stm32f103.cpp
        firmware/src/main_target.cpp
    )
    target_link_libraries(r2_firmware_bringup PRIVATE r2_platform_core)
    target_compile_options(r2_firmware_bringup PRIVATE
        -Wall -Wextra -Wpedantic -Werror -Wconversion -Wsign-conversion
        -Wshadow -Wdouble-promotion -Wformat=2 -Wundef
        -fno-exceptions -fno-rtti
        -fno-threadsafe-statics -fno-use-cxa-atexit
    )
    target_link_options(r2_firmware_bringup PRIVATE
        "-T${R2_BRINGUP_LINKER_SCRIPT}"
        -nostartfiles
        --specs=nano.specs
        --specs=nosys.specs
        -Wl,--gc-sections
        -Wl,-Map=r2_firmware_bringup.map
    )
    set_target_properties(r2_firmware_bringup PROPERTIES
        SUFFIX ".elf"
        LINK_DEPENDS "${R2_BRINGUP_LINKER_SCRIPT}")
    if(R2_OBJCOPY)
        add_custom_command(TARGET r2_firmware_bringup POST_BUILD
            COMMAND ${R2_OBJCOPY} -O binary
                    $<TARGET_FILE:r2_firmware_bringup> r2_firmware_bringup.bin
            VERBATIM)
    endif()
    if(R2_SIZE)
        add_custom_command(TARGET r2_firmware_bringup POST_BUILD
            COMMAND ${R2_SIZE} $<TARGET_FILE:r2_firmware_bringup>
            VERBATIM)
    endif()
endif()

if(R2_BUILD_TESTS)
    include(CTest)
    add_executable(r2_platform_tests tests/test_main.cpp)
    target_link_libraries(r2_platform_tests PRIVATE r2_platform_core)
    target_include_directories(r2_platform_tests PRIVATE fuzz)
    target_compile_options(r2_platform_tests PRIVATE
        -Wall -Wextra -Wpedantic -Werror -Wconversion -Wsign-conversion
        -Wshadow -Wdouble-promotion -Wformat=2 -Wundef
        -fno-exceptions -fno-rtti
    )
    add_test(NAME r2_platform_tests COMMAND r2_platform_tests)
endif()

if(R2_BUILD_SIMULATION)
    add_executable(r2_deterministic_sim simulation/r2_sim.cpp)
    target_link_libraries(r2_deterministic_sim PRIVATE r2_platform_core)
    target_compile_options(r2_deterministic_sim PRIVATE
        -Wall -Wextra -Wpedantic -Werror -Wconversion -Wsign-conversion
        -Wshadow -Wdouble-promotion -Wformat=2 -Wundef
        -fno-exceptions -fno-rtti
    )
endif()

# libFuzzer decoder target. Coverage-guided fuzzing of the untrusted R2CP decode
# path; requires a Clang toolchain for -fsanitize=fuzzer. The per-PR firmware CI
# also exercises the same oracle deterministically under ASAN/UBSAN via the
# r2_platform_tests test_decode_fuzz sweep, so this target is opt-in (OFF by
# default) and driven by the dedicated decoder-fuzz CI job.
if(R2_BUILD_FUZZERS)
    if(NOT CMAKE_CXX_COMPILER_ID MATCHES "Clang")
        message(FATAL_ERROR "R2_BUILD_FUZZERS requires a Clang toolchain (-fsanitize=fuzzer)")
    endif()
    add_executable(r2_decode_fuzz
        fuzz/decode_fuzz_libfuzzer.cpp
        protocol/src/wire.cpp
    )
    target_include_directories(r2_decode_fuzz PRIVATE protocol/include fuzz)
    target_compile_features(r2_decode_fuzz PRIVATE cxx_std_17)
    target_compile_options(r2_decode_fuzz PRIVATE
        -Wall -Wextra -Wpedantic -Werror -Wconversion -Wsign-conversion
        -Wshadow -Wdouble-promotion -Wformat=2 -Wundef
        -fsanitize=fuzzer,address,undefined -fno-omit-frame-pointer
    )
    target_link_options(r2_decode_fuzz PRIVATE
        -fsanitize=fuzzer,address,undefined
    )
endif()
