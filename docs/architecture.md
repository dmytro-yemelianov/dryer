# Architecture — current implementation view

The full architecture is defined by the Dryer spec (Draft v0.1). This page maps
the spec's layers to what exists in the workspace today.

## Layers (spec §3.2)

```text
Applications and UI          — none (deliberate; §29: no premature UI)
Platform API                 — none
Workflow runtime             — none
Machine state service        — none
Motion planner               — none
Control protocol             — control-protocol (v1 framing) + control-client (outbound/receive boundary)
MCU runtime                  — simulator (behavioral test model, no firmware)
Hardware                     — none
─────────────────────────────────────────────
Firmware artifacts           — firmware-build (safety, build plans, reference images)
Deployment planning          — firmware-flash (read-only plans; no executor)
Reproducibility              — machine-lock
Machine resolution           — machine-resolver
Configuration language       — machine-schema, machine-parser
Package ecosystem            — package-model
Resolver vocabulary          — resource-model
```

The workspace has crossed the original Phase 0-only boundary through testable
planning and protocol seams: the simulator models controller behavior and
`control-protocol` fixes the v1 command-frame ABI. There is still no production
host runtime, MCU firmware, or mutating deployment implementation.

## Crate dependency rule

| Crate | Internal dependencies |
|---|---|
| `machine-schema` | none |
| `package-model` | `machine-schema` |
| `machine-parser` | `machine-schema`, `package-model` |
| `resource-model` | none |
| `machine-resolver` | all four crates above |
| `machine-lock` | schema, parser, package model, resolver |
| `firmware-build` | `machine-lock` |
| `firmware-flash` | `machine-lock`, `package-model`, `firmware-build` |
| `simulator` | none in its public library; firmware-build/lock/resolution crates in end-to-end test dependencies |

- `machine-schema` is the leaf: document types, quantities, identifiers, and the
  shared `Diagnostic` type. (The spec defines `Diagnostic` under the resolver §11.3;
  it lives here so the parser and resolver share one type. It includes exact
  `SourceSpan` ranges and `related` locations while retaining the v0.1
  `path`/`line`/`column` compatibility projection.)
- Dependencies point downward only. `machine-resolver` depends on the model/parser
  layer; no model crate depends on the resolver.
- `machine-resolver` joins board pins to chip capabilities and treats connector,
  electrical, bus-frequency, measured latency/jitter, and DMA-route requirements as
  deterministic hard allocation constraints. Accepted evidence stays on assignments.
- `machine-lock` captures successful resolution, portable registry identity and exact
  descriptor hash, exact package-tree content digests, controller-local safety
  bindings, and deterministic firmware target/build inputs. `firmware-build` turns
  those bindings into versioned, byte-stable safety/build plans and an inspectable
  reference controller image without rereading package policy or target metadata.
  The plan pins the exact image size and hash while explicitly marking it
  non-deployable. `firmware-flash` consumes the same derived build output plus the lock
  and board metadata, rejects registry, package, plan, or artifact drift, but cannot
  call back into resolution or mutate hardware.
- The simulator's public semantic types stay independent of configuration crates;
  only its tests adapt the compiled controller artifact into simulated inputs.

## Graph lifecycle (spec §5.5)

`machine-parser` owns the **source graph**. `machine-resolver` expands templates and
produces a separate `ResolvedGraph`; it never mutates `MachineDoc`. `machine-lock`
serializes a reproducibility projection of that result. `firmware-build` creates
controller-safety, controller-build-plan, and reference-image artifacts from each
locked partition. A flash plan binds a locked controller to an observed USB candidate
and verified artifact, but it is not yet a
`DeployedGraph`: activation, confirmation, and persistent physical-controller identity
do not exist. Runtime observation remains a future state-service type.

The committed package root contains `packages/registry.yaml`. Its
`dryer.registry/v1` descriptor gives the registry a stable logical id and a portable
absolute URI; workstation-local `file:` locations are rejected. Lockfile v5 stores a
`dryer.registry-source/v1` projection with the SHA-256 of the exact descriptor bytes.
Package content hashes remain independent, so provenance and immutable package bytes
are both checked rather than conflated.

## External integration intent

Toolpath/job auditing is out of scope for Dryer itself: the job pipeline is
expected to delegate pre-flight G-code review to a program auditor
([dry](https://github.com/dmytro-yemelianov/dry)). The dependency direction is
Dryer → dry, never the reverse.
