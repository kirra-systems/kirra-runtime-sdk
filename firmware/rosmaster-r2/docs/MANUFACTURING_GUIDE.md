# Manufacturing and service guide

## Required records

For each unit record chassis/PCB/MCU/IMU/servo/motor revisions, MCU unique ID,
device certificate/key ID, bootloader and application hashes, configuration
generation, fixture/software versions, operator/station, timestamp and all test
results. A lot-level assumption never replaces unit traceability.

## Station sequence

1. visual inspection and photographed board identification;
2. continuity/short/open fixture test with propulsion power isolated;
3. SWD identity, flash/RAM capacity and recovery-path check;
4. initial signed manufacturing image;
5. GPIO/peripheral bed-of-nails test with bridge disabled;
6. motor channel/polarity and encoder sign test, wheels elevated;
7. steering connector, center, endpoints and feedback test;
8. IMU identity/orientation and battery ADC supply sweep;
9. independent E-stop electrical timing, watchdog and brownout injection;
10. calibration profile creation and readback;
11. production bootloader/application flash and signature/rollback tests;
12. elevated end-of-line motion, controlled stop and communication-loss test;
13. export/sign station report, then apply approved protection settings.

Any unresolved hardware option, unsigned image, missing test record, invalid
configuration, deadline miss or safety-fault mismatch quarantines the unit.

## Key handling

Production signing keys remain offline/HSM-backed. The station receives signed
artifacts and per-device provisioning envelopes; it never receives the release
private key. Development and production trust roots are distinct. Device secrets
are injected only on controlled stations, never logged, and verified by
challenge. Key rotation and revocation are rehearsed before shipment.

STM32 readout/write protection is applied only after ROM-loader/SWD recovery and
rollback behavior are validated on sacrificial units. The irreversible RDP
level is a product-security decision, not a default firmware setting.

## Service

Service mode requires motors physically lifted or propulsion isolated,
authenticated tooling and an event-log entry. Firmware update always targets the
inactive slot. Calibration changes retain the previous valid generation. A
factory reset restores `calibrated=false`; it never restores guessed motion
constants.

Field returns preserve event logs and exact firmware/configuration images before
rework. Repaired units repeat the complete affected station sequence plus the
end-of-line safety test.
