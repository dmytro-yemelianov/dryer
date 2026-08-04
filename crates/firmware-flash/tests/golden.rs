use dryer_firmware_flash::{
    plan_dry_run, ArtifactSpec, DiscoveredUsbDevice, DryRunRequest, PlanError,
};
use dryer_machine_lock::Lockfile;
use dryer_package_model::LocalRegistry;
use std::path::Path;

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type().unwrap();
        if file_type.is_dir() {
            copy_tree(&source_path, &destination_path);
        } else if file_type.is_file() {
            std::fs::copy(source_path, destination_path).unwrap();
        } else {
            panic!("fixture package contains an unsupported filesystem entry");
        }
    }
}

#[test]
fn minimal_cartesian_flash_plan_is_drift_gated() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lock = Lockfile::from_yaml(
        &std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.lock")).unwrap(),
    )
    .unwrap();
    let registry = LocalRegistry::load(&root.join("packages"));
    assert!(registry.diagnostics.is_empty());
    let inventory: Vec<DiscoveredUsbDevice> = serde_json::from_str(
        &std::fs::read_to_string(
            root.join("examples/minimal-cartesian/usb-inventory.fixture.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let artifact = root.join("examples/minimal-cartesian/firmware.fixture.bin");
    let plan = plan_dry_run(DryRunRequest {
        controller: "mainboard",
        lock: &lock,
        registry: &registry,
        discovered_devices: &inventory,
        artifact: ArtifactSpec {
            path: &artifact,
            plan_path: "examples/minimal-cartesian/firmware.fixture.bin",
            expected_sha256:
                "sha256:6c92abd61b162679e332cdad7b2a7753d1888de5fecb3363331207ca99d73c2a",
            signature: None,
        },
        expected_current_firmware: "dryer-simulator/0.1.0",
    })
    .unwrap();
    assert!(plan.ready, "{:?}", plan.blocked_reasons);

    let expected =
        std::fs::read_to_string(root.join("examples/minimal-cartesian/flash-plan.golden.json"))
            .unwrap();
    assert_eq!(plan.to_pretty_json(), expected);
}

#[test]
fn ambiguity_and_artifact_drift_are_both_blocking() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lock = Lockfile::from_yaml(
        &std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.lock")).unwrap(),
    )
    .unwrap();
    let registry = LocalRegistry::load(&root.join("packages"));
    let inventory: Vec<DiscoveredUsbDevice> = serde_json::from_str(
        &std::fs::read_to_string(
            root.join("examples/minimal-cartesian/usb-inventory.fixture.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let devices = [inventory[0].clone(), inventory[0].clone()];
    let artifact = root.join("examples/minimal-cartesian/firmware.fixture.bin");
    let plan = plan_dry_run(DryRunRequest {
        controller: "mainboard",
        lock: &lock,
        registry: &registry,
        discovered_devices: &devices,
        artifact: ArtifactSpec {
            path: &artifact,
            plan_path: "examples/minimal-cartesian/firmware.fixture.bin",
            expected_sha256:
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            signature: None,
        },
        expected_current_firmware: "dryer-simulator/0.1.0",
    })
    .unwrap();
    assert!(!plan.ready);
    assert_eq!(plan.blocked_reasons.len(), 2);
    assert!(plan.blocked_reasons[0].contains("2 USB devices match"));
    assert!(plan.blocked_reasons[1].contains("sha256"));
}

#[test]
fn registry_source_drift_is_rejected_before_flash_planning() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lock = Lockfile::from_yaml(
        &std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.lock")).unwrap(),
    )
    .unwrap();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let changed_registry_dir = std::env::temp_dir().join(format!(
        "dryer-flash-registry-drift-{}-{nonce}",
        std::process::id()
    ));
    copy_tree(&root.join("packages"), &changed_registry_dir);
    std::fs::write(
        changed_registry_dir.join("registry.yaml"),
        b"schema: dryer.registry/v1\nid: dryer-official\nuri: git+https://example.invalid/dryer.git?subdir=packages\n",
    )
    .unwrap();
    let registry = LocalRegistry::load(&changed_registry_dir);
    assert!(registry.diagnostics.is_empty());
    let artifact = root.join("examples/minimal-cartesian/firmware.fixture.bin");

    let error = plan_dry_run(DryRunRequest {
        controller: "mainboard",
        lock: &lock,
        registry: &registry,
        discovered_devices: &[],
        artifact: ArtifactSpec {
            path: &artifact,
            plan_path: "examples/minimal-cartesian/firmware.fixture.bin",
            expected_sha256:
                "sha256:6c92abd61b162679e332cdad7b2a7753d1888de5fecb3363331207ca99d73c2a",
            signature: None,
        },
        expected_current_firmware: "dryer-simulator/0.1.0",
    })
    .unwrap_err();
    match error {
        PlanError::RegistryDrift(message) => {
            assert!(message.contains("registry source expected"), "{message}")
        }
        other => panic!("expected registry source drift, got {other}"),
    }

    std::fs::remove_dir_all(changed_registry_dir).unwrap();
}

#[test]
fn package_companion_file_drift_is_blocking() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lock = Lockfile::from_yaml(
        &std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.lock")).unwrap(),
    )
    .unwrap();
    let mut registry = LocalRegistry::load(&root.join("packages"));
    let board = registry
        .packages
        .iter_mut()
        .find(|package| package.reference.to_string() == "boards/example-mainboard@1.0.0")
        .unwrap();
    let source_dir = board.dir.clone();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let changed_dir = std::env::temp_dir().join(format!(
        "dryer-flash-package-drift-{}-{nonce}",
        std::process::id()
    ));
    copy_tree(&source_dir, &changed_dir);
    std::fs::write(changed_dir.join("README.md"), b"changed after locking\n").unwrap();
    board.dir = changed_dir.clone();

    let inventory: Vec<DiscoveredUsbDevice> = serde_json::from_str(
        &std::fs::read_to_string(
            root.join("examples/minimal-cartesian/usb-inventory.fixture.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let artifact = root.join("examples/minimal-cartesian/firmware.fixture.bin");
    let error = plan_dry_run(DryRunRequest {
        controller: "mainboard",
        lock: &lock,
        registry: &registry,
        discovered_devices: &inventory,
        artifact: ArtifactSpec {
            path: &artifact,
            plan_path: "examples/minimal-cartesian/firmware.fixture.bin",
            expected_sha256:
                "sha256:6c92abd61b162679e332cdad7b2a7753d1888de5fecb3363331207ca99d73c2a",
            signature: None,
        },
        expected_current_firmware: "dryer-simulator/0.1.0",
    })
    .unwrap_err();
    match error {
        PlanError::RegistryDrift(message) => {
            assert!(message.contains("package content"), "{message}")
        }
        other => panic!("expected package content drift, got {other}"),
    }

    std::fs::remove_dir_all(changed_dir).unwrap();
}
