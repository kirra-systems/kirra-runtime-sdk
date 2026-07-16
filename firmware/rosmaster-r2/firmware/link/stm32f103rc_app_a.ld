/*
 * Linker script — ROSMASTER R2 application slot A (STM32F103RCT6, Cortex-M3).
 *
 * Memory map per docs/SAFETY_AND_PRODUCTION.md "Preliminary flash map":
 *   0x08000000  24 KiB  bootloader + public key (not owned by this script)
 *   0x08006000 104 KiB  application A   <-- this image
 *   0x08020000 104 KiB  application B
 *   0x0803A000   8 KiB  configuration A/B + calibration
 *   0x0803C000  16 KiB  event log / boot metadata
 *
 * The 104 KiB application slot is a hard exit criterion (do not grow the image
 * by removing rollback or diagnostics). The vector table is placed first in the
 * slot; startup points VTOR at the slot base so the bootloader can hand off.
 *
 * NOTE: build/link + size verification only. Slot B / config / metadata regions
 * and a real dm-verity-style root of trust are separate phase gates.
 */

ENTRY(Reset_Handler)

MEMORY
{
    FLASH (rx)  : ORIGIN = 0x08006000, LENGTH = 104K
    RAM   (rwx) : ORIGIN = 0x20000000, LENGTH = 48K
}

/* Top of stack = end of RAM (full-descending stack). */
_estack = ORIGIN(RAM) + LENGTH(RAM);

/* Minimum heap/stack the image reserves; the link fails if RAM cannot hold it
 * on top of .data/.bss, catching an over-large static footprint. */
_Min_Heap_Size  = 0x400;   /* 1 KiB */
_Min_Stack_Size = 0x800;   /* 2 KiB */

SECTIONS
{
    .isr_vector :
    {
        . = ALIGN(4);
        KEEP(*(.isr_vector))
        . = ALIGN(4);
    } >FLASH

    .text :
    {
        . = ALIGN(4);
        *(.text)
        *(.text*)
        *(.glue_7)
        *(.glue_7t)
        *(.eh_frame)
        KEEP(*(.init))
        KEEP(*(.fini))
        . = ALIGN(4);
        _etext = .;
    } >FLASH

    .rodata :
    {
        . = ALIGN(4);
        *(.rodata)
        *(.rodata*)
        . = ALIGN(4);
    } >FLASH

    .ARM.extab : { *(.ARM.extab* .gnu.linkonce.armextab.*) } >FLASH
    .ARM :
    {
        __exidx_start = .;
        *(.ARM.exidx*)
        __exidx_end = .;
    } >FLASH

    /* C++ static constructors / init arrays (run by __libc_init_array). */
    .preinit_array :
    {
        . = ALIGN(4);
        PROVIDE_HIDDEN(__preinit_array_start = .);
        KEEP(*(.preinit_array*))
        PROVIDE_HIDDEN(__preinit_array_end = .);
    } >FLASH
    .init_array :
    {
        . = ALIGN(4);
        PROVIDE_HIDDEN(__init_array_start = .);
        KEEP(*(SORT(.init_array.*)))
        KEEP(*(.init_array*))
        PROVIDE_HIDDEN(__init_array_end = .);
    } >FLASH
    .fini_array :
    {
        . = ALIGN(4);
        PROVIDE_HIDDEN(__fini_array_start = .);
        KEEP(*(SORT(.fini_array.*)))
        KEEP(*(.fini_array*))
        PROVIDE_HIDDEN(__fini_array_end = .);
    } >FLASH

    /* Initialised data: load address in FLASH, run address in RAM. */
    _sidata = LOADADDR(.data);
    .data :
    {
        . = ALIGN(4);
        _sdata = .;
        *(.data)
        *(.data*)
        . = ALIGN(4);
        _edata = .;
    } >RAM AT>FLASH

    .bss :
    {
        . = ALIGN(4);
        _sbss = .;
        __bss_start__ = _sbss;
        *(.bss)
        *(.bss*)
        *(COMMON)
        . = ALIGN(4);
        _ebss = .;
        __bss_end__ = _ebss;
    } >RAM

    /* Reserve, and range-check, heap + stack. */
    ._user_heap_stack :
    {
        . = ALIGN(8);
        PROVIDE(end = .);
        PROVIDE(_end = .);
        . = . + _Min_Heap_Size;
        . = . + _Min_Stack_Size;
        . = ALIGN(8);
    } >RAM

    /DISCARD/ : { *(.ARM.attributes) }
}
