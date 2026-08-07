#!/usr/bin/env python3
"""Tests for the Kirra World bidirectional fence.

Every case builds a THROWAWAY fixture workspace in a temp directory and runs the
real checker against it. Nothing here needs ROS, a network, cargo, or actuator
hardware.

Two properties are being established, and the negative half matters more:

  * the fence goes RED on each forbidden shape (a gate that cannot fail proves
    nothing);
  * the fence stays GREEN on the shapes the architecture requires — a product
    consumer reading Kirra World, Occy depending on it to plan, prose that
    merely mentions the forbidden symbols. A fence that blocks the intended
    design would simply be switched off.

Run:  python3 ci/test_kirra_world_bidirectional_fence.py
"""

from __future__ import annotations

import shutil
import sys
import tempfile
import textwrap
from pathlib import Path

CI = Path(__file__).resolve().parent
sys.path.insert(0, str(CI))

import importlib.util

_spec = importlib.util.spec_from_file_location(
    "fence", CI / "check_kirra_world_bidirectional_fence.py"
)
fence = importlib.util.module_from_spec(_spec)
sys.modules["fence"] = fence
_spec.loader.exec_module(fence)

TODAY = "2026-08-02"

# A minimal but honest allowlist for fixtures: the checker requires one to
# exist, and Check 7 validates its shape.
BASE_ALLOWLIST = """
[[safety_authoritative_impl]]
trait = "CorridorSource"
type = "MapCorridor"
file = "crates/kirra-map/src/lib.rs"
rationale = "derived from the lane map the safety path reads itself"
adr = "ADR-0039"
owner = "test-owner"
"""


# ---------------------------------------------------------------------------
# Fixture construction
# ---------------------------------------------------------------------------


class Fixture:
    def __init__(self) -> None:
        self.root = Path(tempfile.mkdtemp(prefix="kirra-fence-"))
        (self.root / "ci").mkdir(parents=True)
        self.allowlist(BASE_ALLOWLIST)

    def crate(self, name: str, deps: list[str] | None = None, dev: list[str] | None = None) -> Path:
        d = self.root / "crates" / name
        (d / "src").mkdir(parents=True, exist_ok=True)
        lines = [f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n']
        lines.append("[dependencies]")
        for dep in deps or []:
            lines.append(f'{dep} = {{ path = "../{dep}" }}')
        lines.append("\n[dev-dependencies]")
        for dep in dev or []:
            lines.append(f'{dep} = {{ path = "../{dep}" }}')
        (d / "Cargo.toml").write_text("\n".join(lines) + "\n")
        (d / "src" / "lib.rs").write_text("// fixture crate\n")
        return d

    def rust(self, rel: str, body: str) -> None:
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(textwrap.dedent(body))

    def py(self, rel: str, body: str) -> None:
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(textwrap.dedent(body))

    def unit(self, rel: str, body: str) -> None:
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(textwrap.dedent(body))

    def allowlist(self, body: str) -> None:
        (self.root / fence.ALLOWLIST_NAME).write_text(textwrap.dedent(body))

    def run(self):
        return fence.run(self.root, TODAY)

    def close(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)


def base_safety_workspace(fx: Fixture) -> None:
    """A miniature but structurally faithful safety path."""
    fx.crate("kirra-core")
    fx.crate("kirra-trajectory", deps=["kirra-core"])
    fx.crate("kirra-release-token", deps=["kirra-core"])
    fx.crate("kirra-safety-authority", deps=["kirra-core"])
    fx.crate("kirra-actuation-consumer", deps=["kirra-release-token"])
    fx.crate("kirra-map", deps=["kirra-core"])
    fx.rust(
        "crates/kirra-map/src/lib.rs",
        """
        pub struct MapCorridor;
        impl CorridorSource for MapCorridor {
            fn contains(&self) -> bool { true }
        }
        """,
    )


# ---------------------------------------------------------------------------
# Assertions
# ---------------------------------------------------------------------------

RESULTS: list[tuple[bool, str, str]] = []


def record(ok: bool, name: str, detail: str = "") -> None:
    RESULTS.append((ok, name, detail))


def expect_intact(fx: Fixture, name: str) -> None:
    rep = fx.run()
    if rep.violations:
        record(False, name, "expected INTACT, got:\n" + "\n".join(v.render() for v in rep.violations))
    else:
        record(True, name)


def expect_breach(fx: Fixture, name: str, fence_letter: str, needle: str) -> None:
    rep = fx.run()
    hits = [v for v in rep.violations if v.fence == fence_letter and needle.lower() in
            (v.detail + v.location + v.check).lower()]
    if hits:
        record(True, name)
    else:
        got = "\n".join(v.render() for v in rep.violations) or "  (no violations at all)"
        record(False, name, f"expected Fence {fence_letter} breach matching {needle!r}, got:\n{got}")


# ===========================================================================
# PASSING CASES — the architecture working as designed
# ===========================================================================


def t01_no_world_crate_yet() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        expect_intact(fx, "01 no Kirra World crate exists yet")
        rep = fx.run()
        assert any("not present yet" in n for n in rep.notes), rep.notes
    finally:
        fx.close()


def t02_product_mick_reads_world() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.crate("kirra-world")
        fx.crate("kirra-mick", deps=["kirra-world"])  # product side, outside the closure
        expect_intact(fx, "02 product-side Mick adapter reads Kirra World")
    finally:
        fx.close()


def t03_occy_depends_on_world() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.crate("kirra-world")
        fx.crate("kirra-planner", deps=["kirra-world", "kirra-core"])
        # The safety root test-harnesses against the planner, as it really does.
        fx.crate("kirra-trajectory", deps=["kirra-core"], dev=["kirra-planner"])
        expect_intact(fx, "03 Occy depends on Kirra World to generate a proposal")
    finally:
        fx.close()


def t04_shared_allowlisted_map() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.crate("kirra-world")
        fx.allowlist(
            BASE_ALLOWLIST
            + """
        [[shared_source_artifact]]
        artifact = "maps/town.osm"
        world_consumer = "kirra-world"
        safety_consumer = "kirra-map"
        independent_validation = "each parses the file separately; no derived structure crosses"
        adr = "ADR-0042"
        owner = "test-owner"
        reason = "a byte-identical input read twice is not a runtime dependency"
        """
        )
        expect_intact(fx, "04 checker and Kirra World independently read an allowlisted source map")
    finally:
        fx.close()


def t05_comments_and_docs_mention_symbols() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.rust(
            "crates/kirra-trajectory/src/lib.rs",
            """
            //! This crate must never use kirra_world::SemanticPlace, must never
            //! publish cmd_vel, and must never read /var/lib/kirra/world/.
            /* KIRRA_WORLD_URL is reserved and must not appear in safety code. */
            pub fn validate() -> bool { true }
            """,
        )
        expect_intact(fx, "05 comments and docs mention forbidden symbols")
    finally:
        fx.close()


def t06_test_fixture_string_cmd_vel() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.crate("kirra-world")
        fx.py(
            "robot/kirra_world_fence_selftest.py",
            '''
            """A test that asserts the fence catches cmd_vel publication."""
            EXPECTED_VIOLATION_DESCRIPTION = "a module that publishes to the topic"
            def test_描述(): pass
            ''',
        )
        expect_intact(fx, "06 test fixture describes cmd_vel without publishing it")
    finally:
        fx.close()


# ===========================================================================
# FENCE A FAILURES — Kirra World reaching actuation
# ===========================================================================


def t07_world_depends_on_release_token() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.crate("kirra-world", deps=["kirra-release-token"])
        expect_breach(fx, "07 kirra-world depends on the release-token crate", "A", "kirra-release-token")
    finally:
        fx.close()


def t08_world_python_publishes_cmd_vel() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.py(
            "robot/kirra_world_bridge.py",
            """
            TOPIC = "cmd_vel"
            def publish(node):
                node.create_publisher(TOPIC)
            """,
        )
        expect_breach(fx, "08 Kirra World Python module publishes cmd_vel", "A", "cmd_vel")
    finally:
        fx.close()


def t09_world_unit_calls_motor_consumer() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.unit(
            "deploy/systemd/kirra-world.service",
            """
            [Unit]
            Description=Kirra World
            Requires=kirra-consumer.service

            [Service]
            ExecStart=/usr/bin/python3 /opt/kirra/robot/kirra_motor_consumer.py
            """,
        )
        expect_breach(fx, "09 Kirra World systemd unit calls a motor consumer", "A", "actuation")
    finally:
        fx.close()


def t10_world_python_opens_actuator_device() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.py(
            "robot/kirra_world_store.py",
            """
            PORT = "/dev/myserial"
            def drive():
                return open(PORT, "wb")
            """,
        )
        expect_breach(fx, "10 Kirra World opens an actuator device", "A", "/dev/myserial")
    finally:
        fx.close()


# ===========================================================================
# FENCE B FAILURES — the safety path depending on Kirra World
# ===========================================================================


def t11_checker_depends_on_world() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.crate("kirra-world")
        fx.crate("kirra-trajectory", deps=["kirra-core", "kirra-world"])
        expect_breach(fx, "11 checker directly depends on kirra-world", "B", "kirra-world")
    finally:
        fx.close()


def t12_core_transitively_depends_on_world() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.crate("kirra-world")
        fx.crate("kirra-neutral-util", deps=["kirra-world"])
        fx.crate("kirra-core", deps=["kirra-neutral-util"])
        expect_breach(fx, "12 kirra-core transitively depends on kirra-world", "B", "kirra-world")
    finally:
        fx.close()


def t13_shared_neutral_crate_hidden_edge() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.crate("kirra-world-store")
        fx.crate("kirra-shared-helpers", deps=["kirra-world-store"])
        fx.crate("kirra-safety-authority", deps=["kirra-core", "kirra-shared-helpers"])
        expect_breach(fx, "13 shared neutral crate introduces a hidden Kirra World dependency",
                      "B", "kirra-world-store")
    finally:
        fx.close()


def t14_python_consumer_imports_world() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.py(
            "robot/kirra_motor_consumer.py",
            """
            import kirra_world
            def drive():
                return kirra_world.query()
            """,
        )
        expect_breach(fx, "14 Python verifying consumer imports Kirra World", "B", "kirra_world")
    finally:
        fx.close()


def t15_safety_unit_gets_world_url() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.unit(
            "robot/install/systemd/kirra-consumer.service",
            """
            [Service]
            Environment=KIRRA_WORLD_URL=http://127.0.0.1:8500
            ExecStart=/usr/bin/python3 /opt/kirra/robot/kirra_motor_consumer.py
            """,
        )
        expect_breach(fx, "15 safety systemd unit receives KIRRA_WORLD_URL", "B", "KIRRA_WORLD_URL")
    finally:
        fx.close()


def t16_safety_code_opens_world_db() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.rust(
            "crates/kirra-trajectory/src/store.rs",
            """
            pub fn open() -> String {
                String::from("/var/lib/kirra/world/world.sqlite")
            }
            """,
        )
        expect_breach(fx, "16 safety code opens the Kirra World database", "B", "/var/lib/kirra/world")
    finally:
        fx.close()


def t17_public_api_exposes_world_type() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.rust(
            "crates/kirra-trajectory/src/api.rs",
            """
            pub fn validate(input: kirra_world::SemanticPlace) -> bool {
                let _ = input;
                true
            }
            """,
        )
        expect_breach(fx, "17 public safety API exposes a Kirra World type", "B", "kirra world type")
    finally:
        fx.close()


# ===========================================================================
# TRAIT / ADAPTER CASES — the disguised edge
# ===========================================================================


def t18_impl_corridor_for_worldmodel() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.rust(
            "crates/kirra-map/src/world_adapter.rs",
            """
            pub struct WorldModelCorridor;
            impl CorridorSource for WorldModelCorridor {
                fn contains(&self) -> bool { true }
            }
            """,
        )
        expect_breach(fx, "18 impl CorridorSource for WorldModelCorridor", "B", "adapter marker")
    finally:
        fx.close()


def t19_unknown_impl_fails_until_reviewed() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.rust(
            "crates/kirra-map/src/fresh.rs",
            """
            pub struct BrandNewCorridor;
            impl CorridorSource for BrandNewCorridor {
                fn contains(&self) -> bool { true }
            }
            """,
        )
        expect_breach(fx, "19 new unknown CorridorSource impl fails until reviewed",
                      "B", "not in ci/kirra_world_fence_allowlist.toml")
    finally:
        fx.close()


def t20_approved_impl_passes_with_allowlist() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.rust(
            "crates/kirra-map/src/fresh.rs",
            """
            pub struct MapDerivedCorridor;
            impl CorridorSource for MapDerivedCorridor {
                fn contains(&self) -> bool { true }
            }
            """,
        )
        fx.allowlist(
            BASE_ALLOWLIST
            + """
        [[safety_authoritative_impl]]
        trait = "CorridorSource"
        type = "MapDerivedCorridor"
        file = "crates/kirra-map/src/fresh.rs"
        rationale = "built from the lane map the safety path validates independently"
        adr = "ADR-0039"
        owner = "test-owner"
        """
        )
        expect_intact(fx, "20 approved independent map-based impl passes with allowlist evidence")
    finally:
        fx.close()


def t21_neutral_name_but_world_dependency() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.crate("kirra-world")
        # Neutral crate + neutral type name -- only the dependency edge betrays it.
        fx.crate("kirra-terrain", deps=["kirra-world", "kirra-core"])
        fx.rust(
            "crates/kirra-terrain/src/lib.rs",
            """
            pub struct TerrainCorridor;
            impl CorridorSource for TerrainCorridor {
                fn contains(&self) -> bool { true }
            }
            """,
        )
        expect_breach(fx, "21 adapter with neutral name but Kirra World dependency",
                      "B", "depends on a kirra-world")
    finally:
        fx.close()


# ===========================================================================
# ALLOWLIST HYGIENE
# ===========================================================================


def t22_missing_rationale_fails() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.allowlist(
            """
        [[safety_authoritative_impl]]
        trait = "CorridorSource"
        type = "MapCorridor"
        file = "crates/kirra-map/src/lib.rs"
        adr = "ADR-0039"
        owner = "test-owner"
        """
        )
        expect_breach(fx, "22 allowlist entry missing rationale fails", "B", "missing required field `rationale`")
    finally:
        fx.close()


def t23_missing_adr_fails() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.allowlist(
            """
        [[safety_authoritative_impl]]
        trait = "CorridorSource"
        type = "MapCorridor"
        file = "crates/kirra-map/src/lib.rs"
        rationale = "because"
        owner = "test-owner"
        """
        )
        expect_breach(fx, "23 allowlist entry missing ADR reference fails", "B", "missing required field `adr`")
    finally:
        fx.close()


def t24_expired_entry_fails() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.allowlist(
            BASE_ALLOWLIST
            + """
        [[safety_authoritative_impl]]
        trait = "CorridorSource"
        type = "TemporaryCorridor"
        file = "crates/kirra-map/src/temp.rs"
        rationale = "interim during migration"
        adr = "ADR-0039"
        owner = "test-owner"
        expires = "2020-01-01"
        """
        )
        expect_breach(fx, "24 expired allowlist entry fails", "B", "expired")
    finally:
        fx.close()


def t25_duplicate_entry_fails() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.allowlist(BASE_ALLOWLIST + BASE_ALLOWLIST)
        expect_breach(fx, "25 duplicate allowlist entry fails", "B", "duplicate entry")
    finally:
        fx.close()


# ===========================================================================
# ERROR QUALITY
# ===========================================================================


def t26_error_output_quality() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.crate("kirra-world")
        fx.crate("kirra-trajectory", deps=["kirra-core", "kirra-world"])
        rep = fx.run()
        v = next((x for x in rep.violations if "kirra-world" in x.detail), None)
        if v is None:
            record(False, "26 failure output names fence/location/path/ADR/repair", "no violation produced")
            return
        problems = []
        if v.fence not in {"A", "B"}:
            problems.append("no fence letter")
        if "kirra-trajectory" not in v.location and "kirra-world" not in v.location:
            problems.append(f"location unhelpful: {v.location}")
        if "->" not in v.detail:
            problems.append("no dependency path in detail")
        if "ADR" not in v.adr:
            problems.append("no ADR reference")
        if len(v.repair) < 20:
            problems.append("no repair direction")
        record(not problems, "26 failure output names fence/location/path/ADR/repair", "; ".join(problems))
    finally:
        fx.close()


# ===========================================================================
# The real repository must be intact, and the allowlist must parse.
# ===========================================================================


def _map_sharing_fixture(fx: Fixture, with_world: bool = True) -> None:
    """A safety-side map consumer that is a REAL package, optionally shared.

    `kirra-ros2-adapter` must be created as a crate, not just written into: a
    directory with no Cargo.toml is not a package, so it never enters the safety
    closure and no pairing can be detected. Getting this wrong made three of the
    tests below pass vacuously — they asserted "no breach" against a fixture
    incapable of producing one.
    """
    fx.crate("kirra-ros2-adapter", deps=["kirra-core"])
    fx.rust("crates/kirra-ros2-adapter/src/lib.rs", 'pub fn load() { let _ = "town.osm"; }\n')
    if with_world:
        fx.crate("kirra-world")
        fx.rust("crates/kirra-world/src/lib.rs", 'pub fn places() { let _ = "town.osm"; }\n')


def t30_shared_artifact_without_allowlist_fails() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        _map_sharing_fixture(fx)
        expect_breach(fx, "30 Kirra World and safety share an artifact with no allowlist entry",
                      "B", "lanelet2-osm-map")
    finally:
        fx.close()


def t31_shared_artifact_with_allowlist_passes() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        _map_sharing_fixture(fx)
        fx.allowlist(
            BASE_ALLOWLIST
            + """
        [[shared_source_artifact]]
        artifact = "lanelet2-osm-map"
        world_consumer = "kirra-world"
        safety_consumer = "kirra-ros2-adapter"
        independent_validation = "each parses the OSM document separately; no parsed or derived structure crosses between them"
        adr = "ADR-0042"
        owner = "test-owner"
        reason = "a byte-identical input read twice is not a runtime dependency"
        """
        )
        expect_intact(fx, "31 the same sharing passes once reviewed and allowlisted")
    finally:
        fx.close()


def t32_safety_only_consumer_is_not_sharing() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        # The safety path reads the map; nobody from Kirra World does. That is
        # the situation today and it is not sharing.
        _map_sharing_fixture(fx, with_world=False)
        expect_intact(fx, "32 a safety-only artifact consumer is not a shared artifact")
    finally:
        fx.close()


def t33_comment_mention_is_not_a_consumer() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        _map_sharing_fixture(fx)  # a REAL breach exists, so the run is non-vacuous
        # kirra-core only TALKS about Lanelet2; it does not read it. Counting
        # prose as consumption put two crates on the real baseline that read
        # nothing.
        fx.rust(
            "crates/kirra-core/src/notes.rs",
            "// The Lanelet2 cxx bridge stays in the adapter, deliberately not here.\n",
        )
        rep = fx.run()
        shared = [v for v in rep.violations if "shared source artifact" in v.check]
        named_core = [v for v in shared if "kirra-core" in v.location]
        problems = []
        if not shared:
            problems.append("the planted real sharing should still breach — test is vacuous otherwise")
        if named_core:
            problems.append("kirra-core was counted as a consumer on the strength of a comment")
        record(not problems, "33 a comment mentioning the artifact is not a consumer",
               "; ".join(problems))
    finally:
        fx.close()


def t27_real_repo_intact() -> None:
    repo = CI.parent
    rep = fence.run(repo, TODAY)
    record(
        not rep.violations,
        "27 the real repository passes the fence",
        "\n".join(v.render() for v in rep.violations),
    )


def t28_real_allowlist_valid() -> None:
    repo = CI.parent
    rep = fence.Report()
    allow = fence.load_allowlist(repo, rep)
    fence.check_7_allowlist(repo, allow, TODAY, rep)
    problems = [v.render() for v in rep.violations]
    entries = allow.get("safety_authoritative_impl", [])
    if not entries:
        problems.append("  the shipped allowlist has no impl entries — it should record the real ones")
    record(not problems, "28 the shipped allowlist parses and validates", "\n".join(problems))


def t29_real_corridor_impls_are_covered() -> None:
    """Every production CorridorSource impl in the repo must be reviewed."""
    repo = CI.parent
    rep = fence.Report()
    man = fence.find_manifests(repo)
    allow = fence.load_allowlist(repo, rep)
    found = fence.check_4_trait_impls(repo, man, allow, rep)
    prod = [f for f in found if not f["test"]]
    unreviewed = [v for v in rep.violations if "not in ci/" in v.detail]
    record(
        bool(prod) and not unreviewed,
        f"29 all {len(prod)} production CorridorSource impls are reviewed",
        "\n".join(v.render() for v in unreviewed),
    )



# ---------------------------------------------------------------------------
# Fence A coverage for Kirra World work that is outside the kirra-world*
# namespace (FENCE_A_EXTRA_PACKAGES).
# ---------------------------------------------------------------------------


def _extra_package(fx: Fixture, deps: list[str] | None = None, ext: list[str] | None = None) -> None:
    """The WM-2 harness's real shape: a workspace-DETACHED crate under tools/.

    Written under `tools/` rather than `crates/` on purpose. The checker finds
    manifests by rglob, so the location does not change the result today — but a
    fixture that puts the package where the real one is not would stop
    representing it the moment location ever starts to matter.
    """
    name = next(iter(fence.FENCE_A_EXTRA_PACKAGES))
    d = fx.root / "tools" / name
    (d / "src").mkdir(parents=True, exist_ok=True)
    lines = [f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n', "[workspace]\n", "[dependencies]"]
    for dep in deps or []:
        lines.append(f'{dep} = {{ path = "../../crates/{dep}" }}')
    for dep in ext or []:
        lines.append(f'{dep} = "1"')
    (d / "Cargo.toml").write_text("\n".join(lines) + "\n")
    (d / "src" / "main.rs").write_text("fn main() {}\n")


def t34_extra_package_with_an_actuation_transport_breaches() -> None:
    """The hole this closes: the harness is not named kirra-world*, so before
    FENCE_A_EXTRA_PACKAGES existed a serialport dependency added to it passed."""
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        _extra_package(fx, ext=["serialport"])
        expect_breach(fx, "34 an extra Fence A package taking a transport crate breaches",
                      "A", "serialport")
    finally:
        fx.close()


def t35_extra_package_reaching_an_actuation_crate_breaches() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        # Transitive, not direct: harness -> kirra-actuation-consumer is the
        # obvious edge; harness -> a crate that depends on it is the one that
        # gets argued for.
        fx.crate("bench-helper", deps=["kirra-actuation-consumer"])
        _extra_package(fx, deps=["bench-helper"])
        expect_breach(fx, "35 an extra Fence A package reaching actuation transitively breaches",
                      "A", "kirra-actuation-consumer")
    finally:
        fx.close()


def t36_extra_package_with_only_benign_deps_is_intact() -> None:
    """The required-pass half: the real harness depends on rusqlite alone."""
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        _extra_package(fx, ext=["rusqlite"])
        expect_intact(fx, "36 an extra Fence A package with only storage deps is intact")
    finally:
        fx.close()


def t37_a_vanished_extra_package_is_noted_not_silently_dropped() -> None:
    fx = Fixture()
    try:
        base_safety_workspace(fx)  # deliberately does NOT create the extra package
        rep = fx.run()
        noted = [n for n in rep.notes if "extra Fence A package not found" in n]
        record(bool(noted),
               "37 a configured extra Fence A package that vanished is reported",
               "no note emitted; the fence would have shrunk silently")
    finally:
        fx.close()


def t38_the_real_harness_is_actually_covered() -> None:
    """Against the REAL repository, not a fixture: the shipped harness must be
    present and inside Fence A. A config entry naming a package that does not
    exist protects nothing."""
    repo = CI.parent
    man = fence.find_manifests(repo)
    missing = [n for n in fence.FENCE_A_EXTRA_PACKAGES if n not in man]
    record(not missing,
           f"38 all {len(fence.FENCE_A_EXTRA_PACKAGES)} configured extra Fence A package(s) exist",
           f"not found in the repository: {missing}")

def t39_a_perceived_object_import_path_breaches() -> None:
    """The gate must FIRE. A check that cannot fail is not a guard.

    ADR-0040 forbids building a `PerceivedObject` import path until a stated
    rule exists for where its confidence and validity come from.
    """
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.crate("kirra-world")
        fx.rust(
            "crates/kirra-world/src/import.rs",
            """
            pub fn ingest(o: PerceivedObject) -> Claim {
                Claim::from(o)
            }
            """,
        )
        rep = fx.run()
        hits = [v for v in rep.violations if "PerceivedObject" in v.check]
        record(
            bool(hits),
            "39 a PerceivedObject import path breaches Fence A",
            "the ADR-0040 condition is not enforced -- the check never fired",
        )
    finally:
        fx.close()


def t40_prose_about_perceived_object_is_not_an_import_path() -> None:
    """Describing the rule is not breaking it.

    `observation.rs` quotes this exact condition in a doc comment, so a check
    that counted prose would red the real repository for explaining itself.
    """
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.crate("kirra-world")
        body = (
            "//! ADR-0040 forbids a PerceivedObject import path until the rule exists.\n"
            "/// See the PerceivedObject condition.\n"
            'pub const WHY: &str = "PerceivedObject has no confidence field";\n'
        )
        fx.rust("crates/kirra-world/src/notes.rs", body)
        rep = fx.run()
        hits = [v for v in rep.violations if "PerceivedObject" in v.check]
        record(
            not hits,
            "40 prose about PerceivedObject is not an import path",
            "a doc comment or string literal was counted as a breach",
        )
    finally:
        fx.close()


def t41_the_condition_is_scoped_to_world_packages() -> None:
    """Scope asserted, not implied.

    The check sees `kirra-world*` only. An importer built elsewhere is outside
    what it can detect, and the test says so rather than letting a reader infer
    total coverage from a green run.
    """
    fx = Fixture()
    try:
        base_safety_workspace(fx)
        fx.crate("kirra-world")
        # A non-world package using the type is ordinary -- the safety path is
        # where PerceivedObject legitimately lives.
        fx.rust(
            "crates/kirra-trajectory/src/rss.rs",
            """
            pub fn check(o: &PerceivedObject) -> bool { true }
            """,
        )
        rep = fx.run()
        hits = [v for v in rep.violations if "PerceivedObject" in v.check]
        record(
            not hits,
            "41 the PerceivedObject condition is scoped to kirra-world*",
            "a non-world package was flagged; the condition binds Kirra World only",
        )
    finally:
        fx.close()


ALL = [v for k, v in sorted(globals().items()) if k.startswith("t") and k[1:3].isdigit()]


def main() -> int:
    for fn in ALL:
        try:
            fn()
        except Exception as exc:  # a crashing test is a failing test
            record(False, fn.__name__, f"raised {type(exc).__name__}: {exc}")

    failed = [r for r in RESULTS if not r[0]]
    for ok, name, detail in RESULTS:
        print(f"  {'PASS' if ok else 'FAIL'}  {name}")
        if not ok and detail:
            print(textwrap.indent(detail, "        "))
    print(f"\n{len(RESULTS) - len(failed)}/{len(RESULTS)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
