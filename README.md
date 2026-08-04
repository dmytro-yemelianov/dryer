# Dryer

A modular, cross-platform control platform for 3D printers and other motion-control
machines. Its core abstraction is a typed, validated **Machine Graph** describing the
complete machine; a deterministic **resolver** turns that graph plus a package registry
into locked, explainable firmware and runtime configuration.

> **Naming:** the project is **Dryer** (named 2026-08-04; the spec was drafted under
> the working name *ForgeOS*, preserved as-is in `docs/spec.md`). Crates are
> `dryer-*`; the manifest API version is `dryer.machine/v0.1`. Sibling project:
> [dry](https://github.com/dmytro-yemelianov/dry) compiles and audits the *job*;
> Dryer configures and (eventually) runs the *machine*.

**Status: configuration layer complete, no runtime yet.** This repository contains
the Machine Graph v0.1 schema and parser (exact source locations), the package model
with a multi-version local registry, the resource model, a nine-phase deterministic
resolver (transitive dependency closure, graph expansion from machine templates,
explicit + search-based connector allocation, electrical and safety-coverage
validation, explainable assignments), and hashed `machine.lock` generation with a
drift-gated golden. There is no firmware, no motion stack, and no control protocol
yet; see `docs/implementation-roadmap.md` for exactly what exists versus what is
planned — a checked box there means tests pass, not that a type was declared.

The authoritative specification lives in `docs/spec.md` (Draft v0.1). Key principles:

- the Machine Graph is the single source of truth;
- explicit, versioned layer boundaries;
- local-first operation;
- reproducible configuration (manifest + lockfile + calibration + build metadata);
- safety enforced at the edge (every hazardous-output MCU protects itself);
- compatibility (G-code, Klipper, Moonraker) as adapter boundaries, never the domain model;
- no arbitrary code on safety-critical controllers.

## Workspace

| Crate | What it is |
|---|---|
| `crates/machine-schema` | Machine Graph v0.1 document types, typed physical quantities, identifier rules, shared diagnostics |
| `crates/machine-parser` | YAML → validated Machine Graph with structured, located diagnostics |
| `crates/resource-model` | Generic hardware resource/constraint/preference model used by the future resolver |
| `crates/package-model` | Package identity (`namespace/name@version`), manifests, board payloads, dependency ranges, local directory registry |
| `crates/machine-resolver` | Deterministic resolution: 9 explicit phases, explicit-claim + search-based connector allocation, electrical + safety-coverage checks, explainable assignments, conflict diagnostics with suggestions |
| `crates/machine-lock` | `machine.lock`: canonical, hashed capture of a resolution (drift-gated golden in `examples/`) |

```bash
cargo test --workspace
cargo run -p dryer-machine-parser --example validate examples/minimal-cartesian/machine.yaml
```

## License

Apache-2.0. Before any Klipper-derived compatibility work lands, its provenance and
license terms must be recorded per spec §23.5.
