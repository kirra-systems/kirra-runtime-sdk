#include "r2/application/status_led_stm32.hpp"

#include <cstdint>

namespace r2::application {
namespace {

// STM32F103 register addresses (RM0008).
constexpr std::uintptr_t kRccApb2Enr = 0x4002'1018U;  // RCC_APB2ENR
constexpr std::uintptr_t kGpioCCrh = 0x4001'1004U;    // GPIOC_CRH (pins 8..15)
constexpr std::uintptr_t kGpioCBsrr = 0x4001'1010U;   // GPIOC_BSRR (atomic set)
constexpr std::uintptr_t kGpioCBrr = 0x4001'1014U;    // GPIOC_BRR  (atomic reset)

constexpr std::uint32_t kIopcEn = 1U << 4U;  // RCC_APB2ENR IOPCEN

// PC13 config lives in CRH bits [23:20]: MODE13 = 0b10 (output, 2 MHz),
// CNF13 = 0b00 (general-purpose push-pull).
constexpr std::uint32_t kPc13CfgShift = 20U;
constexpr std::uint32_t kPc13CfgMask = 0xFU << kPc13CfgShift;
constexpr std::uint32_t kPc13Cfg = 0x2U << kPc13CfgShift;
constexpr std::uint32_t kPc13Bit = 1U << 13U;

[[nodiscard]] volatile std::uint32_t& reg(std::uintptr_t address) noexcept {
    return *reinterpret_cast<volatile std::uint32_t*>(address);
}

}  // namespace

void status_led_init() noexcept {
    reg(kRccApb2Enr) = reg(kRccApb2Enr) | kIopcEn;
    reg(kGpioCCrh) = (reg(kGpioCCrh) & ~kPc13CfgMask) | kPc13Cfg;
}

void status_led_write(const bool on) noexcept {
    // Active-low: LED on = drive PC13 low (BRR), off = drive high (BSRR).
    if (on) {
        reg(kGpioCBrr) = kPc13Bit;
    } else {
        reg(kGpioCBsrr) = kPc13Bit;
    }
}

}  // namespace r2::application
