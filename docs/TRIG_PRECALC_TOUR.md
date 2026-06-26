# Trig & Precalc in the Real World — a tour of a safety codebase

This repository is an **autonomous-vehicle / robot safety governor**: software whose
job is to stop a robot from issuing an unsafe motion command. The math in it isn't
decorative — every sine, tangent, and quadratic is *load-bearing*. If the trig is
wrong, the robot crashes. That makes it a nice place to see why a trig/precalc course
is actually about something.

Each section names the concept, shows the real code, and points to the file so you can
go read it. At the end there are **worked examples with real numbers** you can check by
hand.

---

## 1. Sine & cosine — splitting motion into x and y

The most-used idea in the whole repo. A robot moving at speed `v` with heading angle `θ`
moves a little East and a little North each instant:

> **ẋ = v·cos(θ)   ẏ = v·sin(θ)**

```rust
// crates/kirra-core/src/kinematics_sim.rs:139
new_x = x + v * psi.cos() * dt;
new_y = y + v * psi.sin() * dt;
```

This is the **unit circle** doing a job: cosine takes the horizontal share of the speed,
sine the vertical share. It shows up everywhere motion is predicted.

## 2. Tangent — how hard you can turn

```rust
// crates/kirra-core/src/kinematics_sim.rs:144
heading_rate = (v / wheelbase) * delta_rad.tan();   // θ̇ = (v/L)·tan(δ)
```

The steering angle `δ` controls how fast the heading changes through **tan(δ)** — which
is why steering gets dramatically more aggressive near 90° (tan races toward infinity).
The same `tan` bounds cornering force, `a_lat = v²·tan(δ)/L`; turn too hard at speed and
you'd skid, so the code rejects commands that exceed the limit.

## 3. The Pythagorean theorem — distance

Straight out of `a² + b² = c²`:

```rust
(obj.x - tr.x).hypot(obj.y - tr.y)     // distance between two points = √(Δx² + Δy²)
(x.powi(2) + y.powi(2)).sqrt()         // distance from the origin
```

`hypot` is literally "length of the hypotenuse." It answers: how far apart are two
objects, how fast is something going (`speed = √(vx² + vy²)`), how long is a path piece.

## 4. Inverse trig: atan2 — recovering an angle from the triangle

The reverse of #1: given the two legs, find the angle.

```rust
// "which way is this object heading?" from its velocity (vx, vy)
heading = vel.y.atan2(vel.x);
```

`atan2(y, x)` is the careful version of `arctan(y/x)` that knows all four quadrants (so it
can tell northeast from southwest). It recovers a heading toward a goal, a lane's
direction, and even pulls a compass heading out of a 3-D sensor orientation.

## 5. Radians, π, and "angle wrapping"

Angles are in **radians** (π = 180°) and they wrap around the circle, so the code folds
every angle back into the range (−π, π]:

```rust
// crates/kirra-taj/src/lib.rs:578
fn wrap_pi(a) { take a mod 2π, then shift into (−π, π] }
```

This is the unit-circle fact that θ and θ + 2π point the same way — needed so "turn left
10°" and "turn right 350°" come out equal. You'll also meet `FRAC_PI_2` (π/2), `TAU` (2π),
and a lidar scan that slices a half-circle into evenly spaced rays: `increment = π/(n−1)`.

## 6. Rotation matrices — spinning a shape by an angle

To check the rectangular robot fits inside its lane, the code rotates each corner by the
heading using the **rotation matrix** from precalc:

```rust
// crates/kirra-core/src/containment.rs:287
x' = x·cos(θ) − y·sin(θ)
y' = x·sin(θ) + y·cos(θ)
```

$$\begin{bmatrix}x'\\y'\end{bmatrix}=\begin{bmatrix}\cos\theta&-\sin\theta\\\sin\theta&\cos\theta\end{bmatrix}\begin{bmatrix}x\\y\end{bmatrix}$$

The *inverse* rotation (flip the minus sign) converts the world into the robot's own
"forward / sideways" frame, to decide whether an object is ahead of it or behind it.

## 7. Quadratics — why speed is dangerous

The heart of the safety math, and it's a **quadratic**: braking distance grows with the
**square** of speed.

```rust
// parko/crates/parko-core/src/rss.rs:233
d_brake    = v² / (2 · a);      // v²/(2a) — stopping distance
d_reaction = v·t + 0.5·a·t²;    // ½at² — the classic kinematics quadratic
```

Double your speed → **quadruple** your stopping distance. Every "is the gap safe?"
decision is built from these `v²/(2a)` and `½at²` pieces. There's even a literal use of
the **quadratic formula**, `(√((a·t)² + 2a·d) − a·t)`, to solve "how fast may I approach a
blind corner?"

## 8. Functions: parametric, piecewise, interpolation, min/max

- **Parametric equations** — a path as position vs. time: `(x,y) = (x₀,y₀) + t·(cos θ, sin θ)`.
- **Linear interpolation (lerp)** — `y = y₀ + t·(y₁ − y₀)`, the point-slope idea, to read a
  boundary's height between two known points.
- **Smoothstep** — a cubic `3t² − 2t³` for a smooth lane-change curve.
- **Piecewise min/max envelopes** — the speed limit is the *minimum* of several rules
  (cruise cap, curve cap, braking cap); the tightest wins:
  ```rust
  limit = limit.min((lat_accel / curvature).sqrt());
  ```

## 9. Coordinate geometry & vectors

- **Dot products** to project a point onto a line segment and get perpendicular distance —
  "how far is the robot from the edge of its lane?"
- **Normal vectors** (a 90° rotation, `(−dy, dx)/length`) to build lane boundaries offset
  from the centerline.
- **Point-in-polygon** ray casting to test whether the robot sits inside the drivable
  corridor.

## 10. Bonus — graphs & shortest paths

Not trig, but precalc-adjacent: the map is a **weighted graph**, and **Dijkstra's
algorithm** finds the cheapest route, pricing a lane-change higher (cost 3) than going
straight (cost 1) so the planner prefers to stay in its lane.

---

# Worked examples (real numbers from this codebase)

The robot here is a small sidewalk-courier: about **0.9 m long × 0.6 m wide**, top speed
**1.5 m/s**, guaranteed braking **1.0 m/s²**, reaction time **0.5 s**. Grab a calculator.

### A. Braking distance is a quadratic — `d = v²/(2a)`

At top speed `v = 1.5 m/s`, braking `a = 1.0 m/s²`:

$$d = \frac{v^2}{2a} = \frac{1.5^2}{2(1.0)} = \frac{2.25}{2} = 1.125\ \text{m}$$

Now **double** the speed to `3.0 m/s`:

$$d = \frac{3.0^2}{2(1.0)} = \frac{9}{2} = 4.5\ \text{m}$$

Speed ×2 → distance **×4**. That "×4" is the whole reason a quadratic, not a line, governs
safety. (This is the `v²/(2a)` term in `rss.rs`.)

### B. Full RSS following gap — a quadratic *plus* a linear piece

Before it can brake, the robot keeps moving for its reaction time `t = 0.5 s`, and may
even still be speeding up at `a = 1.0 m/s²`. Total distance to stop from `v = 1.5 m/s`:

- Reaction distance (½at² form): `v·t + ½·a·t² = 1.5(0.5) + 0.5(1.0)(0.5²) = 0.75 + 0.125 = 0.875 m`
- Speed when braking starts: `v_after = v + a·t = 1.5 + 1.0(0.5) = 2.0 m/s`
- Braking distance: `v_after²/(2a) = 2.0²/2 = 2.0 m`
- **Total ≈ 2.875 m**

So this little robot needs almost 3 m to stop from walking pace — the linear reaction term
plus the quadratic braking term, exactly as in `longitudinal_safe_distance`.

### C. atan2 + Pythagoras — heading and speed from a velocity vector

An object's velocity components are `(vx, vy) = (3, 4) m/s`.

- Speed (hypotenuse): `√(3² + 4²) = √25 = 5 m/s`  ← the 3-4-5 triangle!
- Heading: `atan2(4, 3) ≈ 0.927 rad ≈ 53.1°` north of east.

That's `hypot` and `atan2` in `prediction.rs`/`taj/lib.rs`, turning raw sensor numbers into
"how fast, and which way."

### D. A rotation matrix in action — 90° turns "forward" into "sideways"

Take the front-left corner of the robot in its own frame, `(x, y) = (0.45, 0.30)` (0.45 m
ahead of center, 0.30 m to the left). Rotate by heading `θ = 90°`, where `cos 90° = 0`,
`sin 90° = 1`:

$$x' = 0.45(0) - 0.30(1) = -0.30, \qquad y' = 0.45(1) + 0.30(0) = 0.45$$

The corner that pointed **forward-left** now points **left-and-slightly-back** — exactly
what you'd expect after turning the robot a quarter-turn. That's `footprint_corners` in
`containment.rs` deciding whether the *rotated* robot still fits the lane.

### E. Angle wrapping — why 350° and −10° are the same

A heading of `350°` is `6.108 rad`. Wrapping subtracts a full turn `2π = 6.283 rad`:

$$6.108 - 6.283 = -0.175\ \text{rad} = -10°$$

So "350° left" is stored as "10° right" — the *shortest* way around. Without this, the
robot might think a tiny turn is a near-full-circle spin. That's `wrap_pi`.

### F. Tangent — cornering force grows with speed² *and* steering

Lateral (sideways) acceleration in a turn is `a_lat = v²·tan(δ)/L`. With wheelbase
`L = 0.9 m`, speed `v = 1.5 m/s`, steering `δ = 20°` (`tan 20° ≈ 0.364`):

$$a_{lat} = \frac{1.5^2 \cdot 0.364}{0.9} = \frac{2.25 \cdot 0.364}{0.9} \approx 0.91\ \text{m/s}^2$$

Notice it scales with `v²` (quadratic) *and* `tan(δ)` (which blows up near 90°) — both your
units in one formula. The governor compares this to a skid limit and clamps the command if
it's too high (`kinematics_proptest.rs`).

---

## The one-line takeaway

Sine and cosine split motion into directions, the Pythagorean theorem measures distance,
`atan2` and tangent convert between angles and slopes, rotation matrices spin shapes, and
quadratics (`v²/2a`) are *why* a fast robot needs far more room to stop. In this codebase
it's all load-bearing: get the trig wrong and the robot hits something.
