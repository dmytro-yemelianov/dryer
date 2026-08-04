use dryer_firmware_flash::{plan_dry_run, ArtifactSpec, DiscoveredUsbDevice, DryRunRequest};
use dryer_machine_lock::Lockfile;
use dryer_package_model::LocalRegistry;
use std::path::Path;

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
