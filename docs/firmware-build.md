# Controller firmware artifacts

Status: Implemented deterministic reference-image backend · Target: spec §11.2
phases 11–12, §18.2, and §21.1 build plans.

`dryer-firmware-build` compiles the current v5 `machine.lock` into deterministic
controller artifacts: `dryer.controller-safety/v1` for the edge-enforced safety
projection, `dryer.controller-build-plan/v2` for the selected target/toolchain and
expected output, and an inspectable `dryer.controller-image/v1` reference image. The
reference image is a deterministic runtime/configuration handoff, not executable MCU
code.

## Data flow

```text
safety profile policy
  → resolver safety validation
  → firmware partitioning by concrete controller resource
  → artifact planning from chip target metadata
  → machine.lock v5
  ├─→ dryer.controller-safety/v1 artifact → simulator today
  └─→ dryer.controller-build-plan/v2
      └─→ dryer.controller-image/v1 + exact size/sha256
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

Lockfile v4 introduced `safety` and `build` blocks for every controller. Lockfile v5
also requires portable registry-source identity and an exact descriptor hash. Older
v1-v4 locks remain readable for inspection and retain their historical build
semantics; safety artifacts require v3+, while build plans require v4 because missing
target metadata must never become an implicit default. A malformed v5 lock cannot
produce either artifact.

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
partition. v2 also pins the reference image's format, bundle-relative path, exact byte
length, SHA-256, and deployment eligibility. Its golden is
`examples/minimal-cartesian/controller-build-plan.golden.json`.

The reference backend serializes the same lock-bound target/resource/safety payload
as canonical pretty JSON. Inspection rejects alternate encodings, unknown schemas,
plan drift, and any byte difference from the locked build inputs. The exact backend
output is drift-gated at
`examples/minimal-cartesian/controller-image.golden.json`.

The output digest lives in the persisted build plan, not back inside `machine.lock`.
The image embeds the lock hash, so inserting the image hash into that same lock would
create a lock-hash/output-hash cycle. Flash planning accepts both artifacts and first
reproduces the supplied build plan from the lock, preserving an independent output
pin without self-reference.

Build the committed lock without rereading the package registry:

```bash
cargo run -p dryer-firmware-build --example build -- \
  examples/minimal-cartesian/machine.lock \
  mainboard \
  -o build/mainboard/firmware.dryer.json \
  --plan-out build/mainboard/build-plan.json
```

## Deliberate boundary

The reference image deliberately carries `deployable: false`. Flash planning verifies
its pinned hash and structure but remains blocked from treating it as a controller
executable. This slice does not install or invoke a target toolchain, link native MCU
code, define startup/vector-table behavior, or choose a signing key. A future native
backend needs chip-family linker/runtime metadata that the current generic fixture
does not yet provide; it must consume the same build inputs rather than rebuilding
target or safety configuration from source packages.
