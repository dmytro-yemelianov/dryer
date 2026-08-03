# ForgeOS

A modular, cross-platform control platform for 3D printers and other motion-control
machines. Its core abstraction is a typed, validated **Machine Graph** describing the
complete machine; a deterministic **resolver** turns that graph plus a package registry
into locked, explainable firmware and runtime configuration.

> **Naming note:** *ForgeOS* is a working name (spec §1). Crate names carry a `forge-`
> prefix provisionally; nothing here is a stable public identifier yet.

**Status: Phase 0 — specifications and repository foundations.** This repository
currently contains the Machine Graph v0.1 schema types and parser, the package model
with local registry loading, and the resource model — the first four steps of the
implementation order (spec §29). There is no firmware, no resolver, and no motion
stack yet; see `docs/implementation-roadmap.md` for what exists versus what is planned.

The authoritative specification lives in `docs/` (imported from the ForgeOS Codex
spec, Draft v0.1). Key principles:

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
| `crates/machine-resolver` | Deterministic resolution: 7 explicit phases, explicit-claim connector allocation, explainable assignments, conflict diagnostics with suggestions |

```bash
cargo test --workspace
cargo run -p forge-machine-parser --example validate examples/minimal-cartesian/machine.yaml
```

## License

Apache-2.0. Before any Klipper-derived compatibility work lands, its provenance and
license terms must be recorded per spec §23.5.
