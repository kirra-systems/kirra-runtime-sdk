# Parko TensorRT GPU doer on l4t-jetpack (ADR-0036 GPU follow-up)

The Step-3 Jazzy runtime (`deploy/orin-jazzy/`) deliberately shipped the **CPU-only
safety spine** and left the **GPU doers** (parko TensorRT inference / camera
perception) as a follow-up. This is that follow-up's scaffold + design.

## The decision: the GPU doer is a DOER — it stays where its CUDA stack lives

CUDA/TensorRT on Jetson comes from NVIDIA's **l4t-jetpack** base image, which is
**JetPack 6 = Ubuntu 22.04**. ROS 2 Jazzy is 24.04-native. That's the same distro
tension the whole migration is about — and it resolves the same way it does for
Autoware:

> A **doer** may run on whatever distro its hardware stack requires; the **checker**
> is distro-independent and bounds it over the curated DDS boundary. (ADR-0036.)

So the parko TensorRT doer runs on **l4t-jetpack (22.04) + ROS 2 Humble**
(native to 22.04; `r2r` 0.9.5 supports it), publishing on the curated boundary
topics that the **Jazzy checker** (`deploy/orin-jazzy/` `kirra-adapter`) consumes
over DDS. It is the **third leg** of the topology, exactly parallel to the
Autoware-Humble doer:

```
        curated topics over DDS (host net, shared ROS_DOMAIN_ID)
 ┌────────────────────────┐        ┌──────────────────────────────┐        ┌────────────────────────┐
 │ Autoware doer (Humble) │◄──────►│  KIRRA checker/adapter        │◄──────►│ parko TRT doer         │
 │ deploy/autoware-       │        │  (Jazzy) deploy/orin-jazzy/   │        │ (l4t-jetpack, Humble)  │
 │  isolation/            │        │  — bounds everything          │        │ this scaffold          │
 └────────────────────────┘        └──────────────────────────────┘        └────────────────────────┘
```

No safety-authority change: parko-tensorrt is a doer producing proposals; the
checker admits/bounds them. `parko-kirra`'s diverse-governor comparison is a
*checker* function and is unaffected by where the inference runs.

## Provisioning (grounded in the on-Orin bench, `parko/crates/parko-tensorrt/Q1B_ORIN.md`)

The TensorRT execution provider is reached through a **TensorRT-enabled ONNX
Runtime**, provisioned exactly as the measured Q-1b run did:

- JetPack 6.x (`jp6/cu126`), **`onnxruntime-gpu` 1.23.x** wheel, `load-dynamic`,
  `ort = "=2.0.0-rc.11"` (ORT_API_VERSION 23 — do **not** move to rc.12 / ORT
  1.24.x; it deadlocks, #144, and is why Dependabot #667 is held).
- `ORT_DYLIB_PATH` → the wheel's `.../onnxruntime/capi/libonnxruntime.so.1.23.0`.
- **Fail-closed:** `PARKO_TRT_REQUIRE_EP=1` turns "TensorRT EP not registered"
  from a silent skip into a hard failure — the entrypoint runs the
  `parko-tensorrt` fail-closed probe before the node, so a CPU-only ORT (no TRT
  EP) **refuses to start** rather than degrading to a slower path unnoticed.

## Build / run

```bash
# On the Orin (or an aarch64 builder with nvcr.io access — `docker login nvcr.io`):
docker build -f deploy/docker/Dockerfile.parko-trt-jetson -t kirra-parko-trt:local .

# Runs ONLY on Jetson with the NVIDIA container runtime (CUDA/TRT from the host):
docker run --rm --runtime nvidia --network host \
  -e ROS_DOMAIN_ID=28 -e PARKO_TRT_REQUIRE_EP=1 \
  -v /path/to/model.onnx:/opt/parko/models/model.onnx:ro \
  kirra-parko-trt:local
```

`--runtime nvidia` is required; without it the CUDA/TensorRT libraries aren't
present and the fail-closed probe stops the node (which is the point).

## Honest open items (why this is a scaffold, not a shipped image)
1. **Not CI-buildable / not validated here** — l4t-jetpack pulls from `nvcr.io`
   (NGC login) and only runs on Jetson hardware; there is no GPU in CI or in this
   environment. Build + probe must be run on the Orin.
2. **`onnxruntime-gpu` wheel source** — the exact `jp6/cu126` index URL is
   integrator-provided (NVIDIA / jetson-ai-lab). The Dockerfile takes it as a
   build ARG; pin it to the 1.23.x wheel the Q-1b run used.
3. **l4t base tag pinning** — `nvcr.io/nvidia/l4t-jetpack:r36.x` is a build ARG;
   pin to the r36 minor that matches the Orin's flashed L4T. (Not Dependabot-
   tracked — nvcr.io is outside the `docker` ecosystem it watches.)
4. **ROS Humble on the l4t base** — installed from the ros.org apt repo (arm64,
   22.04). The curated `autoware_*_msgs` + `kirra_safety` overlay builds on Humble
   unchanged (the ADR-0036 cross-distro invariant).
5. **Camera perception** — the depth-camera (Astra Plus) perception doer is a
   separate node; this scaffold covers the parko inference doer. Same container
   family when it lands.

## Retirement
When NVIDIA ships a Jazzy-native l4t-jetpack (JetPack 7 / 24.04 with CUDA), this
doer collapses onto the same distro as the checker and the Humble bridge drops —
the same retirement as `deploy/autoware-isolation/` and `deploy/orin-jazzy/`.
