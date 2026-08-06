# examples/multi-mcu-toolhead

Two-controller CoreXY machine mirroring spec §5.1: a USB mainboard and a CAN
toolhead board, using the same `machines/corexy-standard` kinematics template
as `examples/corexy` so any artifact difference between the two examples is
attributable to multi-controller topology, not kinematics.

## Covers

- `boards/example-mainboard@1.1.0`'s `downlinks.can0` and the toolhead's
  `transport: { type: can, parent: mainboard.can0 }` (E1121/E1122 pass; see
  `machine-resolver`'s unit tests for the negative cases).
- Two independent safety partitions — bed heater on `mainboard`, hotend
  heater on `toolhead` — exercising the §18.2 "required sensor resolves
  through an input connector on the same controller" rule
  (E1503/E1504/E1507) on a real split rather than a synthetic unit test.
- E1203 (component ambiguity) avoidance: every component names its
  controller explicitly, because an unclaimed component is ambiguous once
  more than one controller exists.

## Does not cover

- The §19.2 cross-controller contract (authoritative state owner,
  synchronization requirements, acceptable clock uncertainty, link-loss
  behavior, safe degraded state) — not modeled. This example declares
  topology only.
- **No `job-trace.golden`.** The simulator is single-controller by design,
  and multi-controller clock skew is explicitly deferred (accepted design
  Q3: it needs a real synchronization protocol, not assumptions). A
  mainboard-only trace was rejected because it would silently omit the
  toolhead's heater — exactly the half-truth this repository's honesty
  policy targets.
- `transports.*.peripheral` and `downlinks.*` are not validated against the
  chip's peripheral table (pre-existing gap for `transports`; now also true
  for `downlinks`).
- The toolhead's committed build-plan/image goldens list `usb_transport` in
  their firmware features, even though its runtime transport is CAN — this
  comes from `chips/generic-mcu`'s `firmware.features` being chip-derived,
  not transport-derived. Not a bug; recorded here because it could otherwise
  read as an unexplained inconsistency.

## Regenerating goldens

Same commands as `examples/corexy`, run once per controller where the test
table requires it (`mainboard`, `toolhead`):
- `cargo test -p dryer-machine-lock`
- `cargo test -p dryer-firmware-build`
- `cargo test -p dryer-firmware-flash`
