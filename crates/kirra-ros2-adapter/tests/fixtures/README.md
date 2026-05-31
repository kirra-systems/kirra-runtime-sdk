# Lanelet2 test fixtures

These fixtures back the `lanelet2_tests` module in
`src/corridor/lanelet2.rs`. They are **not committed** because
generating them requires `python3-lanelet2` (i.e. the ROS environment
this crate's `ros2` feature already requires), and the binary format is
not portable across Boost versions
(see `OCCY_131_OPTIONB_DESIGN.md` §10).

## Required fixture

```
crates/kirra-ros2-adapter/tests/fixtures/straight_corridor.osm.bin
```

50 m straight corridor along +X, 4 m wide (2 m each side of centreline).
Two lanelets with IDs `1001` and `1002`, each ≥ 4 vertices per boundary,
sharing endpoint vertices so the dedup-on-join logic in
`extract_corridor` is exercised.

## Regenerate (one-shot)

With ROS sourced and lanelet2 installed:

```sh
sudo apt install -y python3-lanelet2 ros-${ROS_DISTRO}-lanelet2

cd crates/kirra-ros2-adapter/tests/fixtures

# 1. Author the OSM-XML source. The polylines below give a 50 m × 4 m
#    corridor split into two 25 m lanelets.
cat > straight_corridor.osm <<'OSM'
<?xml version='1.0' encoding='UTF-8'?>
<osm version='0.6' generator='kirra-ros2-adapter'>
  <node id='1'   visible='true' version='1' lat='0' lon='0' />
  <node id='2'   visible='true' version='1' lat='0' lon='0' />
  <!-- ... 12 nodes total; left/right polylines × 2 lanelets ... -->
  <way id='101'><nd ref='1'/><nd ref='2'/><nd ref='3'/><nd ref='4'/></way>
  <way id='102'><nd ref='5'/><nd ref='6'/><nd ref='7'/><nd ref='8'/></way>
  <relation id='1001'>
    <member type='way' ref='101' role='left' />
    <member type='way' ref='102' role='right' />
    <tag k='type'    v='lanelet' />
    <tag k='subtype' v='road'    />
    <tag k='speed_limit' v='50'  />
  </relation>
  <!-- analogous lanelet 1002 for the second 25 m segment ... -->
</osm>
OSM

# 2. Convert to binary via the Python lanelet2 bindings. The
#    serialization is via boost::archive::binary_oarchive — match the
#    boost version of the rest of the integrator's environment.
python3 - <<'PY'
import lanelet2
origin = lanelet2.io.Origin(0, 0)
proj   = lanelet2.projection.UtmProjector(origin)
m = lanelet2.io.load("straight_corridor.osm", proj)
lanelet2.io.write("straight_corridor.osm.bin", m, proj)
PY

# Resulting file is typically 4–8 KB.
ls -la straight_corridor.osm.bin
```

## When to regenerate

- Boost serialization version bump on the integrator's environment (the
  bin format is boost-version-pinned per the spike report §6.4).
- Lanelet2 schema changes (uncommon).
- Adding or removing fixtures (currently only the straight corridor).
