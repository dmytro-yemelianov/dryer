//! `cargo run -p dryer-firmware-flash --example plan -- <machine.lock>
//!   <packages-dir> <controller> <artifact> <expected-sha256>
//!   <expected-current-firmware> [--inventory devices.json]`
//!
//! Read-only: prints a dry-run plan and never opens or flashes a device.

use dryer_firmware_flash::{
    discover_usb_devices, plan_dry_run, ArtifactSpec, DiscoveredUsbDevice, DryRunRequest,
};
use dryer_machine_lock::Lockfile;
use dryer_package_model::LocalRegistry;
use std::{path::Path, process::ExitCode};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(lock_path) = args.next() else {
        return usage();
    };
    let Some(packages_path) = args.next() else {
        return usage();
    };
    let Some(controller) = args.next() else {
        return usage();
    };
    let Some(artifact_path) = args.next() else {
        return usage();
    };
    let Some(expected_sha256) = args.next() else {
        return usage();
    };
    let Some(expected_current_firmware) = args.next() else {
        return usage();
    };
    let mut inventory_path = None;
    while let Some(arg) = args.next() {
        if arg != "--inventory" || inventory_path.is_some() {
            return usage();
        }
        inventory_path = args.next();
        if inventory_path.is_none() {
            return usage();
        }
    }

    let lock_text = match std::fs::read_to_string(&lock_path) {
        Ok(text) => text,
        Err(error) => return fail(format!("cannot read {lock_path}: {error}")),
    };
    let lock = match Lockfile::from_yaml(&lock_text) {
        Ok(lock) => lock,
        Err(error) => return fail(format!("cannot parse {lock_path}: {error}")),
    };
    let registry = LocalRegistry::load(Path::new(&packages_path));
    if !registry.diagnostics.is_empty() {
        for diagnostic in &registry.diagnostics {
            eprintln!("{diagnostic}");
        }
        return ExitCode::from(2);
    }
    let devices = match inventory_path {
        Some(path) => match read_inventory(&path) {
            Ok(devices) => devices,
            Err(error) => return fail(error),
        },
        None => match discover_usb_devices() {
            Ok(devices) => devices,
            Err(error) => return fail(error.to_string()),
        },
    };

    let request = DryRunRequest {
        controller: &controller,
        lock: &lock,
        registry: &registry,
        discovered_devices: &devices,
        artifact: ArtifactSpec {
            path: Path::new(&artifact_path),
            plan_path: &artifact_path,
            expected_sha256: &expected_sha256,
            signature: None,
        },
        expected_current_firmware: &expected_current_firmware,
    };
    match plan_dry_run(request) {
        Ok(plan) => {
            print!("{}", plan.to_pretty_json());
            if plan.ready {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(error) => fail(error.to_string()),
    }
}

fn read_inventory(path: &str) -> Result<Vec<DiscoveredUsbDevice>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read inventory {path}: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("cannot parse inventory {path}: {error}"))
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: plan <machine.lock> <packages-dir> <controller> <artifact> \
         <expected-sha256> <expected-current-firmware> [--inventory devices.json]"
    );
    ExitCode::from(2)
}

fn fail(message: String) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(2)
}
