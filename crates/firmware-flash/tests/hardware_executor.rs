use dryer_firmware_build::ControllerBuildPlanArtifact;
use dryer_firmware_flash::{
    plan_dry_run, sha256, ArtifactPlan, ArtifactSpec, DiscoveredUsbDevice, DryRunRequest, FlashExecutionError,
    FlashExecutor, FlashPlan, FlashToolMethod, MockFlashExecutor, NativeFlashExecutor, MatchStatus,
    UsbSelectionRule, DeviceSelection, FLASH_PLAN_SCHEMA,
};
use dryer_machine_lock::Lockfile;
use dryer_package_model::LocalRegistry;
use std::path::Path;

fn build_plan(root: &Path) -> ControllerBuildPlanArtifact {
    serde_json::from_str(
        &std::fs::read_to_string(
            root.join("examples/minimal-cartesian/controller-build-plan.mainboard.golden.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn integration_plan_dry_run_with_native_and_mock_executors() {
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
    let artifact_path = root.join("examples/minimal-cartesian/controller-image.mainboard.golden.json");

    // Generate flash plan via plan_dry_run
    let plan = plan_dry_run(DryRunRequest {
        controller: "mainboard",
        lock: &lock,
        build_plan: &build_plan,
        registry: &registry,
        discovered_devices: &inventory[..1],
        artifact: ArtifactSpec {
            path: &artifact_path,
            signature: None,
        },
        expected_current_firmware: "dryer-simulator/0.1.0",
    })
    .unwrap();

    assert!(!plan.ready);
    assert!(!plan.blocked_reasons.is_empty());

    let artifact_bytes = std::fs::read(&artifact_path).unwrap();

    let mut native = NativeFlashExecutor::dry_run();
    let err = native.execute_flash(&plan, &artifact_bytes).unwrap_err();
    assert!(matches!(err, FlashExecutionError::PlanNotReady { .. }));

    // Mark plan as ready for execution testing
    let mut executable_plan = plan.clone();
    executable_plan.ready = true;
    executable_plan.blocked_reasons.clear();
    executable_plan.artifact.deployable = true;

    // Verify ready plan execution with MockFlashExecutor
    let mut mock = MockFlashExecutor::default();
    let mock_result = mock.execute_flash(&executable_plan, &artifact_bytes).unwrap();
    assert!(mock_result.success);
    assert_eq!(mock_result.controller_id, "mainboard");

    // Verify ready plan execution with NativeFlashExecutor (dry run mode)
    let native_result = native.execute_flash(&executable_plan, &artifact_bytes).unwrap();
    assert!(native_result.success);
    assert_eq!(native_result.controller_id, "mainboard");
    assert!(native_result
        .execution_log
        .iter()
        .any(|log| log.contains("dfu-util")));
}

#[test]
fn integration_command_line_generation_for_all_supported_tools() {
    let payload = b"artifact_binary_data";
    let hash = sha256(payload);

    let make_plan = |method: &str, candidates: Vec<DiscoveredUsbDevice>| FlashPlan {
        schema: FLASH_PLAN_SCHEMA.into(),
        mode: "dry_run".into(),
        ready: true,
        controller: "mainboard".into(),
        lock_hash: "sha256:dummy".into(),
        board: "boards/example-mainboard@1.0.0".into(),
        method: method.into(),
        transport: "usb".into(),
        device_selection: DeviceSelection {
            rule: UsbSelectionRule {
                usb_vid: 0x1209,
                usb_pid: 0xd003,
                serial_number: None,
                manufacturer: None,
                product: None,
            },
            status: MatchStatus::Unique,
            candidates,
        },
        expected_current_firmware: "1.0.0".into(),
        artifact: ArtifactPlan {
            format: "bin".into(),
            path: "target/firmware.bin".into(),
            size_bytes: payload.len() as u64,
            expected_sha256: hash.clone(),
            observed_sha256: hash.clone(),
            hash_matches: true,
            deployable: true,
            signature: None,
        },
        steps: Vec::new(),
        blocked_reasons: Vec::new(),
        recovery: Vec::new(),
    };

    let dev_usb = DiscoveredUsbDevice {
        platform: "linux".into(),
        bus_id: "2".into(),
        location: "/dev/ttyUSB0".into(),
        device_address: 3,
        usb_vid: 0x1209,
        usb_pid: 0xd003,
        serial_number: Some("SN999".into()),
        manufacturer: None,
        product: None,
    };

    let executor = NativeFlashExecutor::dry_run();

    // DfuUtil
    let dfu_plan = make_plan("dfu_util", vec![dev_usb.clone()]);
    let dfu_cmd = executor.build_command(&dfu_plan, Path::new("target/firmware.bin")).unwrap();
    assert_eq!(dfu_cmd.tool, FlashToolMethod::DfuUtil);
    assert_eq!(dfu_cmd.program, "dfu-util");
    assert_eq!(dfu_cmd.args, vec!["-d", "1209:d003", "-S", "SN999", "-D", "target/firmware.bin", "-R"]);

    // Stm32Flash
    let stm_plan = make_plan("stm32flash", vec![dev_usb.clone()]);
    let stm_cmd = executor.build_command(&stm_plan, Path::new("target/firmware.bin")).unwrap();
    assert_eq!(stm_cmd.tool, FlashToolMethod::Stm32Flash);
    assert_eq!(stm_cmd.program, "stm32flash");
    assert_eq!(stm_cmd.args, vec!["-w", "target/firmware.bin", "-v", "-g", "0x0", "/dev/ttyUSB0"]);

    // Bossac
    let dev_acm = DiscoveredUsbDevice {
        location: "/dev/ttyACM0".into(),
        ..dev_usb.clone()
    };
    let bossa_plan = make_plan("bossac", vec![dev_acm]);
    let bossa_cmd = executor.build_command(&bossa_plan, Path::new("target/firmware.bin")).unwrap();
    assert_eq!(bossa_cmd.tool, FlashToolMethod::Bossac);
    assert_eq!(bossa_cmd.program, "bossac");
    assert_eq!(bossa_cmd.args, vec!["-p", "/dev/ttyACM0", "-e", "-w", "-v", "-b", "-R", "target/firmware.bin"]);

    // Picotool
    let pico_plan = make_plan("picotool", vec![dev_usb.clone()]);
    let pico_cmd = executor.build_command(&pico_plan, Path::new("target/firmware.uf2")).unwrap();
    assert_eq!(pico_cmd.tool, FlashToolMethod::Picotool);
    assert_eq!(pico_cmd.program, "picotool");
    assert_eq!(pico_cmd.args, vec!["load", "--bus", "2", "--address", "3", "target/firmware.uf2", "-x", "-v"]);
}

#[test]
fn integration_plan_drift_and_error_handling() {
    let payload = b"checksum_test_bytes";
    let hash = sha256(payload);

    let plan = FlashPlan {
        schema: FLASH_PLAN_SCHEMA.into(),
        mode: "dry_run".into(),
        ready: true,
        controller: "mainboard".into(),
        lock_hash: "sha256:dummy".into(),
        board: "boards/example-mainboard@1.0.0".into(),
        method: "dfu".into(),
        transport: "usb".into(),
        device_selection: DeviceSelection {
            rule: UsbSelectionRule {
                usb_vid: 0x1209,
                usb_pid: 0xd003,
                serial_number: None,
                manufacturer: None,
                product: None,
            },
            status: MatchStatus::Unique,
            candidates: vec![DiscoveredUsbDevice {
                platform: "linux".into(),
                bus_id: "1".into(),
                location: "port1".into(),
                device_address: 1,
                usb_vid: 0x1209,
                usb_pid: 0xd003,
                serial_number: None,
                manufacturer: None,
                product: None,
            }],
        },
        expected_current_firmware: "1.0.0".into(),
        artifact: ArtifactPlan {
            format: "bin".into(),
            path: "target/firmware.bin".into(),
            size_bytes: payload.len() as u64,
            expected_sha256: hash.clone(),
            observed_sha256: hash.clone(),
            hash_matches: true,
            deployable: true,
            signature: None,
        },
        steps: Vec::new(),
        blocked_reasons: Vec::new(),
        recovery: Vec::new(),
    };

    let mut native = NativeFlashExecutor::dry_run();

    // Checksum mismatch
    let err = native.execute_flash(&plan, b"tampered_data").unwrap_err();
    assert!(matches!(err, FlashExecutionError::ChecksumMismatch { .. }));

    // Unready plan
    let mut unready_plan = plan.clone();
    unready_plan.ready = false;
    unready_plan.blocked_reasons.push("device selection mismatch".into());
    let err = native.execute_flash(&unready_plan, payload).unwrap_err();
    assert!(matches!(err, FlashExecutionError::PlanNotReady { .. }));

    // Missing device
    let mut missing_dev_plan = plan.clone();
    missing_dev_plan.device_selection.status = MatchStatus::Missing;
    missing_dev_plan.device_selection.candidates.clear();
    let err = native.execute_flash(&missing_dev_plan, payload).unwrap_err();
    assert!(matches!(err, FlashExecutionError::DeviceNotFound { .. }));
}
