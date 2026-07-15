cmake_minimum_required(VERSION 3.21)
project(kirra_rosmaster_r2_firmware VERSION 0.1.0 LANGUAGES CXX)

option(R2_BUILD_TESTS "Build host-based unit and integration tests" ON)
option(R2_BUILD_SIMULATION "Build the deterministic host simulation" ON)
option(R2_ENABLE_SANITIZERS "Enable address and undefined-behavior sanitizers" OFF)
option(R2_ENABLE_COVERAGE "Enable GCC/Clang coverage instrumentation" OFF)

add_library(r2_platform_core STATIC
    kinematics/src/ackermann.cpp
    control/src/motion_controller.cpp
    protocol/src/wire.cpp
    safety/src/safety_manager.cpp
    firmware/src/configuration.cpp
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
    -Wall -Wextra -Wpedantic -Wconversion -Wsign-conversion
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

if(R2_BUILD_TESTS)
    include(CTest)
    add_executable(r2_platform_tests tests/test_main.cpp)
    target_link_libraries(r2_platform_tests PRIVATE r2_platform_core)
    target_compile_options(r2_platform_tests PRIVATE
        -Wall -Wextra -Wpedantic -Wconversion -Wsign-conversion
        -Wshadow -Wdouble-promotion -Wformat=2 -Wundef
        -fno-exceptions -fno-rtti
    )
    add_test(NAME r2_platform_tests COMMAND r2_platform_tests)
endif()

if(R2_BUILD_SIMULATION)
    add_executable(r2_deterministic_sim simulation/r2_sim.cpp)
    target_link_libraries(r2_deterministic_sim PRIVATE r2_platform_core)
    target_compile_options(r2_deterministic_sim PRIVATE
        -Wall -Wextra -Wpedantic -Wconversion -Wsign-conversion
        -Wshadow -Wdouble-promotion -Wformat=2 -Wundef
        -fno-exceptions -fno-rtti
    )
endif()
