import { ev, evRow, pageHero, ctaBand, SPIKE, GATED, LIVE } from "../template.mjs";

export const meta = {
  slug: "research",
  title: "Research",
  desc: "Kirra's research threads: a diverse dual-governor comparator, an evolution-trained planner the checker can catch, local-LLM robotics with a structural actuation fence, and zero-copy transport evaluation.",
};

export const body = `
${pageHero({
  eyebrow: "Research",
  title: "Questions we're answering<br>in public.",
  lede: "Some of the repository is settled engineering. Some of it is active inquiry — spikes, prototypes, and experiments, each clearly labeled so nobody mistakes a research thread for a shipped guarantee. This page is the honest tour.",
})}

    <section aria-labelledby="h-threads">
      <div class="container">
        <div class="grid grid--2">
          <div class="card" data-reveal>
            <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px"><h3 style="margin:0">Diverse dual governors</h3>${LIVE}</div>
            <p>Two independently-implemented governors evaluate every cycle; a comparator accumulates divergence and
            escalates the effective posture when they disagree. Diversity as a defense against common-mode bugs —
            the CERT-006 thread, with property tests over the divergence logic.</p>
            ${evRow("docs/safety/COMPARATOR_DIVERSITY.md", "parko/crates/parko-kirra")}
          </div>
          <div class="card" data-reveal>
            <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px"><h3 style="margin:0">A learnable doer, safely wrong</h3>${LIVE}</div>
            <p>An evolution-strategy-trained planner deliberately built to be misalignable: train it on a
            progress-only teacher and the checker catches it — quantified continuously by the KPI gate's
            admissibility floors. The doer-invariance thesis, made falsifiable.</p>
            ${evRow("crates/kirra-planner/src/learned.rs", "crates/kirra-doer-eval")}
          </div>
          <div class="card" data-reveal>
            <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px"><h3 style="margin:0">KITT: conversational robotics</h3>${GATED}</div>
            <p>A talking robot with a structural guarantee: the LLM (a local Gemma via Ollama, with whisper.cpp ears
            and Piper voice) has two channels — speak freely, or act through the one typed-intent door. A CI fence
            proves no compile path from conversation to actuation. It can be KITT; it cannot drive you into a wall.</p>
            ${evRow("docs/hardware/KITT_CONVERSATION_DESIGN.md", "ci/check_mick_actuation_fence.py", "robot/kitt_model_smoketest.py")}
          </div>
          <div class="card" data-reveal>
            <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px"><h3 style="margin:0">Parko: vendor-neutral inference</h3>${SPIKE}</div>
            <p>An early-prototype Rust inference substrate — ONNX Runtime (CPU/CUDA), OpenVINO, TensorRT skeleton —
            with fail-closed backend selection (no silent CPU fallback) and cross-backend numerical-equivalence
            tests. Real hardware benchmarks await the Jetson lane; the README says "not yet production," and so do we.</p>
            ${evRow("parko/README.md", "docs/adr/0009-parko-tensorrt-precision-a2-native-nvinfer.md")}
          </div>
          <div class="card" data-reveal>
            <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px"><h3 style="margin:0">Zero-copy transport</h3>${SPIKE}</div>
            <p>The iceoryx2 spike answers a narrow question — can the frozen contract ride a zero-copy channel with
            the full fault matrix intact, on a minimal (empty) feature set? Yes, measured. Adoption remains a
            deliberate, separate decision; the dependency stays quarantined until then.</p>
            ${evRow("tools/iceoryx2-spike/README.md")}
          </div>
          <div class="card" data-reveal>
            <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px"><h3 style="margin:0">Quantization under a governor</h3>${SPIKE}</div>
            <p>What does int8 quantization do to a governed planner? The committed scorecard shows fp32 and int8-PTQ
            byte-equal on admissibility — and a v2 model trading strict acceptance for progress, visible in numbers
            because the eval harness measures exactly the axes the safety case cares about.</p>
            ${evRow("parko/QUANTIZATION_DESIGN.md", "artifacts/doer-eval/scorecard.json")}
          </div>
        </div>
      </div>
    </section>

${ctaBand("Research with its status on the label.", "Spike means spike. Prototype means prototype. The interesting part is that the checker holds either way.")}
`;
