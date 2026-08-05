> **Historical note (2026-08-04):** this spec was drafted under the working name
> **ForgeOS**; the project has since been named **Dryer**. The body is preserved
> verbatim — read every occurrence of "ForgeOS"/`forge.*` as Dryer/`dryer.*`.

# Project Specification: Modular Rust Platform for Machine Control

Status: Draft v0.1  
Working name: **ForgeOS**  
Primary audience: Codex and repository contributors

This document is the repository-ready implementation specification for the architecture discussed in “Better Versions of KIAUH.” It is intentionally implementation-oriented. The key words **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, and **MAY** describe requirement strength.

## 1. Project name

Working name: **ForgeOS**

The name is temporary. Do not couple package names, protocol identifiers, or public APIs to this name unless explicitly marked stable.

## 2. Objective

Build a modular, cross-platform control platform for 3D printers and other motion-control machines.

The platform must provide:

- firmware written primarily in Rust;
- support for multiple microcontroller families;
- a native host runtime for Linux, macOS, and Windows;
- declarative machine configuration;
- a registry of boards, devices, machine profiles, workflows, and drivers;
- automatic hardware resource resolution;
- reproducible firmware builds;
- typed workflows instead of text-based macros;
- multi-controller synchronization;
- local-first operation;
- compatibility paths for Klipper, Moonraker, Mainsail, and Fluidd.

This project is not a direct Klipper rewrite.

Its core abstraction is a typed and validated **Machine Graph** describing the complete machine.

---

## 3. Core principles

### 3.1 Machine Graph as the source of truth

The complete machine must be represented as a graph containing:

- controller boards;
- microcontrollers;
- physical connectors;
- buses;
- sensors;
- actuators;
- motors;
- motor drivers;
- heaters;
- fans;
- endstops;
- probes;
- accelerometers;
- toolheads;
- kinematics;
- functional roles;
- safety rules;
- workflows;
- calibration values;
- firmware assignments.

Users should describe logical relationships instead of manually assigning raw pins whenever possible.

Example:

```yaml
motors:
  x_motor:
    driver: x_driver
    role: axis.x

drivers:
  x_driver:
    type: tmc2209
    connected_to: mainboard.motor0

boards:
  mainboard:
    package: boards/btt-octopus-pro@1.1
```

The resolver derives the physical assignments from board manifests.

### 3.2 Explicit layer boundaries

The system must be divided into these layers:

```text
Applications and UI
Platform API
Workflow runtime
Machine state service
Motion planner
Control protocol
MCU runtime
Hardware
```

Each layer must expose a versioned interface.

No layer may depend on undocumented behavior from another layer.

### 3.3 Local-first operation

The platform must work without cloud connectivity.

Cloud services may provide:

- package discovery;
- remote monitoring;
- shared configuration libraries;
- analytics;
- update mirrors.

The machine must still be able to:

- boot;
- print;
- load local jobs;
- validate configuration;
- update from local files;
- recover from failed updates;
- operate its UI over the local network.

### 3.4 Reproducible configuration

A machine deployment must be reproducible from:

```text
machine manifest
package lockfile
calibration state
firmware build metadata
```

The same locked configuration must produce equivalent firmware and runtime artifacts.

### 3.5 Safety at the edge

Every controller responsible for dangerous hardware must enforce local safety independently of the host.

A host crash, disconnected USB cable, network outage, malformed workflow, or stale command queue must not leave heaters or motors in an unsafe state.

### 3.6 Compatibility is an adapter boundary

G-code, Klipper configuration, the Klipper MCU protocol, and Moonraker-compatible APIs are compatibility surfaces. They must not become the internal domain model. Native components communicate through typed, versioned interfaces.

### 3.7 No arbitrary code on safety-critical controllers

MCUs may load resolved declarative configuration and verified resource tables. They must not dynamically load arbitrary third-party Rust code or untrusted bytecode. Extensibility belongs primarily on the host and must be capability-restricted.

---

## 4. Repository layout

Create the initial repository using the following structure:

```text
forgeos/
├── Cargo.toml
├── rust-toolchain.toml
├── README.md
├── LICENSES/
├── docs/
│   ├── architecture.md
│   ├── machine-graph.md
│   ├── package-format.md
│   ├── resource-resolution.md
│   ├── firmware.md
│   ├── protocol.md
│   ├── workflows.md
│   ├── safety-model.md
│   ├── compatibility.md
│   └── implementation-roadmap.md
├── crates/
│   ├── machine-schema/
│   ├── machine-parser/
│   ├── machine-resolver/
│   ├── machine-lock/
│   ├── package-model/
│   ├── package-registry/
│   ├── resource-model/
│   ├── motion-core/
│   ├── motion-kinematics/
│   ├── control-protocol/
│   ├── device-protocol/
│   ├── workflow-model/
│   ├── workflow-runtime/
│   ├── safety-model/
│   ├── simulator/
│   ├── host-runtime/
│   ├── platform-api/
│   ├── firmware-core/
│   ├── firmware-hal/
│   ├── firmware-build/
│   ├── firmware-flash/
│   └── klipper-compat/
├── firmware/
│   ├── rp2040/
│   ├── stm32g0/
│   └── simulated-mcu/
├── packages/
│   ├── chips/
│   ├── boards/
│   ├── devices/
│   ├── machines/
│   ├── workflows/
│   └── safety-profiles/
├── schemas/
│   ├── machine.schema.json
│   ├── chip-package.schema.json
│   ├── board-package.schema.json
│   ├── device-package.schema.json
│   ├── workflow-package.schema.json
│   └── lockfile.schema.json
├── examples/
│   ├── minimal-cartesian/
│   ├── corexy/
│   └── multi-mcu-toolhead/
└── tests/
    ├── resolver/
    ├── protocol/
    ├── workflows/
    └── compatibility/
```

Use a Cargo workspace.

Do not create one oversized crate.

---

## 5. Machine Graph v0.1

### 5.1 Top-level document

Define the initial format in YAML.

Example:

```yaml
api_version: forge.machine/v0.1
kind: Machine

metadata:
  name: voron-24
  description: Voron 2.4 with CAN toolhead

packages:
  - boards/btt-octopus-pro@1.1.0
  - boards/ebb36@1.0.0
  - devices/tmc2209@2.1.0
  - devices/adxl345@1.2.0
  - machines/corexy@1.0.0

controllers:
  mainboard:
    board: boards/btt-octopus-pro
    transport:
      type: usb

  toolhead:
    board: boards/ebb36
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

  hotend_heater:
    type: heater
    output: toolhead.heater0
    sensor: hotend_sensor

  hotend_sensor:
    type: thermistor
    model: generic-3950
    input: toolhead.thermistor0

kinematics:
  type: corexy

safety:
  profile: safety-profiles/desktop-fdm

workflows:
  home: workflows/corexy-home
  print_start: workflows/default-print-start

calibration:
  source: calibration.yaml
```

### 5.2 Required top-level fields

The following fields are required:

- `api_version`
- `kind`
- `metadata`
- `controllers`
- `components`
- `kinematics`
- `safety`

### 5.3 Stable identifiers

Every graph node must have a stable machine-local identifier.

Identifiers must:

- use lowercase ASCII;
- use letters, digits, `_`, and `-`;
- begin with a letter;
- remain stable across configuration formatting changes.

### 5.4 Units

Never represent physical quantities as ambiguous bare numbers when the unit is not implicit.

Preferred form:

```yaml
max_velocity: 300 mm/s
max_acceleration: 8000 mm/s^2
heater_power: 50 W
supply_voltage: 24 V
sample_rate: 8000 Hz
```

Internally, parse values into strongly typed quantities.

Use a units crate or implement transparent newtypes.

Never mix:

- millimeters and meters;
- Celsius and Kelvin;
- seconds and milliseconds;
- radians and degrees;
- amperes and milliamperes.

### 5.5 Graph lifecycle

The system must distinguish:

```text
source graph
expanded graph
resolved graph
deployed graph
observed runtime graph
```

The source graph is user-authored. Package templates expand it. The resolver assigns concrete resources. Deployment binds artifacts and controller identities. Runtime observation adds current state without mutating the source graph.

### 5.6 Calibration separation

Calibration is machine-local state and must not be silently embedded into community packages. A calibration record must state:

- machine identity;
- graph and lockfile hashes;
- calibration procedure and version;
- value, type, and units;
- timestamp;
- validity constraints;
- provenance.

---

## 6. Package model

### 6.1 Package types

The registry must support these package kinds:

```text
chip
board
device
machine
workflow
safety-profile
kinematics
host-extension
firmware-extension
```

### 6.2 Package identity

Every package has:

```yaml
package:
  namespace: boards
  name: btt-octopus-pro
  version: 1.1.0
  kind: board
```

Use semantic versioning.

Package references use:

```text
namespace/name@version
```

Example:

```text
boards/btt-octopus-pro@1.1.0
```

### 6.3 Package manifest

Every package must contain:

```text
package.yaml
README.md
LICENSE
```

Optional:

```text
schemas/
templates/
assets/
tests/
firmware/
migrations/
```

### 6.4 Package dependencies

Example:

```yaml
dependencies:
  chips/stm32f446:
    version: "^1.0"
  devices/tmc2209:
    version: ">=2.0,<3.0"
```

The resolver must create a deterministic lockfile.

### 6.5 Package trust levels

Every package source must expose a trust classification:

```text
official
verified
community
local
untrusted
```

The trust level must not silently alter runtime behavior.

It is metadata used by:

- UI warnings;
- installation policy;
- update policy;
- safety policy.

### 6.6 Package immutability and integrity

A published version is immutable. Registry metadata must include a content hash and signature information. A lockfile must identify both the logical version and the exact content digest. Replacing bytes under an existing version is an integrity failure.

### 6.7 Package compatibility declarations

Packages may declare compatibility constraints for:

- schema versions;
- firmware ABI versions;
- host runtime versions;
- controller capabilities;
- board revisions;
- safety profiles;
- transport features.

Compatibility must be evaluated by the resolver and emitted as structured diagnostics.

---

## 7. Chip packages

A chip package describes one MCU family or concrete MCU variant.

Required information:

```yaml
kind: chip

architecture:
  family: arm
  isa: thumbv7em
  endianness: little

memory:
  flash: 512 KiB
  ram: 128 KiB

clocks:
  sources:
    - hsi
    - hse
  max_core_frequency: 180 MHz

peripherals:
  gpio:
    count: 81
  timers:
    - id: tim1
      channels: 4
      advanced: true
  adc:
    - id: adc1
      channels: 16
  dma:
    controllers: 2
  can:
    - id: can1
      type: bxcan
  usb:
    - id: usb_fs
      modes:
        - device

flashing:
  methods:
    - dfu
    - swd

boot:
  default_bootloader_offset: 0
```

Chip packages must describe capabilities, not board wiring.

---

## 8. Board packages

A board package binds an MCU to a physical PCB.

Required information:

- chip package;
- board revision;
- oscillator configuration;
- connector definitions;
- pin mappings;
- voltage domains;
- fixed peripherals;
- current limits;
- bootloader;
- supported flashing methods;
- known hardware limitations.

Example:

```yaml
kind: board

chip: chips/stm32f446re@1.0.0

hardware:
  manufacturer: BigTreeTech
  model: Octopus Pro
  revision: "1.1"

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

transports:
  usb:
    peripheral: usb_fs

  can0:
    peripheral: can1

flash:
  default_method: dfu
```

Board packages must not contain printer-specific assignments such as “X motor” or “hotend heater.”

---

## 9. Device packages

A device package describes a reusable physical or logical component.

Examples:

- stepper driver;
- accelerometer;
- temperature sensor;
- probe;
- load cell;
- fan;
- servo;
- CAN toolhead;
- filament sensor;
- laser module.

A device declares required capabilities.

Example:

```yaml
kind: device

device:
  class: accelerometer
  model: adxl345

requires:
  bus:
    kind: spi
    min_frequency: 1 MHz
    preferred_frequency: 8 MHz
    dma_signals: [rx]
    max_latency: 50 us
    max_jitter: 10 us

  signals:
    interrupt:
      kind: gpio_input
      optional: true

runtime:
  driver: drivers/adxl345

capabilities:
  - acceleration.sample
  - acceleration.stream
```

The device package must not hardcode board pins.

---

## 10. Resource model

Create a generic resource model used by the resolver.

Initial resource types:

```text
GPIO input
GPIO output
GPIO alternate function
ADC channel
PWM channel
timer
timer channel
DMA channel
SPI controller
I2C controller
UART controller
CAN controller
USB endpoint
interrupt line
memory region
flash region
clock source
power output
voltage domain
connector
```

Every resource must support:

- unique identity;
- ownership;
- exclusivity mode;
- sharing rules;
- timing properties;
- electrical constraints;
- dependencies;
- conflicts;
- optional preferences.

Example Rust model:

```rust
pub struct ResourceId(pub String);

pub enum ResourceKind {
    Gpio,
    Timer,
    TimerChannel,
    DmaChannel,
    SpiBus,
    I2cBus,
    Uart,
    Can,
    UsbEndpoint,
    AdcChannel,
    PwmChannel,
    PowerOutput,
    MemoryRegion,
}

pub struct ResourceRequirement {
    pub kind: ResourceKind,
    pub constraints: Vec<Constraint>,
    pub preferred: Vec<Preference>,
}
```

### 10.1 Constraint classes

Constraints must cover at least:

```text
hard equality and membership
electrical voltage/current limits
alternate-function pin compatibility
timer/channel coupling
DMA routing
bus frequency and mode
interrupt priority
latency and jitter budgets
memory and flash budgets
clock-domain compatibility
exclusive and shared ownership
physical connector occupancy
```

Preferences influence ranking but never override hard constraints.

---

## 11. Resolver

### 11.1 Responsibilities

The resolver converts:

```text
Machine Graph
+ package registry
+ target platform
+ package versions
```

into:

```text
validated resolved graph
package lockfile
firmware build plans
runtime configuration
flash plan
compatibility report
warnings
errors
```

### 11.2 Resolver phases

Implement explicit phases:

1. Parse.
2. Schema validation.
3. Package dependency resolution.
4. Package loading.
5. Graph expansion.
6. Capability matching.
7. Hardware resource allocation.
8. Electrical validation.
9. Timing validation.
10. Safety validation.
11. Firmware partitioning.
12. Artifact planning.
13. Lockfile generation.

Each phase should return structured diagnostics.

### 11.3 Diagnostics

Diagnostics must contain:

```rust
pub struct Diagnostic {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    pub source: Option<SourceSpan>,
    pub related: Vec<RelatedDiagnostic>,
    pub suggestions: Vec<Suggestion>,
}
```

Source positions are 1-based and span ends are exclusive. A span may name a
stable document identity as well as its dotted semantic path. `related` contains
the other declarations or claims that participated in a conflict; package
documents use portable package identities rather than checkout-specific absolute
paths.

Example:

```text
E1204: Timer conflict

The X step generator and heater PWM both require TIM1_CH1.

Suggested fixes:
- Move heater0 to connector heater1.
- Use DMA-based stepping on controller mainboard.
- Select board revision 1.2.
```

Do not expose raw internal errors to users.

### 11.4 Determinism

Given the same:

- machine manifest;
- package set;
- resolver version;
- target;
- policy;

the resolver must produce the same output.

When multiple equivalent assignments exist, apply stable ordering rules.

### 11.5 Resolution output provenance

Every assignment must be explainable. The resolved graph should record:

- the requirement that requested a resource;
- candidates considered;
- hard constraints applied;
- preference score;
- selected resource;
- packages and source spans that contributed the decision.

The CLI must expose this through an `explain` command.

---

## 12. Lockfile

Create `machine.lock`.

It must contain:

- exact package versions;
- package hashes;
- registry source;
- resolved board revisions;
- resolved resources;
- firmware target triples;
- protocol versions;
- feature flags;
- build profile;
- resolver version;
- safety profile version.

Example:

```yaml
lock_version: 2

machine_hash: sha256:...

packages:
  - id: boards/btt-octopus-pro@1.1.0
    manifest_hash: sha256:...
    content_hash: sha256:...

controllers:
  mainboard:
    target: thumbv7em-none-eabihf
    firmware_profile: release
    resolved_resources:
      x_step: tim1.ch1
      x_dir: gpio.pe10
      heater0_pwm: tim3.ch2
```

The lockfile is generated.

Users should not edit it manually.

---

## 13. Firmware architecture

### 13.1 Firmware core

The MCU firmware must be:

- `no_std`;
- heap-free by default;
- deterministic;
- modular;
- capability-driven;
- transport-independent where possible.

Initial firmware modules:

```text
clock
scheduler
command queue
GPIO scheduler
step generator
PWM
ADC sampling
endstop capture
watchdog
heartbeat
thermal safety
transport
boot control
telemetry
fault log
```

### 13.2 Motion HAL

Define a platform-specific HAL above `embedded-hal`.

Initial traits:

```rust
pub trait MonotonicClock {
    type Tick: Copy + Ord;
    fn now(&self) -> Self::Tick;
}

pub trait ScheduledGpio {
    type Tick;
    type Error;

    fn schedule_set(
        &mut self,
        pin: PinId,
        level: bool,
        at: Self::Tick,
    ) -> Result<(), Self::Error>;
}

pub trait StepGenerator {
    type Tick;
    type Error;

    fn enqueue(
        &mut self,
        channel: StepChannel,
        segment: StepSegment<Self::Tick>,
    ) -> Result<(), Self::Error>;
}

pub trait EdgeCapture {
    type Tick;
    type Error;

    fn capture(
        &mut self,
        input: InputId,
    ) -> Result<Option<EdgeEvent<Self::Tick>>, Self::Error>;
}

pub trait SafePwm {
    type Error;

    fn set_duty(
        &mut self,
        output: OutputId,
        duty: UnitInterval,
    ) -> Result<(), Self::Error>;

    fn force_safe_state(&mut self, output: OutputId);
}
```

Do not expose raw MCU registers outside target-specific crates.

### 13.3 Controller capabilities

At startup, the controller must report:

- firmware version;
- protocol version;
- board identity;
- MCU identity;
- clock frequency;
- queue capacity;
- supported command types;
- maximum event rate;
- available telemetry;
- safety features;
- bootloader capabilities.

### 13.4 Hybrid firmware model

There must not be one opaque universal binary for all boards. Use a hybrid model:

- a small shared Rust firmware core;
- a target-specific MCU backend;
- a compiled set of native drivers;
- a generated declarative device graph;
- a verified resolved-resource table;
- a board-specific boot and flash plan.

Fast paths may use timers, DMA, PIO, RMT, MCPWM, or specialized interrupt handlers. The capability interface is portable; the implementation is target-specific.

### 13.5 Support tiers

Each chip, board, and firmware combination must have one support tier:

```text
Experimental
Validated
Production
Safety-tested
```

The registry must not recommend an experimental target for hazardous output control without an explicit, blocking acknowledgement.

---

## 14. Host runtime

The host runtime must run natively on:

- Linux;
- macOS;
- Windows.

Initial targets:

```text
x86_64-unknown-linux-gnu
aarch64-unknown-linux-gnu
x86_64-apple-darwin
aarch64-apple-darwin
x86_64-pc-windows-msvc
aarch64-pc-windows-msvc
```

Host responsibilities:

- load the resolved machine graph;
- connect to one or more controllers;
- synchronize clocks;
- maintain command queues;
- parse jobs;
- plan motion;
- execute workflows;
- expose the platform API;
- persist state;
- stream telemetry;
- manage firmware updates;
- produce structured logs.

The host must not require Docker, WSL, a virtual machine, or systemd.

Platform-specific service installation may be implemented separately.

The operating system does not generate each step pulse. It must keep timestamped controller queues safely filled. Suspend, forced restart, USB removal, or process termination remain operational faults and must trigger edge-enforced safe behavior.

---

## 15. Motion system

The initial motion system must support:

- Cartesian kinematics;
- CoreXY kinematics;
- trapezoidal velocity profiles;
- synchronized multi-axis motion;
- homing;
- endstop handling;
- configurable velocity and acceleration limits;
- queue look-ahead;
- multi-MCU timestamp scheduling.

Later versions may add:

- input shaping;
- pressure advance;
- Delta kinematics;
- SCARA;
- non-planar motion;
- closed-loop control;
- toolchanger coordination.

Separate geometry, kinematics, planning, and MCU scheduling.

Do not place all motion logic in one module.

---

## 16. Control protocol

### 16.1 Protocol goals

The protocol between the host and controllers must be:

- binary;
- compact;
- versioned;
- deterministic;
- transport-independent;
- bounded;
- suitable for USB serial, CAN, TCP, and simulated transports;
- capable of clock synchronization;
- capable of queue monitoring;
- recoverable after partial connection loss.

### 16.2 Message categories

Initial categories:

```text
Handshake
Capability discovery
Clock synchronization
Configuration
Command queue
Scheduled GPIO
Step segments
PWM updates
ADC subscriptions
Input events
Telemetry
Faults
Heartbeat
Bootloader control
Firmware update
Emergency stop
```

### 16.3 Message framing

Every frame contains:

```text
protocol version
message type
sequence number
payload length
payload
checksum
```

The v1 Dryer command codec (`dryer.control/v1`) is a bounded custom
fixed-layout encoding. All multi-byte integers are little-endian. The frame
layout is:

```text
offset  size  field
0       2     magic: 0x44 0x52 ("DR")
2       1     protocol version: 1
3       1     message type: 1 (command) or 2 (queue status)
4       4     sequence number: u32
8       2     payload length: u16
10      N     payload
10+N    4     CRC-32C (Castagnoli), little-endian
```

CRC-32C covers bytes 2 through the end of the payload; the magic and checksum
are excluded. Command payloads begin with an envelope-flags byte (bit 0 marks
an optional `execute_at: u64` timestamp; all other bits are reserved), then a
command tag: `0` heartbeat, `1` heater target, `2` home, or `3` move. Strings
are a one-byte UTF-8 length followed by bytes and are limited to 63 bytes.
Payloads are limited to 128 bytes and complete frames to 142 bytes.

The v1 codec rejects unsupported versions/types, unknown flags/tags, malformed
lengths, invalid UTF-8, and checksum failures in a deterministic order. It is
transport-independent, allocation-free on encode, and does not use JSON in the
real-time control channel. The reference implementation and byte-level vectors
live in `crates/control-protocol`; simulator behavior continues to operate on
typed commands and uses the codec only for compatibility tests.

### 16.4 Buffer management

Controllers must report:

- queue capacity;
- queue fill level;
- earliest accepted timestamp;
- latest accepted timestamp;
- underrun state.

In `dryer.control/v1`, a controller reports this state with message type `2`
(`queue status`) using the §16.3 frame envelope. Its payload is exactly 22
bytes; all multi-byte integers are little-endian:

```text
offset  size  field
0       1     flags: 0 (all bits reserved)
1       2     queue capacity: u16
3       2     queue fill level: u16
5       8     earliest accepted timestamp: u64 ticks
13      8     latest accepted timestamp: u64 ticks
21      1     state flags: bit 0 underrun; bits 1–7 reserved and zero
```

The response reuses the frame sequence field as a `u32`. Decoders apply the
same prefix/header/version/type/bounds/exact-frame-length/checksum ordering as
command frames, then require the exact payload length before validating the
payload flags and state flags. Unknown reserved bits are invalid. The v1 wire
codec does not infer additional relationships among the reported numeric
values; controller and host scheduling policy enforce those relationships.

The host must maintain a configurable scheduling horizon.

Example:

```text
target queue horizon: 250 ms
minimum safe horizon: 100 ms
maximum horizon: 1000 ms
```

### 16.5 Clock synchronization

Implement:

- monotonic controller clocks;
- round-trip measurements;
- offset estimation;
- drift estimation;
- confidence bounds;
- periodic resynchronization.

The motion planner uses host time internally.

The transport layer converts host timestamps to controller-local ticks.

### 16.6 Control and management planes

Keep two logical channels:

```text
Control plane: bounded timestamped commands, inputs, heartbeats, faults.
Management plane: discovery, configuration, logs, telemetry, updates, diagnostics.
```

The management plane may use HTTP, WebSocket, JSON-RPC, or gRPC. Its parsing or backpressure must never block the real-time control path.

### 16.7 Failure semantics

Specify behavior for:

- duplicate, missing, late, malformed, or out-of-window frames;
- queue underrun and overrun;
- clock uncertainty above limit;
- host heartbeat loss;
- controller reset during a job;
- partial multi-controller connectivity;
- protocol version mismatch.

Every fault must map to a deterministic controller action and a structured host-visible diagnostic.

---

## 17. Workflow system

### 17.1 Objective

Replace text-based macro systems with typed, validated workflows.

A workflow must support:

- typed parameters;
- typed outputs;
- preconditions;
- resource locks;
- timeouts;
- cancellation;
- nested workflow calls;
- safe-exit handlers;
- conditional execution;
- loops with explicit bounds;
- state queries;
- capability permissions.

### 17.2 Example workflow

```yaml
api_version: forge.workflow/v0.1
kind: Workflow

metadata:
  name: print-start

parameters:
  bed_temperature:
    type: temperature
  nozzle_temperature:
    type: temperature

requires:
  capabilities:
    - motion.home
    - heater.set_target
    - heater.wait
    - motion.move

locks:
  - motion
  - heaters

steps:
  - call: heater.set_target
    with:
      heater: bed
      target: ${bed_temperature}

  - call: motion.home
    with:
      axes:
        - x
        - y
        - z

  - call: heater.wait
    with:
      heater: bed
      target: ${bed_temperature}
      timeout: 10 min

on_cancel:
  - call: motion.stop

  - call: heaters.safe_state
```

### 17.3 Runtime representation

Do not execute YAML directly.

Compilation pipeline:

```text
workflow source
→ parser
→ type checker
→ capability checker
→ intermediate representation
→ executable workflow plan
```

### 17.4 Host extensions

Future host-side third-party extensions should use WebAssembly components.

Extensions must receive explicit capabilities.

No extension gets unrestricted:

- filesystem access;
- network access;
- process creation;
- raw USB access;
- firmware flashing;
- memory access;
- raw GPIO access.

Example granted capabilities:

```text
motion.move
heater.set_target
printer.read_state
workflow.sleep
```

### 17.5 Workflow safety and concurrency

Locks must have stable ordering to prevent deadlock. Cancellation must be cooperative at defined yield points, but emergency-stop actions bypass ordinary workflow scheduling. Loops require static bounds or runtime timeouts. A workflow failure executes the narrowest applicable safe-exit handler before propagating the structured error.

---

## 18. Safety model

### 18.1 Safety ownership

The host coordinates policy. Every MCU directly controlling hazardous hardware enforces its local safety envelope.

An MCU must independently:

- force heaters off after heartbeat loss;
- enforce minimum and maximum sensor readings;
- detect implausible temperature rate-of-change;
- stop scheduled outputs on queue starvation when required by policy;
- service a hardware watchdog;
- boot and reset into safe output states;
- latch critical faults;
- support a physical emergency stop where hardware permits.

### 18.2 Safe states

Every output and actuator must declare a safe state. Examples:

```yaml
outputs:
  hotend_heater:
    safe_state: off
    heartbeat_timeout: 500 ms

  part_fan:
    safe_state: off

  stepper_enable:
    safe_state: disabled
```

Safe-state configuration must be compiled into controller firmware artifacts or signed controller configuration. It must not rely solely on host initialization.

### 18.3 Thermal safety

Thermal control must include:

- sensor open/short detection;
- minimum and maximum temperature thresholds;
- heating-rate and cooling-rate plausibility;
- target-tracking timeouts;
- maximum continuous duty policy;
- redundant sensor support where declared;
- latched shutdown for critical violations.

### 18.4 Fault classes

Define at least:

```text
Notice
Recoverable
Job-fatal
Controller-fatal
Machine-fatal
Emergency
```

The safety profile maps each fault code to local MCU action, host action, latching behavior, acknowledgement requirements, and recovery procedure.

### 18.5 Safety case artifacts

For Safety-tested packages, store:

- threat and hazard analysis;
- declared safety envelope;
- test evidence;
- hardware revisions covered;
- firmware and package hashes;
- known limitations;
- reviewer identity and expiry or revalidation policy.

---

## 19. Multi-controller systems

### 19.1 Controller identities

Each controller has a stable logical ID and an observed hardware identity. Replacement hardware must require explicit rebinding when the identity or capabilities differ.

### 19.2 Partitioning

The resolver assigns components and safety rules to controllers. A cross-controller dependency must define:

- authoritative state owner;
- synchronization requirements;
- acceptable clock uncertainty;
- behavior on link loss;
- safe degraded state.

### 19.3 Distributed motion

Multi-MCU motion uses future timestamps in a shared host time domain. The scheduler must refuse a segment when confidence bounds cannot meet the declared synchronization budget.

### 19.4 Plug-and-play discovery

Smart USB or CAN modules may report:

- hardware ID and revision;
- firmware ABI;
- capabilities;
- queue and timing limits;
- local clock properties;
- update mechanism.

Example discovery summary:

```text
Detected:
  2 steppers
  1 heater
  2 thermistor inputs
  1 accelerometer
  3 fans
  2 endstop inputs
```

Discovery identifies capabilities; it does not guess ordinary wiring, attached thermistor models, current capacity, supply voltage, or the user's intended machine role. Those bindings require a trusted manifest or explicit confirmation.

---

## 20. Registry and distribution

### 20.1 Registry functions

The registry provides:

- package metadata and content;
- version and dependency indexes;
- signatures and integrity hashes;
- trust and support levels;
- compatibility claims and test evidence;
- deprecation and security advisories;
- mirrors and offline export bundles.

### 20.2 Local and self-hosted operation

The client must support:

- local directory sources;
- offline package bundles;
- organization registries;
- public registries;
- deterministic source priority;
- pinned registry identities.

### 20.3 Signed updates

Package indexes, firmware, manifests, lockfiles, and deployment bundles should be signed. Update metadata should separate root trust, targets, snapshots, and freshness information. Multi-controller deployment must prevent partial incompatible updates and support rollback or a defined recovery route.

### 20.4 Installation policy

Policy may prohibit:

- unsigned packages;
- untrusted safety profiles;
- experimental firmware on hazardous controllers;
- packages with unresolved advisories;
- incompatible license combinations;
- network-fetched content during a locked offline build.

---

## 21. Build, flash, and deployment

### 21.1 Build plans

The resolver emits one deterministic build plan per controller. It contains:

- target triple;
- board and chip packages;
- features and native drivers;
- memory layout and bootloader offset;
- resolved resources;
- safety configuration;
- protocol and ABI versions;
- toolchain identity;
- expected artifact hashes when reproducible.

### 21.2 Flash plans

A flash plan must identify:

- device selection rule;
- transport and flashing method;
- bootloader transition steps;
- expected current firmware identity;
- artifact hash and signature;
- verification method;
- rollback or recovery instructions.

Example:

```yaml
controller: mainboard
method: dfu
select:
  usb_vid: 0x0483
  usb_pid: 0xdf11
artifact: build/mainboard/forgeos.bin
verify:
  sha256: sha256:...
post_flash:
  expect_board: boards/btt-octopus-pro@1.1.0
```

### 21.3 Transactional deployment

Deployment must use prepare, verify, activate, and confirm phases. In a multi-controller machine, activation must either preserve compatibility across the set or stop before an unsafe mixed version is used.

### 21.4 Cross-platform flashing

The flashing tool must run natively on Linux, macOS, and Windows and support dry-run output. Device matching must be explicit enough to avoid flashing an unrelated attached device.

---

## 22. Platform API and state service

The platform API covers:

```text
machine description
state and capabilities
jobs and files
motion and heaters
workflows
configuration and resolution
packages and updates
telemetry and logs
faults and recovery
backups and restore
```

Requirements:

- versioned schemas;
- authenticated writes;
- read-only local discovery mode where policy permits;
- idempotency keys for retryable commands;
- structured errors;
- event sequence numbers and resumable subscriptions;
- explicit units and timestamps;
- authorization by capability, not UI route.

The internal state service is authoritative. UI clients and compatibility adapters project that state; they do not own it.

---

## 23. Compatibility

### 23.1 Klipper configuration adapter

Phase 1 must be able to generate Klipper-compatible configuration and firmware build parameters from the resolved Machine Graph. Import of existing configuration is best-effort and must produce an ambiguity report rather than silently inventing semantics.

### 23.2 Klipper MCU protocol

Phase 2 may implement a Rust MCU compatible with the existing Klipper host protocol for a deliberately narrow target and command set. Treat protocol behavior as a compatibility contract with conformance tests.

### 23.3 Moonraker-compatible API

The native host may expose a compatibility API sufficient for Mainsail and Fluidd. Keep this adapter separate from the native platform API and test both HTTP/JSON-RPC behavior and WebSocket event semantics.

### 23.4 G-code

G-code remains a job and command adapter. Convert accepted commands to typed internal operations. Preserve unknown or unsupported commands as diagnostics, never implicit no-ops in safety-relevant paths.

### 23.5 Licensing boundary

Before copying or deriving code, protocol tables, tests, or configuration data, record their licenses and provenance. A clean-room implementation must be based on permissible specifications and independently authored code. Repository license policy must be decided before distributing combined artifacts.

---

## 24. Simulation and testing

### 24.1 Simulator

Provide a simulated MCU transport and machine model capable of:

- virtual monotonic time;
- deterministic queue execution;
- configurable transport latency, jitter, loss, and duplication;
- synthetic sensors and endstops;
- thermal plant models;
- controller reset and link-loss injection;
- event trace capture and replay.

### 24.2 Test layers

Required layers:

```text
schema fixtures
package validation tests
resolver golden tests
property tests for units and allocation
protocol codec and fuzz tests
clock synchronization simulations
workflow type and cancellation tests
safety fault-injection tests
hardware-in-the-loop tests
compatibility conformance tests
reproducible build tests
```

### 24.3 Golden fixtures

Store expected resolved graphs, diagnostics, lockfiles, and build plans for representative machines. Golden output changes require explicit review because they may alter wiring or safety behavior.

### 24.4 Performance budgets

Each validated target must publish measured budgets for:

- maximum scheduled event rate;
- worst observed interrupt latency;
- queue capacity;
- clock drift and synchronization error;
- telemetry load;
- flash erase pauses;
- transport latency and recovery.

---

## 25. Observability, recovery, and backups

Logs, metrics, traces, and fault records must use stable codes and monotonic plus wall-clock timestamps where available. A support bundle should contain redacted configuration, versions, graph and lock hashes, recent diagnostics, controller fault logs, and platform information.

Persist:

- source manifests;
- lockfiles;
- calibration records;
- deployment history;
- firmware artifacts or reproducible references;
- job state and fault history.

Recovery procedures must be documented for host loss, controller replacement, interrupted updates, corrupt local state, and forgotten credentials.

---

## 26. Command-line interface

Initial CLI surface:

```text
forge init
forge validate machine.yaml
forge resolve machine.yaml
forge explain machine.lock <resource-or-diagnostic>
forge build --locked
forge flash <controller> --dry-run
forge deploy --locked
forge discover
forge simulate machine.yaml
forge workflow check <file>
forge import klipper <printer.cfg>
forge export klipper
forge registry search <query>
forge doctor
forge support-bundle
```

All mutating commands must support a preview or dry-run when practical. Machine-readable output should use a versioned JSON schema.

---

## 27. Security model

Threats include malicious packages, compromised registries, hostile network clients, extension escape, device impersonation, rollback to vulnerable firmware, corrupted configuration, and accidental operator action.

Minimum controls:

- signed artifacts and pinned trust roots;
- authenticated, authorized API writes;
- capability-scoped extensions;
- least-privilege host services;
- bounded parsing on all MCU-facing inputs;
- replay and downgrade protection where applicable;
- secret separation from machine manifests and support bundles;
- security advisories tied to package and firmware ranges;
- auditable deployment actions;
- safe defaults on missing or invalid policy.

Security controls must not weaken local-first operation.

---

## 28. Implementation roadmap

### Phase 0 — specifications and repository foundations

Deliver:

- repository workspace and crate boundaries;
- Machine Graph v0.1 schema;
- package manifests and lockfile schemas;
- diagnostic code conventions;
- representative machine fixtures;
- protocol and safety design records;
- CI for formatting, linting, schemas, and docs.

Exit criteria:

- minimal Cartesian and CoreXY fixtures validate;
- schema changes are versioned;
- architecture boundaries are documented;
- no safety-critical behavior depends on an unspecified default.

### Phase 1 — platform layer over Klipper

Create:

- Machine Graph parser and validator;
- registry of initial boards and modules;
- deterministic dependency and resource resolver;
- Klipper configuration generator;
- firmware build-parameter generator;
- native cross-platform flasher for macOS, Windows, and Linux;
- lockfile, compatibility report, and simulator.

Exit criteria:

- one minimal Cartesian and one CoreXY machine resolve reproducibly;
- generated Klipper configuration boots on a supported test machine;
- invalid electrical and timer assignments fail with actionable diagnostics;
- a locked build and flash plan can be reproduced offline.

### Phase 2 — Rust MCU compatible with Klipper

Choose one initial target, preferably RP2040 or STM32G0. Implement:

- USB transport;
- GPIO and scheduled output;
- step queue;
- PWM;
- ADC sampling;
- endstops;
- watchdog and local thermal safety;
- the required subset of the existing Klipper MCU protocol.

Exit criteria:

- the controller operates with Klippy, Moonraker, and Mainsail;
- queue underrun, heartbeat loss, reset, and sensor faults enter verified safe states;
- timing and throughput budgets are measured and published;
- protocol conformance tests run in simulation and on hardware.

### Phase 3 — native Rust host

Implement:

- cross-platform machine state service;
- geometry, kinematics, motion planning, and scheduling crates;
- native control protocol client and simulated transport;
- multi-MCU clock synchronization;
- workflow compiler and runtime;
- native platform API;
- Klipper/Moonraker compatibility API;
- import of legacy configuration with ambiguity reports.

Exit criteria:

- the host runs natively on Linux, macOS, and Windows;
- a complete simulated print is deterministic;
- the reference hardware completes the agreed motion and thermal test suite;
- compatibility clients function without controlling the internal architecture.

### Phase 4 — native ecosystem

Add:

- protocol v2;
- typed workflow registry;
- capability-scoped WebAssembly host extensions;
- CAN and USB capability discovery;
- signed OTA and coordinated multi-controller updates;
- distributed toolheads;
- automatic resource allocation across a broader package registry;
- support-tier evidence and safety-tested profiles.

Exit criteria:

- third-party packages cannot bypass declared capabilities;
- offline and self-hosted registry workflows are complete;
- multi-controller update rollback is tested;
- automatic allocation remains deterministic and explainable.

### Roadmap rule

Do not start with a full motion-stack rewrite. Phase 1 produces immediate value and builds the compatibility dataset needed by later phases. Each phase must leave a useful, testable system and must not make later native interfaces depend on legacy adapters.

---

## 29. Initial implementation order

Within Phase 0 and Phase 1, implement in this order:

1. `machine-schema` and schema fixtures.
2. `package-model` and local registry loading.
3. `resource-model` with stable identifiers and units.
4. `machine-parser` with source spans.
5. `machine-resolver` phases 1–7 and structured diagnostics.
6. Electrical, timing, and safety validation.
7. `machine-lock` canonical serialization and hashing.
8. Klipper config and build-parameter export.
9. Simulated controller and end-to-end golden tests.
10. Cross-platform flash discovery and dry-run plans.

Avoid premature UI work. A CLI and stable machine-readable diagnostics are sufficient until the data model and resolver behavior settle.

---

## 30. Definition of done for v0.1

The v0.1 specification milestone is complete when:

- all normative schemas are versioned and have positive and negative fixtures;
- the resolver is deterministic and its decisions are explainable;
- all physical quantities are typed;
- the minimal Cartesian, CoreXY, and multi-MCU toolhead examples validate;
- the lockfile captures exact packages, resources, targets, protocol versions, and safety policy;
- firmware and host boundaries are documented by versioned interfaces;
- real-time control and management planes are separate;
- heartbeat, queue starvation, sensor failure, and reset behavior are specified and tested;
- Klipper, Moonraker, Mainsail, Fluidd, and G-code appear only behind compatibility adapters;
- local offline build, validation, deployment planning, and recovery are documented;
- the repository contains no unresolved “magic” wiring or safety defaults.

---

## 31. Explicit non-goals for the first milestone

Do not attempt in v0.1:

- support for every MCU or controller board;
- arbitrary runtime code loading on MCUs;
- cloud-required operation;
- automatic inference of ordinary wiring;
- a complete slicer;
- a production UI before the platform API stabilizes;
- full compatibility with every Klipper macro or extension;
- safety certification claims without defined evidence;
- closed-loop motion, non-planar printing, or advanced robotics.

---

## 32. Architectural summary

The durable value of ForgeOS is not simply that firmware is written in Rust. It is the combination of:

1. **Machine Graph** as the single source of truth.
2. **Resolver** that checks software, electrical, timing, and safety compatibility.
3. **Registry** of boards, devices, workflows, safety profiles, and verified combinations.
4. **Stable capability interfaces** between hardware, firmware, host, workflows, and extensions.

Rust provides a strong implementation foundation. The product is effectively a package manager, compiler, deployment system, and operating platform for modular machines.

The immediate next step is to formalize Machine Graph v0.1, package types, resolver diagnostics, and the lockfile before implementing the native motion stack.
