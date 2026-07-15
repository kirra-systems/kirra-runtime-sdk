# Calibration guide

All calibration is per unit or per controlled hardware lot. No value from the
vendor API is automatically a physical SI-unit calibration. Perform motion work
elevated with an independent E-stop and current-limited supply.

## Profile lifecycle

1. load factory defaults (`calibrated=false`, motion cannot arm);
2. capture raw fixture data with firmware/tool versions and environmental data;
3. fit calibration off the MCU with uncertainty and residual checks;
4. send a complete versioned profile;
5. MCU validates ranges, writes inactive slot, reads back and CRC-checks;
6. reboot, verify generation and repeat measurements;
7. authorize floor use only after acceptance criteria pass.

An interrupted or invalid write retains the previous valid generation.

## Sequence

1. **Clock:** compare MCU capture timer to a traceable reference across
   temperature; record ppm drift.
2. **Battery:** sweep a calibrated supply across the operating range; fit ADC
   gain/offset and undervoltage hysteresis. Do not assume divider `4.03`.
3. **IMU:** identify variant; six-position accelerometer fit, stationary gyro
   bias/noise, turntable scale if available, magnetometer hard/soft-iron fit away
   from motor current.
4. **Encoders:** count both channels over at least ten wheel revolutions in each
   direction; establish sign, counts/revolution and missing-edge behavior.
5. **Wheel radius:** loaded rolling-distance measurement, not unloaded diameter.
6. **Geometry:** measure rear track and rear-axle-to-virtual-front-axle wheelbase
   with uncertainty.
7. **Steering:** determine pulse center from straight tracking, then pulse-to-road
   wheel angle at multiple left/right points; set software endpoints inside
   mechanical bind. Fit a monotonic piecewise-linear map.
8. **Motor feedforward:** elevated no-load characterization followed by low-speed
   floor identification under representative load; retain separate left/right
   maps and dead zones.
9. **PID:** tune wheel loops independently below hard limits; step/ramp tests
   verify settling, overshoot and saturation recovery.
10. **Odometry covariance:** repeated straight/arc trials against external ground
    truth; fit uncertainty growth and slip inflation.

## Acceptance

- steering zero produces straight tracking in both directions;
- requested and measured steering remain within calibrated residual;
- encoder sign/count repeatability passes in both directions;
- wheel speed has no sustained oscillation and anti-windup recovers;
- command limits, E-stop and timeout work at every calibration stage;
- covariance is conservative against the validation data;
- configuration CRC, generation, rollback and factory restore are demonstrated.

The approximate existing observations (0.229 m wheelbase, 834.5 left
ticks/revolution and 66.675 mm wheel diameter) are test-planning inputs only.
They are not compiled defaults.
