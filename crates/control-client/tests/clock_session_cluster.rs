use dryer_clock_sync::HostTick;
use dryer_control_client::{ClockSession, FrameSink, HostClock};
use dryer_simulator::{
    ClockTransportConfig, ControllerClock, SimClockCluster, SimClockClusterError,
};

struct ScriptClock {
    ticks: Vec<HostTick>,
}

impl HostClock for ScriptClock {
    fn now(&mut self) -> HostTick {
        self.ticks.remove(0)
    }
}

struct ClusterSink<'a> {
    cluster: &'a mut SimClockCluster,
    controller: u32,
    host_send: u64,
}

impl FrameSink for ClusterSink<'_> {
    type Error = SimClockClusterError;

    fn send_frame(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        self.cluster
            .send_request(self.controller, self.host_send, frame)
    }
}

#[test]
fn session_round_trips_through_a_routed_controller_clock() {
    let mut cluster = SimClockCluster::new([(
        42,
        ClockTransportConfig {
            request: dryer_simulator::ClockLinkConfig {
                latency_ticks: 100,
                ..dryer_simulator::ClockLinkConfig::default()
            },
            response: dryer_simulator::ClockLinkConfig {
                latency_ticks: 100,
                ..dryer_simulator::ClockLinkConfig::default()
            },
            processing_ticks: 10,
            controller_clock: ControllerClock::new(0, 5_000, 0).unwrap(),
            ..ClockTransportConfig::default()
        },
    )])
    .unwrap();
    let mut session = ClockSession::new(0, 1_000).unwrap();
    let mut clock = ScriptClock {
        ticks: vec![HostTick(1_000), HostTick(1_210)],
    };
    let (receipt, response) = {
        let mut sink = ClusterSink {
            cluster: &mut cluster,
            controller: 42,
            host_send: 1_000,
        };
        let receipt = session.begin(&mut sink, &mut clock).unwrap();
        let response = sink
            .cluster
            .receive_due(42, 1_210)
            .unwrap()
            .expect("response is due");
        (receipt, response)
    };
    assert_eq!(receipt.sequence, 0);
    let completed = session
        .accept_response(response.as_bytes(), &mut clock)
        .unwrap();
    assert_eq!(completed.sequence, 0);
    assert_eq!(completed.sample.host_send, HostTick(1_000));
    assert_eq!(completed.sample.host_receive, HostTick(1_210));
    assert_eq!(completed.sample.controller_receive.0, 6_100);
    assert_eq!(completed.sample.controller_send.0, 6_110);
    assert!(completed.estimate.is_none());
}
