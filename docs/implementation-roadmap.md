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
- [x] **4. `machine-parser` with source spans** — collect-all-diagnostics
  validation (version/kind, identifiers, package refs, quantity dimensions,
  intra-document references, transport parents) with dotted paths and **exact,
  exclusive source ranges** from a marked YAML event walk (`spans::SpanIndex`,
  yaml-rust2): every key and sequence item is indexed, aliases advance sequence
  positions, and multiline quoted/block scalars retain their physical end position;
  unrecorded paths locate at their nearest ancestor. A failed YAML scan discards its
  partial index. The original `path`/`line`/`column` fields remain as a compatibility
  projection, and the reusable index flows into resolver diagnostics.
- [x] **5. `machine-resolver` phases 1–7 and structured diagnostics** — the seven-phase
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
  errors are hard (E1131/E1134/E1206). Slice 10 completed §11.3 diagnostics:
  resolver errors inherit exact machine-source ranges; E1103/E1104 retain portable
  package-manifest ranges for every dependency constraint; template-expanded
  component claims retain their package-template ranges; E1200/E1314 link the current
  connector/timer claim to the original reservation through `related`, even when one
  or both claims came from a package template.
  Rich diagnostic JSON remains deterministic and older diagnostic JSON still
  deserializes with empty range/related fields.
- [x] **6. Electrical, timing, and safety validation** — first electrical check landed
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
  Slice 9 added **bus matching** (§9
  `requires.bus`): the bus family must appear in the connector's capabilities and
  the chip instance must declare a sufficient max_frequency (E1315, hard search
  filter, recorded in provenance); device packages now drive the expected connector
  kind for explicit claims, retiring the v0 attribute table wherever a device
  exists. Slice 12 added **DMA routing plus latency/jitter budgets**: device bus
  requirements name required DMA signals and maximum measured timing bounds; chip
  targets publish explicit routes and worst-case values; missing/excess capability
  is blocking and accepted evidence is recorded in assignment provenance. DMA
  ownership remains deferred to firmware allocation. Slice 13 completed **safe-state
  firmware partitioning** (§11.2 phase 11, §18.2): the safety action vocabulary is
  closed (`off`, `disabled`), heartbeat timeouts compile to positive 1 us ticks,
  logical actuators inherit only validated driver-socket resources and require class
  coverage just like direct outputs (E1501/E1506), and required sensors must resolve
  through an input connector on the same controller (E1503/E1504/E1507); a covered
  actuator without a concrete local resource is blocking (E1505), and multiple
  actions for one physical resource are rejected (E1508). The resolver emits
  controller-local bindings;
  lockfile v3 pins them; `dryer-firmware-build` emits the byte-stable, hashed
  `dryer.controller-safety/v1` artifact. The simulator consumes that artifact rather
  than rereading package policy. Golden:
  `examples/minimal-cartesian/controller-safety.mainboard.golden.json`; see
  [`firmware-build.md`](firmware-build.md). Slice 14 added **artifact planning**
  (§11.2 phase 12, §21.1): chip packages declare validated memory/boot budgets,
  target triple, toolchain, build profile, protocol/ABI versions, and feature flags;
  the resolver combines those with exact board/chip packages and selected native
  device drivers. Missing target metadata is blocking (E1600/E1601/E1602).
- [x] **7. `machine-lock` canonical serialization and hashing** — `dryer-machine-lock`
  produces a deterministic lockfile (canonical JSON bytes + `sha256:` lock hash; YAML
  on disk) binding the machine-source hash, exact package versions, resolver version,
  and per-controller resolved resources. Golden at
  `examples/minimal-cartesian/machine.lock`, drift-gated in CI. Slice 3 added the
  pinned `safety_profile` (id + manifest hash, §12). Slice 11 introduced lockfile v2
  and §6.6 **full package content digests**: a domain-separated, length-framed sha256
  over sorted portable paths and every regular-file byte; symlinks and non-UTF-8 paths
  are rejected, v1 locks remain readable, and flash planning blocks companion-file
  drift as well as manifest drift. Slice 13 introduced lockfile v3, requiring a
  versioned compiled safety partition for every controller while v1/v2 remain
  readable. Slice 14 introduced lockfile v4, pinning the resolved target triple,
  toolchain/build profile, memory and boot layout, protocol/ABI versions, sorted
  features, and exact native-driver packages; v3 locks remain readable. Slice 15
  introduced lockfile v5 and **portable registry provenance**: every lock records a
  validated logical registry id and portable non-`file:` URI plus a sha256 over the
  exact `registry.yaml` descriptor bytes. Descriptorless registries remain inspectable
  but cannot produce v5 locks; flash planning compares the live descriptor before
  reading board metadata or artifacts; legacy v1-v4 locks remain readable. Slice 16
  completed the remaining **reproducible output contract** (§21.1): build-plan v2
  records the exact format, stable path, byte length, and SHA-256 emitted by the
  deterministic `dryer.controller-image/v1` reference backend. The canonical image is
  lock-bound, inspectable, drift-gated, and independently verified by flash planning.
  It is explicitly `deployable: false`, so no planning path can mistake the reference
  configuration container for executable MCU firmware. A native target runtime and
  linker backend remain later firmware work, not an unpinned lockfile default.
- [ ] **8. Klipper config and build-parameter export** *(license/provenance record
  required first — §23.5)*
- [x] **9. Simulated controller and end-to-end golden tests** — `dryer-simulator`
  per [`simulator.md`](simulator.md): 1 µs virtual clock, typed command semantics
  (wire framing deliberately later), transport with seeded jitter/loss/duplication
  and link faults, bounded queue, first-order thermal plant + endstop homing with
  exact clamped motion, and the §18.1 edge-enforced safety envelope — heartbeat
  loss forces the COMPILED controller artifact's safe state within its integer timeout, resets
  latch faults and reject further commands. Byte-stable integer-only traces;
  the fixture job golden (`examples/minimal-cartesian/job-trace.golden`) is
  drift-gated by test with an UPDATE_TRACE regeneration path; seeded-fault
  determinism pinned (identical seeds ⇒ identical traces under jitter+loss+dup).
  A follow-up slice completed the single-controller queue contract: scheduled
  commands are accepted only within the reported lead/horizon window, must align to
  the 1 ms execution quantum, remain timestamp-ordered even when they arrive out of
  order, and execute only when due. Structured replay reports now include the first
  divergent index/tick plus expected and actual events, with a read-only replay CLI.
  Multi-controller clock skew stays explicitly deferred per the accepted design Q3;
  it requires a real synchronization protocol and multi-controller fixture rather
  than assumptions in this single-controller contract.
  Slice 17 adds the transport-independent `dryer.control/v1` command codec:
  bounded little-endian frames with sequence numbers, scheduled-command envelopes,
  CRC-32C integrity, byte goldens, malformed-input coverage, and simulator
  compatibility checks. The codec is a wire boundary only; no native host client
  or MCU firmware is implied by this slice.
  Slice 18 adds `dryer-control-client`: a synchronous outbound boundary with a
  reusable bounded frame buffer, sequence rollover/error semantics, and a
  simulator sink adapter. It deliberately does not perform OS I/O, clock sync,
  motion planning, workflow execution, or receive/ack handling yet.
  Slice 19 adds the protocol-only §16.4 queue-status response: message type `2`
  reuses the `dryer.control/v1` envelope with an exact 22-byte payload carrying
  `u16` capacity/fill, `u64` earliest/latest accepted controller ticks, and an
  underrun state bit. Reserved bits, lengths, prefixes, and checksums are
  strictly validated with byte-level goldens. Client receive handling exposes
  that strict decoder without adding transport policy; simulator wire responses
  remain follow-up work.
  Slice 20 adds the transport-independent §16.5 clock estimator:
  `dryer-clock-sync` consumes four-timestamp exchanges and widens offset intervals
  around the host midpoint using the physically measured host exchange span plus
  an explicit maximum relative-slew bound. It never subtracts controller processing
  duration from host duration in the bounded path. The estimator derives
  conservative signed drift bounds and projects checked integer-only
  controller-time confidence windows using at least the configured drift envelope.
  Host and controller ticks remain distinct clock domains already normalized to the
  same one-microsecond resolution; the estimator does not perform unit conversion.
  The standalone zero-slew observation helper retains its nominal
  processing-excluded residual metric. The crate is allocation-free and `no_std`;
  obtaining timestamps on a wire and applying queue/scheduling policy remain
  separate follow-up boundaries.
- [x] **21. Clock synchronization wire ABI** — `dryer-control-protocol` now
  defines type-3 clock requests and type-4 responses with strict fixed payload
  lengths, reserved-bit validation, CRC checks, and a sequence carried for
  external exchange correlation.
  `dryer-control-client` exposes transport-agnostic response decoding; host
  timestamp capture and estimator/session matching remain deliberately outside
  the codec.
- [x] **22. Clock exchange session boundary** — `dryer-control-client` now
  provides a one-outstanding event-driven session that captures caller clock
  readings immediately around transport handoff and complete-frame receipt,
  rejects unsolicited or mismatched responses, retires timed-out exchanges,
  prevents sequence reuse, and feeds `dryer-clock-sync`. It adds no OS I/O or
  blocking receive policy.
  Slice 23 adds simulator wire-frame processing (`SimController::process_wire_frame`),
  handling type-1 command execution, type-2 queue status response generation, and
  type-3/4 clock request/response frame exchanges. Workflow package resolution now
  includes declared machine workflows in the resolution package closure and validates
  invocation parameters against package schemas (`E1701`).
  Slice 24 adds `MultiControllerClockSync` to `dryer-control-client`, enabling
  event-driven clock sessions across multi-controller clusters (§16.5) with cluster-wide
  synchronization status tracking. Resolver workflow validation now enforces capability
  requirements (`E1702`), locked resource bindings (`E1703`), and workflow step action
  calls (`E1704`).
  Slice 25 adds `dryer-toolpath-auditor` (§23.6) for pre-flight toolpath auditing
  (kinematics position bounds, maximum feed rates, thermal safety ceilings), `FlashExecutor`
  hardware flashing trait + `MockFlashExecutor` in `dryer-firmware-flash`, and an end-to-end
  workflow lowering, pre-flight audit, wire encoding, and simulator job execution integration
  test suite (`e2e_job_execution.rs`).
  Slice 26 adds `dryer-cli` (`crates/dryer-cli`), providing a unified command-line tool for
  document checking (`dryer check`), canonical lockfile generation (`dryer lock`), USB discovery
  and dry-run flash planning (`dryer flash-plan`), pre-flight toolpath auditing (`dryer audit`),
  and job execution simulation (`dryer sim`).
  Slice 27 adds `dryer-controller-daemon` (`crates/controller-daemon`), providing host-side
  controller session management, heartbeat safety auditing, queue fill tracking, cluster clock
  synchronization, and `dryer daemon` subcommand integration.
  Slice 28 adds `dryer-transport-adapter` (`crates/transport-adapter`), providing stream transport
  adapters (`MemoryTransport`, `ChannelTransport`, `SerialTransportSpec`), frame stream delimitation,
  and loss recovery for `ControllerDaemon`.
  Slice 29 adds `dryer-workflow-runner` (`crates/workflow-runner`), providing streaming job queue
  management, pre-flight toolpath auditing, dynamic queue replenishment, horizon throttling, and underrun protection.
  Slice 30 extends `dryer-firmware-flash` with `NativeFlashExecutor`, supporting mutating hardware flash
  execution across `dfu-util`, `stm32flash`, `bossac`, and `picotool` methods with dry-run plan validation and drift protection.
  Slice 31 adds `dryer-gcode-lowerer` (`crates/gcode-lowerer`), transpiling slicer-emitted `.gcode` into
  typed `dryer_control_protocol::Command` streams, with `dryer-cli` gaining slicer `.gcode` file support
  alongside its existing job-JSON input.
  Slice 32 adds the Dryer OS web application control dashboard (`ui/`) and a `dryer ui` subcommand that
  opens it in the browser. These two slices were previously implemented without a corresponding roadmap
  entry; this line records them retroactively.
- [x] **10. Cross-platform flash discovery, dry-run plans, and hardware flash execution** —
  `dryer-firmware-flash` enumerates USB devices through native Linux, macOS, and
  Windows backends, normalizes their portable identity, and deterministically applies
  the locked board package's VID/PID plus optional exact string constraints. Missing
  and multi-match results are blocking states; selection never falls back to a partial
  match. Board packages carry validated flash recipes (method, bootloader-mode
  selector, transition instructions, sha256 verification, recovery). The planner
  verifies registry manifest/full-content drift and artifact bytes against the derived
  build-plan output pin, then emits versioned, byte-stable JSON containing expected
  current firmware, exact board identity, artifact format/deployment eligibility,
  signature slot, planned steps, and recovery. The fixture plan is
  drift-gated at `examples/minimal-cartesian/flash-plan.mainboard.golden.json`; see
  [`firmware-flash.md`](firmware-flash.md). `NativeFlashExecutor` implements mutating tool-based flash execution.
- [x] **§30 example coverage** — `examples/corexy` (single controller, template-
  expanded CoreXY, exercising I1132 driver-shadowing and I1133 template-limit
  contribution) and `examples/multi-mcu-toolhead` (two controllers, CAN
  toolhead, mirroring spec §5.1) now exist alongside `examples/minimal-cartesian`,
  each with full golden parity (lockfile, safety partition, build plan,
  controller image, flash plan) drift-gated in CI via table-driven tests.
  Fixed a pre-existing bug: all three machine-class template packages
  (`corexy-standard`, `delta-basic`, `toolchanger-corexy`) spelled the
  acceleration unit `mm/s2` instead of `mm/s^2`, which made every machine that
  let the template's `max_acceleration` flow through fail resolution; the
  regression test that should have caught this was a no-op (`.replace()`
  against a string that didn't occur in the fixture) and is now rewritten to
  actually swap the package under test. Added a board `downlinks:` section
  (`boards/example-mainboard@1.1.0`) plus three new resolver diagnostics
  making `transport.parent` a checked reference: `E1121` (port not declared
  as a downlink), `E1122` (child transport type disagrees with the parent
  downlink's type), `E1123` (controller parent cycle, including
  self-parenting). Deferred: the §19.2 cross-controller contract (state
  ownership, clock uncertainty, link-loss behavior) remains unmodeled — the
  multi-MCU example declares topology only; multi-controller execution
  traces remain blocked on design Q3 (no synchronization protocol);
  `transports.*.peripheral` and `downlinks.*` are not validated against the
  chip's peripheral table; `role:` remains an open, unvalidated attribute;
  parse-time intra-document reference checking still runs before template
  expansion (source components still cannot reference template-provided
  components). The CAN toolhead's committed build-plan/image goldens also
  list `usb_transport` in their firmware features because `chips/generic-mcu`'s
  `firmware.features` is chip-derived rather than transport-derived, not
  because the toolhead actually uses USB.

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
  is fully implemented (pin tables, wiring check, capabilities, step timing, bus matching).
- **Simulated controller** (§29 step 9): implemented — see the step-9 entry above.
- **Flash discovery and planning** (§29 step 10): implemented, including mutating
  hardware flash execution via `NativeFlashExecutor` (Slice 30) — see the step-10
  entry above.
- Package schemas for chip/board/device/workflow kinds (§7–§9) land with the
  resolver slices that consume them; only `machine.schema.json` is normative today.
