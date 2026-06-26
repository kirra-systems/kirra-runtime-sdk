# The Math That Keeps a Robot From Hitting You

### I started writing this for my daughter, a brand-new physics major. It turned into a love letter to the equations running under a sidewalk delivery robot — twenty times a second.

---

My daughter just started as a physics major. She's in the thick of the
build-up courses right now — trig, precalc, the stuff that can feel like a long
hallway of symbols before you get to the rooms they open. She asked me, more or
less, the question every student eventually asks: *what is all this actually
for?*

I happen to work near some code that's a pretty good answer. So I started writing
her a note. The note got away from me, and this is what it became. I'm sharing it
because I don't think you need to be a physics major — or remember a single
formula — to enjoy what's underneath one of those little delivery robots you've
probably seen trundling down a sidewalk. You just have to be willing to be a
little charmed by how much careful thinking goes into a machine *not* bumping into
you.

Here we go.

---

## A decision, every twentieth of a second

Picture one of those knee-high robots rolling along a sidewalk with someone's
lunch inside. Twenty times every second, it asks itself one question:

> *If I do the thing I'm about to do, will I hit anything?*

It has about five-hundredths of a second to answer, and it has to be right. Get it
wrong and a person gets hurt.

What I find quietly amazing is that the answer is *not* magic, and it's *not* some
inscrutable AI black box. The answer is math — and not even fancy math. It's the
math from school, the stuff that can seem so abstract, suddenly load-bearing. If
the math is wrong, the robot crashes. That's a sentence that turns "why do we
learn this?" into something with real weight.

Let me show you the good parts.

## Two brains: the clever one and the careful one

The robot has two brains, and the split is the whole trick.

The first brain is the clever one. It looks at the camera and laser data and
*proposes* a move: "roll forward at walking pace, curve a little left." This brain
is often a neural network — modern, capable, and, crucially, *not fully
trustworthy.* Nobody can completely prove what a neural network will do in every
situation. That's a fine quality in a brainstormer and a terrifying one in
something steering near a toddler.

So there's a second brain: the careful one. Its only job is to take the clever
brain's proposal and ask, using plain physics, *"is this actually safe?"* If yes,
it lets the command through. If no, it overrules it and tells the robot to stop.
The careful brain is simple enough that we can *prove* it's right.

That's the design: **let the clever part dream, but put a careful, provable,
physics-based bouncer at the door.** Everything that follows is how the bouncer
thinks. And the bouncer thinks in exactly the things we teach in school.

## Where am I going? (sines and cosines, finally useful)

The first thing the careful brain needs is to imagine where a move would take the
robot. If you're heading in some direction at some speed, how much of that is
"forward" and how much is "to the side"?

That's the very first thing you learn in trig: break an arrow into its horizontal
and vertical parts using sine and cosine. The robot does precisely this, thousands
of times a minute, to sketch out where every possible move would lead. The unit
circle — that diagram that feels like pure busywork in school — is the dictionary
the robot uses to translate *"this fast, this direction"* into *"here."*

That's it. The thing that felt like trivia is the robot's sense of direction.

## The robot is doing calculus without calling it that

Here's a lovely sleight of hand. The robot doesn't have a formula for its whole
future path. It only knows the *rule* for how things change from one instant to
the next — "in a tiny slice of time, move this much" — and it takes that rule and
steps it forward, slice by slice, to predict the next half-second.

That stepping-forward-by-tiny-amounts is calculus. Specifically it's how you solve
a *differential equation* — an equation written in terms of how fast things are
changing — on a computer, when there's no neat formula to be had. The same
technique simulates planets orbiting and pendulums swinging in a physics lab. Here
it's quietly deciding whether a robot drifts out of its lane.

You don't have to know the word for it to feel the elegance: *describe how things
change moment to moment, then let the moments add up.*

## Why speed is dangerous — and why it's a curve, not a line

This is the most important equation in the whole machine, and it's one nearly
everyone meets and nearly everyone forgets. The distance you need to stop depends
on the **square** of your speed:

> stopping distance ∝ (speed)²

Not proportional to speed — proportional to speed *squared.* That little exponent
is the difference between a fender-bender and a tragedy. Let's make it concrete
with the actual robot, which brakes gently:

- At a brisk walk, it needs about **1.1 meters** to stop.
- Going twice as fast, it needs about **4.5 meters** — *four times* as far.

Twice the speed, four times the distance. That's a parabola — the simplest curve
in algebra — and it is the single fact most responsible for keeping the robot (and,
scaled up, your car) from hurting someone. Every "is there enough room?" judgment
the robot makes is built on it.

I love this one because it's the rare equation that, once it's in your bones,
quietly changes how you drive for the rest of your life.

## Turning is a tug-of-war with friction

When the robot turns, it's curving along a circle, and circles demand a constant
inward pull to stay on track — more pull the faster you go, and (again) it grows
with the *square* of the speed. The only thing providing that pull is the grip
between the wheels and the ground. Friction.

So before every turn, the careful brain does a little tug-of-war calculation: *does
the move I'm about to make demand more sideways grip than I actually have?* If yes
— if the robot would skid — it refuses. That's a high-school friction problem,
running in real time, deciding whether a machine keeps its feet.

## It's all about your point of view (this one's deep)

Here's an idea that starts in a math class and ends up, no exaggeration, at
Einstein.

The robot constantly juggles two points of view: the world's (fixed to the
ground) and its own (forward and sideways, riding along with it). To switch between
them — to take what its sensors see "from its own seat" and place it on the map —
it uses a *rotation*: a compact piece of math that spins a point of view by an
angle.

To check whether its body fits inside a lane, it takes the four corners of itself,
spins them to match whichever way it's pointing, and asks whether the rotated
rectangle still fits between the lines. Same shape, different point of view.

That idea — that the same situation looks different from different vantage points,
and that a clean transformation connects them — is one of the most powerful in all
of science. It's the seed of relativity. The reason this robot can navigate using
sensors that only see "from its own seat," without needing perfect GPS, is exactly
this: it knows how to change its point of view on purpose. A delivery robot is, in
its small way, a working monument to *choose the right perspective and the hard
problem gets easy.*

## Being honest about being wrong

One more, and it's less an equation than a temperament.

In the real world, every measurement is a little bit wrong. Sensors jitter. Lasers
get confused by rain. A serious science education spends a lot of time teaching
something subtle: how to reason *honestly* about uncertainty instead of pretending
it away.

The robot is built around that honesty. When its sense of the world goes missing or
goes stale, it does not assume the road is clear and roll on hopefully. It assumes
the worst and slows or stops. Engineers call this *failing closed*, but really it's
just a piece of scientific humility turned into a safety rule: **never mistake the
absence of evidence for evidence of safety.** I wish more of the world ran that
way.

## What it's really for

Step back and here's the whole thing: the robot senses, predicts the near future
with equations of motion, checks the prediction against the hard limits of physics,
acts — and then does it again, twenty times a second, all day. Sensing, predicting,
correcting, forever. Underneath the cute exterior is a small, relentless argument
made entirely of math.

And that's the answer I landed on for my daughter, and for anyone who's ever
wondered what the symbols are *for.*

In a textbook, a problem ends when you box your answer. Out here, the answer
*becomes* something — a robot that either does or doesn't stop in time. The whole
difference between "this usually works" and "I can *prove* this won't hurt anyone"
is the difference between hoping and a real, math-shaped argument. The braking
curve isn't a step toward a grade; it's the reason a gap is safe. The change of
perspective isn't an exercise; it's how the machine knows where its own edges are.

People ask what physics and math are good for. Here's one honest answer: they are
the most powerful tools we have for making *guarantees* about the physical world.
Bridges that stay up. Spacecraft that arrive. Robots that stop. Every one of those
is somebody taking the math seriously enough to be *sure.*

My daughter is learning how to be one of those people. So, in a smaller way, is
anyone who ever pushed past "what is this for" and let the abstract thing become a
tool. The robot on the sidewalk is already counting on people who did — and there's
room for more of us than you'd think.

---

*This started as a note to my daughter and grew into something I wanted to share.
Every example here — the sense of direction, the stop-distance curve, the
change-of-perspective trick, the careful refusal to assume the road is clear — is
real code running on a real machine. If she takes nothing else from her physics
degree, I hope it's this: the equations aren't the obstacle before the interesting
part. They are the interesting part.*
