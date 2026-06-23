cmake_minimum_required(VERSION 3.8)
project(kirra_bridge_cpp)

if(NOT CMAKE_CXX_STANDARD)
  set(CMAKE_CXX_STANDARD 17)
endif()

find_package(ament_cmake REQUIRED)
find_package(rclcpp REQUIRED)
find_package(geometry_msgs REQUIRED)
find_package(std_msgs REQUIRED)

add_executable(kirra_bridge_node src/kirra_bridge_node.cpp)

target_include_directories(kirra_bridge_node PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/../../../include")
target_link_directories(kirra_bridge_node PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/../../../target/release")

ament_target_dependencies(kirra_bridge_node rclcpp geometry_msgs std_msgs)
target_link_libraries(kirra_bridge_node kirra_verifier)

set_target_properties(kirra_bridge_node PROPERTIES INSTALL_RPATH "${CMAKE_CURRENT_SOURCE_DIR}/../../../target/release")

install(TARGETS kirra_bridge_node DESTINATION lib/${PROJECT_NAME})
ament_package()
