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
  intra-document references, transport parents) with dotted paths and **exact
  line:column locations** from a marked YAML event walk (`spans::SpanIndex`,
  yaml-rust2): every key and sequence item is indexed; unrecorded paths locate at
  their nearest ancestor. The original key-scan heuristic is deleted. Still future:
  §11.3 range spans + `related` diagnostics, which belong to the resolver's
  multi-source conflicts.
- [~] **5. `machine-resolver` phases 1–7 and structured diagnostics** — the seven-phase
  skeleton runs end-to-end with a recorded `phases_run` trace: parse/schema delegate to
  the parser; phase 3 checks pinned packages against the registry (exact-version match,
  flat-pin dependency-range checks — no transitive solving yet); phase 4 loads board
  payloads (§8 connectors/transports, quantity-validated `max_current`/`pullup`) and
  checks controller transports; phase 5 is a recorded no-op (no templates exist);
  phases 6–7 validate and allocate **explicit connector claims** with exclusivity,
  kind-compatibility (a v0 attribute→kind table, documented as a stopgap for device-
  package requirements §9), §11.5-style explainable assignments, and E1200 conflicts
  that suggest free same-kind connectors. Determinism pinned by test (byte-equal JSON
  across runs). Slice 2 added **search-based allocation**: device packages carry
  `requires.connector` (§9 subset), unclaimed components get the first free
  kind-compatible connector in stable order (§11.4), all candidates recorded (§11.5),
  with E1203 ambiguity (multi-controller without `controller:`) and E1205 exhaustion
  errors. Slice 5 rebuilt phase 3 as **transitive closure resolution** over a
  **multi-version registry** (`packages/<ns>/<name>/<version>/` layout, E0606
  dir-version agreement): roots = explicit pins + implicit references (controller
  boards, the safety profile); per-package ranges intersect; each package resolves to
  the highest satisfying version at a fixpoint (64-round cap, E1107); machine pins
  are absolute (range-excluded pin = E1103, empty intersection = E1104 naming every
  requirer, unpinned implicit root = E1106 warning). The closure is published on
  `ResolvedGraph::packages`. Slice 6 implemented **graph expansion** (§5.5):
  machine-kind packages carry a board-agnostic `template` (components placed by
  search allocation + kinematics-limit defaults); expansion never mutates the
  source — source shadows template (I1132), every contribution is surfaced
  (I1133), a kinematics-type mismatch warns (E1130), and template-injected claim
  errors are hard (E1131/E1134/E1206). *Remaining for [x]:* bus/signal requirement
  matching, range spans + `related` diagnostics.
- [~] **6. Electrical, timing, and safety validation** — first electrical check landed
  as resolver phase 8: a component's declared `current` draw is quantity-parsed and
  compared against the assigned connector's `max_current` (E1300; malformed draw
  E1301). Slice 3 added **safety validation** (resolver phase 10 analogue): safety
  profiles are real packages (`classes` → `safe_state`/`heartbeat_timeout` (typed
  Time)/`requires_sensor`, §18.2–§18.3); the machine's profile must exist (E1500),
  every component driving a `power_output` must belong to a covered class (E1501 —
  the §30 "no unresolved safety defaults" gate), and class requirements are enforced
  (sensorless heater → E1502). Slice 7 added **voltage-domain validation**: devices
  declare acceptable domains (§10.1 membership); explicit-claim mismatches are E1302
  — a connector declaring NO domain never satisfies a non-empty requirement — and
  search allocation treats the domain as a hard candidate filter, recorded in the
  assignment provenance. Slice 8 implemented **peripheral mapping**
  (docs/peripheral-mapping.md): chip packages carry pin-function tables (E065x
  validation), board wiring is checked against the chip (E1312), every assignment
  carries derived `pin_capabilities`, and a declared `max_step_rate` enforces
  exclusive timer channels on step pins (E1310 gpio-only, E1314 conflicts) —
  interleaved with allocation so search steers around reserved channels.
  *Remaining:* bus/signal requirement matching (§9 `requires.bus` — the frequency
  data now exists on chip buses), DMA/latency budgets, and compiling safe states
  into controller artifacts (firmware phases).
- [~] **7. `machine-lock` canonical serialization and hashing** — `dryer-machine-lock`
  produces a deterministic lockfile (canonical JSON bytes + `sha256:` lock hash; YAML
  on disk) binding the machine-source hash, exact package versions with manifest
  hashes, resolver version, and per-controller resolved resources. Golden at
  `examples/minimal-cartesian/machine.lock`, drift-gated in CI. Slice 3 added the
  pinned `safety_profile` (id + manifest hash, §12). *Remaining for [x]:* registry
  source identity, full package content digests (§6.6), firmware targets, protocol
  versions, feature flags — each gated on machinery that does not exist yet (stated
  in the crate docs).
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

- **Klipper compatibility provenance** (§23.5): DRAFT record at
  [`klipper-provenance.md`](klipper-provenance.md) — awaiting owner approval; the
  no-Klipper-derived-content gate holds until its Status line reads Approved.
- **Toolpath verification slot**: the job pipeline should name a program auditor
  (e.g. [dry](https://github.com/dmytro-yemelianov/dry)) as its pre-flight gate —
  a §23.6 to be written.
- **Timing/bus-signal data model**: [`peripheral-mapping.md`](peripheral-mapping.md)
  phases 1–3 are implemented for step timing; bus matching remains.
- **Simulated controller** (§29 step 9): designed in [`simulator.md`](simulator.md)
  (virtual clock, typed commands before wire frames, safety-envelope enforcement
  from the resolved profile, drift-gated trace goldens); implementation not started.
- Package schemas for chip/board/device/workflow kinds (§7–§9) land with the
  resolver slices that consume them; only `machine.schema.json` is normative today.
