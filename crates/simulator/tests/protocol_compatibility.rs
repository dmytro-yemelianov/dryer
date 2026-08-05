use dryer_control_protocol as protocol;
use dryer_simulator::{Command, CommandEnvelope, SimTransport, Tick, TransportConfig};

#[test]
fn protocol_types_are_the_simulators_public_transport_types() {
    let tick: Tick = 42;
    let protocol_tick: protocol::Tick = tick;
    let command: protocol::Command = Command::Move {
        axis: "x".into(),
        distance_um: -100,
        rate_um_s: 2_000,
    };
    let envelope: protocol::CommandEnvelope = CommandEnvelope {
        execute_at: Some(protocol_tick),
        command: command.clone(),
    };

    assert_eq!(envelope.execute_at, Some(42));
    let mut transport = SimTransport::new(TransportConfig::default());
    transport.send(0, command.clone());
    transport.send_scheduled(0, protocol_tick, command);
}
