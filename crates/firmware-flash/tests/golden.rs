use dryer_firmware_build::ControllerBuildPlanArtifact;
use dryer_firmware_flash::{
    plan_dry_run, ArtifactSpec, DiscoveredUsbDevice, DryRunRequest, PlanError, FLASH_PLAN_SCHEMA,
};
use dryer_machine_lock::Lockfile;
use dryer_package_model::LocalRegistry;
use std::path::Path;

fn build_plan(root: &Path) -> ControllerBuildPlanArtifact {
    serde_json::from_str(
        &std::fs::read_to_string(
            root.join("examples/minimal-cartesian/controller-build-plan.golden.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

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
    let build_plan = build_plan(&root);
    let registry = LocalRegistry::load(&root.join("packages"));
    assert!(registry.diagnostics.is_empty());
    let inventory: Vec<DiscoveredUsbDevice> = serde_json::from_str(
        &std::fs::read_to_string(
            root.join("examples/minimal-cartesian/usb-inventory.fixture.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let artifact = root.join("examples/minimal-cartesian/controller-image.golden.json");
    let plan = plan_dry_run(DryRunRequest {
        controller: "mainboard",
        lock: &lock,
        build_plan: &build_plan,
        registry: &registry,
        discovered_devices: &inventory,
        artifact: ArtifactSpec {
            path: &artifact,
            signature: None,
        },
        expected_current_firmware: "dryer-simulator/0.1.0",
    })
    .unwrap();
    assert!(!plan.ready);
    assert_eq!(plan.blocked_reasons.len(), 1);
    assert!(plan.blocked_reasons[0].contains("not a deployable controller executable"));

    let expected =
        std::fs::read_to_string(root.join("examples/minimal-cartesian/flash-plan.golden.json"))
            .unwrap();
    assert_eq!(plan.to_pretty_json(), expected);
}

#[test]
fn normative_schema_tracks_the_flash_plan() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("schemas/flash-plan.schema.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(schema["properties"]["schema"]["const"], FLASH_PLAN_SCHEMA);
    assert_eq!(
        schema["$defs"]["artifact"]["properties"]["expected_sha256"]["$ref"],
        "#/$defs/sha256"
    );
    let required = schema["$defs"]["artifact"]["required"].as_array().unwrap();
    assert!(required.iter().any(|field| field == "format"));
    assert!(required.iter().any(|field| field == "deployable"));
}

#[test]
fn build_plan_drift_is_rejected_before_artifact_io() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lock = Lockfile::from_yaml(
        &std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.lock")).unwrap(),
    )
    .unwrap();
    let mut build_plan = build_plan(&root);
    build_plan.expected_artifact.sha256 =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000".into();
    let registry = LocalRegistry::load(&root.join("packages"));
    let missing_artifact = root.join("examples/minimal-cartesian/does-not-exist.bin");

    let error = plan_dry_run(DryRunRequest {
        controller: "mainboard",
        lock: &lock,
        build_plan: &build_plan,
        registry: &registry,
        discovered_devices: &[],
        artifact: ArtifactSpec {
            path: &missing_artifact,
            signature: None,
        },
        expected_current_firmware: "dryer-simulator/0.1.0",
    })
    .unwrap_err();
    match error {
        PlanError::BuildOutput(message) => {
            assert!(message.contains("build plan mismatch"), "{message}")
        }
        other => panic!("expected build-plan drift, got {other}"),
    }
}

#[test]
fn ambiguity_and_artifact_drift_are_both_blocking() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lock = Lockfile::from_yaml(
        &std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.lock")).unwrap(),
    )
    .unwrap();
    let build_plan = build_plan(&root);
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
        build_plan: &build_plan,
        registry: &registry,
        discovered_devices: &devices,
        artifact: ArtifactSpec {
            path: &artifact,
            signature: None,
        },
        expected_current_firmware: "dryer-simulator/0.1.0",
    })
    .unwrap();
    assert!(!plan.ready);
    assert_eq!(plan.blocked_reasons.len(), 3);
    assert!(plan.blocked_reasons[0].contains("2 USB devices match"));
    assert!(plan.blocked_reasons[1].contains("sha256"));
    assert!(plan.blocked_reasons[2].contains("not a deployable controller executable"));
}

#[test]
fn registry_source_drift_is_rejected_before_flash_planning() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lock = Lockfile::from_yaml(
        &std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.lock")).unwrap(),
    )
    .unwrap();
    let build_plan = build_plan(&root);
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
        build_plan: &build_plan,
        registry: &registry,
        discovered_devices: &[],
        artifact: ArtifactSpec {
            path: &artifact,
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
    let build_plan = build_plan(&root);
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
        build_plan: &build_plan,
        registry: &registry,
        discovered_devices: &inventory,
        artifact: ArtifactSpec {
            path: &artifact,
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
