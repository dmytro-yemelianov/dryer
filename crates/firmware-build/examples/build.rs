//! `cargo run -p dryer-firmware-build --example build -- <machine.lock>
//!   <controller> -o <firmware-image> [--plan-out <build-plan.json>]`
//!
//! Builds the deterministic `dryer.controller-image/v1` reference image.

use dryer_firmware_build::build_controller;
use dryer_machine_lock::Lockfile;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(lock_path) = args.next() else {
        return usage();
    };
    let Some(controller) = args.next() else {
        return usage();
    };
    let mut image_path = None;
    let mut plan_path = None;
    while let Some(flag) = args.next() {
        let value = args.next();
        match (flag.as_str(), value) {
            ("-o", Some(value)) if image_path.is_none() => image_path = Some(value),
            ("--plan-out", Some(value)) if plan_path.is_none() => plan_path = Some(value),
            _ => return usage(),
        }
    }
    let Some(image_path) = image_path else {
        return usage();
    };

    let lock_text = match std::fs::read_to_string(&lock_path) {
        Ok(text) => text,
        Err(error) => return fail(format!("cannot read {lock_path}: {error}")),
    };
    let lock = match Lockfile::from_yaml(&lock_text) {
        Ok(lock) => lock,
        Err(error) => return fail(format!("cannot parse {lock_path}: {error}")),
    };
    let built = match build_controller(&lock, &controller) {
        Ok(built) => built,
        Err(error) => return fail(error.to_string()),
    };
    if let Err(error) = write_parented(Path::new(&image_path), &built.bytes) {
        return fail(format!("cannot write {image_path}: {error}"));
    }
    if let Some(plan_path) = plan_path {
        if let Err(error) = write_parented(
            Path::new(&plan_path),
            built.plan.to_pretty_json().as_bytes(),
        ) {
            return fail(format!("cannot write {plan_path}: {error}"));
        }
    }
    println!(
        "built: {image_path} ({} bytes, {})",
        built.plan.expected_artifact.size_bytes, built.plan.expected_artifact.sha256
    );
    ExitCode::SUCCESS
}

fn write_parented(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: build <machine.lock> <controller> -o <firmware-image> \
         [--plan-out <build-plan.json>]"
    );
    ExitCode::from(2)
}

fn fail(message: String) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(2)
}
