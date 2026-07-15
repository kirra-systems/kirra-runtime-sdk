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
};

// This manifest describes shared expansion-board capability, not the R2 harness.
// Unverified R2 connector selections must be resolved during board inspection.
inline constexpr std::array<PinAssignment, 25> kSharedBoardPinManifest{{
    {"status_led", "PC13", "GPIO active-low", Evidence::official_shared_board},
    {"buzzer", "PC5", "GPIO", Evidence::official_shared_board},
    {"user_key", "PD2", "GPIO", Evidence::official_shared_board},
    {"sbc_uart_tx", "PA9", "USART1_TX", Evidence::official_shared_board},
    {"sbc_uart_rx", "PA10", "USART1_RX", Evidence::official_shared_board},
    {"imu_chip_select", "PB12", "SPI2_CS (ICM20948 variant)", Evidence::official_r2},
    {"imu_clock", "PB13", "SPI2_SCK / soft-I2C SCL", Evidence::official_r2},
    {"imu_miso", "PB14", "SPI2_MISO / MPU9250 AD0", Evidence::official_r2},
    {"imu_mosi", "PB15", "SPI2_MOSI / soft-I2C SDA", Evidence::official_r2},
    {"servo_s1", "PC3", "TIM7 software PWM", Evidence::official_shared_board},
    {"servo_s2", "PC2", "TIM7 software PWM", Evidence::official_shared_board},
    {"servo_s3", "PC1", "TIM7 software PWM", Evidence::official_shared_board},
    {"servo_s4", "PC0", "TIM7 software PWM", Evidence::official_shared_board},
    {"motor_m1_a", "PC6", "TIM8_CH1", Evidence::official_shared_board},
    {"motor_m1_b", "PC7", "TIM8_CH2", Evidence::official_shared_board},
    {"motor_m2_a", "PC8", "TIM8_CH3", Evidence::official_shared_board},
    {"motor_m2_b", "PC9", "TIM8_CH4", Evidence::official_shared_board},
    {"motor_m3_a", "PA11", "TIM1_CH4", Evidence::corroborated},
    {"motor_m3_b", "PA8", "TIM1_CH1", Evidence::corroborated},
    {"motor_m4_a", "PB0", "TIM1_CH2N", Evidence::corroborated},
    {"motor_m4_b", "PB1", "TIM1_CH3N", Evidence::corroborated},
    {"encoder_m1", "PA15/PB3", "TIM2_CH1/CH2", Evidence::official_shared_board},
    {"encoder_m2", "PB6/PB7", "TIM4_CH1/CH2", Evidence::official_shared_board},
    {"encoder_m3", "PA0/PA1", "TIM5_CH1/CH2", Evidence::official_shared_board},
    {"encoder_m4", "PA6/PA7", "TIM3_CH1/CH2", Evidence::official_shared_board},
}};

inline constexpr const char* kTargetMcu = "STM32F103RCT6";
inline constexpr std::uint32_t kCpuFrequencyHz = 72'000'000U;
inline constexpr std::uint32_t kFlashBytes = 256U * 1024U;
inline constexpr std::uint32_t kSramBytes = 48U * 1024U;

}  // namespace r2::hal
