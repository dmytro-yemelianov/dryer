# Controller Daemon & Dryer Workspace Handover (2026-08-05)

## Workspace & Repository Overview

The **Dryer** workspace is an end-to-end deterministic machine configuration, resolution, flashing, protocol, and simulation framework written in Rust.

All tests in the workspace pass cleanly across all crates (`cargo test` verified with 0 failures).

---

## Completed Slices & Recent Work

### 1. Unified Command-Line Interface (`dryer-cli`) — Slice 26
- **Location**: [`crates/dryer-cli/src/main.rs`](file:///Users/dmytro/Documents/github/dryer/crates/dryer-cli/src/main.rs)
- **Commands Implemented**:
  - `dryer check <machine.yaml>` — schema & resolver validation with exact source-span diagnostics.
  - `dryer lock <machine.yaml> [out.lock]` — generates canonical lockfile v5 with full package digests and provenance.
  - `dryer verify-lock <machine.lock>` — validates lockfile content digests and SHA-256 hashes against disk packages.
  - `dryer flash-plan <machine.lock>` — dry-run USB flash discovery and verified flash plan generation.
  - `dryer audit <gcode/toolpath>` — pre-flight toolpath auditing against machine safety limits.
  - `dryer sim <machine.lock> [job-trace]` — runs full simulated job execution on the virtual controller.

### 2. Multi-Controller Clock Synchronization & Workflow Lowering — Slices 24–25
- **`MultiControllerClockSync`**: Event-driven clock synchronization manager across multi-controller clusters in `dryer-control-client`.
- **Workflow Step Lowering**: `WorkflowStep::lower` converts declared workflow steps into `dryer.control/v1` wire frames.
- **Toolpath Auditor**: `dryer-toolpath-auditor` enforces kinematics bounds, maximum feed rates, and thermal safety ceilings.
- **Flash Execution Engine**: `FlashExecutor` trait and `MockFlashExecutor` in `dryer-firmware-flash`.
- **E2E Pipeline Test**: `e2e_job_execution.rs` validates end-to-end resolution, lock, toolpath audit, lowering, and simulated execution.

### 3. Machine Template Package Expansion
- Added package templates in `packages/machines/`:
  - `corexy-standard`
  - `delta-basic`
  - `toolchanger-corexy`

---

## Current Architecture Seams & Contracts

```text
Applications / CLI (`dryer-cli`)
      │
      ▼
Host State & Controller Daemon (`dryer-controller-daemon` — Slice 27 Next)
      │
      ▼
Control Client & Cluster Sync (`dryer-control-client`, `dryer-clock-sync`)
      │
      ▼
Wire Protocol (`dryer.control/v1` in `dryer-control-protocol`)
      │
      ▼
Hardware / Controller Simulator (`dryer-simulator`)
```

---

## Handover Task: Slice 27 — Controller Daemon (`dryer-controller-daemon`)

### Core Objective
Implement `dryer-controller-daemon` as the host-side state service that manages controller connections, queue horizons, clock drift bounds, heartbeat monitoring, and scheduled command dispatch over `dryer.control/v1`.

### Key Component Specifications
1. **`ControllerSession`**:
   - Binds a `ControlClient` to a transport target (simulator or hardware serial/USB/socket).
   - Manages queue fill status (Type 2 queue status decoding).
   - Enforces lead/horizon scheduling windows (1 ms execution quantum).

2. **`ClusterDaemon`**:
   - Manages multiple `ControllerSession` instances using `MultiControllerClockSync`.
   - Maintains continuous multi-controller clock alignment and calculates joint execution time bounds.

3. **`HeartbeatGuard`**:
   - Monitors per-controller heartbeat interval (`heartbeat_timeout`).
   - Issues safe-state action or triggers emergency shutdown if controller response deadline is missed.

4. **Integration with `dryer-cli`**:
   - Add `dryer daemon <machine.lock>` subcommand to launch host controller daemon runtime.

---

## Verification Strategy

1. `cargo check --workspace`
2. `cargo test --workspace`
3. E2E simulator integration tests in `crates/simulator/tests/` verifying daemon command dispatch, clock sync drift updates, and heartbeat timeout safe-state latching.
