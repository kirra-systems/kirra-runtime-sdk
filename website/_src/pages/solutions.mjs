import { ev, evRow, pageHero, ctaBand, LIVE, GATED, SPIKE } from "../template.mjs";

export const meta = {
  slug: "solutions",
  title: "Solutions",
  desc: "What Kirra makes safer, in plain language: autonomous vehicles, robots that work around people, industrial control systems, LLM-driven machines, and mixed fleets — with the exact mechanism behind each.",
};

/* A use-case block: scenario story → what Kirra does → evidence */
const useCase = ({ id, pill, title, story, does, chips, footnote }) => `
    <section id="${id}" aria-labelledby="h-${id}">
      <div class="container">
        <div class="grid grid--2" style="gap:48px;align-items:start">
          <div>
            <div style="display:flex;align-items:center;gap:14px;flex-wrap:wrap">
              <h2 id="h-${id}" style="margin:0">${title}</h2>${pill}
            </div>
            <div class="compare-note" style="margin-top:22px">${story}</div>
            ${footnote ? `<p class="evidence-note" style="margin-top:14px">${footnote}</p>` : ""}
          </div>
          <div style="display:grid;gap:14px">
            ${does.map(([h, p, e]) => `<div class="card" data-reveal><h3 style="font-size:1.05rem">${h}</h3><p>${p}</p>${e}</div>`).join("")}
            <p class="evidence-row">${chips}</p>
          </div>
        </div>
      </div>
    </section>
    <hr class="rule">`;

export const body = `
${pageHero({
  eyebrow: "Solutions",
  title: "One checker.<br>Every machine that moves.",
  lede: "Kirra sits between whatever decides — a planner, a PLC master, a language model — and whatever moves. Here is what that means for each class of machine, told as the failure it prevents. Every mechanism links to its source.",
  extra: `<nav aria-label="Use cases" class="evidence-row" style="margin-top:28px">
          <a class="evidence" href="#av">autonomous vehicles</a>
          <a class="evidence" href="#robots">robots around people</a>
          <a class="evidence" href="#industrial">industrial control</a>
          <a class="evidence" href="#llm">LLM-driven machines</a>
          <a class="evidence" href="#fleet">fleet operations</a>
        </nav>`,
})}

${useCase({
  id: "av",
  pill: LIVE,
  title: "Autonomous vehicles",
  story: `<strong>October 2023, San Francisco:</strong> after a collision, an AV executed its pullover
    at ~3&nbsp;m/s — under its speed ceiling, so nothing stopped it — dragging a pedestrian 20 feet.
    The stack was following its rules. The rules were wrong. Kirra's Degraded posture was designed against
    exactly this failure: after a fault, the governor admits only commands that <em>converge to a stop</em>,
    holds at zero, and never authors re-acceleration. A crawl that satisfies a speed cap is still denied,
    because the rule is about the direction of travel toward stillness — not a number.`,
  does: [
    ["A physics envelope the planner can't argue with",
     "Every command is clamped to a per-class kinematic contract — speed, acceleration, steering rate, lateral load — hard boundary first, rate limits second. The robotaxi profile is the frozen reference, and NaN anywhere is an instant deny, proved over all 2⁶⁴ bit patterns.",
     evRow("docs/CONTRACT_PROFILES.md", "verification/kani/src/proofs_kinematics.rs")],
    ["Formal distance keeping, not tuned heuristics",
     "RSS safe-distance mathematics — longitudinal and lateral as a conjunction, occlusion caps at blind junctions, multi-modal prediction of cut-ins — refuse trajectories that current-position checks would miss.",
     evRow("parko/crates/parko-core/src/rss.rs", "docs/adr/0017-multi-modal-predictive-rss.md")],
    ["Governs the stack you already run",
     "The decision of record keeps Autoware as the driving stack, isolated in its own container; Kirra bounds its output at a frozen contract. You don't replace your planner — you make it accountable.",
     evRow("docs/adr/0036-autoware-distro-migration-occy-gap.md")],
  ],
  chips: "",
  footnote: "Degraded decel-to-stop is enforced at four points in the stack and machine-checked by proof; the safe-state spec (KIRRA-SSS-001) assigns a safe state to every fault class.",
})}

${useCase({
  id: "robots",
  pill: LIVE,
  title: "Robots that work around people",
  story: `A sidewalk delivery robot shares space with children, dogs, and people who aren't watching it.
    The dangerous failure isn't the obstacle it sees — it's the pedestrian sensor that quietly died
    ten seconds ago while the robot keeps driving on stale confidence. Kirra treats <em>silence as a
    fault</em>: an armed pedestrian channel that goes quiet caps speed to zero. The robot stops because
    it can no longer prove the sidewalk is clear — which is the only honest thing a blind robot can do.`,
  does: [
    ["An envelope sized for sidewalks, not shrunk from cars",
     "The courier class is its own ODD: 3.0 m/s max, 0.6 m lateral band, omnidirectional pedestrian RSS (people step out of doorways behind you), and a bounded yaw channel for differential drive.",
     evRow("docs/adr/0028-sidewalk-courier-odd.md", "docs/safety/PEDESTRIAN_RSS.md")],
    ["Watchdogs on every input",
     "Telemetry silence beyond 2 seconds marks the source untrusted and recomputes posture within 100 ms. Perception freshness budgets degrade prediction gracefully — but never let old data pass as current.",
     evRow("src/telemetry_watchdog.rs", "crates/kirra-trajectory/src/vru_channel.rs")],
    ["Proven on a physical robot",
     "This isn't a simulator story: first governed motion on an Ackermann robot with a live lidar ran in July 2026, on the same code paths CI tests — with the bring-up runbooks and known limitations published.",
     evRow("robot/install/README.md", "docs/adr/0014-rosmaster-r2-orin-nx-kirra-integration.md")],
  ],
  chips: "",
})}

${useCase({
  id: "industrial",
  pill: LIVE,
  title: "Industrial control &amp; OT",
  story: `A compromised engineering workstation — or just a fat-fingered setpoint — writes a 400%
    output command to a PLC. The protocol is valid, the credentials check out, and the actuator will
    happily comply. Kirra's industrial adapters decode the actual control write on the wire —
    a DNP3 analog setpoint, a CANopen SDO download, an EtherNet/IP attribute write — check the decoded
    value against a configured physical envelope, and <em>deny what's out of range</em>. A payload that
    can't be faithfully decoded is refused too: the governor never guesses what a malformed frame meant.`,
  does: [
    ["Native protocol awareness",
     "Modbus and OPC-UA event mapping, DNP3 Analog Output (g41) magnitude envelopes, per-target typed CANopen SDO bounds, CIP Set_Attribute_Single bounds — each with a strict mode that denies writes to any target without a configured bound.",
     evRow("docs/protocol_adapters.md", "crates/kirra-industrial", "src/protocol_adapter.rs")],
    ["Posture across the plant",
     "Assets form a dependency fabric: a faulted upstream asset degrades what depends on it, and industrial events feed the same fleet-posture engine that gates every command class.",
     evRow("docs/multi_asset_fabric.md")],
    ["An audit trail regulators can verify",
     "Every denied setpoint lands in the hash-chained ledger with the decoded value that was refused — shippable off-box to WORM storage and independently re-verifiable. The DNP3 deny path is mandatory-audited by test.",
     evRow("src/audit_shipper.rs", "src/bin/kirra_verifier_service/dnp3_mandatory_audit_tests.rs")],
  ],
  chips: "",
  footnote: "Decoders for hostile inputs are fuzzed continuously — the DNP3 setpoint decoder is one of the four standing fuzz targets.",
})}

${useCase({
  id: "llm",
  pill: LIVE,
  title: "LLM-driven machines",
  story: `You want a robot you can talk to. The model, sooner or later, will tell it to do something
    insane — a hallucinated 999&nbsp;m/s velocity, an action type that doesn't exist, a confidently wrong
    tool call. Kirra's answer is structural: language can only become one thing — a typed intent through a
    single fail-closed parse — and that intent still faces the full checker like any other proposal.
    On our own talking robot, a CI gate proves the conversational code has <em>no compile path</em> to
    actuation. The model can say anything. It can't do anything.`,
  does: [
    ["One door from words to motion",
     "Free text → typed MickIntent (seven verbs, one parse, malformed ⇒ HOLD) → planner grounds it against the map → checker validates the trajectory. A model outage stops the conversation, never the safety case.",
     evRow("crates/kirra-planner/src/mick_llm.rs", "docs/llm_integration_guide.md")],
    ["Action claims, filtered",
     "Structured LLM/agent outputs pass an action filter that validates every claim against current posture before anything downstream sees it — and the JSON intent parser is a standing fuzz target.",
     evRow("src/action_filter.rs", "fuzz/fuzz_targets/llm_json_intent.rs")],
    ["The fence is compile-time",
     "The LLM-facing crates cannot link the release token, the serial consumer, or the ROS/DDS actuation path — checked by a CI gate on the dependency graph, not by convention.",
     evRow("ci/check_mick_actuation_fence.py")],
  ],
  chips: "",
  footnote: "The attempt is recorded either way: hallucinated commands become permanent, hash-chained audit entries.",
})}

${useCase({
  id: "fleet",
  pill: LIVE,
  title: "Fleet operations",
  story: `It's 2 AM and one vehicle in your fleet fails attestation — its software isn't what you signed.
    In most fleets, you find out in a postmortem. In Kirra's model, trust is recomputed continuously from
    cryptographic proof: the node goes Untrusted, everything depending on it degrades, and the next command
    routed anywhere in that dependency graph feels the change. Recovery isn't a timer — it's five clean
    reports in ten seconds, or an operator's signature.`,
  does: [
    ["Identity by proof, not assertion",
     "Every node answers challenges with an Ed25519 signature over a fresh nonce — optionally rooted in a TPM measured-boot quote. No proof, no trust, no route.",
     evRow("crates/kirra-safety-authority/src/attestation.rs", "src/tpm_quote.rs")],
    ["Updates that halt before they hurt",
     "Governor artifacts roll out in staged campaigns that are fail-closed on fleet posture — a degraded fleet halts the rollout automatically — with Uptane-verified metadata and health-gated A/B rollback on every node.",
     evRow("crates/kirra-ota-campaign", "crates/kirra-ota-installer")],
    ["A control plane that survives its own failures",
     "HA failover with durable epoch fencing means exactly one writer even when the old primary comes back; the drill runs in CI, and the lease algebra is machine-proved.",
     evRow("tests/ha_failover.rs", "verification/kani/src/proofs_lease.rs")],
  ],
  chips: "",
})}

${ctaBand("Your machines, held to an envelope.", "Whatever proposes — a planner, a PLC master, a language model — Kirra decides what reaches the actuators. Start with the class that matches your fleet.")}
`;
