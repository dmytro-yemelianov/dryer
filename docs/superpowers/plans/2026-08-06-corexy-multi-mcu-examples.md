# CoreXY and multi-MCU toolhead example machines Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the `examples/corexy/` and `examples/multi-mcu-toolhead/` fixtures with full golden parity, fix the pre-existing `mm/s2` unit bug (and the no-op test that let it survive), make `transport.parent` a checked reference via a new board `downlinks:` model, and close out the roadmap/doc honesty pass — completing spec §30's requirement that Cartesian, CoreXY, and multi-MCU examples all validate.

**Architecture:** Follows `docs/superpowers/specs/2026-08-05-corexy-multi-mcu-examples-design.md` exactly. No new crates. Changes land in `machine-schema` (none needed), `package-model` (new `downlinks` field on board packages), `machine-resolver` (three new diagnostics in `targets.rs`), two new/migrated packages under `packages/boards/` and one fixed bug in `packages/machines/*`, two new example directories under `examples/`, and test-file rewrites in `machine-lock`, `firmware-build`, `firmware-flash` to go table-driven per example/controller.

**Tech Stack:** Rust (stable, 2021 edition workspace), `serde`/`serde_yaml`/`serde_json`, `cargo test --workspace`.

## Global Constraints

- `cargo test --workspace` must pass with zero failures after every task.
- Diagnostic codes follow the existing convention: `E11xx`/`E12xx`/`E13xx` are resolver-phase codes; `E06xx` is package/registry structure. New codes in this plan: `E1121`, `E1122`, `E1123`.
- JSON/binary goldens are byte-stable and drift-gated by test; never hand-edit a computed hash, digest, or lock field — regenerate via the project's existing "missing/mismatched golden prints the actual value" convention (see each golden test's panic message) or the `UPDATE_TRACE=1` env var for simulator traces, then copy the printed value into the file verbatim.
- Package content digests (`package-model/src/lib.rs`, `portable_relative_path`) are computed over paths *relative to the package's own root directory* — migrating a package from flat (`packages/boards/name/`) to versioned (`packages/boards/name/1.0.0/`) layout must not change any digest, since the set of relative paths (`package.yaml`, `README.md`, `LICENSE`) is unchanged. This is verified by the existing drift-gated golden tests, not assumed — if any existing golden's bytes change after the migration, stop and investigate; do not re-baseline.
- New example machines must resolve with **no errors and no warnings** (only informational `I1132`/`I1133` diagnostics are expected and must be asserted by exact content). `examples/minimal-cartesian` is left exactly as-is — including its existing `E1106` warning — since changing it would move its goldens.
- Every new/changed package still needs `README.md` and `LICENSE` (E0603 warns otherwise) at its version directory.
- Commit after each task.

---

### Task 1: Fix the `mm/s2` unit bug and the no-op regression test

**Files:**
- Modify: `crates/machine-resolver/src/tests.rs:1064-1090` (test `corexy_delta_and_toolchanger_templates_resolve_cleanly`)
- Modify: `packages/machines/corexy-standard/package.yaml:20`
- Modify: `packages/machines/delta-basic/package.yaml:20`
- Modify: `packages/machines/toolchanger-corexy/package.yaml:22`

**Context:** `UNIT_TABLE` in `crates/machine-schema/src/lib.rs:283-289` defines the acceleration unit as `mm/s^2` (note the caret). All three machine-class template packages spell it `mm/s2` (no caret), which is not a valid quantity — any machine that lets the template's `max_acceleration` flow through (i.e. doesn't override it in the machine source) fails resolution with `E1134`. The test meant to catch this, `corexy_delta_and_toolchanger_templates_resolve_cleanly`, builds its inputs with `fixture().replace("class: cartesian-basic", "class: corexy-standard")` — but `examples/minimal-cartesian/machine.yaml` contains zero occurrences of the string `class: cartesian-basic` (it references templates via `packages: - machines/cartesian-basic@1.0.0` and `kinematics: { type: cartesian, limits: {...} }`), so every `.replace()` call is a no-op: the test resolves the same cartesian fixture three times and passes without exercising any of the three template packages.

- [ ] **Step 1: Rewrite the test to swap the actual package reference and kinematics type, and drop the explicit limits so the template's (buggy) values flow through**

Replace lines 1064-1090 of `crates/machine-resolver/src/tests.rs` with:

```rust
fn fixture() -> String {
    // (unchanged, defined above at line 63 — do not duplicate; this is the same function)
}

#[test]
fn corexy_delta_and_toolchanger_templates_resolve_cleanly() {
    let reg = registry();

    // 1. CoreXY template machine — swap the machine-class package and the
    // kinematics type, and drop the explicit limits entirely so all three
    // template-supplied limits (including the buggy max_acceleration) get
    // quantity-parsed and validated.
    let corexy_src = fixture()
        .replace(
            "- machines/cartesian-basic@1.0.0",
            "- machines/corexy-standard@1.0.0",
        )
        .replace(
            "kinematics:\n  type: cartesian\n  limits:\n    max_velocity: 300 mm/s\n    max_acceleration: 3000 mm/s^2",
            "kinematics:\n  type: corexy",
        );
    let o = resolve_source(&corexy_src, &reg);
    assert!(o.is_ok(), "corexy resolution diagnostics: {:?}", o.diagnostics);

    // 2. Delta template machine
    let delta_src = fixture()
        .replace(
            "- machines/cartesian-basic@1.0.0",
            "- machines/delta-basic@1.0.0",
        )
        .replace(
            "kinematics:\n  type: cartesian\n  limits:\n    max_velocity: 300 mm/s\n    max_acceleration: 3000 mm/s^2",
            "kinematics:\n  type: delta",
        );
    let o = resolve_source(&delta_src, &reg);
    assert!(o.is_ok(), "delta resolution diagnostics: {:?}", o.diagnostics);

    // 3. Toolchanger template machine
    let toolchanger_src = fixture()
        .replace(
            "- machines/cartesian-basic@1.0.0",
            "- machines/toolchanger-corexy@1.0.0",
        )
        .replace(
            "kinematics:\n  type: cartesian\n  limits:\n    max_velocity: 300 mm/s\n    max_acceleration: 3000 mm/s^2",
            "kinematics:\n  type: corexy",
        );
    let o = resolve_source(&toolchanger_src, &reg);
    assert!(o.is_ok(), "toolchanger resolution diagnostics: {:?}", o.diagnostics);
}
```

(Do not literally duplicate the `fn fixture()` definition — it already exists at line 63; only replace the test function body itself, lines 1064-1090.)

- [ ] **Step 2: Run the test and confirm it now fails against the unfixed packages**

Run: `cargo test -p dryer-machine-resolver corexy_delta_and_toolchanger_templates_resolve_cleanly -- --nocapture`

Expected: FAIL, with a diagnostic dump containing `E1134` and the message `'5000 mm/s2' is not a valid quantity` (for the corexy case; the delta and toolchanger cases will show their own `3000 mm/s2` / `6000 mm/s2` failures). This confirms the rewritten test now actually exercises the broken packages.

- [ ] **Step 3: Fix the unit bug in all three machine-class packages**

In `packages/machines/corexy-standard/package.yaml`, line 20:
```yaml
      max_acceleration: 5000 mm/s2
```
→
```yaml
      max_acceleration: 5000 mm/s^2
```

In `packages/machines/delta-basic/package.yaml`, line 20:
```yaml
      max_acceleration: 3000 mm/s2
```
→
```yaml
      max_acceleration: 3000 mm/s^2
```

In `packages/machines/toolchanger-corexy/package.yaml`, line 22:
```yaml
      max_acceleration: 6000 mm/s2
```
→
```yaml
      max_acceleration: 6000 mm/s^2
```

These are edited in place at `1.0.0` rather than version-bumped: no lockfile, golden, or example pins any of them today, so there is no published content digest to invalidate.

- [ ] **Step 4: Run the test again and confirm it passes**

Run: `cargo test -p dryer-machine-resolver corexy_delta_and_toolchanger_templates_resolve_cleanly -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Run the full workspace test suite**

Run: `cargo test --workspace`

Expected: all pass, 0 failures (this change touches only `machine-resolver` and packages nothing else pins yet, so no other crate's goldens should move).

- [ ] **Step 6: Commit**

```bash
git add crates/machine-resolver/src/tests.rs packages/machines/corexy-standard/package.yaml packages/machines/delta-basic/package.yaml packages/machines/toolchanger-corexy/package.yaml
git commit -m "fix: mm/s2 -> mm/s^2 in machine-class templates; make the template test actually swap packages"
```

---

### Task 2: Add `downlinks:` to the board package model

**Files:**
- Modify: `crates/package-model/src/board.rs`

**Interfaces:**
- Produces: `pub struct Downlink { pub kind: String }` (deserialized from YAML `type:`) and a new field `pub downlinks: BTreeMap<String, Downlink>` on `BoardPackageFile`, defaulting to empty when absent. Task 3 (`machine-resolver/src/targets.rs`) reads `payload.downlinks` (a `BTreeMap<String, Downlink>`) where `payload: BoardPackageFile`.

**Context:** A board's existing `transports:` map (keyed by transport *type*, e.g. `usb`, `can`) describes the uplink a controller *uses*, and is already checked against `controller.transport.kind` (E1120 in `targets.rs`). It cannot express a port a board offers *to children* (`parent: mainboard.can0`) — and modeling that as a connector was rejected because connector claims are exclusive while CAN is multi-drop (legitimate wiring has two children sharing one bus). This task adds the new, separate `downlinks:` section.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `crates/package-model/src/board.rs` (after the existing three tests, same `use` statements apply):

```rust
    #[test]
    fn downlinks_parse_when_declared() {
        let board: BoardPackageFile = serde_yaml::from_str(
            r#"
package:
  namespace: boards
  name: with-downlink
  version: 1.0.0
  kind: board
downlinks:
  can0:
    type: can
"#,
        )
        .unwrap();
        assert_eq!(board.downlinks.len(), 1);
        assert_eq!(board.downlinks["can0"].kind, "can");
    }

    #[test]
    fn downlinks_default_to_empty_when_absent() {
        let board: BoardPackageFile = serde_yaml::from_str(
            r#"
package:
  namespace: boards
  name: no-downlink
  version: 1.0.0
  kind: board
"#,
        )
        .unwrap();
        assert!(board.downlinks.is_empty());
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `cargo test -p dryer-package-model downlinks -- --nocapture`

Expected: compile error, `no field 'downlinks' on type 'BoardPackageFile'` (the field does not exist yet).

- [ ] **Step 3: Add the `downlinks` field and `Downlink` struct**

In `crates/package-model/src/board.rs`, modify `BoardPackageFile`:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct BoardPackageFile {
    pub package: crate::PackageIdentity,
    /// Chip package reference (`chips/stm32f446re@1.0.0`); optional in
    /// Phase 0 fixtures, required once chip packages exist.
    #[serde(default)]
    pub chip: Option<String>,
    #[serde(default)]
    pub hardware: Option<Hardware>,
    #[serde(default)]
    pub connectors: BTreeMap<String, Connector>,
    #[serde(default)]
    pub transports: BTreeMap<String, Transport>,
    /// Ports this board offers to *child* controllers (e.g. a mainboard's
    /// CAN bus that a toolhead board attaches to via
    /// `transport: { parent: mainboard.can0 }`). Distinct from `transports`,
    /// which describes the uplink this board's own controller uses: two
    /// child controllers may legitimately share one downlink (CAN is
    /// multi-drop), which an exclusive connector claim could not express.
    #[serde(default)]
    pub downlinks: BTreeMap<String, Downlink>,
    /// Board-specific flashing recipes. These describe how to select a
    /// bootloader device and how an operator can recover it; they never
    /// contain a machine-specific controller identity.
    #[serde(default)]
    pub flash: Option<FlashConfig>,
}
```

Add the new struct near `Transport`:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct Downlink {
    #[serde(rename = "type")]
    pub kind: String,
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dryer-package-model downlinks -- --nocapture`

Expected: PASS, both tests.

- [ ] **Step 5: Run the full package-model test suite to confirm no regression**

Run: `cargo test -p dryer-package-model`

Expected: all pass, including `the_fixture_board_payload_parses_with_typed_quantities` (unaffected — `example-mainboard@1.0.0` has no `downlinks:` section yet, so `payload.downlinks` is simply empty for it, and that test asserts nothing about downlinks).

- [ ] **Step 6: Commit**

```bash
git add crates/package-model/src/board.rs
git commit -m "feat(package-model): add optional downlinks section to board packages"
```

---

### Task 3: Add E1121/E1122/E1123 resolver diagnostics with negative fixture coverage

**Files:**
- Modify: `crates/machine-resolver/src/targets.rs`
- Modify: `crates/machine-resolver/src/tests.rs` (add new tests near the existing E1120 coverage)

**Interfaces:**
- Consumes: `BoardPackageFile::downlinks: BTreeMap<String, Downlink>` from Task 2; `MachineDoc::controllers: BTreeMap<String, Controller>` and `Controller::transport: Transport { kind: String, parent: Option<String> }` from `dryer_machine_schema` (already exist, unchanged).
- Produces: three new diagnostic codes usable by later tasks' fixtures: `E1121` (parent names an undeclared downlink port), `E1122` (child transport type disagrees with the parent downlink's declared type), `E1123` (controller parent cycle, including self-parenting).

**Context:** `machine-parser` (`crates/machine-parser/src/lib.rs:240-263`) already validates that `transport.parent` is shaped `controller.port` and names a *known controller* (E0502/E0503) — but never checks that the named *port* is one the parent board actually offers, or that the child's transport type agrees with it, or that the parent chain terminates. `targets.rs::load()` (`crates/machine-resolver/src/targets.rs:12-124`) is where board payloads are already loaded per controller and where the existing `E1120` transport-type check lives (lines 47-58); this task adds two more passes to the same function, run after the main per-controller loop so a controller's own board payload is loaded regardless of alphabetical `BTreeMap` iteration order relative to its parent's.

- [ ] **Step 1: Write the failing tests**

Add to `crates/machine-resolver/src/tests.rs`, near the other `registry_with_*` helpers (after `registry_with_safety_fixture`, around line 30):

```rust
fn registry_with_downlink_board() -> LocalRegistry {
    let mut registry = registry();
    let package = registry
        .packages
        .iter_mut()
        .find(|package| package.reference.to_string() == "boards/example-mainboard@1.0.0")
        .expect("example mainboard");
    package.dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mainboard-with-downlinks");
    registry
}

fn two_controller_source(child_kind: &str, parent_ref: &str) -> String {
    format!(
        r#"api_version: dryer.machine/v0.1
kind: Machine
metadata:
  name: two-controller-test
packages:
  - boards/example-mainboard@1.0.0
  - devices/tmc2209@2.1.0
  - machines/cartesian-basic@1.0.0
controllers:
  mainboard:
    board: boards/example-mainboard
    transport:
      type: usb
  child:
    board: boards/example-mainboard
    transport:
      type: {child_kind}
      parent: {parent_ref}
components:
  x_motor:
    type: stepper_motor
    driver: x_driver
    role: axis.x
  x_driver:
    type: tmc2209
    connected_to: mainboard.motor0
  hotend_heater:
    type: heater
    output: mainboard.heater0
    sensor: hotend_sensor
    current: 2 A
  hotend_sensor:
    type: thermistor
    model: generic-3950
    input: mainboard.thermistor0
kinematics:
  type: cartesian
  limits:
    max_velocity: 300 mm/s
    max_acceleration: 3000 mm/s^2
safety:
  profile: safety-profiles/desktop-fdm
"#
    )
}

#[test]
fn transport_parent_naming_an_undeclared_downlink_is_e1121() {
    // registry() alone: example-mainboard@1.0.0 declares no downlinks at all.
    let src = two_controller_source("can", "mainboard.can0");
    let o = resolve_source(&src, &registry());
    let d = o
        .diagnostics
        .iter()
        .find(|d| d.code == "E1121")
        .expect("E1121 diagnostic");
    assert!(d.message.contains("can0"));
    assert!(d.message.contains("boards/example-mainboard"));
}

#[test]
fn transport_type_disagreeing_with_parent_downlink_is_e1122_not_e1121() {
    let src = two_controller_source("usb", "mainboard.can0");
    let o = resolve_source(&src, &registry_with_downlink_board());
    assert!(
        !o.diagnostics.iter().any(|d| d.code == "E1121"),
        "port exists on the board, E1121 must not fire: {:?}",
        o.diagnostics
    );
    let d = o
        .diagnostics
        .iter()
        .find(|d| d.code == "E1122")
        .expect("E1122 diagnostic");
    assert!(d.message.contains("usb"));
    assert!(d.message.contains("can"));
}

#[test]
fn matching_downlink_port_and_type_raises_neither_e1121_nor_e1122() {
    let src = two_controller_source("can", "mainboard.can0");
    let o = resolve_source(&src, &registry_with_downlink_board());
    assert!(
        !o.diagnostics
            .iter()
            .any(|d| d.code == "E1121" || d.code == "E1122"),
        "matching port+type must raise neither: {:?}",
        o.diagnostics
    );
}

#[test]
fn self_parenting_controller_is_e1123() {
    // child's own transport.parent points at itself.
    let src = two_controller_source("can", "child.can0");
    let o = resolve_source(&src, &registry_with_downlink_board());
    let d = o
        .diagnostics
        .iter()
        .find(|d| d.code == "E1123")
        .expect("E1123 diagnostic for self-parenting");
    assert!(d.message.contains("child"));
}

#[test]
fn mutual_parent_cycle_is_e1123() {
    let src = r#"api_version: dryer.machine/v0.1
kind: Machine
metadata:
  name: cycle-test
packages:
  - boards/example-mainboard@1.0.0
  - devices/tmc2209@2.1.0
  - machines/cartesian-basic@1.0.0
controllers:
  a:
    board: boards/example-mainboard
    transport:
      type: can
      parent: b.can0
  b:
    board: boards/example-mainboard
    transport:
      type: can
      parent: a.can0
components:
  x_motor:
    type: stepper_motor
    driver: x_driver
    role: axis.x
  x_driver:
    type: tmc2209
    connected_to: a.motor0
  hotend_heater:
    type: heater
    output: a.heater0
    sensor: hotend_sensor
    current: 2 A
  hotend_sensor:
    type: thermistor
    model: generic-3950
    input: a.thermistor0
kinematics:
  type: cartesian
  limits:
    max_velocity: 300 mm/s
    max_acceleration: 3000 mm/s^2
safety:
  profile: safety-profiles/desktop-fdm
"#;
    let o = resolve_source(src, &registry_with_downlink_board());
    let cycles: Vec<_> = o.diagnostics.iter().filter(|d| d.code == "E1123").collect();
    assert_eq!(cycles.len(), 1, "one cycle reported once, not twice: {:?}", o.diagnostics);
}
```

Then create the test fixture board `crates/machine-resolver/tests/fixtures/mainboard-with-downlinks/package.yaml` (copy of `packages/boards/example-mainboard/package.yaml`'s content, plus a `downlinks:` section), with matching `README.md` and `LICENSE`:

`crates/machine-resolver/tests/fixtures/mainboard-with-downlinks/package.yaml`:
```yaml
package:
  namespace: boards
  name: example-mainboard
  version: 1.0.0
  kind: board

chip: chips/generic-mcu

hardware:
  manufacturer: Example
  model: Mainboard
  revision: "1.0"

connectors:
  motor0:
    kind: stepper_driver_socket
    pins:
      step: PE11
      dir: PE10
      enable: PE9
      uart: PE7
    voltage_domain: logic_3v3

  motor1:
    kind: stepper_driver_socket
    pins:
      step: PD5
      dir: PD4
      enable: PD3
      uart: PD1
    voltage_domain: logic_3v3

  motor2:
    kind: stepper_driver_socket
    pins:
      step: PE13
      dir: PE14
      enable: PE15
      uart: PE8
    voltage_domain: logic_3v3

  motor3:
    kind: stepper_driver_socket
    pins:
      step: PD7
      dir: PD8
      enable: PD9
      uart: PD10
    voltage_domain: logic_3v3

  heater0:
    kind: power_output
    pin: PA2
    max_current: 5 A
    supply: vin

  thermistor0:
    kind: analog_input
    pin: PF4
    pullup: 4.7 kOhm

  accel0:
    kind: accessory_socket
    pins:
      sck: PA5
      miso: PA6
      mosi: PA7
      cs: PB0
    voltage_domain: logic_3v3

  accel1:
    kind: accessory_socket
    pins:
      sck: PD11
      miso: PD12
      mosi: PD13
      cs: PD14
    voltage_domain: logic_3v3

transports:
  usb:
    peripheral: usb_fs

downlinks:
  can0:
    type: can

flash:
  default_method: dfu
  methods:
    dfu:
      transport: usb
      select:
        usb_vid: "0x1209"
        usb_pid: "0xd003"
      enter_bootloader:
        - Hold BOOT while tapping RESET, then release BOOT.
      verify: sha256
      recovery:
        - Re-enter DFU mode and retry with the same locked artifact.
        - If DFU is unavailable, use the board's hardware recovery probe.
```

`crates/machine-resolver/tests/fixtures/mainboard-with-downlinks/README.md`:
```markdown
Test-only fixture: `boards/example-mainboard@1.0.0` plus a `downlinks.can0`
section, used to test E1121/E1122/E1123 before the real
`boards/example-mainboard@1.1.0` package exists.
```

`crates/machine-resolver/tests/fixtures/mainboard-with-downlinks/LICENSE`:
```
Apache-2.0 (see the repository LICENSE)
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p dryer-machine-resolver e1121 e1122 e1123 -- --nocapture` (also try `transport_parent`, `mutual_parent_cycle`, `self_parenting`, `matching_downlink` as substrings if the above doesn't match all five)

Expected: FAIL — `E1121`/`E1122`/`E1123` are not yet produced by the resolver, so each `.expect(...)` panics with `None`.

- [ ] **Step 3: Implement the checks in `targets.rs`**

In `crates/machine-resolver/src/targets.rs`, after the closing brace of the main `for (controller_name, controller) in &doc.controllers { ... }` loop (currently ending right before the final `ControllerTargets { boards, chips, chip_refs }` return), insert two new passes:

```rust
    // --- transport.parent structural checks (E1121/E1122) ---
    // A second pass: the parent controller's board may sort after the
    // child's in `doc.controllers` (a BTreeMap), so this only runs once
    // every controller's board payload has been loaded above. Shape
    // (`controller.port`) and "known controller" are already checked by
    // the parser (E0502/E0503); a board that itself failed to load is
    // already diagnosed above and is silently skipped here.
    for (name, ctrl) in &doc.controllers {
        let Some(parent) = &ctrl.transport.parent else {
            continue;
        };
        let Some((pctrl, port)) = parent.split_once('.') else {
            continue;
        };
        let Some(parent_board) = boards.get(pctrl) else {
            continue;
        };
        match parent_board.downlinks.get(port) {
            None => {
                let available: Vec<&str> =
                    parent_board.downlinks.keys().map(String::as_str).collect();
                let mut d = Diagnostic::error(
                    "E1121",
                    format!(
                        "controller '{name}': transport.parent '{parent}' names port '{port}', which board '{}' does not declare as a downlink",
                        doc.controllers[pctrl].board
                    ),
                )
                .at(format!("controllers.{name}.transport.parent"));
                d = if available.is_empty() {
                    d.suggest(format!(
                        "'{}' declares no downlinks",
                        doc.controllers[pctrl].board
                    ))
                } else {
                    d.suggest(format!("available downlinks: {}", available.join(", ")))
                };
                diagnostics.push(d);
            }
            Some(downlink) if downlink.kind != ctrl.transport.kind => {
                diagnostics.push(
                    Diagnostic::error(
                        "E1122",
                        format!(
                            "controller '{name}': transport type '{}' disagrees with parent downlink '{parent}' type '{}'",
                            ctrl.transport.kind, downlink.kind
                        ),
                    )
                    .at(format!("controllers.{name}.transport.type")),
                );
            }
            Some(_) => {}
        }
    }

    // --- controller parent cycles (E1123): whole-graph check ---
    // Every controller has at most one parent, so a cycle is detected by
    // walking each controller's parent chain until it either terminates
    // (no `transport.parent`) or revisits a controller already seen on
    // this walk (which also catches self-parenting on the first step).
    // `reported` avoids emitting the same cycle once per member.
    let mut reported: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for start in doc.controllers.keys() {
        if reported.contains(start) {
            continue;
        }
        let mut path = vec![start.clone()];
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        seen.insert(start.as_str());
        let mut current = start.as_str();
        loop {
            let Some(ctrl) = doc.controllers.get(current) else {
                break;
            };
            let Some(parent) = &ctrl.transport.parent else {
                break;
            };
            let Some((pctrl, _)) = parent.split_once('.') else {
                break;
            };
            if !seen.insert(pctrl) {
                diagnostics.push(
                    Diagnostic::error(
                        "E1123",
                        format!(
                            "controller '{start}': transport.parent chain cycles back through '{pctrl}'"
                        ),
                    )
                    .at(format!("controllers.{start}.transport.parent")),
                );
                reported.extend(path.iter().cloned());
                reported.insert(pctrl.to_string());
                break;
            }
            path.push(pctrl.to_string());
            current = pctrl;
        }
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p dryer-machine-resolver -- --nocapture` (run the whole crate; the new tests don't share a common substring)

Expected: PASS — all five new tests, plus every pre-existing `machine-resolver` test unaffected (E1120 test still passes since it doesn't touch `downlinks`).

- [ ] **Step 5: Run the full workspace test suite**

Run: `cargo test --workspace`

Expected: all pass, 0 failures.

- [ ] **Step 6: Commit**

```bash
git add crates/machine-resolver/src/targets.rs crates/machine-resolver/src/tests.rs crates/machine-resolver/tests/fixtures/mainboard-with-downlinks
git commit -m "feat(resolver): validate transport.parent against board downlinks (E1121/E1122) and detect controller parent cycles (E1123)"
```

---

### Task 4: Migrate `example-mainboard` to a versioned layout and add `example-toolhead@1.0.0`

**Files:**
- Create: `packages/boards/example-mainboard/1.0.0/package.yaml` (byte-identical to current `packages/boards/example-mainboard/package.yaml`)
- Create: `packages/boards/example-mainboard/1.0.0/README.md`, `packages/boards/example-mainboard/1.0.0/LICENSE` (byte-identical copies)
- Create: `packages/boards/example-mainboard/1.1.0/package.yaml` (= 1.0.0 plus `downlinks.can0`)
- Create: `packages/boards/example-mainboard/1.1.0/README.md`, `packages/boards/example-mainboard/1.1.0/LICENSE`
- Delete: `packages/boards/example-mainboard/package.yaml`, `packages/boards/example-mainboard/README.md`, `packages/boards/example-mainboard/LICENSE` (the old flat-layout files)
- Create: `packages/boards/example-toolhead/1.0.0/package.yaml`, `.../README.md`, `.../LICENSE`

**Context:** `LocalRegistry::load` (`crates/package-model/src/lib.rs:433-499`) treats a package directory as single-version if `dir.join("package.yaml")` exists directly, or multi-version if instead its subdirectories are version directories (`E0606` enforces the directory name matches the manifest version). `example-mainboard` is currently single-version; this task migrates it to multi-version so `1.1.0` can add `downlinks` without touching the `1.0.0` bytes any existing lock/golden pins.

- [ ] **Step 1: Migrate `example-mainboard` to the versioned layout**

```bash
mkdir -p packages/boards/example-mainboard/1.0.0
git mv packages/boards/example-mainboard/package.yaml packages/boards/example-mainboard/1.0.0/package.yaml
git mv packages/boards/example-mainboard/README.md packages/boards/example-mainboard/1.0.0/README.md
git mv packages/boards/example-mainboard/LICENSE packages/boards/example-mainboard/1.0.0/LICENSE
```

- [ ] **Step 2: Run the full workspace test suite to confirm zero drift from the migration alone**

Run: `cargo test --workspace`

Expected: all pass, 0 failures, with byte-identical goldens (this is the assertion from the design doc: the migration must be verified by the existing drift gates, not assumed). If anything fails here, stop — the migration broke something and must be investigated before continuing to Step 3.

- [ ] **Step 3: Add `example-mainboard@1.1.0`**

Create `packages/boards/example-mainboard/1.1.0/package.yaml` — identical to `1.0.0/package.yaml` except the version and the new `downlinks` section (inserted after `transports:`, before the `flash:` comment):

```yaml
package:
  namespace: boards
  name: example-mainboard
  version: 1.1.0
  kind: board

chip: chips/generic-mcu

hardware:
  manufacturer: Example
  model: Mainboard
  revision: "1.0"

connectors:
  motor0:
    kind: stepper_driver_socket
    pins:
      step: PE11
      dir: PE10
      enable: PE9
      uart: PE7
    voltage_domain: logic_3v3

  motor1:
    kind: stepper_driver_socket
    pins:
      step: PD5
      dir: PD4
      enable: PD3
      uart: PD1
    voltage_domain: logic_3v3

  motor2:
    kind: stepper_driver_socket
    pins:
      step: PE13
      dir: PE14
      enable: PE15
      uart: PE8
    voltage_domain: logic_3v3

  motor3:
    kind: stepper_driver_socket
    pins:
      step: PD7
      dir: PD8
      enable: PD9
      uart: PD10
    voltage_domain: logic_3v3

  heater0:
    kind: power_output
    pin: PA2
    max_current: 5 A
    supply: vin

  thermistor0:
    kind: analog_input
    pin: PF4
    pullup: 4.7 kOhm

  accel0:
    kind: accessory_socket
    pins:
      sck: PA5
      miso: PA6
      mosi: PA7
      cs: PB0
    voltage_domain: logic_3v3

  accel1:
    kind: accessory_socket
    pins:
      sck: PD11
      miso: PD12
      mosi: PD13
      cs: PD14
    voltage_domain: logic_3v3

transports:
  usb:
    peripheral: usb_fs

# The mainboard's CAN bus, offered to child controllers (e.g. a CAN
# toolhead board) via transport.parent. Distinct from `transports` above,
# which describes this board's own USB uplink.
downlinks:
  can0:
    type: can

# Synthetic bootloader identity used by the flash-planner fixture. A real
# board package must publish the VID/PID it exposes while in flashing mode.
flash:
  default_method: dfu
  methods:
    dfu:
      transport: usb
      select:
        usb_vid: "0x1209"
        usb_pid: "0xd003"
      enter_bootloader:
        - Hold BOOT while tapping RESET, then release BOOT.
      verify: sha256
      recovery:
        - Re-enter DFU mode and retry with the same locked artifact.
        - If DFU is unavailable, use the board's hardware recovery probe.
```

`packages/boards/example-mainboard/1.1.0/README.md`:
```markdown
# boards/example-mainboard

Synthetic reference mainboard used by Phase 0/1 fixtures and resolver tests.
Not a real product; connector layout loosely follows common 32-bit printer boards.

`1.1.0` adds `downlinks.can0`, the CAN bus this board offers to a child
controller (see `boards/example-toolhead`). `1.0.0` is unchanged and remains
the version every existing example pins.
```

`packages/boards/example-mainboard/1.1.0/LICENSE`:
```
Apache-2.0 (see the repository LICENSE)
```

- [ ] **Step 4: Add `example-toolhead@1.0.0`**

Create `packages/boards/example-toolhead/1.0.0/package.yaml`:

```yaml
package:
  namespace: boards
  name: example-toolhead
  version: 1.0.0
  kind: board

chip: chips/generic-mcu

hardware:
  manufacturer: Example
  model: Toolhead
  revision: "1.0"

connectors:
  motor0:
    kind: stepper_driver_socket
    pins:
      step: PE11
      dir: PE10
      enable: PE9
      uart: PE7
    voltage_domain: logic_3v3

  heater0:
    kind: power_output
    pin: PA2
    max_current: 5 A
    supply: vin

  thermistor0:
    kind: analog_input
    pin: PF4
    pullup: 4.7 kOhm

  accel0:
    kind: accessory_socket
    pins:
      sck: PA5
      miso: PA6
      mosi: PA7
      cs: PB0
    voltage_domain: logic_3v3

transports:
  can:
    peripheral: can1

# Synthetic bootloader identity, distinct from example-mainboard's. The
# runtime transport is CAN but the flash method is USB DFU — this matches
# real CAN toolhead boards, and keeps flash discovery honest: selection
# matches the board's own bootloader identity, never its runtime transport.
flash:
  default_method: dfu
  methods:
    dfu:
      transport: usb
      select:
        usb_vid: "0x1209"
        usb_pid: "0xd004"
      enter_bootloader:
        - Hold BOOT while tapping RESET, then release BOOT.
      verify: sha256
      recovery:
        - Re-enter DFU mode and retry with the same locked artifact.
        - If DFU is unavailable, use the board's hardware recovery probe.
```

Every pin above (`PE11`, `PE10`, `PE9`, `PE7`, `PA2`, `PF4`, `PA5`, `PA6`, `PA7`, `PB0`) already exists in `chips/generic-mcu@1.5.0`'s `pin_functions` table (verified: `packages/chips/generic-mcu/1.5.0/package.yaml`), so no chip package changes are required and E1312 (unknown pin) will not fire.

`packages/boards/example-toolhead/1.0.0/README.md`:
```markdown
# boards/example-toolhead

Synthetic reference CAN toolhead board used by `examples/multi-mcu-toolhead`.
Not a real product. Its runtime transport is CAN (`transports.can`) but its
flash method is USB DFU with a bootloader identity distinct from
`boards/example-mainboard` — this matches real CAN toolhead boards, where
flashing happens over a local USB connection independent of the runtime bus.
```

`packages/boards/example-toolhead/1.0.0/LICENSE`:
```
Apache-2.0 (see the repository LICENSE)
```

- [ ] **Step 5: Add a package-model test proving the multi-version board resolves correctly**

Add to `crates/package-model/src/board.rs`'s test module:

```rust
    #[test]
    fn example_mainboard_has_two_versions_and_only_the_newer_declares_downlinks() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = crate::LocalRegistry::load(&root);
        let v1_0_0 = reg
            .find_version(
                "boards",
                "example-mainboard",
                &"1.0.0".parse().unwrap(),
            )
            .expect("1.0.0 still resolvable")
            .board_payload()
            .unwrap();
        assert!(v1_0_0.downlinks.is_empty());
        let latest = reg
            .find("boards", "example-mainboard")
            .expect("highest version")
            .board_payload()
            .unwrap();
        assert_eq!(latest.package.version.to_string(), "1.1.0");
        assert_eq!(latest.downlinks["can0"].kind, "can");
    }

    #[test]
    fn example_toolhead_board_payload_parses() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = crate::LocalRegistry::load(&root);
        let board = reg
            .find("boards", "example-toolhead")
            .expect("example toolhead board")
            .board_payload()
            .unwrap();
        assert_eq!(board.connectors["motor0"].kind, "stepper_driver_socket");
        assert!(board.transports.contains_key("can"));
        assert_eq!(
            board.flash.unwrap().methods["dfu"].select.usb_pid,
            0xd004
        );
    }
```

(`find_version` already exists per `crates/package-model/src/lib.rs:594`; confirm its exact signature by reading that function before use — it takes `&semver::Version`, hence `.parse().unwrap()` on a `&str`.)

- [ ] **Step 6: Run the package-model test suite**

Run: `cargo test -p dryer-package-model`

Expected: all pass, including the two new tests and the pre-existing `the_fixture_board_payload_parses_with_typed_quantities` (which now resolves to `1.1.0` via `reg.find`, a superset of `1.0.0`'s connectors/flash — still satisfies every assertion in that test).

- [ ] **Step 7: Run the full workspace test suite**

Run: `cargo test --workspace`

Expected: all pass, 0 failures. In particular confirm no existing golden moved (`git status` should show no unexpected diffs to files under `examples/`).

- [ ] **Step 8: Commit**

```bash
git add packages/boards/example-mainboard packages/boards/example-toolhead crates/package-model/src/board.rs
git commit -m "feat(packages): migrate example-mainboard to versioned layout, add 1.1.0 downlinks, add example-toolhead board"
```

---

### Task 5: Add `examples/corexy/` with full golden parity

**Files:**
- Create: `examples/corexy/machine.yaml`
- Create: `examples/corexy/README.md`
- Create: `examples/corexy/machine.lock` (generated)
- Create: `examples/corexy/controller-safety.mainboard.golden.json` (generated)
- Create: `examples/corexy/controller-build-plan.mainboard.golden.json` (generated)
- Create: `examples/corexy/controller-image.mainboard.golden.json` (generated)
- Create: `examples/corexy/usb-inventory.fixture.json`
- Create: `examples/corexy/flash-plan.mainboard.golden.json` (generated)
- Create: `examples/corexy/job-trace.golden` (generated)
- Modify: `crates/machine-resolver/src/tests.rs` (add a resolution-cleanliness assertion for this example)

**Note on naming:** this task creates the corexy goldens directly under the **final** per-controller naming scheme (`<artifact>.mainboard.golden.json`), since this example is new — there is nothing to rename here. Task 7 renames `minimal-cartesian`'s *existing* goldens into this same scheme and rewrites the three crates' golden tests to be table-driven over both examples at once. Until Task 7 lands, the table-driven test infrastructure doesn't exist yet, so this task instead adds one-off tests per artifact, modeled directly on the existing `minimal-cartesian` tests, so the corexy goldens are pinned as they're created rather than left uncovered.

- [ ] **Step 1: Write `examples/corexy/machine.yaml`**

```yaml
api_version: dryer.machine/v0.1
kind: Machine

metadata:
  name: corexy
  description: Single-controller CoreXY machine using the corexy-standard template

packages:
  - boards/example-mainboard@1.0.0
  - devices/tmc2209@2.1.0
  - machines/corexy-standard@1.0.0
  - safety-profiles/desktop-fdm@1.0.0

controllers:
  mainboard:
    board: boards/example-mainboard
    transport:
      type: usb

components:
  x_motor:
    type: stepper_motor
    driver: x_driver
    role: axis.x

  x_driver:
    type: tmc2209
    connected_to: mainboard.motor0

  y_motor:
    type: stepper_motor
    driver: y_driver
    role: axis.y

  y_driver:
    type: tmc2209
    connected_to: mainboard.motor1

  z_motor:
    type: stepper_motor
    driver: z_driver

  z_driver:
    type: tmc2209
    connected_to: mainboard.motor2

  hotend_heater:
    type: heater
    output: mainboard.heater0
    sensor: hotend_sensor
    current: 2 A

  hotend_sensor:
    type: thermistor
    model: generic-3950
    input: mainboard.thermistor0

kinematics:
  type: corexy

safety:
  profile: safety-profiles/desktop-fdm
```

Both new examples pin the safety profile explicitly (`safety-profiles/desktop-fdm@1.0.0` in `packages:`), unlike `minimal-cartesian` which leaves it implicit and resolves with an `E1106` warning — so this example must resolve with **zero** diagnostics other than `I1132`/`I1133`.

`x_motor`/`y_motor`/`z_motor` name their own drivers via `driver:`, and each driver is declared as a real source component (not left to the template), because template-injected components are invisible to the parser's intra-document reference check, which runs before expansion — a source `driver: x_driver` pointing at a template-only component would fail `E0501`. `corexy-standard`'s template also declares `x_driver`/`y_driver`/`z_driver` (all `type: tmc2209`, no `connected_to`); since the source declares the same three names explicitly, the source versions shadow the template's (informational `I1132`, one per driver). No `limits:` are declared under `kinematics`, so all three (`max_velocity`, `max_acceleration`, `max_z_velocity`) come from the template (informational `I1133`, one per limit) — this is exactly what exercises the unit bug fixed in Task 1.

- [ ] **Step 2: Write `examples/corexy/README.md`**

```markdown
# examples/corexy

Single-controller CoreXY machine using the `machines/corexy-standard` template.

## Covers

- Template-expanded kinematics limits (`max_velocity`, `max_acceleration`,
  `max_z_velocity`) — none declared in source, so all three come from the
  template (informational `I1133`).
- Template-declared drivers (`x_driver`, `y_driver`, `z_driver`) shadowed by
  identically-named source components that add `connected_to` (informational
  `I1132`) — required because template-injected components are invisible to
  the parser's intra-document reference check, so a motor's `driver:` must
  resolve at parse time against a *source* component.

## Does not cover

- Multi-controller topology or `downlinks:` validation — see
  `examples/multi-mcu-toolhead`.

## Regenerating goldens

- `machine.lock`: `cargo test -p dryer-machine-lock` — a missing or stale
  golden causes the test to panic printing the actual value; copy it into
  `machine.lock` verbatim.
- `controller-safety.mainboard.golden.json`,
  `controller-build-plan.mainboard.golden.json`,
  `controller-image.mainboard.golden.json`: `cargo test -p dryer-firmware-build`,
  same missing-golden-prints-actual convention.
- `flash-plan.mainboard.golden.json`: `cargo test -p dryer-firmware-flash`.
- `job-trace.golden`: `UPDATE_TRACE=1 cargo test -p dryer-simulator --test golden`.
```

- [ ] **Step 3: Confirm the machine resolves cleanly, then generate `machine.lock`**

First, add a resolver-level cleanliness test to `crates/machine-resolver/src/tests.rs` (near `corexy_delta_and_toolchanger_templates_resolve_cleanly`):

```rust
#[test]
fn the_corexy_example_resolves_with_no_errors_or_warnings_beyond_expansion_notices() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/corexy/machine.yaml");
    let source = std::fs::read_to_string(&root).unwrap();
    let o = resolve_source(&source, &registry());
    assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
    for d in &o.diagnostics {
        assert!(
            d.code.starts_with('I'),
            "unexpected non-informational diagnostic: {d:?}"
        );
    }
    let codes: Vec<&str> = o.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(
        codes.iter().filter(|c| **c == "I1132").count(),
        3,
        "one I1132 per shadowed driver: {codes:?}"
    );
    assert_eq!(
        codes.iter().filter(|c| **c == "I1133").count(),
        3,
        "one I1133 per template-contributed limit: {codes:?}"
    );
}
```

Run: `cargo test -p dryer-machine-resolver the_corexy_example_resolves_with_no_errors_or_warnings_beyond_expansion_notices -- --nocapture`

Expected: PASS. If it fails, inspect the printed diagnostics and adjust `examples/corexy/machine.yaml` (e.g. a connector-claim conflict) before proceeding — do not touch the golden generation steps below until this passes.

Now generate the lock. Temporarily append a throwaway `#[ignore]`d test to `crates/machine-lock/tests/golden.rs` (that crate already has the right dev-dependencies), mirroring its existing `minimal_cartesian_lock_is_drift_gated` but pointed at `examples/corexy/machine.yaml` and an as-yet-nonexistent `examples/corexy/machine.lock`:

```rust
#[test]
#[ignore]
fn generate_corexy_lock() {
    let root = repo_root();
    let source = std::fs::read_to_string(root.join("examples/corexy/machine.yaml")).unwrap();
    let registry = LocalRegistry::load(&root.join("packages"));
    let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
        .resolved
        .unwrap();
    let yaml = lock(&source, &registry, &resolved).unwrap().to_yaml();
    std::fs::write(root.join("examples/corexy/machine.lock"), yaml).unwrap();
}
```

Run: `cargo test -p dryer-machine-lock generate_corexy_lock -- --ignored`

This writes `examples/corexy/machine.lock` directly. Delete the temporary `#[ignore]`d test afterward (it is scaffolding, not part of the committed test suite — Task 7's table-driven rewrite is the permanent test).

- [ ] **Step 4: Generate the firmware-build artifacts**

Temporarily add (mirroring the existing three `minimal_cartesian_controller_*` tests in `crates/firmware-build/tests/golden.rs`) three `#[ignore]`d generator tests following the same pattern as Step 3 — resolve `examples/corexy/machine.yaml`, call `compile_controller(&lock, "mainboard")`, `plan_controller(&lock, "mainboard")`, `build_controller(&lock, "mainboard")`, and `std::fs::write` each result (`.to_pretty_json()` for safety/build-plan, `.bytes` for the image) to:
- `examples/corexy/controller-safety.mainboard.golden.json`
- `examples/corexy/controller-build-plan.mainboard.golden.json`
- `examples/corexy/controller-image.mainboard.golden.json`

Run: `cargo test -p dryer-firmware-build generate_corexy -- --ignored`

Delete the generator tests afterward.

- [ ] **Step 5: Write `examples/corexy/usb-inventory.fixture.json`**

Identical shape to `examples/minimal-cartesian/usb-inventory.fixture.json` (same board, same VID/PID, since `examples/corexy` also pins `boards/example-mainboard@1.0.0`):

```json
[
  {
    "platform": "fixture",
    "bus_id": "usb-fixture-0",
    "location": "fixture-port-2.1",
    "device_address": 7,
    "usb_vid": 4617,
    "usb_pid": 53251,
    "serial_number": "DRYER-FIXTURE-001",
    "manufacturer": "Example",
    "product": "Mainboard DFU"
  }
]
```

- [ ] **Step 6: Generate the flash plan golden**

Temporarily add a generator test to `crates/firmware-flash/tests/golden.rs`, mirroring `minimal_cartesian_flash_plan_is_drift_gated`, pointed at `examples/corexy/`'s files (`machine.lock`, `controller-build-plan.mainboard.golden.json`, `usb-inventory.fixture.json`, `controller-image.mainboard.golden.json` as the artifact, `expected_current_firmware: "dryer-simulator/0.1.0"`), writing `plan.to_pretty_json()` to `examples/corexy/flash-plan.mainboard.golden.json`.

Run: `cargo test -p dryer-firmware-flash generate_corexy -- --ignored`

Delete the generator test afterward.

- [ ] **Step 7: Generate the job trace**

Temporarily duplicate `crates/simulator/tests/golden.rs`'s `rig_from_resolution` and job (home X, home Y, heat, move — per the design doc) pointed at `examples/corexy/machine.yaml`, with `UPDATE_TRACE=1` semantics writing to `examples/corexy/job-trace.golden`. Follow the crate's existing `UPDATE_TRACE=1` regeneration path exactly (read `crates/simulator/tests/golden.rs` in full before writing this, since the exact job sequence and trace-writing mechanism must match byte-for-byte what Task 7's table-driven rewrite will later assert against).

Run: `UPDATE_TRACE=1 cargo test -p dryer-simulator generate_corexy_trace -- --ignored`

Delete the generator test afterward (Task 7 adds the permanent, non-generator version).

- [ ] **Step 8: Run the full workspace test suite**

Run: `cargo test --workspace`

Expected: all pass. The new `examples/corexy/*` files exist but are not yet referenced by any *permanent* test (Task 7 wires them in) except the resolver cleanliness test from Step 3 — confirm that one still passes and no generator/`#[ignore]`d scaffolding tests remain in the tree (`grep -rn "generate_corexy" crates/` should return nothing).

- [ ] **Step 9: Commit**

```bash
git add examples/corexy
git commit -m "feat: add examples/corexy fixture with full golden parity"
```

---

### Task 6: Add `examples/multi-mcu-toolhead/` with full golden parity

**Files:**
- Create: `examples/multi-mcu-toolhead/machine.yaml`
- Create: `examples/multi-mcu-toolhead/README.md`
- Create: `examples/multi-mcu-toolhead/machine.lock` (generated)
- Create: `examples/multi-mcu-toolhead/controller-safety.mainboard.golden.json`, `...controller-safety.toolhead.golden.json` (generated)
- Create: `examples/multi-mcu-toolhead/controller-build-plan.mainboard.golden.json`, `...controller-build-plan.toolhead.golden.json` (generated)
- Create: `examples/multi-mcu-toolhead/controller-image.mainboard.golden.json`, `...controller-image.toolhead.golden.json` (generated)
- Create: `examples/multi-mcu-toolhead/usb-inventory.fixture.json`
- Create: `examples/multi-mcu-toolhead/flash-plan.mainboard.golden.json`, `...flash-plan.toolhead.golden.json` (generated)
- Modify: `crates/machine-resolver/src/tests.rs` (resolution-cleanliness assertion)

**Note:** no `job-trace.golden` for this example — see the README below for why (recorded deferral, not an oversight).

- [ ] **Step 1: Write `examples/multi-mcu-toolhead/machine.yaml`**

```yaml
api_version: dryer.machine/v0.1
kind: Machine

metadata:
  name: multi-mcu-toolhead
  description: Two-controller CoreXY machine with a CAN toolhead, mirroring spec §5.1

packages:
  - boards/example-mainboard@1.1.0
  - boards/example-toolhead@1.0.0
  - devices/tmc2209@2.1.0
  - machines/corexy-standard@1.0.0
  - safety-profiles/desktop-fdm@1.0.0

controllers:
  mainboard:
    board: boards/example-mainboard
    transport:
      type: usb
  toolhead:
    board: boards/example-toolhead
    transport:
      type: can
      parent: mainboard.can0

components:
  x_motor:
    type: stepper_motor
    driver: x_driver
    role: axis.x

  x_driver:
    type: tmc2209
    connected_to: mainboard.motor0

  y_motor:
    type: stepper_motor
    driver: y_driver
    role: axis.y

  y_driver:
    type: tmc2209
    connected_to: mainboard.motor1

  z_motor:
    type: stepper_motor
    driver: z_driver

  z_driver:
    type: tmc2209
    connected_to: mainboard.motor2

  extruder_motor:
    type: stepper_motor
    driver: extruder_driver

  extruder_driver:
    type: tmc2209
    connected_to: toolhead.motor0

  hotend_heater:
    type: heater
    output: toolhead.heater0
    sensor: hotend_sensor
    current: 2 A

  hotend_sensor:
    type: thermistor
    model: generic-3950
    input: toolhead.thermistor0

  bed_heater:
    type: heater
    output: mainboard.heater0
    sensor: bed_sensor
    current: 4 A

  bed_sensor:
    type: thermistor
    model: generic-3950
    input: mainboard.thermistor0

kinematics:
  type: corexy

safety:
  profile: safety-profiles/desktop-fdm
```

Kinematics use the same `machines/corexy-standard` template as `examples/corexy`, so the two examples differ **only** in controller topology — any artifact difference between them is attributable to multi-controller behavior, not kinematics. Every component names its controller explicitly (`connected_to`/`output`/`input` all use `mainboard.*` or `toolhead.*`) because an unclaimed component is ambiguous (E1203) once more than one controller exists — the resolver correctly refuses to guess which MCU a motor is wired to.

- [ ] **Step 2: Write `examples/multi-mcu-toolhead/README.md`**

```markdown
# examples/multi-mcu-toolhead

Two-controller CoreXY machine mirroring spec §5.1: a USB mainboard and a CAN
toolhead board, using the same `machines/corexy-standard` kinematics template
as `examples/corexy` so any artifact difference between the two examples is
attributable to multi-controller topology, not kinematics.

## Covers

- `boards/example-mainboard@1.1.0`'s `downlinks.can0` and the toolhead's
  `transport: { type: can, parent: mainboard.can0 }` (E1121/E1122 pass; see
  `machine-resolver`'s unit tests for the negative cases).
- Two independent safety partitions — bed heater on `mainboard`, hotend
  heater on `toolhead` — exercising the §18.2 "required sensor resolves
  through an input connector on the same controller" rule
  (E1503/E1504/E1507) on a real split rather than a synthetic unit test.
- E1203 (component ambiguity) avoidance: every component names its
  controller explicitly, because an unclaimed component is ambiguous once
  more than one controller exists.

## Does not cover

- The §19.2 cross-controller contract (authoritative state owner,
  synchronization requirements, acceptable clock uncertainty, link-loss
  behavior, safe degraded state) — not modeled. This example declares
  topology only.
- **No `job-trace.golden`.** The simulator is single-controller by design,
  and multi-controller clock skew is explicitly deferred (accepted design
  Q3: it needs a real synchronization protocol, not assumptions). A
  mainboard-only trace was rejected because it would silently omit the
  toolhead's heater — exactly the half-truth this repository's honesty
  policy targets.
- `transports.*.peripheral` and `downlinks.*` are not validated against the
  chip's peripheral table (pre-existing gap for `transports`; now also true
  for `downlinks`).

## Regenerating goldens

Same commands as `examples/corexy`, run once per controller where the test
table requires it (`mainboard`, `toolhead`):
- `cargo test -p dryer-machine-lock`
- `cargo test -p dryer-firmware-build`
- `cargo test -p dryer-firmware-flash`
```

- [ ] **Step 3: Add the resolver cleanliness test and confirm it passes before generating goldens**

Add to `crates/machine-resolver/src/tests.rs`:

```rust
#[test]
fn the_multi_mcu_toolhead_example_resolves_with_no_errors_or_warnings_beyond_expansion_notices() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/multi-mcu-toolhead/machine.yaml");
    let source = std::fs::read_to_string(&path).unwrap();
    let o = resolve_source(&source, &registry());
    assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
    for d in &o.diagnostics {
        assert!(
            d.code.starts_with('I'),
            "unexpected non-informational diagnostic: {d:?}"
        );
    }
}
```

Run: `cargo test -p dryer-machine-resolver the_multi_mcu_toolhead_example_resolves_with_no_errors_or_warnings_beyond_expansion_notices -- --nocapture`

Expected: PASS. If it fails, inspect and fix `examples/multi-mcu-toolhead/machine.yaml` before proceeding — in particular check for E1503/E1504/E1507 (sensor must resolve on the same controller as its heater) and E1200 (connector conflicts) before assuming a resolver bug.

- [ ] **Step 4: Generate `machine.lock`, per-controller safety/build-plan/image goldens, `usb-inventory.fixture.json`, and per-controller flash-plan goldens**

Same mechanism as Task 5 Steps 3-4-6: temporary `#[ignore]`d generator tests in `machine-lock`/`firmware-build`/`firmware-flash`'s existing golden test files, calling `compile_controller`/`plan_controller`/`build_controller`/`plan_dry_run` once for `"mainboard"` and once for `"toolhead"`, writing to the per-controller-named files listed above. Delete each generator test immediately after it has produced its file.

`examples/multi-mcu-toolhead/usb-inventory.fixture.json` must list **both** boards' bootloader identities so flash planning selects the correct one per controller instead of matching whichever appears first:

```json
[
  {
    "platform": "fixture",
    "bus_id": "usb-fixture-0",
    "location": "fixture-port-2.1",
    "device_address": 7,
    "usb_vid": 4617,
    "usb_pid": 53251,
    "serial_number": "DRYER-FIXTURE-001",
    "manufacturer": "Example",
    "product": "Mainboard DFU"
  },
  {
    "platform": "fixture",
    "bus_id": "usb-fixture-1",
    "location": "fixture-port-2.2",
    "device_address": 8,
    "usb_vid": 4617,
    "usb_pid": 53252,
    "serial_number": "DRYER-FIXTURE-002",
    "manufacturer": "Example",
    "product": "Toolhead DFU"
  }
]
```
(`53251` = `0xd003`, the mainboard's PID; `53252` = `0xd004`, the toolhead's PID — both under shared VID `4617` = `0x1209`, matching the `pid.codes` convention the mainboard's fixture already uses.)

- [ ] **Step 5: Run the full workspace test suite**

Run: `cargo test --workspace`

Expected: all pass; no `#[ignore]`d generator scaffolding remains (`grep -rn "generate_multi_mcu\|generate_toolhead" crates/` returns nothing).

- [ ] **Step 6: Commit**

```bash
git add examples/multi-mcu-toolhead crates/machine-resolver/src/tests.rs
git commit -m "feat: add examples/multi-mcu-toolhead fixture with full golden parity"
```

---

### Task 7: Table-driven goldens, per-controller renames for `minimal-cartesian`, and roadmap/doc honesty pass

**Files:**
- Modify: `crates/machine-lock/tests/golden.rs`
- Modify: `crates/firmware-build/tests/golden.rs`
- Modify: `crates/firmware-flash/tests/golden.rs`
- Modify: `crates/simulator/tests/golden.rs`
- Rename: `examples/minimal-cartesian/controller-safety.golden.json` → `controller-safety.mainboard.golden.json` (and similarly for `controller-build-plan`, `controller-image`, `flash-plan`; `firmware.fixture.bin` stays as-is, it is not per-controller-named in the design doc's list — confirm during implementation whether it needs a rename by checking whether it is referenced by basename anywhere outside the test files already found)
- Modify: `docs/firmware-build.md`, `docs/firmware-flash.md`, `docs/implementation-roadmap.md` (path references)
- Modify: `docs/implementation-roadmap.md` (new slice entry, deferrals, stale "Not yet decided" fixes)
- Delete: temporary generator tests if any were left over from Tasks 5-6 (there should be none; this step is a final sweep)

**Context:** this is the task the design doc's `CASES` table example maps to directly. `crates/machine-resolver/src/tests.rs:1064` grep confirmed only `firmware-build`/`firmware-flash`/`machine-lock` reference the old filenames in code (`crates/firmware-build/tests/golden.rs`, `crates/firmware-flash/tests/golden.rs`, `crates/firmware-flash/tests/hardware_executor.rs`); docs referencing them are `docs/firmware-build.md:63,70,76`, `docs/firmware-flash.md:115-118`, `docs/implementation-roadmap.md:102,236`.

- [ ] **Step 1: Rename `minimal-cartesian`'s goldens into the per-controller scheme**

```bash
git mv examples/minimal-cartesian/controller-safety.golden.json examples/minimal-cartesian/controller-safety.mainboard.golden.json
git mv examples/minimal-cartesian/controller-build-plan.golden.json examples/minimal-cartesian/controller-build-plan.mainboard.golden.json
git mv examples/minimal-cartesian/controller-image.golden.json examples/minimal-cartesian/controller-image.mainboard.golden.json
git mv examples/minimal-cartesian/flash-plan.golden.json examples/minimal-cartesian/flash-plan.mainboard.golden.json
```

A rename that changes bytes must fail the gate rather than be re-baselined — `git mv` alone cannot change file contents, so this is safe by construction; the drift gates in Step 2-4 below are what actually prove it.

- [ ] **Step 2: Rewrite `crates/machine-lock/tests/golden.rs` to be table-driven over examples**

Replace the `minimal_cartesian_lock_is_drift_gated` test with:

```rust
const EXAMPLES: &[&str] = &["minimal-cartesian", "corexy", "multi-mcu-toolhead"];

#[test]
fn example_locks_are_drift_gated() {
    let root = repo_root();
    for example in EXAMPLES {
        let dir = root.join("examples").join(example);
        let source = std::fs::read_to_string(dir.join("machine.yaml")).unwrap();
        let registry = LocalRegistry::load(&root.join("packages"));
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap_or_else(|| panic!("{example}: does not resolve"));
        let actual = lock(&source, &registry, &resolved).unwrap().to_yaml();
        let path = dir.join("machine.lock");
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!("{example}: missing golden {}\n\n{actual}", path.display())
        });
        assert_eq!(
            actual, expected,
            "{example}: machine.lock drifted; replace the golden deliberately with:\n{actual}"
        );
    }
}
```

Leave `normative_schema_tracks_the_current_lock_contract` as-is (it only needs one resolved example to check schema shape; `minimal-cartesian` remains fine for that).

- [ ] **Step 3: Rewrite `crates/firmware-build/tests/golden.rs` to be table-driven over (example, controller) pairs**

Replace the three `minimal_cartesian_controller_*` tests with:

```rust
const CASES: &[(&str, &str)] = &[
    ("minimal-cartesian", "mainboard"),
    ("corexy", "mainboard"),
    ("multi-mcu-toolhead", "mainboard"),
    ("multi-mcu-toolhead", "toolhead"),
];

fn resolve(root: &Path, example: &str) -> (dryer_machine_lock::Lockfile, LocalRegistry) {
    let source =
        std::fs::read_to_string(root.join("examples").join(example).join("machine.yaml")).unwrap();
    let registry = LocalRegistry::load(&root.join("packages"));
    let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
        .resolved
        .unwrap_or_else(|| panic!("{example}: does not resolve"));
    let lock = lock(&source, &registry, &resolved).unwrap();
    (lock, registry)
}

#[test]
fn controller_safety_is_drift_gated_for_every_example() {
    let root = repo_root();
    for (example, controller) in CASES {
        let (lock, _registry) = resolve(&root, example);
        let artifact = compile_controller(&lock, controller).unwrap();
        let actual = artifact.to_pretty_json();
        let path = root
            .join("examples")
            .join(example)
            .join(format!("controller-safety.{controller}.golden.json"));
        let expected = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing golden {}\n\n{actual}", path.display()));
        assert_eq!(actual, expected, "{example}/{controller}: controller safety artifact drifted");
    }
}

#[test]
fn controller_build_plan_is_drift_gated_for_every_example() {
    let root = repo_root();
    for (example, controller) in CASES {
        let (lock, _registry) = resolve(&root, example);
        let plan = plan_controller(&lock, controller).unwrap();
        let actual = plan.to_pretty_json();
        let path = root
            .join("examples")
            .join(example)
            .join(format!("controller-build-plan.{controller}.golden.json"));
        let expected = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing golden {}\n\n{actual}", path.display()));
        assert_eq!(actual, expected, "{example}/{controller}: controller build plan drifted");
    }
}

#[test]
fn controller_image_is_drift_gated_for_every_example() {
    let root = repo_root();
    for (example, controller) in CASES {
        let (lock, _registry) = resolve(&root, example);
        let built = build_controller(&lock, controller).unwrap();
        let path = root
            .join("examples")
            .join(example)
            .join(format!("controller-image.{controller}.golden.json"));
        let expected = std::fs::read(&path).unwrap_or_else(|_| {
            panic!("missing golden {}\n\n{}", path.display(), built.image.to_pretty_json())
        });
        assert_eq!(built.bytes, expected, "{example}/{controller}: controller image drifted");
    }
}
```

Keep `normative_schemas_track_the_build_outputs` as-is.

- [ ] **Step 4: Update `crates/firmware-flash/tests/golden.rs` and `crates/firmware-flash/tests/hardware_executor.rs` path references**

In `crates/firmware-flash/tests/golden.rs`:
- `fn build_plan(root: &Path)` currently reads a single hardcoded path; make it take an `example: &str` parameter and read `examples/{example}/controller-build-plan.mainboard.golden.json` (all of this crate's non-table tests target `mainboard` specifically — `minimal_cartesian_flash_plan_is_drift_gated` becomes table-driven per the design doc's `CASES`, but `build_plan_drift_is_rejected_before_artifact_io`, `ambiguity_and_artifact_drift_are_both_blocking`, `registry_source_drift_is_rejected_before_flash_planning`, and `package_companion_file_drift_is_blocking` test generic drift-detection *mechanisms*, not example-specific behavior — leave those four targeting `minimal-cartesian` only, just updated to the new filename `controller-build-plan.mainboard.golden.json`. Do not duplicate them across all four `(example, controller)` cases; that would add no coverage of anything new, only wall-clock cost).

Rewrite `minimal_cartesian_flash_plan_is_drift_gated` as:

```rust
const CASES: &[(&str, &str)] = &[
    ("minimal-cartesian", "mainboard"),
    ("corexy", "mainboard"),
    ("multi-mcu-toolhead", "mainboard"),
    ("multi-mcu-toolhead", "toolhead"),
];

#[test]
fn flash_plan_is_drift_gated_for_every_example() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (example, controller) in CASES {
        let dir = root.join("examples").join(example);
        let lock = Lockfile::from_yaml(&std::fs::read_to_string(dir.join("machine.lock")).unwrap()).unwrap();
        let build_plan = build_plan(&root, example, controller);
        let registry = LocalRegistry::load(&root.join("packages"));
        assert!(registry.diagnostics.is_empty());
        let inventory: Vec<DiscoveredUsbDevice> =
            serde_json::from_str(&std::fs::read_to_string(dir.join("usb-inventory.fixture.json")).unwrap()).unwrap();
        let artifact = dir.join(format!("controller-image.{controller}.golden.json"));
        let plan = plan_dry_run(DryRunRequest {
            controller,
            lock: &lock,
            build_plan: &build_plan,
            registry: &registry,
            discovered_devices: &inventory,
            artifact: ArtifactSpec { path: &artifact, signature: None },
            expected_current_firmware: "dryer-simulator/0.1.0",
        }).unwrap();
        assert!(!plan.ready);
        assert_eq!(plan.blocked_reasons.len(), 1, "{example}/{controller}: {:?}", plan.blocked_reasons);
        assert!(plan.blocked_reasons[0].contains("not a deployable controller executable"));
        let expected = std::fs::read_to_string(dir.join(format!("flash-plan.{controller}.golden.json"))).unwrap();
        assert_eq!(plan.to_pretty_json(), expected, "{example}/{controller}: flash plan drifted");
    }
}
```

with `fn build_plan(root: &Path, example: &str, controller: &str) -> ControllerBuildPlanArtifact` updated to:

```rust
fn build_plan(root: &Path, example: &str, controller: &str) -> ControllerBuildPlanArtifact {
    serde_json::from_str(
        &std::fs::read_to_string(
            root.join("examples").join(example).join(format!("controller-build-plan.{controller}.golden.json")),
        )
        .unwrap(),
    )
    .unwrap()
}
```

Update the four remaining single-fixture tests' calls from `build_plan(&root)` to `build_plan(&root, "minimal-cartesian", "mainboard")`, and their hardcoded `examples/minimal-cartesian/controller-build-plan.golden.json`-shaped paths to `examples/minimal-cartesian/controller-build-plan.mainboard.golden.json`.

In `crates/firmware-flash/tests/hardware_executor.rs`, update the three path references found (lines 14, 32, 37) from `controller-build-plan.golden.json` / `usb-inventory.fixture.json` / `controller-image.golden.json` to `controller-build-plan.mainboard.golden.json` / (unchanged — `usb-inventory.fixture.json` is not per-controller) / `controller-image.mainboard.golden.json`.

- [ ] **Step 5: Add the corexy job-trace test to `crates/simulator/tests/golden.rs`**

Add a second, permanent (non-generator) test alongside `the_fixture_job_trace_matches_the_golden`, following the exact same structure but pointed at `examples/corexy/machine.yaml` and `examples/corexy/job-trace.golden`, with the job sequence used in Task 5 Step 7 (home X, home Y, heat, move, heartbeats throughout). Read the existing test in full first so the new one matches its structure (transport setup, command sequence shape, trace comparison, `UPDATE_TRACE` support) exactly rather than approximating it.

- [ ] **Step 6: Run the full workspace test suite**

Run: `cargo test --workspace`

Expected: all pass, 0 failures. This is the first point where the renamed `minimal-cartesian` goldens and the table-driven tests are exercised together — if any assertion fails, check whether it's a path typo (most likely) before suspecting a real drift.

- [ ] **Step 7: Update documentation path references**

In `docs/firmware-build.md` lines 63, 70, 76: replace `examples/minimal-cartesian/controller-safety.golden.json` → `examples/minimal-cartesian/controller-safety.mainboard.golden.json`, and similarly for `controller-build-plan` and `controller-image`.

In `docs/firmware-flash.md` lines 115-118 (a CLI usage example): replace `controller-build-plan.golden.json` → `controller-build-plan.mainboard.golden.json` and `controller-image.golden.json` → `controller-image.mainboard.golden.json`.

In `docs/implementation-roadmap.md` line 102: replace `examples/minimal-cartesian/controller-safety.golden.json` → `examples/minimal-cartesian/controller-safety.mainboard.golden.json`. Line 236: replace `examples/minimal-cartesian/flash-plan.golden.json` → `examples/minimal-cartesian/flash-plan.mainboard.golden.json`.

- [ ] **Step 8: Roadmap honesty pass**

In `docs/implementation-roadmap.md`, under the "Not yet decided / blocked" section:
- Remove or correct the "Flash discovery and planning ... no mutating flash executor exists yet" bullet — Slice 30 already added `NativeFlashExecutor` (confirmed present in the git log and roadmap's own step-10 entry, which already documents `NativeFlashExecutor`; the "Not yet decided" section is simply stale and duplicates/contradicts the step-10 entry above it). Delete the stale bullet.
- Add a note that the gcode-lowerer and web UI slices (already implemented per git history: `dryer-gcode-lowerer`, `ui/`) are unrecorded in this roadmap file's step-by-step list; add one line each under a new "Slice 31/32" style entry or fold into the closing prose — whichever matches the file's existing convention once you re-read its current tail section.

Add a new entry recording this slice, e.g. appended after the existing step-10 entry or as its own bullet under §29:

```markdown
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
  components).
```

- [ ] **Step 9: Run the full workspace test suite one final time**

Run: `cargo test --workspace`

Expected: all pass, 0 failures.

- [ ] **Step 10: Commit**

```bash
git add examples/minimal-cartesian crates/machine-lock/tests/golden.rs crates/firmware-build/tests/golden.rs crates/firmware-flash/tests/golden.rs crates/firmware-flash/tests/hardware_executor.rs crates/simulator/tests/golden.rs docs/firmware-build.md docs/firmware-flash.md docs/implementation-roadmap.md
git commit -m "refactor: table-driven per-example/per-controller goldens; rename minimal-cartesian goldens; roadmap honesty pass"
```

---

## Self-Review Notes

- **Spec coverage:** every numbered item from the design doc's "Scope" section maps to a task: board model change → Task 2; new packages → Task 4; two examples → Tasks 5-6; new diagnostics → Task 3; per-controller goldens/table-driven tests → Task 7; roadmap honesty pass → Task 7 Step 8; the two pre-existing bugs (unit typo, no-op test) → Task 1.
- **Deferred items** (§19.2 cross-controller contract, multi-controller execution traces, `transports.*.peripheral`/`downlinks.*` chip-peripheral binding, `role:` semantics, parse/expansion ordering) are deliberately *not* tasks — they are recorded as deferrals in Task 7 Step 8, matching the design doc's own "Deferred" section, which explicitly puts them out of scope.
- **Ordering rationale:** Task 3 (new diagnostics) uses its own self-contained test fixture (`mainboard-with-downlinks`) rather than depending on Task 4's real `example-mainboard@1.1.0`/`example-toolhead`, so Tasks 2-4 and 5 have no forced ordering beyond 2→3 (downlinks field must exist before it can be checked) and 1→5/6 (the unit bug must be fixed before any example that lets template limits flow through can resolve). Task 7 must come last since it renames files Task 5/6 create pre-named already, and depends on both examples existing.
