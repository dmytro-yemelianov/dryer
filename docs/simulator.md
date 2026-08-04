# Simulated controller — design

Status: Implemented (v0: single controller; multi-controller skew deferred by the
accepted Q3 decision) · Target: §29 step 9 ("simulated controller and end-to-end
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
- **Commands, not wire frames**: the simulator speaks typed heartbeat, heater,
  homing, and move semantics plus an optional scheduled-command envelope. Events
  record acceptance, rejection, execution, telemetry, endstops, safe-state entry,
  reset, and latched faults — the §16 *semantics* without the §16.3 framing. The
  wire codec becomes a later layer that must round-trip to these types.
- **SimTransport**: an in-memory duplex carrying a delivery tick, command, and
  optional execution tick, with configurable latency, jitter (seeded PRNG — the seed
  is part of the test, never ambient), loss, and duplication (§24.1). Link loss uses
  `drop_link`; controller reset is injected directly through `SimController::reset`.
- **SimController**: bounded command queue with reported capacity, fill level, and
  earliest/latest accepted timestamps (§16.4). `send_scheduled` commands are
  validated when they arrive (transport latency consumes lead time), rejected when
  early/late or off the 1 ms execution quantum, kept in timestamp order, and executed
  only when due. Immediate commands retain the original v0 behavior. The controller
  enforces the safety envelope locally — heartbeat timeout forces declared safe
  states (§18.1), and faults latch. Safe states and timeouts come from the resolved
  safety profile, not test constants. Continuous step-segment underrun remains tied
  to the future step-segment vocabulary; this v0 queue does not invent that policy.
- **Plant models**: first-order thermal plant per heater
  (`dT/dt = (P·k − (T − T_amb))/τ`) with integer-tick integration; endstop =
  position threshold on a virtual axis. Enough to make `heater.wait` and homing
  *mean* something; fidelity beyond first-order is a non-goal.
- **TraceLog**: every event tick-stamped, `serde`-serializable, byte-stable —
  the golden artifact. `replay_report` returns a structured match/divergence report
  with event counts, first divergent index and tick, and expected/actual events. The
  `replay` example exposes it as a read-only CLI with meaningful exit codes.

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

## Decisions and deferred boundary

1. Tick resolution is 1 µs; plant integration and scheduled execution use a fixed
   1 ms quantum.
2. Job vocabulary stays test-internal until the workflow system (§17) defines the
   real one.
3. Multi-controller clock skew (§16.5) is deferred until a multi-controller fixture
   and clock-synchronization protocol exist. The single-controller trace contract
   must not guess those interfaces.

Compare two traces directly:

```bash
cargo run -p dryer-simulator --example replay -- \
  examples/minimal-cartesian/job-trace.golden \
  examples/minimal-cartesian/job-trace.golden
```

Exit status is 0 for a match, 1 for a divergence, and 2 for usage/IO/parse errors.
