# Aegis — Engineering Roadmap

This directory contains pre-execution architecture sketches and execution plans
for planned integrations and extensions. Each document represents a reviewed
and approved roadmap item with honest caveats, effort estimates, and explicit
sequencing dependencies.

## Documents

| Document | Description | Priority | Gating Dependencies |
|----------|-------------|----------|-------------------|
| [APOLLO_AEGIS_INTEGRATION.md](APOLLO_AEGIS_INTEGRATION.md) | Apollo AV stack integration — Cyber RT bridge between Control and Canbus | After v1.0.5, robot demo, WeRide doc | QNX, TPM, ROS2 demo |
| [RSS_AEGIS_INTEGRATION.md](RSS_AEGIS_INTEGRATION.md) | IEEE 2846 / RSS-style behavioral safety extension | After Apollo integration | Apollo bridge, IEEE 2846 purchased and read |

## Sequencing

The correct execution order is:

1. QNX resource manager — **active priority**
2. TPM integration
3. Tag v1.0.5
4. Robot arrives → ROS2 interlock demo
5. WeRide architecture document
6. Apollo integration (4-5 weeks)
7. RSS / IEEE 2846 extension (11-13 weeks)

Do not start item 6 before items 1-5 are complete.
Do not start item 7 before item 6 Phase 2 is complete.
