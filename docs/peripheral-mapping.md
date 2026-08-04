# Peripheral mapping — design

Status: Implemented (step timing, bus matching, DMA routing, and latency/jitter budgets).

## Problem

The resolver validates what packages *declare*: connector kinds, voltage domains,
current limits. It cannot validate anything that depends on **which MCU peripheral
sits behind a connector pin** — step-rate timing budgets (§10.1 "latency and jitter
budgets", §24.4), timer/channel coupling, DMA routing, or bus requirements
(§9 `requires.bus`). Boards map connectors to *pins* (`step: PE11`); nothing maps
pins to *capabilities* (`PE11 = tim1.ch2 | gpio`).

## Design

One new declaration, one derived table, no new resolver phase.

### 1. Chip packages declare pin functions

`kind: chip` payload gains a pin-function table (§7 already gives chips a
peripheral inventory; this joins it to pins):

```yaml
peripherals:
  timers:
    - id: tim1
      channels: 4
  spi:
    - id: spi1
      max_frequency: 42 MHz

pin_functions:
  PE11: [tim1.ch2, gpio]
  PE10: [gpio]
  PA5:  [spi1.sck, adc1.ch5, gpio]
```

Rules: every function token must name a declared peripheral (or `gpio`);
frequencies are typed quantities; the table is *capability*, not wiring (§7:
chips must not describe boards).

### 2. Boards bind chips; the resolver derives connector capabilities

A board already names its chip and its connector pins. The resolver joins them:

```text
connector capability = ∪ over connector pins of chip.pin_functions[pin]
```

Derived capabilities are attached to the resolved assignment (new field
`pin_capabilities: BTreeMap<signal, Vec<String>>`), so `explain` and the lock
show *why* a socket can or cannot meet a requirement. A board pin absent from
the chip table is an error (the board claims wiring the chip does not have).

### 3. What becomes checkable

- **Step timing** (first check): if the machine declares
  `kinematics.limits.max_step_rate: 200 kHz`, every allocated
  `stepper_driver_socket`'s `step` pin must carry a `timN.chM` function —
  GPIO-bit-banged stepping cannot honor a declared rate budget. Diagnostic:
  `E1310`, naming the pin and its actual functions.
- **Bus requirements** (§9): `requires.bus: {kind: spi, min_frequency: 1 MHz}`
  matches connectors whose pins carry `spiN.*` functions with
  `max_frequency >= min_frequency`. Search allocation treats it as a hard
  filter, like voltage domains.
- **DMA routing and timing budgets** (§10.1, §24.4): device bus requirements
  may add `dma_signals`, `max_latency`, and `max_jitter`. Chip targets publish
  explicit DMA channel routes plus measured `worst_case_latency` and
  `worst_case_jitter` for each bus. Missing data never satisfies a declared
  requirement. Search and explicit claims both treat these as hard constraints;
  accepted assignments record the exact measured bounds and DMA channel.
- **Timer conflicts** (the spec's own E1204 example): two allocations needing
  the same `timN.chM` become detectable — the resource-model's
  `TimerCoupling`/`Exclusivity` machinery finally gets real operands.

### 4. Fixtures and phasing

1. `chips/generic-mcu@1.3.0` gains `peripherals` + `pin_functions` covering the
   example board's pins (new version — exercises multi-version selection).
2. Board-pin/chip-table join + `pin_capabilities` on assignments (no new
   checks yet; provenance only).
3. `E1310` step-timing check + bus matching + timer-conflict detection, each
   with a failing fixture.
4. `chips/generic-mcu@1.5.0` adds measured bus bounds and DMA routes; the
   `dma-stream-sensor` fixture exercises accepted and rejected budgets.

## Non-goals

Interrupt priorities, DMA channel ownership/exclusivity (this slice validates
routing only), clock-tree validation, and per-silicon errata remain deferred.
