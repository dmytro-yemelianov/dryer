# CoreXY and multi-MCU toolhead example machines — design

Date: 2026-08-05
Status: approved for planning

## Why

Spec §30 requires "the minimal Cartesian, CoreXY, and multi-MCU toolhead examples
validate" before v0.1 is done. Only `examples/minimal-cartesian` exists. Every
golden artifact in the repository — lockfile, safety partition, build plan,
controller image, flash plan, job trace — is pinned to that single fixture, so
nothing proves the resolver, lockfile, and firmware boundaries generalize past
the shape they were written against.

This slice adds the two missing examples with full golden parity, and closes the
structural validation gaps the multi-controller shape exposes.

## Scope

In scope:

- `examples/corexy/` — single controller, template-expanded CoreXY.
- `examples/multi-mcu-toolhead/` — two controllers, CAN toolhead, mirroring §5.1.
- `boards/example-mainboard@1.1.0` and `boards/example-toolhead@1.0.0`.
- A `downlinks:` section in the board package model, plus three new resolver
  diagnostics that make `transport.parent` a checked reference.
- Per-controller golden naming and table-driven golden tests.
- Roadmap honesty pass.

Out of scope, recorded as deferrals (see "Deferred" below): the §19.2
cross-controller contract, multi-controller execution traces, and chip-peripheral
binding for transports and downlinks.

## Board model change: `downlinks:`

A board's `transports:` map is keyed by transport *type* (`usb`, `can`) and is
checked by `machine-resolver/src/targets.rs` against the controller's own
`transport.type` (E1120). It describes the uplink a controller *uses*. It cannot
express `parent: mainboard.can0`, which names a port the parent *offers to
children*.

Modeling the downlink as a connector was rejected: connector claims are
exclusive, and CAN is multi-drop, so two child controllers sharing one bus —
legitimate wiring — would be rejected as a conflict.

New optional board section:

```yaml
downlinks:
  can0:
    type: can
```

`type` is the only field. `transports.*.peripheral` is already unvalidated
against the chip's peripheral table; adding a second unchecked reference on
downlinks would create the appearance of validation where there is none. Binding
either to the chip is deferred.

## Packages

| Package | Change |
|---|---|
| `boards/example-mainboard` | Directory migrates from the legacy flat layout to `packages/boards/example-mainboard/1.0.0/`, byte-identical. New `1.1.0` = `1.0.0` plus `downlinks.can0`. |
| `boards/example-toolhead@1.0.0` | New. |
| `chips/generic-mcu` | Unchanged. |
| `machines/corexy-standard@1.0.0` | **Bug fix** — see below. Consumed by both examples. |
| `machines/delta-basic@1.0.0`, `machines/toolchanger-corexy@1.0.0` | Same bug fix; not consumed by any example. |

The content digest is computed over paths relative to the package root
(`package-model/src/lib.rs`, `portable_relative_path`), and goldens reference
packages by id (`boards/example-mainboard@1.0.0`), not by path — so the layout
migration must leave every existing golden byte-identical. This is an assertion
to be *verified by the existing drift gates*, not assumed: if any golden moves,
the migration is wrong and must be investigated, not re-baselined.

`example-mainboard@1.1.0` is the first board package with two versions, which
also gives range resolution a second subject beyond `chips/generic-mcu`.

`boards/example-toolhead@1.0.0` declares `chip: chips/generic-mcu`, one
`stepper_driver_socket` (extruder), a `heater0` power output, a `thermistor0`
analog input, an `accel0` SPI accessory socket, `transports.can`, and a USB DFU
flash recipe with its own VID/PID distinct from the mainboard's. Every pin it
names already exists in `generic-mcu`'s `pin_functions` table, so no chip package
changes are required.

The toolhead's runtime transport is CAN but its flash method is USB DFU. This
matches real CAN toolhead boards, and keeps flash discovery honest: selection
matches the board's own bootloader identity, never its runtime transport.

### Pre-existing bug: invalid acceleration unit in every machine template

Confirmed by running the resolver against a probe machine:

```
[E1134] template limit 'max_acceleration' from 'machines/corexy-standard':
        '5000 mm/s2' is not a valid quantity
```

`UNIT_TABLE` (`machine-schema/src/lib.rs:283`) defines `mm/s^2`; all three
machine-class packages spell it `mm/s2`. Any machine consuming them fails
resolution. The fix is `mm/s2` → `mm/s^2` in `corexy-standard`, `delta-basic`,
and `toolchanger-corexy`.

These are edited in place at `1.0.0` rather than bumped: no lockfile, golden, or
example pins any of them today, so there is no published content digest to
invalidate. `examples/corexy` and `examples/multi-mcu-toolhead` become the first
consumers, and they pin the fixed 1.0.0.

### Pre-existing bug: the test that should have caught it is a no-op

`corexy_delta_and_toolchanger_templates_resolve_cleanly`
(`machine-resolver/src/tests.rs:1064`) builds its inputs with
`fixture().replace("class: cartesian-basic", "class: corexy-standard")`.
`examples/minimal-cartesian/machine.yaml` contains **zero** occurrences of
`class: cartesian-basic` — it references templates through
`packages: - machines/cartesian-basic@1.0.0`. Every replace is a no-op, so the
test resolves the same cartesian fixture three times and passes while asserting
nothing about the three template packages.

The test is rewritten to swap the actual package reference, and must fail against
the unfixed packages before the unit fix lands. This is the reason the broken
packages survived: a green test asserting nothing.

## `examples/corexy/`

Single controller on `example-mainboard@1.0.0`. Packages: the board,
`devices/tmc2209@2.1.0`, `machines/corexy-standard@1.0.0`, and
`safety-profiles/desktop-fdm@1.0.0`.

Both new examples pin the safety profile explicitly. `minimal-cartesian` leaves
it implicit and therefore resolves with an `E1106` warning ("not version-pinned;
selected 1.0.0"), observed in the probe run. New fixtures should resolve with no
errors and no warnings — only the informational `I1132`/`I1133` expansion
notices — so that any future warning on them is a real signal.
`minimal-cartesian` is left as-is: changing it would move its goldens, and its
implicit-root path is worth keeping covered.

The machine declares `x_motor`, `y_motor`, `z_motor` (with `role:` attributes and
`driver:` links), their three `tmc2209` drivers, the hotend heater, and the
hotend thermistor. It declares `kinematics: {type: corexy}` with **no limits** —
`max_velocity`, `max_acceleration`, and `max_z_velocity` all default from the
template.

Two parser facts, confirmed by probe, force this shape:

- `kinematics:` is required in the machine source even when a template supplies
  it (`E0100 machine manifest does not parse: missing field 'kinematics'`).
  Templates fill in *limits*, not the block itself.
- Template-injected components are invisible to the parser's intra-document
  reference check, which runs before expansion. A source `driver: x_driver`
  pointing at a template-provided driver fails `E0501 component 'x_motor'
  references unknown component 'x_driver'`. So any driver a motor names must be
  declared in the source, shadowing the template (I1132).

The example therefore covers I1132 on the three drivers and I1133 on all three
kinematics limits. The parse-order gap is recorded as a deferral rather than
fixed here: making expansion-provided components visible to reference checking
means reordering parse and expansion, which is resolver surgery, not fixture
work.

Motors carry `role: axis.x` / `axis.y`, matching the spec §5.1 example, which
pairs `role: axis.x` with corexy kinematics. `role` is an open attribute today;
the README notes that the physical belt motors are conventionally A/B and that
role semantics are not yet validated by the resolver.

## `examples/multi-mcu-toolhead/`

Mirrors spec §5.1. Two controllers:

```yaml
controllers:
  mainboard:
    board: boards/example-mainboard
    transport: { type: usb }
  toolhead:
    board: boards/example-toolhead
    transport: { type: can, parent: mainboard.can0 }
```

Components: X/Y/Z drivers on `mainboard.motor0/1/2`, extruder driver on
`toolhead.motor0`, hotend heater + thermistor on the toolhead, bed heater +
thermistor on the mainboard. Kinematics are corexy, from the same template as
`examples/corexy`, so the two examples differ **only** in controller topology and
any artifact difference is attributable to multi-controller behavior.

Both controllers therefore own a non-trivial safety partition, and §18.2's
"required sensor resolves through an input connector on the same controller"
rule (E1503/E1504/E1507) is exercised on a real split rather than a unit test.

Drivers are declared in the source with explicit `connected_to:` for two
independent reasons, both of which the README states rather than leaving them
looking like boilerplate: motors must be able to name their drivers (E0501, see
above), and with two controllers an unclaimed component raises E1203 (ambiguity,
multi-controller without `controller:`) — the resolver correctly refusing to
guess which MCU a motor is wired to.

Kinematics limits still default from the template, so the I1133 contribution path
is exercised on this fixture too.

## New diagnostics

Written as failing tests before the fixtures are built, so the examples land on a
resolver that actually checks the wiring they depend on. `machine-parser` today
validates only that `transport.parent` is `controller.port`-shaped and names a
known controller (`machine-parser/src/lib.rs:240`).

| Code | Condition |
|---|---|
| `E1121` | `transport.parent` names a port that is not a `downlinks:` entry on the parent controller's board. Message lists the parent board's available downlink ports, following E1200's suggestion style. |
| `E1122` | Child `transport.type` disagrees with the parent downlink's declared `type`. |
| `E1123` | Controller parent cycle, including self-parenting, or no root controller. `a → b → a` resolves clean today. |

Each gets a negative fixture test asserting the exact code, message, and source
range, per the existing convention. E1121/E1122 are checked in the same pass as
E1120 in `targets.rs`; E1123 is a whole-graph check over the controller map.

## Goldens and tests

Artifact goldens become per-controller by name:

- `controller-safety.<controller>.golden.json`
- `controller-build-plan.<controller>.golden.json`
- `controller-image.<controller>.golden.json`
- `flash-plan.<controller>.golden.json`
- `firmware.<controller>.fixture.bin`

`minimal-cartesian`'s existing goldens are renamed into this scheme with content
untouched. A rename that changes bytes must fail the gate rather than be
re-baselined. Docs referencing the old paths (`firmware-build.md`,
`firmware-flash.md`, `implementation-roadmap.md`) are updated in the same commit.

`machine-lock/tests/golden.rs`, `firmware-build/tests/golden.rs`, and
`firmware-flash/tests/golden.rs` collapse from copy-paste-per-example to a table:

```rust
const CASES: &[(&str, &str)] = &[
    ("minimal-cartesian",  "mainboard"),
    ("corexy",             "mainboard"),
    ("multi-mcu-toolhead", "mainboard"),
    ("multi-mcu-toolhead", "toolhead"),
];
```

`examples/multi-mcu-toolhead/usb-inventory.fixture.json` lists both boards'
bootloader identities, so flash planning must select the correct one per
controller instead of matching whichever appears first.

`examples/corexy/job-trace.golden` covers home X, home Y, heat, move, with
heartbeats throughout, using the existing `UPDATE_TRACE=1` regeneration path.

`examples/multi-mcu-toolhead/` gets no job trace. The simulator is
single-controller by design and multi-controller clock skew was explicitly
deferred (accepted design Q3: it needs a real synchronization protocol, not
assumptions). A mainboard-only trace was rejected: a trace that silently omits
the toolhead's heater is exactly the half-truth this repository's honesty policy
targets. The README and roadmap state the absence and the reason.

Each example also gets a `README.md` describing what it covers, what it
deliberately does not, and how to regenerate its goldens.

## Deferred

Recorded in `docs/implementation-roadmap.md`:

- §19.2 cross-controller contract — authoritative state owner, synchronization
  requirements, acceptable clock uncertainty, link-loss behavior, safe degraded
  state — is not modeled. The multi-MCU example declares topology only.
- Multi-controller execution traces, blocked on design Q3.
- `transports.*.peripheral` and `downlinks.*` are not validated against the
  chip's peripheral table. Pre-existing for transports; now also true for
  downlinks.
- `role:` values are open attributes with no resolver-side semantics.
- Parse-time intra-document reference checking runs before template expansion, so
  source components cannot reference template-provided components (E0501). Fixing
  it means reordering parse and expansion.

The same roadmap pass fixes existing staleness: the gcode-lowerer and UI slices
are unrecorded, and the "Not yet decided" section still claims no mutating flash
executor exists, which Slice 30's `NativeFlashExecutor` invalidated.

## Definition of done

- Both examples resolve with **no errors and no warnings**, and produce
  byte-stable goldens for every artifact listed above, all drift-gated in CI.
  Informational `I1132`/`I1133` expansion diagnostics are expected and are
  asserted by exact content, not merely tolerated.
- The `mm/s2` unit bug is fixed in all three machine-class packages, and the
  rewritten template test fails against the unfixed packages.
- E1121/E1122/E1123 exist with negative fixture coverage.
- `minimal-cartesian`'s goldens are unchanged in content after the rename and the
  board layout migration.
- `cargo test --workspace` passes with no failures.
- The roadmap records this slice and its deferrals accurately.
