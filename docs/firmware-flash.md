# Firmware flash discovery and dry-run plans

Status: Implemented planning boundary · Target: §21.2–§21.4 and §29 step 10

`dryer-firmware-flash` is the safe boundary between a locked machine resolution and a
future method-specific flash executor. It does three things today:

1. enumerate attached USB devices without opening them;
2. match them against the locked board package's flashing-mode identity;
3. verify artifact bytes and emit a deterministic, versioned dry-run plan.

It cannot flash, reset, claim, detach, or otherwise mutate a device. That absence is
intentional: a future DFU/SWD executor must consume the plan's checks rather than
reimplementing looser device selection.

## Board package contract

Board packages may declare:

```yaml
flash:
  default_method: dfu
  methods:
    dfu:
      transport: usb
      select:
        usb_vid: "0x1209"
        usb_pid: "0xd003"
        # Optional exact constraints:
        # serial_number: ABC123
        # manufacturer: Example
        # product: Mainboard DFU
      enter_bootloader:
        - Hold BOOT while tapping RESET, then release BOOT.
      verify: sha256
      recovery:
        - Re-enter DFU mode and retry with the same locked artifact.
```

USB IDs accept YAML integers or quoted hexadecimal values and are stored as typed
16-bit values. v0 accepts only the `usb` transport and `sha256` verification. The
default method must exist, optional string constraints and instructions cannot be
empty, and every method must publish recovery instructions (`E0615`–`E0619`).

The selector identifies the device in flashing/bootloader mode. It belongs to the
board package and therefore must not contain a per-machine serial number unless that
serial is genuinely fixed by the hardware product. A later deployed-graph identity
binding can refine the rule for one physical controller.

## Discovery and selection

Discovery uses `nusb`, which maps to native OS USB APIs on Linux, macOS, and Windows.
The normalized record contains platform, bus, platform-native stable location, address,
VID/PID, and any cached descriptor strings. Records are sorted by the full normalized
identity before serialization.

Matching is an exact conjunction:

- VID and PID must match;
- every optional string constraint must be present and byte-equal;
- zero matches is `missing`;
- one match is `unique`;
- more than one match is `ambiguous` and blocks the plan.

The planner never silently drops a constraint or selects the first of multiple
devices. Board recipes should avoid non-portable string constraints unless required;
for example, Windows may not expose a cached manufacturer string.

## Dry-run plan

`plan_dry_run` requires a controller from `machine.lock`, its persisted build-plan v2,
the locked local registry, a discovered inventory, a firmware artifact, and the
expected current firmware identity. It verifies:

- the live registry's portable source id, URI, and exact descriptor hash match
  lockfile v5;
- the persisted build plan is byte-for-byte reproducible from the lock before any
  artifact IO;
- the controller has exactly one version-pinned board package;
- the local board manifest still matches its lockfile hash;
- every file in the local board package still matches the v2 content hash;
- the board has a valid default flash recipe;
- the artifact's observed size and sha256 equal the expected build output;
- a hash-matching reference image has canonical encoding and matches every locked
  build-plan field;
- device selection is unique.

Artifact-hash mismatch and missing/ambiguous devices are reported together in
`blocked_reasons`. Structural input, lockfile, registry, and IO failures are returned
as errors because no truthful plan can be constructed from them.

Legacy v1-v4 locks remain readable and keep their historical package-drift checks.
New v5 plans additionally fail before artifact or device planning when
`packages/registry.yaml` is absent, invalid, or differs from the locked source.

The `dryer.flash-plan/v0.2` JSON records the lock hash, exact board, selection rule and
candidates, expected current firmware, artifact format/deployment eligibility,
observed and expected artifact hashes, optional signature, ordered would-run steps,
blockers, and recovery instructions. The stable artifact path comes from the derived
build plan, so the plan stays byte-identical when the host reads it from another path.

The current `dryer.controller-image/v1` reference image is intentionally
non-executable and therefore adds a deployment blocker even when its hash matches.
This prevents the dry-run boundary from presenting a configuration container as a
flashable MCU program.

The committed minimal-Cartesian fixture includes the reference backend output, USB
inventory, and golden plan. To exercise the read-only example with that inventory:

```bash
cargo run -p dryer-firmware-flash --example plan -- \
  examples/minimal-cartesian/machine.lock \
  packages \
  mainboard \
  examples/minimal-cartesian/controller-build-plan.golden.json \
  examples/minimal-cartesian/controller-image.golden.json \
  dryer-simulator/0.1.0 \
  --inventory examples/minimal-cartesian/usb-inventory.fixture.json
```

Omit `--inventory` to perform read-only discovery on the current host. Exit status is
0 for a ready plan, 1 for a plan with blockers (including the current reference image),
and 2 when plan construction fails.

## Deferred execution boundary

A mutating executor remains deliberately out of scope until a real firmware artifact
and supported board exist. It must additionally provide:

- method-specific DFU/SWD implementation and privilege diagnostics;
- observation of the current firmware identity rather than only an expectation;
- signature/trust-policy enforcement;
- post-flash identity probing and hash/read-back verification where hardware permits;
- transactional multi-controller prepare/verify/activate/confirm behavior;
- interruption and rollback tests on hardware.
