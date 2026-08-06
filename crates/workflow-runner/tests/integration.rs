use std::collections::BTreeMap;

use dryer_control_client::{CommandClient, FrameSink};
use dryer_control_protocol::{Command, CommandEnvelope};
use dryer_controller_daemon::ControllerSessionStatus;
use dryer_simulator::{
    AxisCfg, HeaterCfg, SimController, SimTransport, TransportConfig, STEP_TICKS,
};
use dryer_toolpath_auditor::AxisLimit;
use dryer_workflow_runner::{AuditLimits, RunnerConfig, RunnerState, WorkflowRunner};

struct DirectSimSink<'a> {
    sim: &'a mut SimController,
}

impl<'a> FrameSink for DirectSimSink<'a> {
    type Error = String;

    fn send_frame(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        self.sim
            .process_wire_frame(frame)
            .map(|_| ())
            .map_err(|e| format!("Wire frame error: {e}"))
    }
}

fn create_sim_controller() -> SimController {
    let heaters = vec![HeaterCfg {
        name: "hotend".into(),
        gain_milli_c: 200_000,
        tau_ms: 50,
        safe_state: "off".into(),
        heartbeat_timeout: Some(500_000),
    }];
    let axes = vec![AxisCfg {
        name: "x".into(),
        start_position_um: 0,
    }];
    SimController::new(20_000, heaters, axes)
}

fn sample_limits() -> AuditLimits {
    let mut axes = BTreeMap::new();
    axes.insert(
        "x".into(),
        AxisLimit {
            min_um: 0,
            max_um: 500_000,
        },
    );
    let mut heaters = BTreeMap::new();
    heaters.insert("hotend".into(), 300_000);

    AuditLimits {
        axes,
        max_feed_rate_um_s: 100_000,
        heater_ceilings_milli_c: heaters,
    }
}

#[test]
fn integration_streaming_workflow_with_throttling_and_completion() {
    let mut sim = create_sim_controller();
    let mut transport = SimTransport::new(TransportConfig::default());

    let config = RunnerConfig {
        target_horizon: 6,
        min_threshold: 2,
        max_batch_size: 3,
        schedule_lead_margin_us: 10_000,
    };
    let mut runner = WorkflowRunner::with_auditor(config, sample_limits());

    // Generate a workflow of 15 commands
    let mut commands = Vec::new();
    commands.push(CommandEnvelope {
        execute_at: None,
        command: Command::Home {
            axis: "x".into(),
            rate_um_s: 10_000,
        },
    });
    commands.push(CommandEnvelope {
        execute_at: None,
        command: Command::SetHeaterTarget {
            heater: "hotend".into(),
            target_milli_c: 200_000,
        },
    });

    for i in 1..=13 {
        commands.push(CommandEnvelope {
            execute_at: None,
            command: Command::Move {
                axis: "x".into(),
                distance_um: 1_000,
                rate_um_s: 10_000 + (i * 500),
            },
        });
    }

    let enqueued = runner
        .enqueue_batch(commands)
        .expect("workflow enqueues cleanly");
    assert_eq!(enqueued, 15);
    assert_eq!(runner.pending_count(), 15);

    let mut step_count = 0;
    let mut peak_controller_fill = 0;
    let mut observed_throttled = false;

    // Execution loop driving host dispatch & simulator time
    while !runner.is_completed() && step_count < 2_000 {
        step_count += 1;

        // 1. Read controller status
        let qstat = sim.queue_status();
        peak_controller_fill = peak_controller_fill.max(qstat.fill_level);

        let session_status = ControllerSessionStatus {
            controller_id: "mcu1".into(),
            heartbeat_timeout_us: 500_000,
            last_seen_host_us: sim.now(),
            queue_capacity: qstat.capacity as u16,
            queue_fill: qstat.fill_level as u16,
            earliest_accepted_tick: qstat.earliest_accepted_timestamp,
            latest_accepted_tick: qstat.latest_accepted_timestamp,
            underrun: sim.latched_fault().is_some(),
            heartbeat_ok: true,
        };

        runner.update_controller_status(session_status);

        if runner.is_throttled() {
            observed_throttled = true;
        }

        // 2. Dispatch available batch to client -> simulator wire interface
        let sink = DirectSimSink { sim: &mut sim };
        let mut client = CommandClient::new(sink);

        let _receipts = runner
            .dispatch_to_client(&mut client)
            .expect("dispatch to client succeeds");

        // 3. Advance simulator time by 1 ms step
        sim.run(&mut transport, sim.now() + STEP_TICKS);
    }

    assert!(runner.is_completed());
    assert_eq!(runner.stats().total_enqueued, 15);
    assert_eq!(runner.stats().total_dispatched, 15);
    assert_eq!(runner.stats().total_completed, 15);

    // Controller queue fill should never exceed target_horizon (6)
    assert!(peak_controller_fill <= 6);
    assert!(observed_throttled);
    assert_eq!(runner.underrun_count(), 0);
}

#[test]
fn integration_underrun_prevention_and_recovery() {
    let mut runner = WorkflowRunner::new(RunnerConfig {
        target_horizon: 10,
        min_threshold: 3,
        max_batch_size: 4,
        schedule_lead_margin_us: 1_000,
    });

    // Enqueue 10 heartbeat commands
    let cmds: Vec<_> = (0..10)
        .map(|_| CommandEnvelope {
            execute_at: None,
            command: Command::Heartbeat,
        })
        .collect();
    runner.enqueue_batch(cmds).unwrap();

    // Initial status: fill level is 1 (near underrun threshold 3)
    let status1 = ControllerSessionStatus {
        controller_id: "mcu1".into(),
        heartbeat_timeout_us: 50_000,
        last_seen_host_us: 1_000,
        queue_capacity: 16,
        queue_fill: 1,
        earliest_accepted_tick: 2_000,
        latest_accepted_tick: 100_000,
        underrun: false,
        heartbeat_ok: true,
    };
    runner.update_controller_status(status1.clone());

    // Runner dispatches batch of 4 to replenish queue
    let batch1 = runner.next_dispatch_batch();
    assert_eq!(batch1.len(), 4);
    assert_eq!(runner.pending_count(), 6);

    // Controller queue fill rises to 5
    let status2 = ControllerSessionStatus {
        queue_fill: 5,
        ..status1
    };
    runner.update_controller_status(status2.clone());
    assert_eq!(runner.stats().total_completed, 0);

    // Next batch dispatches another 4
    let batch2 = runner.next_dispatch_batch();
    assert_eq!(batch2.len(), 4);
    assert_eq!(runner.pending_count(), 2);

    // Simulate controller reporting underrun fault
    let mut fault_status = status2;
    fault_status.underrun = true;
    runner.update_controller_status(fault_status);

    assert_eq!(runner.underrun_count(), 1);
    assert_eq!(runner.state(), RunnerState::UnderrunWarning);
}
