#pragma once

#include <array>
#include <cstdint>

namespace r2::hal {

enum class Evidence : std::uint8_t {
    official_r2,
    official_shared_board,
    corroborated,
    unverified,
};

struct PinAssignment {
    const char* function;
    const char* pin;
    const char* peripheral;
    Evidence evidence;
    const char* source_id;
};

// This manifest describes shared expansion-board capability, not the R2 harness.
// Unverified R2 connector selections must be resolved during board inspection.
inline constexpr std::array<PinAssignment, 25> kSharedBoardPinManifest{{
    {"status_led", "PC13", "GPIO active-low", Evidence::official_shared_board, "YB-GPIO"},
    {"buzzer", "PC5", "GPIO", Evidence::official_shared_board, "YB-BUZZER"},
    {"user_key", "PD2", "GPIO", Evidence::official_shared_board, "YB-GPIO"},
    {"sbc_uart_tx", "PA9", "USART1_TX", Evidence::official_shared_board, "YB-UART"},
    {"sbc_uart_rx", "PA10", "USART1_RX", Evidence::official_shared_board, "YB-UART"},
    {"imu_chip_select", "PB12", "SPI2_CS (ICM20948 candidate)", Evidence::unverified, "YB-R2-IMU"},
    {"imu_clock", "PB13", "SPI2_SCK / soft-I2C SCL candidate", Evidence::unverified, "YB-R2-IMU"},
    {"imu_miso", "PB14", "SPI2_MISO / MPU9250 AD0 candidate", Evidence::unverified, "YB-R2-IMU"},
    {"imu_mosi", "PB15", "SPI2_MOSI / soft-I2C SDA candidate", Evidence::unverified, "YB-R2-IMU"},
    {"servo_s1", "PC3", "TIM7 software PWM", Evidence::official_shared_board, "YB-SERVO"},
    {"servo_s2", "PC2", "TIM7 software PWM", Evidence::official_shared_board, "YB-SERVO"},
    {"servo_s3", "PC1", "TIM7 software PWM", Evidence::official_shared_board, "YB-SERVO"},
    {"servo_s4", "PC0", "TIM7 software PWM", Evidence::official_shared_board, "YB-SERVO"},
    {"motor_m1_a", "PC6", "TIM8_CH1", Evidence::official_shared_board, "YB-MOTOR"},
    {"motor_m1_b", "PC7", "TIM8_CH2", Evidence::official_shared_board, "YB-MOTOR"},
    {"motor_m2_a", "PC8", "TIM8_CH3", Evidence::official_shared_board, "YB-MOTOR"},
    {"motor_m2_b", "PC9", "TIM8_CH4", Evidence::official_shared_board, "YB-MOTOR"},
    {"motor_m3_a", "PA11", "TIM1_CH4", Evidence::corroborated, "CORR-MOTOR"},
    {"motor_m3_b", "PA8", "TIM1_CH1", Evidence::corroborated, "CORR-MOTOR"},
    {"motor_m4_a", "PB0", "TIM1_CH2N", Evidence::corroborated, "CORR-MOTOR"},
    {"motor_m4_b", "PB1", "TIM1_CH3N", Evidence::corroborated, "CORR-MOTOR"},
    {"encoder_m1", "PA15/PB3", "TIM2_CH1/CH2", Evidence::official_shared_board, "YB-ENCODER"},
    {"encoder_m2", "PB6/PB7", "TIM4_CH1/CH2", Evidence::official_shared_board, "YB-ENCODER"},
    {"encoder_m3", "PA0/PA1", "TIM5_CH1/CH2", Evidence::official_shared_board, "YB-ENCODER"},
    {"encoder_m4", "PA6/PA7", "TIM3_CH1/CH2", Evidence::official_shared_board, "YB-ENCODER"},
}};

struct R2HarnessManifest {
    const char* rear_left_candidate_channel;
    const char* rear_right_candidate_channel;
    const char* steering_connector;
    const char* emergency_stop_path;
    bool continuity_verified;
    bool actuation_enabled;
};

// Bench candidates only. A board-revision BSP must replace this with a
// continuity-verified manifest before it can compile an actuation target.
inline constexpr R2HarnessManifest kUnresolvedR2Harness{
    "M1", "M4", "UNKNOWN", "UNKNOWN", false, false};

inline constexpr const char* kExpectedSharedBoardMcu = "STM32F103RCT6";
inline constexpr std::uint32_t kExpectedCpuFrequencyHz = 72'000'000U;
inline constexpr std::uint32_t kExpectedFlashBytes = 256U * 1024U;
inline constexpr std::uint32_t kExpectedSramBytes = 48U * 1024U;

}  // namespace r2::hal
