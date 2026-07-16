// Startup + vector table for the ROSMASTER R2 application (STM32F103RCT6,
// Cortex-M3), application slot A.
//
// Reset_Handler initialises the C/C++ runtime (copy .data, zero .bss, run the
// static constructors via __libc_init_array) and calls main(). The fault
// handlers enter a safe halt — with the H-bridge already de-energised by
// hardware/POR, a spin is the safe state until the independent watchdog resets
// the part. This is the standard bare-metal reset path; peripheral IRQ vectors
// are added alongside the concrete drivers (#967) that enable those interrupts.
//
// Build/link + size verification only: this image links the whole safety core
// and fits the 104 KiB slot; it is NOT validated on silicon, and the PLL/clock
// tree beyond the reset-default HSI is a follow-up (see main_target.cpp).

#include <cstdint>

extern "C" {

// Provided by the linker script (stm32f103rc_app_a.ld).
extern std::uint32_t _sidata;  // .data init image (in FLASH)
extern std::uint32_t _sdata;   // .data start (in RAM)
extern std::uint32_t _edata;   // .data end (in RAM)
extern std::uint32_t _sbss;    // .bss start
extern std::uint32_t _ebss;    // .bss end
extern std::uint32_t _estack;  // top of stack

// C++ static constructor table (from the linker script). Run manually so the
// image needs no crti/crtn `_init` (we link -nostartfiles).
extern void (*const __init_array_start[])();
extern void (*const __init_array_end[])();

// Application entry.
int r2_app_main();  // application entry (named to avoid the ISO ::main restriction)

void Reset_Handler();
void Default_Handler();
void SystemInit();

// Cortex-M3 core exception handlers. Weakly aliased to Default_Handler so a
// driver can override any of them; the fault handlers spin in the safe state.
void NMI_Handler() __attribute__((weak, alias("Default_Handler")));
void HardFault_Handler();
void MemManage_Handler();
void BusFault_Handler();
void UsageFault_Handler();
void SVC_Handler() __attribute__((weak, alias("Default_Handler")));
void DebugMon_Handler() __attribute__((weak, alias("Default_Handler")));
void PendSV_Handler() __attribute__((weak, alias("Default_Handler")));
void SysTick_Handler() __attribute__((weak, alias("Default_Handler")));

using VectorEntry = void (*)();

// Cortex-M3 core vector table. The first word is the initial stack pointer; the
// bootloader hands off by loading this slot's SP and jumping to Reset_Handler.
// Placed first in FLASH by the linker script; SystemInit points VTOR here.
__attribute__((section(".isr_vector"), used))
const VectorEntry g_vector_table[16] = {
    reinterpret_cast<VectorEntry>(&_estack),
    Reset_Handler,
    NMI_Handler,
    HardFault_Handler,
    MemManage_Handler,
    BusFault_Handler,
    UsageFault_Handler,
    nullptr,  // reserved
    nullptr,  // reserved
    nullptr,  // reserved
    nullptr,  // reserved
    SVC_Handler,
    DebugMon_Handler,
    nullptr,  // reserved
    PendSV_Handler,
    SysTick_Handler,
};

// Application slot base — the vector table lives here (see the linker script).
constexpr std::uint32_t kVectorTableBase = 0x0800'6000U;
// SCB->VTOR (vector table offset register).
constexpr std::uint32_t kScbVtorAddress = 0xE000'ED08U;

void SystemInit() {
    // Point the core at this slot's vector table. Clock configuration beyond the
    // reset-default HSI (the 72 MHz PLL tree) is a follow-up; nothing in the
    // safe-boot image depends on the bus frequency.
    *reinterpret_cast<volatile std::uint32_t*>(kScbVtorAddress) = kVectorTableBase;
}

void Reset_Handler() {
    // Copy the .data initialisers from FLASH to RAM. _sdata/_edata are linker
    // region-boundary symbols, not one object — comparing them walks the region.
    const std::uint32_t* source = &_sidata;
    // cppcheck-suppress comparePointers
    for (std::uint32_t* destination = &_sdata; destination < &_edata;) {
        *destination++ = *source++;
    }
    // Zero .bss (likewise bounded by the _sbss/_ebss linker symbols).
    // cppcheck-suppress comparePointers
    for (std::uint32_t* bss = &_sbss; bss < &_ebss;) {
        *bss++ = 0U;
    }

    SystemInit();
    // Run C++ static constructors (the .init_array). Empty in the current
    // image, but correct as global-scope constructors are added.
    for (void (*const* ctor)() = __init_array_start; ctor < __init_array_end; ++ctor) {
        (*ctor)();
    }
    static_cast<void>(r2_app_main());

    // main() owns the control loop and must not return; if it ever does, halt
    // in the safe state (bridge de-energised) until the watchdog resets us.
    for (;;) {
    }
}

void Default_Handler() {
    for (;;) {
    }
}

void HardFault_Handler() {
    for (;;) {
    }
}

void MemManage_Handler() {
    for (;;) {
    }
}

void BusFault_Handler() {
    for (;;) {
    }
}

void UsageFault_Handler() {
    for (;;) {
    }
}

}  // extern "C"
