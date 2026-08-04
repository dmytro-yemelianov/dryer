# Controller firmware input artifacts

Status: Implemented build-input boundary · Target: spec §11.2 phases 11–12,
§18.2, and §21.1 build-plan inputs.

`dryer-firmware-build` compiles a v4 `machine.lock` into deterministic controller
inputs: `dryer.controller-safety/v1` for the edge-enforced safety projection and
`dryer.controller-build-plan/v1` for the selected target/toolchain contract. It is
intentionally not a firmware executor: a future native backend must consume these
locked artifacts unchanged.

## Data flow

```text
safety profile policy
  → resolver safety validation
  → firmware partitioning by concrete controller resource
  → artifact planning from chip target metadata
  → machine.lock v4
  ├─→ dryer.controller-safety/v1 artifact → simulator today
  └─→ dryer.controller-build-plan/v1 artifact → firmware backend later
```

The resolver performs the semantic work before the lock is created:

- only the closed actions `off` and `disabled` are accepted;
- heartbeat timeouts compile to at least one integer microsecond, matching the
  controller's 1 us time quantum;
- directly assigned outputs retain their connector resource;
- logical actuators such as `stepper_motor` require a covered class and inherit only
  a driver assignment backed by a `stepper_driver_socket`;
- a covered actuator without a concrete controller resource is rejected;
- a policy requiring a sensor is rejected unless that sensor has a concrete input
  resource on the same controller;
- exactly one safety action may govern each physical controller resource.

Artifact planning also requires the chip package to declare positive whole-byte flash
and RAM budgets, a valid bootloader offset, target triple, toolchain, build profile,
protocol/ABI versions, and feature flags. The resolver adds exact board/chip package
versions and native device-driver packages selected by concrete assignments.

Lockfile v4 stores `safety` and `build` blocks for every controller. Older v1-v3 locks
remain readable for inspection; safety artifacts require v3+, while build plans
require v4 because missing target metadata must never become an implicit default.

## Artifact contract

The safety artifact records:

- schema and logical controller id;
- the canonical lock hash;
- exact safety-profile id, manifest hash, and full-content hash;
- sorted local state bindings: component, class, resource, closed action, optional
  heartbeat timeout, and optional local sensor resource.

Canonical compact JSON is hashed with SHA-256; pretty JSON is only a human-facing
render of the same typed value. The minimal-Cartesian artifact is drift-gated at
`examples/minimal-cartesian/controller-safety.golden.json`.

The build-plan artifact additionally records exact board/chip/native-driver packages,
target triple, toolchain and profile, protocol/ABI versions, integer memory and boot
layout, sorted feature flags, resolved resources, and the complete locked safety
partition. Its golden is
`examples/minimal-cartesian/controller-build-plan.golden.json`.

## Deliberate boundary

This slice selects and locks build inputs but does not install or invoke a toolchain,
link a firmware binary, predict an output hash, choose a signing key, or define a
binary container. A future firmware backend must consume the build-plan artifact
rather than rebuilding target or safety configuration from source packages.
