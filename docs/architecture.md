# Architecture — Phase 0 view

The full architecture is defined by the Dryer spec (Draft v0.1). This page maps
the spec's layers to what exists in the workspace today.

## Layers (spec §3.2)

```text
Applications and UI          — none (deliberate; §29: no premature UI)
Platform API                 — none
Workflow runtime             — none
Machine state service        — none
Motion planner               — none
Control protocol             — none
MCU runtime                  — none
Hardware                     — none
─────────────────────────────────────────────
Configuration language       — machine-schema, machine-parser   ← Phase 0 lives here
Package ecosystem            — package-model                     ←
Resolver vocabulary          — resource-model                    ←
```

Phase 0 builds the configuration and package layer *only*: everything above the
line arrives in later phases, and nothing in this workspace may pre-commit their
interfaces.

## Crate dependency rule

```text
machine-parser ──► machine-schema ◄── package-model
                        ▲
resource-model ─────────┘ (shares Diagnostic via machine-schema)
```

- `machine-schema` is the leaf: document types, quantities, identifiers, and the
  shared `Diagnostic` type. (The spec defines `Diagnostic` under the resolver §11.3;
  it lives here so the parser and resolver share one type. The resolver will extend
  it with `SourceSpan`/`related` rather than fork it.)
- Dependencies point downward only. The future resolver depends on all three models;
  no model crate ever depends on the resolver.

## Graph lifecycle (spec §5.5)

Phase 0 implements the **source graph** stage only. The `expanded`, `resolved`,
`deployed`, and `observed` stages are distinct types owned by the resolver and
runtime — they must never be represented by mutating `MachineDoc`.

## External integration intent

Toolpath/job auditing is out of scope for Dryer itself: the job pipeline is
expected to delegate pre-flight G-code review to a program auditor
([dry](https://github.com/dmytro-yemelianov/dry)). The dependency direction is
Dryer → dry, never the reverse.
