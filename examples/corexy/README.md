# examples/corexy

Single-controller CoreXY machine using the `machines/corexy-standard` template.

## Covers

- Template-expanded kinematics limits (`max_velocity`, `max_acceleration`,
  `max_z_velocity`) — none declared in source, so all three come from the
  template (informational `I1133`).
- Template-declared drivers (`x_driver`, `y_driver`, `z_driver`) shadowed by
  identically-named source components that add `connected_to` (informational
  `I1132`) — required because template-injected components are invisible to
  the parser's intra-document reference check, so a motor's `driver:` must
  resolve at parse time against a *source* component.

## Does not cover

- Multi-controller topology or `downlinks:` validation — see
  `examples/multi-mcu-toolhead`.

## Regenerating goldens

- `machine.lock`: `cargo test -p dryer-machine-lock` — a missing or stale
  golden causes the test to panic printing the actual value; copy it into
  `machine.lock` verbatim.
- `controller-safety.mainboard.golden.json`,
  `controller-build-plan.mainboard.golden.json`,
  `controller-image.mainboard.golden.json`: `cargo test -p dryer-firmware-build`,
  same missing-golden-prints-actual convention.
- `flash-plan.mainboard.golden.json`: `cargo test -p dryer-firmware-flash`.
- `job-trace.golden`: `UPDATE_TRACE=1 cargo test -p dryer-simulator --test golden`.
