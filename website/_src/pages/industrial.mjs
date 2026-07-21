import { SITE, ev, evRow, pageHero, ctaBand, LIVE } from "../template.mjs";

export const meta = {
  slug: "industrial",
  title: "Industrial &amp; OT",
  desc: "Kirra for operational technology: a fail-closed governor that decodes the actual control write on the wire — DNP3, CANopen, EtherNet/IP — checks it against a physical envelope, and denies out-of-range setpoints regardless of credentials, with a tamper-evident record.",
};

const wireSvg = `
<svg viewBox="0 0 960 250" role="img" aria-labelledby="otT otD">
  <title id="otT">Setpoint bounding on the wire</title>
  <desc id="otD">A control write travels from an operator or engineering workstation toward a PLC or RTU. Kirra decodes the setpoint by its configured type, checks the magnitude against a physical envelope, and either forwards an in-range value or denies an out-of-range one and records it in the hash-chained ledger — regardless of whether the credentials were valid.</desc>
  <g>
    <rect class="dg-box" x="14" y="90" width="180" height="70" rx="10"/>
    <text class="dg-label" x="32" y="120">HMI / workstation</text>
    <text class="dg-sub" x="32" y="140">valid credentials</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M194 125 L 250 125"/>
  <text class="dg-mono" x="198" y="114">Operate / Set</text>
  <g>
    <rect class="dg-box dg-box--accent" x="252" y="70" width="220" height="110" rx="12"/>
    <text class="dg-label" x="272" y="100">KIRRA — on the wire</text>
    <text class="dg-mono" x="272" y="124">decode by configured type</text>
    <text class="dg-mono" x="272" y="144">check magnitude vs envelope</text>
    <text class="dg-sub" x="272" y="166">undecodable ⇒ refuse, never guess</text>
  </g>
  <path class="dg-edge dg-edge--flow" d="M472 105 L 560 105"/>
  <text class="dg-mono" x="486" y="95">in range</text>
  <g>
    <rect class="dg-box" x="562" y="72" width="180" height="64" rx="10"/>
    <text class="dg-label" x="580" y="100">PLC / RTU</text>
    <text class="dg-sub" x="580" y="120">command applied</text>
  </g>
  <path class="dg-edge dg-edge--deny" d="M472 150 L 560 170"/>
  <text class="dg-mono" x="486" y="150">out of range</text>
  <g>
    <rect class="dg-box dg-box--deny" x="562" y="150" width="220" height="60" rx="10"/>
    <text class="dg-label" x="580" y="178">DENY + hash-chained record</text>
    <text class="dg-sub" x="580" y="198">identity does not excuse physics</text>
  </g>
</svg>`;

export const body = `
${pageHero({
  eyebrow: "Industrial &amp; OT",
  title: `Valid credentials.<br>Catastrophic command.<br><span style="color:var(--accent)">Denied on physics.</span>`,
  lede: "The dangerous failure in operational technology isn't always an intruder — it's a valid, authenticated write with a physically impossible value: a 100-fold lye setpoint, a 400% output, a reversed valve. Kirra decodes the actual control write on the wire and denies on magnitude, regardless of whose credentials carried it — and records the refusal in a ledger a regulator can verify.",
})}

    <section aria-labelledby="h-story">
      <div class="container">
        <div class="grid grid--2" style="gap:48px;align-items:start">
          <div>
            <div class="compare-note" data-reveal>
              <strong>Oldsmar, Florida — February 2021.</strong> An operator watched the sodium-hydroxide setpoint
              at a municipal water plant climb from 100&nbsp;ppm to 11,100&nbsp;ppm on a remotely-accessed HMI —
              a 111× jump toward dosing a town's drinking water, caught by human luck minutes from harm. Whether
              the cause was an intrusion or an operator error is still debated, and that debate <em>is</em> the
              lesson: the control system accepted a physically catastrophic setpoint on valid credentials either
              way. A magnitude envelope on the wire refuses it in both cases.
            </div>
            <p class="evidence-note" style="margin-top:14px">Kirra's founder holds licensed operating experience on public
            water-treatment control systems — the exact class of infrastructure this addresses. <a class="evidence" href="company.html">About the company</a></p>
          </div>
          <div class="diagram-frame flow-anim" data-reveal>${wireSvg}</div>
        </div>
        ${evRow("docs/protocol_adapters.md", "src/protocol_adapter.rs", "crates/kirra-industrial")}
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-protocols">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Native protocol awareness</p>
          <h2 id="h-protocols" data-reveal>It reads the wire, not a wrapper</h2>
          <p class="lede" data-reveal>Kirra decodes each control write by its <em>configured type</em> — because the
          frame on the wire carries width at best, never type — then bounds the decoded value. A write that can't
          be faithfully decoded is refused, never fabricated. Envelopes are configuration; a high-assurance
          strict mode denies any write to a target with no configured bound.</p>
        </div>
        <div class="table-wrap" data-reveal>
          <table class="tbl">
            <caption class="visually-hidden">Industrial protocol adapters</caption>
            <thead><tr><th scope="col">Protocol</th><th scope="col">What Kirra bounds</th><th scope="col">Configuration</th><th scope="col">Status</th></tr></thead>
            <tbody>
              <tr>
                <td>DNP3</td>
                <td>Analog Output (g41) control writes — Operate / Direct_Operate setpoints</td>
                <td class="mono">KIRRA_DNP3_ANALOG_OUTPUT_ENVELOPE</td>
                <td>${LIVE}</td>
              </tr>
              <tr>
                <td>CANopen</td>
                <td>SDO expedited-download setpoints, decoded by the OD entry's type (i8…f32)</td>
                <td class="mono">KIRRA_CANOPEN_SDO_BOUNDS · _STRICT_BOUNDS</td>
                <td>${LIVE}</td>
              </tr>
              <tr>
                <td>EtherNet/IP (CIP)</td>
                <td>Set_Attribute_Single writes, decoded by the attribute's data type</td>
                <td class="mono">KIRRA_CIP_ATTR_BOUNDS · _STRICT_BOUNDS</td>
                <td>${LIVE}</td>
              </tr>
              <tr>
                <td>Modbus / OPC-UA</td>
                <td>Event mapping into the fleet posture engine</td>
                <td class="mono">protocol_adapter.rs</td>
                <td>${LIVE}</td>
              </tr>
            </tbody>
          </table>
        </div>
        ${evRow("CLAUDE.md", "crates/kirra-industrial")}
        <p class="evidence-note">Env-var names are the repository's own; each maps a target (node/index/attribute) to a
        typed <code>min:max</code> range. Unconfigured targets are posture-only; strict mode makes them deny-by-default.</p>
      </div>
    </section>

    <hr class="rule">

    <section aria-labelledby="h-props">
      <div class="container">
        <div class="section-head">
          <p class="eyebrow" data-reveal>Why OT teams trust it</p>
          <h2 id="h-props" data-reveal>Fail-closed, auditable, adversary-tested</h2>
        </div>
        <div class="grid grid--3">
          <div class="card" data-reveal>
            <h3>Deny on physics, not identity</h3>
            <p>The envelope check is on the decoded magnitude. A compromised-but-authenticated workstation, an
            insider, or a fat-fingered setpoint are all refused by the same rule — the one thing they have in
            common is a physically out-of-range value.</p>
            ${evRow("crates/kirra-industrial")}
          </div>
          <div class="card" data-reveal>
            <h3>A record regulators can verify</h3>
            <p>Every denied setpoint lands in a SHA-256 hash-chained ledger with the decoded value that was
            refused — tamper-evident, shippable off-box to WORM storage, and independently re-verifiable. The DNP3
            deny path is mandatory-audited by test.</p>
            ${evRow("src/audit_shipper.rs", "src/bin/kirra_verifier_service/dnp3_mandatory_audit_tests.rs")}
          </div>
          <div class="card" data-reveal>
            <h3>Hostile inputs are fuzzed</h3>
            <p>The decoders face malformed frames continuously — the DNP3 analog-setpoint decoder is one of four
            standing fuzz targets, run on every push and deep-fuzzed weekly. An undecodable payload is refused,
            never interpreted.</p>
            ${evRow("fuzz/fuzz_targets/dnp3_analog_setpoint.rs", ".github/workflows/fuzz-deep-weekly.yml")}
          </div>
          <div class="card" data-reveal>
            <h3>Posture across the plant</h3>
            <p>Assets form a dependency fabric: a faulted upstream asset degrades what depends on it, and
            industrial events feed the same fleet-posture engine that gates every command class across the site.</p>
            ${evRow("docs/multi_asset_fabric.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Sits in front of what you run</h3>
            <p>Kirra is a governor, not a replacement PLC or SCADA. It bounds the control write between the
            issuing layer and the actuator — your existing automation stays; it simply gains a physics-aware
            veto it can't talk its way past.</p>
            ${evRow("docs/adr/0020-doer-invariant-safety-case.md")}
          </div>
          <div class="card" data-reveal>
            <h3>Standards-aligned, honestly</h3>
            <p>The safety case maps to IEC 61508 SIL 3 and the run-time-assurance patterns OT regulators
            recognize — drafted in the open, versioned with the code. Independent assessment has not yet been
            performed, and we say so.</p>
            ${evRow("docs/safety/IEC_61508_SIL3_MAPPING.md", "certification.html")}
          </div>
        </div>
      </div>
    </section>

${ctaBand("Bound the setpoint. Keep the plant.", "Bring us your protocol, your targets, and your physical limits — we'll show you where Kirra's envelope sits in your control path today, and where it's roadmap.")}
`;
