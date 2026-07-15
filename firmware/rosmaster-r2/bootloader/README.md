# Bootloader

The target is a 24 KiB, allocation-free A/B bootloader with SHA-256 and Ed25519
verification, monotonic security version, trial boot, confirmation and rollback.
The abstract verification boundary is in
`include/r2/boot/image_verifier.hpp`; no cryptographic implementation is claimed
in this milestone.

The bootloader may:

- initialize clocks needed for hash/signature verification;
- read reset cause, straps and protected boot metadata;
- receive bounded update blocks while motor outputs remain electrically off;
- erase/write only the inactive slot;
- verify and select one application;
- jump after restoring a documented peripheral state.

It may not implement motion, accept raw memory writes, boot an unverified image,
or treat CRC as authenticity. BOOT0 plus RESET must retain access to the STM32
ROM loader during development and manufacturing recovery.

See `../docs/SAFETY_AND_PRODUCTION.md` for the preliminary memory map, update
transaction and STM32F103 root-of-trust limitations.
