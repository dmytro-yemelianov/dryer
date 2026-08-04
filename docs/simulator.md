# Simulated controller — design

Status: Implemented (v0: single controller; open Q3 multi-controller deferred as proposed) · Target: §29 step 9 ("simulated controller and end-to-end
golden tests"), spec §24.1.

## Purpose

The simulator is the first *executing* consumer of a resolved machine: it takes
the resolved graph + a trivial job and produces a deterministic event trace.
It exists to (a) freeze end-to-end goldens before any firmware exists, and
(b) force the control-protocol semantics (§16) to be designed against an
implementation that can fail loudly.

## Architecture (one crate: `crates/simulator`)

```text
VirtualClock ──► SimTransport ──► SimController ──► TraceLog
   (u64 ticks)    (latency/jitter/    (queue +          (Vec<Event>,
                   loss/duplication)   executor +        tick-stamped,
                                       plant models)     serializable)
```

- **VirtualClock**: monotonic `u64` ticks, advanced explicitly by the test
  driver. No wall clock anywhere (determinism is the point).
- **Commands, not wire frames**: the simulator speaks a typed
  `enum Command { ScheduleGpio{..}, StepSegment{..}, SetPwm{..}, Heartbeat, .. }`
  and `enum Event { Executed{..}, Underrun{..}, FaultLatched{..}, .. }` — the
  §16 *semantics* without the §16.3 framing. The wire codec becomes a later
  layer that must round-trip to these types; designing frames first would
  freeze bytes before behavior.
- **SimTransport**: an in-memory duplex carrying `(deliver_at_tick, Command)`,
  with configurable latency, jitter (seeded PRNG — the seed is part of the test,
  never ambient), loss, and duplication (§24.1). Fault injection = transport
  methods (`drop_link`, `reset_controller`).
- **SimController**: bounded command queue (capacity + earliest/latest accepted
  timestamp, §16.4); executes commands at their tick; enforces the safety
  envelope locally — heartbeat timeout forces declared safe states (§18.1),
  queue underrun follows policy, faults latch. Safe states and timeouts come
  from the resolved safety profile, not test constants.
- **Plant models**: first-order thermal plant per heater
  (`dT/dt = (P·k − (T − T_amb))/τ`) with integer-tick integration; endstop =
  position threshold on a virtual axis. Enough to make `heater.wait` and homing
  *mean* something; fidelity beyond first-order is a non-goal.
- **TraceLog**: every event tick-stamped, `serde`-serializable, byte-stable —
  the golden artifact. A replay helper diffs two traces and reports the first
  divergent tick.

## Golden end-to-end test (the step-9 exit)

```text
resolve(minimal-cartesian) → lock → job: [home X, heat to 60 °C, wait, move]
→ run simulator (fixed seed, fixed tick budget)
→ trace == examples/minimal-cartesian/job-trace.golden   (drift-gated in CI)
```

Plus fault goldens: heartbeat loss mid-heat must show the heater forced off
within the profile's `heartbeat_timeout` ticks; a controller reset must show
safe-state entry and a latched fault (§24.1 "controller reset and link-loss
injection").

## Open questions (decide before coding)

1. Tick resolution (propose 1 µs — fine enough for step timing later, coarse
   enough for u64 headroom).
2. Where job vocabulary lives: the simulator needs a minimal job format; propose
   it stays *test-internal* until the workflow system (§17) defines the real one.
3. Multi-controller clock skew (§16.5): model now (two clocks + offset) or after
   single-controller goldens freeze? Propose after.
