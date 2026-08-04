# Controller safety artifacts

Status: Implemented build-input boundary · Target: spec §11.2 phases 11–12,
§18.2, and the safety-configuration subset of §21.1.

`dryer-firmware-build` compiles the controller-local safety projection in a v3
`machine.lock` into one deterministic `dryer.controller-safety/v1` JSON artifact
per controller. It is intentionally not a firmware toolchain: the artifact is the
versioned input a future native firmware build must embed unchanged.

## Data flow

```text
safety profile policy
  → resolver safety validation
  → firmware partitioning by concrete controller resource
  → machine.lock v3
  → dryer.controller-safety/v1 artifact
  → simulator today / firmware embedding later
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

Lockfile v3 stores a `safety` block for every controller, including controllers with
no applicable states. Older v1/v2 locks remain readable for inspection, but artifact
compilation rejects them because absence of edge configuration must never silently
become a valid firmware input.

## Artifact contract

Each artifact records:

- schema and logical controller id;
- the canonical lock hash;
- exact safety-profile id, manifest hash, and full-content hash;
- sorted local state bindings: component, class, resource, closed action, optional
  heartbeat timeout, and optional local sensor resource.

Canonical compact JSON is hashed with SHA-256; pretty JSON is only a human-facing
render of the same typed value. The minimal-Cartesian artifact is drift-gated at
`examples/minimal-cartesian/controller-safety.golden.json`.

## Deliberate boundary

This slice does not select a target triple, toolchain, native driver set, memory
layout, protocol ABI, signing key, or firmware binary format. Those remain §21.1
build-plan work. A future firmware backend must consume this artifact rather than
rebuilding safety configuration from the source manifest or package registry.
