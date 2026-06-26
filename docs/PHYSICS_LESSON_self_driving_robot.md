# Your Physics Degree, Running 20 Times a Second

### How a self-driving delivery robot is built almost entirely out of the courses you're about to take

---

Somewhere right now a small delivery robot is rolling down a sidewalk. Twenty
times every second it asks itself a question: *"If I carry out the move I'm about
to make, will I hit anything?"* It has about five hundredths of a second to
answer. If it answers wrong, someone could get hurt.

That question is answered entirely with math and physics — and not exotic math.
It's the math and physics you're going to learn over the next two years, used for
real, with real stakes. This is a short tour of one such robot's safety software
(an actual codebase), organized around the courses on your degree plan. The point
is simple: the abstract things you're about to study are the *only* reason a
machine like this can be trusted around people. Let me show you.

---

## The big idea: a "doer" proposes, a "checker" bounds

First, the shape of the whole system, because it's a beautiful engineering idea
and it frames everything else.

The robot has two brains. One brain — call it the **doer** — is clever and
modern: it looks at camera and lidar data and *proposes* a motion ("go forward at
1.2 m/s, curving slightly left"). The doer might be a neural network; it's smart
but not trustable, because nobody can fully prove what a neural network will do.

So there's a second brain — the **checker** — and the checker is pure physics. It
takes the doer's proposal and asks, using equations you can write on paper,
*"is this physically safe?"* If yes, it passes the command through. If no, it
overrides it with a safe stop. The checker is simple enough that we can *prove* it
correct. The whole safety argument rests on it.

Here's the punchline of this essay: **the checker is your physics coursework.**
Every guarantee it makes is a theorem from mechanics. Let's walk through it.

---

## Classical Mechanics I — the robot's position is a kinematics problem

The very first thing you'll do in mechanics is describe motion: position,
velocity, the relationship between them. In 2D, an object moving at speed `v`
pointed in direction `θ` moves like this:

> ẋ = v·cos(θ)     ẏ = v·sin(θ)

That's it. That's the first week of mechanics — resolving a velocity into
components with sine and cosine. And it is *literally* the line of code the robot
runs to figure out where it's going:

```rust
new_x = x + v * cos(θ) * dt;
new_y = y + v * sin(θ) * dt;
```

When your professor draws a vector and drops dashed lines to the x- and y-axes,
they're drawing this. Cosine takes the part of the speed that goes "east," sine
takes the part that goes "north." The robot does this trig thousands of times a
minute to imagine where each possible move would take it. The unit circle you
memorized in trig isn't trivia — it's the dictionary that translates "how fast and
which way" into "where."

---

## Differential Equations + Computational Physics — the robot is an ODE solver

Notice the `dt` in that code — a tiny slice of time. The robot doesn't have a
formula for its whole future path; it has the *rule* for how position changes from
one instant to the next, and it steps that rule forward in little time-steps.

That is exactly what a **differential equation** is: a description of a system in
terms of its rates of change. "Velocity is the rate of change of position" is a
differential equation. And stepping it forward in small `dt` chunks is called
**Euler integration** — the first method you'll learn in Computational Physics for
solving an ODE you can't solve by hand.

The robot solves its own equations of motion numerically, 20 times a second, to
predict the next half-second of every candidate move:

```rust
for each timestep:
    heading += turn_rate * dt        // integrate the angle
    x       += v*cos(heading)*dt     // integrate the position
    y       += v*sin(heading)*dt
```

When you simulate a planet's orbit or a pendulum in a computational physics lab,
you'll write this identical loop. Here it's deciding whether a robot stays inside
its lane. Same math, higher stakes.

---

## The kinematic equations — *why speed is dangerous* (and it's a parabola)

Here's the most important equation in the whole safety system, and it's one you'll
derive in your first month of mechanics. Starting from constant acceleration, you
can show:

> v² = v₀² + 2aΔx

Rearranged for stopping distance (final velocity zero, deceleration `a`):

> **distance to stop = v² / (2a)**

Read that carefully: stopping distance depends on the **square** of speed. This is
the deep reason driving fast is dangerous, and it falls straight out of the
kinematic equations — or, if you like, out of the **work–energy theorem** you'll
meet a little later: kinetic energy is ½mv², the brakes do work (force × distance)
to remove that energy, so the distance needed scales with v².

Let's put real numbers on the robot, which brakes at about 1.0 m/s²:

- At 1.5 m/s (a brisk walk): stop distance = 1.5² / 2 = **1.13 m**
- Double the speed to 3.0 m/s: stop distance = 3.0² / 2 = **4.5 m**

Twice the speed, **four times** the distance. That "×4" is a parabola — the same
y = x² curve from precalc — and it's the single most safety-critical fact the
robot knows. Every "is the gap big enough?" decision is built on `v²/(2a)`. When
your mechanics professor says "the kinematic equations describe constant
acceleration," this is the sentence standing between a robot and a collision.

---

## Circular motion — why it can't corner at any speed it likes

When the robot turns, it's moving on the arc of a circle, and you'll learn that
circular motion requires a **centripetal acceleration** pointing inward:

> a = v² / r

For the robot's steering geometry this works out to `a_lat = v²·tan(δ)/L`, where
`δ` is the steering angle and `L` is its wheelbase. Two things from your courses
live in that one expression:

- the **v²** of circular motion (go twice as fast around the same curve and you
  need four times the sideways grip), and
- the **tangent** from trig, which explains why a sharp steering angle is so much
  more violent than a gentle one — tan(δ) shoots toward infinity as δ approaches
  90°.

The robot computes this sideways acceleration for every proposed turn and compares
it to the grip its wheels can provide (which, when you take friction in mechanics,
you'll write as `a_max = μg`). Too much, and it would skid — so the checker refuses
the command. That's a friction problem from Chapter 5 of your mechanics book,
deciding in real time whether a robot keeps its footing.

---

## Linear Algebra — reference frames, and the most useful matrix you'll meet

The robot constantly switches between two points of view: the **world frame**
(fixed to the ground) and its own **body frame** (forward / sideways, riding along
with it). Translating between them is the job of a **rotation matrix**, which
you'll meet in Linear Algebra and then again, constantly, in mechanics:

```
[ x' ]   [ cos θ   -sin θ ] [ x ]
[ y' ] = [ sin θ    cos θ ] [ y ]
```

To check whether its rectangular body fits inside the lane, the robot takes the
four corners of its footprint, **rotates** them by its current heading, and tests
whether the rotated rectangle stays between the lane edges. Multiply a corner by
that matrix and a point that was "front-left" swings around to wherever the robot
is actually pointing.

This idea — that the same physical situation looks different from different frames,
and that a clean transformation connects them — is one of the most important in all
of physics. It's the seed of **Galilean relativity** (and later, Einstein's). The
reason the robot can reason about obstacles using a lidar that only sees in its own
body frame, *without* needing perfect GPS, is precisely that it can transform
between frames. A self-driving robot is a working monument to "choose the right
reference frame and the problem gets easy."

And a preview: orientation in full 3D (which way an object is tipped, not just
turned) is handled with **quaternions** — a four-number cousin of rotation
matrices that avoids a nasty failure called "gimbal lock." You'll meet those in
upper-level mechanics and graphics. The robot uses them to read its tilt sensor.
When you get there, you'll recognize an old friend.

---

## Vectors and Vector Calculus — the language underneath all of it

Threaded through everything is **vectors**: position (x, y), velocity (vₓ, v_y),
the directions to obstacles. The robot leans on the vector operations you'll drill
in Linear Algebra and Vector Calculus:

- **Magnitude** (the Pythagorean theorem): an object's speed from its components is
  √(vₓ² + v_y²). If a sensor reports velocity (3, 4), the speed is √(9+16) = 5 —
  the 3-4-5 triangle, doing real work.
- **The dot product**, to project an obstacle's position onto the robot's direction
  of travel and ask "how far ahead is it, and how far off to the side?"
- **Normal vectors** (perpendiculars), to build the left and right edges of a lane
  by stepping sideways from its centerline.

Physics is written in the language of vectors. This project is a paragraph in that
language.

---

## Probability & Measurement — because the real world is noisy

Here's something they don't always tell you early: in the real world, *every
measurement is wrong by a little.* Sensors jitter. Lidar returns are noisy. A core
part of a physics education — error analysis in your labs, and later a full
Probability & Statistics course — is learning to reason carefully about
uncertainty instead of pretending it isn't there.

The robot is built around this humility. When perception is missing or stale, it
doesn't *guess* the road is clear — it assumes the worst and slows or stops. This
is a design rule called **"fail closed,"** and it's really just measurement
honesty turned into a safety policy: *never treat the absence of evidence as
evidence of safety.* Your lab courses are training exactly this instinct.

---

## Where it's all heading — Control Theory and Dynamical Systems

Step back and look at the whole loop: the robot senses the world, predicts the
near future with its equations of motion, checks the prediction against physical
limits, and acts — then does it again, 20 times a second. A system that
continuously measures, predicts, and corrects is the subject of **Control Theory
and Dynamical Systems**, some of the most beautiful applied physics there is. The
"doer proposes / checker bounds" design is a safety architecture built on top of
it. You're looking at where the mechanics and the differential equations and the
linear algebra all come together into a system that *does* something in the world.

---

## Why this matters for you

Here's the thing I most want you to take away.

In a textbook, a problem ends when you box your answer. Out here, the answer
*becomes* something — a robot that either does or doesn't stop in time. The
difference between "my code usually works" and "I can *prove* this machine won't
hurt someone" is exactly the difference between hand-waving and a real physics
argument. The braking equation isn't a step toward a grade; it's the reason a
distance is safe. The rotation matrix isn't an exercise; it's how the machine
knows where its own corners are.

People sometimes ask physics majors, "but what is that *for*?" Here's one honest
answer: the equations you're about to learn are the most powerful tools humans have
for making **guarantees** about the physical world. Bridges that won't fall.
Spacecraft that arrive. Robots that stop. Every one of those guarantees is someone
taking the math seriously enough to be sure.

You're about to learn how to be that person. The robot on the sidewalk is already
counting on people who did. Welcome — it's a good field, and the world genuinely
needs you in it.

---

*Everything above is drawn from a real autonomous-robot safety codebase: the
position updates, the v²/(2a) braking checks, the rotation-matrix containment
tests, the centripetal-acceleration skid limits, and the fail-closed handling of
noisy sensors are all running code. If you ever want to see one of these as actual
equations-to-software, every example here points back to a specific file you can
read.*
