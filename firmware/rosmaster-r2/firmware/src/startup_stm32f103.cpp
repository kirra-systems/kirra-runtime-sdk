// Startup + vector table for the ROSMASTER R2 application (STM32F103RCT6,
// Cortex-M3), application slot A.
//
// Reset_Handler repoints the vector table (fail-closed, before anything else),
// initialises the C/C++ runtime (copy .data, zero .bss, run the .init_array
// constructors manually — no crti/crtn `_init` under -nostartfiles) and calls
// the application entry r2_app_main(). The fault handlers enter a safe halt —
// with the H-bridge already de-energised by hardware/POR, a spin is the safe
// state until the independent watchdog resets the part. This is the standard
// bare-metal reset path; peripheral IRQ vectors are added alongside the concrete
// drivers (#967) that enable those interrupts.
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

// Application entry. Named r2_app_main (not main) so the freestanding startup
// can call it without tripping the ISO C++ rule against a program calling ::main.
int r2_app_main();

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

// Cortex-M3 core vector table as an array of 32-bit words: word 0 is the initial
// stack pointer, words 1.. are exception-entry addresses. It is an integer-word
// table (not a function-pointer array) so the initial-SP entry is a well-defined
// object-pointer-to-integer cast, rather than an object-pointer-to-function-
// pointer reinterpret (which is only conditionally-supported in C++). The
// hardware reads raw words, so the handler-address entries are identical bytes.
// Placed first in FLASH by the linker script; Reset_Handler points VTOR at it.
__attribute__((section(".isr_vector"), used))
const std::uintptr_t g_vector_table[16] = {
    reinterpret_cast<std::uintptr_t>(&_estack),
    reinterpret_cast<std::uintptr_t>(&Reset_Handler),
    reinterpret_cast<std::uintptr_t>(&NMI_Handler),
    reinterpret_cast<std::uintptr_t>(&HardFault_Handler),
    reinterpret_cast<std::uintptr_t>(&MemManage_Handler),
    reinterpret_cast<std::uintptr_t>(&BusFault_Handler),
    reinterpret_cast<std::uintptr_t>(&UsageFault_Handler),
    0U,  // reserved
    0U,  // reserved
    0U,  // reserved
    0U,  // reserved
    reinterpret_cast<std::uintptr_t>(&SVC_Handler),
    reinterpret_cast<std::uintptr_t>(&DebugMon_Handler),
    0U,  // reserved
    reinterpret_cast<std::uintptr_t>(&PendSV_Handler),
    reinterpret_cast<std::uintptr_t>(&SysTick_Handler),
};

// SCB->VTOR (vector table offset register).
constexpr std::uint32_t kScbVtorAddress = 0xE000'ED08U;

void SystemInit() {
    // Point the core at the vector table by its own linked address, so the same
    // startup is correct at any link base (the slot-A image at 0x08006000 behind
    // the bootloader, or a standalone bring-up image at 0x08000000). Clock
    // configuration beyond the reset-default HSI (the 72 MHz PLL tree) is a
    // follow-up; nothing in the safe-boot image depends on the bus frequency.
    *reinterpret_cast<volatile std::uint32_t*>(kScbVtorAddress) =
        static_cast<std::uint32_t>(reinterpret_cast<std::uintptr_t>(&g_vector_table));
}

void Reset_Handler() {
    // Fail-closed: repoint the vector table to THIS image's handlers before any
    // other work, so a fault during early memory init (or a stale VTOR a
    // bootloader may have left) vectors into our table, not someone else's.
    SystemInit();

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

    // Run C++ static constructors (the .init_array). Empty in the current image,
    // but correct as global-scope constructors are added.
    for (void (*const* ctor)() = __init_array_start; ctor < __init_array_end; ++ctor) {
        (*ctor)();
    }
    static_cast<void>(r2_app_main());

    // r2_app_main() owns the control loop and must not return; if it ever does,
    // halt in the safe state (bridge de-energised) until the watchdog resets us.
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
