# Implementation roadmap — status

Tracks the spec's Phase 0 deliverables (§28) and initial implementation order (§29)
against what actually exists. This file is honest by policy: a checked box means
tests exist and pass, not that a type was declared.

## §29 initial implementation order

- [x] **1. `machine-schema` and schema fixtures** — document types with
  `deny_unknown_fields`, identifier rules (§5.3), typed quantities with a scale-only
  unit table (§5.4; Kelvin deliberately excluded — offset, not scale), YAML round-trip
  tests, `schemas/machine.schema.json`, and the `examples/minimal-cartesian` fixture.
- [x] **2. `package-model` and local registry loading** — `namespace/name@version`
  references, `package.yaml` manifests, both spec dependency-range forms (`^1.0`,
  `>=2.0,<3.0`), deterministic directory scan with structural diagnostics
  (`E06xx`), committed fixture `packages/devices/tmc2209`.
- [x] **3. `resource-model` with stable identifiers and units** — resource kinds
  (§10), hard-constraint classes (§10.1), preferences, offers with locally-decidable
  constraint evaluation; serde round-trips for future lockfile embedding.
- [x] **4. `machine-parser` with source locations** — collect-all-diagnostics
  validation (version/kind, identifiers, package refs, quantity dimensions,
  intra-document references, transport parents) with dotted paths + heuristic line
  locations. *Known limitation:* lines come from a key-scan heuristic, not spans;
  real `SourceSpan` tracking (§11.3) is step 5's concern.
- [ ] **5. `machine-resolver` phases 1–7 and structured diagnostics**
- [ ] **6. Electrical, timing, and safety validation**
- [ ] **7. `machine-lock` canonical serialization and hashing**
- [ ] **8. Klipper config and build-parameter export** *(license/provenance record
  required first — §23.5)*
- [ ] **9. Simulated controller and end-to-end golden tests**
- [ ] **10. Cross-platform flash discovery and dry-run plans**

## Diagnostic code conventions (Phase 0 deliverable)

| Range | Meaning |
|---|---|
| `E01xx` | parse failures (YAML, IO) |
| `E02xx` | document structure / required fields / versions |
| `E03xx` | identifier rules |
| `E04xx` | units and quantities |
| `E05xx` | references (packages, components, controllers, ports) |
| `E06xx` | package/registry structure |
| `E1xxx` | reserved for resolver phases (§11), e.g. `E12xx` resource conflicts |

## Not yet decided / blocked

- **Klipper compatibility provenance** (§23.5): no Klipper-derived code, tables or
  fixtures may land until their licenses and provenance are recorded here.
- **Toolpath verification slot**: the job pipeline should name a program auditor
  (e.g. [dry](https://github.com/dmytro-yemelianov/dry)) as its pre-flight gate —
  a §23.6 to be written.
- Package schemas for chip/board/device/workflow kinds (§7–§9) land with the
  resolver slices that consume them; only `machine.schema.json` is normative today.
