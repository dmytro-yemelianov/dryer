use std::collections::BTreeMap;
use std::path::PathBuf;

use dryer_control_protocol::*;
use dryer_machine_resolver::resolve_source;
use dryer_package_model::LocalRegistry;
use dryer_simulator::*;
use dryer_toolpath_auditor::*;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn end_to_end_job_execution_pipeline() {
    let root = workspace_root();
    let registry = LocalRegistry::load(&root.join("packages"));
    let machine_path = root.join("examples/minimal-cartesian/machine.yaml");
    let machine_src = std::fs::read_to_string(&machine_path).unwrap();
    let _outcome = resolve_source(&machine_src, &registry);
    assert!(_outcome.is_ok(), "machine resolves: {:?}", _outcome.diagnostics);

    // 1. Resolve and extract workflow step
    let print_start = registry
        .find("workflows", "print-start")
        .expect("print-start package");
    let payload = print_start.workflow_payload().expect("valid payload");

    // 2. Lower steps into typed protocol commands
    let mut commands = Vec::new();
    for step in &payload.steps {
        let mut step_bound = step.clone();
        if step_bound.call.as_deref() == Some("heater.set_target") {
            step_bound.with_arguments.insert(
                "heater".into(),
                serde_yaml::Value::String("hotend_heater".into()),
            );
            step_bound.with_arguments.insert(
                "target_milli_c".into(),
                serde_yaml::Value::Number(200_000.into()),
            );
        }
        commands.push(step_bound.lower().expect("step lowers"));
    }

    // Add motion commands
    commands.push(Command::Home {
        axis: "x".into(),
        rate_um_s: 10_000,
    });
    commands.push(Command::Move {
        axis: "x".into(),
        distance_um: 50_000,
        rate_um_s: 20_000,
    });

    // 3. Run Pre-flight Auditor
    let mut axes = BTreeMap::new();
    axes.insert("x".into(), AxisLimit { min_um: 0, max_um: 200_000 });
    let mut heaters = BTreeMap::new();
    heaters.insert("hotend_heater".into(), 300_000);

    let auditor = ToolpathAuditor::new(AuditLimits {
        axes,
        max_feed_rate_um_s: 50_000,
        heater_ceilings_milli_c: heaters,
    });

    let audit = auditor.audit(&commands);
    assert!(audit.passed, "audit diagnostics: {:?}", audit.diagnostics);

    // 4. Encode & Stream into Simulator
    let heater_cfg = HeaterCfg {
        name: "hotend_heater".into(),
        tau_ms: 10_000,
        gain_milli_c: 400_000,
        safe_state: "off".into(),
        heartbeat_timeout: Some(5_000_000),
    };
    let axis_cfg = AxisCfg {
        name: "x".into(),
        start_position_um: 0,
    };
    let mut sim = SimController::new(25_000, vec![heater_cfg], vec![axis_cfg]);

    for (seq, cmd) in commands.into_iter().enumerate() {
        let frame = CommandFrame {
            sequence: seq as u32 + 1,
            envelope: CommandEnvelope {
                execute_at: None,
                command: cmd,
            },
        };
        let mut buf = vec![0u8; MAX_FRAME_LEN];
        let len = encode_command(&frame, &mut buf).unwrap();
        buf.truncate(len);

        sim.process_wire_frame(&buf).unwrap();
    }

    // Advance 5 seconds
    let mut transport = SimTransport::new(TransportConfig::default());
    sim.run(&mut transport, 5_000 * TICKS_PER_MS);

    // Assert target temperature set and axis moved
    assert_eq!(sim.axis_position_um("x"), Some(50_000));

    // Verify trace recorded acceptance and execution events
    let trace_text = sim.trace.to_json_lines();
    assert!(trace_text.contains("heater hotend_heater target 200000 mC"));
    assert!(trace_text.contains("move x 50000 um"));
}
