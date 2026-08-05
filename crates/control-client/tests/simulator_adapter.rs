use dryer_control_client::{Command, CommandClient, FrameSink, Tick};
use dryer_control_protocol::{decode_command, DecodeError};
use dryer_simulator::{AxisCfg, Event, SimController, SimTransport, TransportConfig};

struct SimulatorSink {
    now: Tick,
    transport: SimTransport,
}

impl FrameSink for SimulatorSink {
    type Error = DecodeError;

    fn send_frame(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        let decoded = decode_command(frame)?;
        match decoded.envelope.execute_at {
            Some(execute_at) => {
                self.transport
                    .send_scheduled(self.now, execute_at, decoded.envelope.command);
            }
            None => self.transport.send(self.now, decoded.envelope.command),
        }
        Ok(())
    }
}

#[test]
fn encoded_immediate_and_scheduled_commands_reach_simulator_semantics() {
    let sink = SimulatorSink {
        now: 0,
        transport: SimTransport::new(TransportConfig {
            latency_ticks: 0,
            ..TransportConfig::default()
        }),
    };
    let mut client = CommandClient::new(sink);

    client
        .send(Command::Move {
            axis: "x".into(),
            distance_um: 1_000,
            rate_um_s: 1_000_000,
        })
        .expect("immediate frame reaches simulator transport");
    client
        .send_scheduled(
            200_000,
            Command::Move {
                axis: "x".into(),
                distance_um: 2_000,
                rate_um_s: 1_000_000,
            },
        )
        .expect("scheduled frame reaches simulator transport");

    let mut sink = client.into_sink();
    let mut controller = SimController::new(
        20_000,
        vec![],
        vec![AxisCfg {
            name: "x".into(),
            start_position_um: 0,
        }],
    );
    controller.run(&mut sink.transport, 199_000);

    assert_eq!(controller.axis_position_um("x"), Some(1_000));
    assert!(controller.trace.0.iter().any(|event| matches!(
        event,
        Event::Accepted { at: 1_000, what } if what == "move x 1000 um"
    )));
    assert!(controller.trace.0.iter().any(|event| matches!(
        event,
        Event::Accepted { at: 1_000, what } if what == "move x 2000 um @ 200000"
    )));

    controller.run(&mut sink.transport, 202_000);
    assert_eq!(controller.axis_position_um("x"), Some(3_000));
    assert!(controller.trace.0.iter().any(|event| matches!(
        event,
        Event::Executed { at: 201_000, what } if what == "move x 2000 um"
    )));
}
