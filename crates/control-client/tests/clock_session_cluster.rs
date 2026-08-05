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

#[test]
fn session_tracks_drift_per_controller_in_one_cluster() {
    let fast_clock = ControllerClock::new(0, 5_000, 1_000).unwrap();
    let slow_clock = ControllerClock::new(0, 10_000, -500).unwrap();
    let mut cluster = SimClockCluster::new([
        (
            1,
            ClockTransportConfig {
                request: dryer_simulator::ClockLinkConfig {
                    latency_ticks: 0,
                    ..dryer_simulator::ClockLinkConfig::default()
                },
                response: dryer_simulator::ClockLinkConfig {
                    latency_ticks: 0,
                    ..dryer_simulator::ClockLinkConfig::default()
                },
                processing_ticks: 0,
                controller_clock: fast_clock,
                ..ClockTransportConfig::default()
            },
        ),
        (
            2,
            ClockTransportConfig {
                request: dryer_simulator::ClockLinkConfig {
                    latency_ticks: 0,
                    ..dryer_simulator::ClockLinkConfig::default()
                },
                response: dryer_simulator::ClockLinkConfig {
                    latency_ticks: 0,
                    ..dryer_simulator::ClockLinkConfig::default()
                },
                processing_ticks: 0,
                controller_clock: slow_clock,
                ..ClockTransportConfig::default()
            },
        ),
    ])
    .unwrap();

    let mut fast_session = ClockSession::new(2_000, 1_000).unwrap();
    let mut fast_clock_host = ScriptClock {
        ticks: vec![
            HostTick(1_000_000),
            HostTick(1_000_000),
            HostTick(3_000_000),
            HostTick(3_000_000),
        ],
    };
    let fast_completed = {
        let mut sink = ClusterSink {
            cluster: &mut cluster,
            controller: 1,
            host_send: 1_000_000,
        };
        let receipt = fast_session.begin(&mut sink, &mut fast_clock_host).unwrap();
        let response = sink
            .cluster
            .receive_due(1, 1_000_000)
            .unwrap()
            .expect("response is due");
        let completed = fast_session
            .accept_response(response.as_bytes(), &mut fast_clock_host)
            .unwrap();
        assert_eq!(receipt.sequence, 0);
        completed
    };
    assert!(fast_completed.estimate.is_none());

    let fast_completed = {
        let mut sink = ClusterSink {
            cluster: &mut cluster,
            controller: 1,
            host_send: 3_000_000,
        };
        let receipt = fast_session.begin(&mut sink, &mut fast_clock_host).unwrap();
        let response = sink
            .cluster
            .receive_due(1, 3_000_000)
            .unwrap()
            .expect("response is due");
        let completed = fast_session
            .accept_response(response.as_bytes(), &mut fast_clock_host)
            .unwrap();
        assert_eq!(receipt.sequence, 1);
        completed
    };
    let fast_estimate = fast_completed.estimate.unwrap();
    assert_eq!(fast_completed.sample.host_send, HostTick(3_000_000));
    assert_eq!(fast_completed.sample.host_receive, HostTick(3_000_000));
    assert_eq!(fast_completed.sample.controller_receive, fast_clock.at(3_000_000).unwrap());
    assert_eq!(fast_completed.sample.controller_send, fast_clock.at(3_000_000).unwrap());
    assert_eq!(fast_estimate.drift.ppb, 1_000);
    assert_eq!(fast_estimate.drift.min_ppb, 1_000);
    assert_eq!(fast_estimate.drift.max_ppb, 1_000);

    let mut slow_session = ClockSession::new(2_000, 1_000).unwrap();
    let mut slow_clock_host = ScriptClock {
        ticks: vec![
            HostTick(1_000_000),
            HostTick(1_000_000),
            HostTick(3_000_000),
            HostTick(3_000_000),
        ],
    };
    let _slow_first = {
        let mut sink = ClusterSink {
            cluster: &mut cluster,
            controller: 2,
            host_send: 1_000_000,
        };
        let receipt = slow_session.begin(&mut sink, &mut slow_clock_host).unwrap();
        let response = sink
            .cluster
            .receive_due(2, 1_000_000)
            .unwrap()
            .expect("response is due");
        let completed = slow_session
            .accept_response(response.as_bytes(), &mut slow_clock_host)
            .unwrap();
        assert_eq!(receipt.sequence, 0);
        completed
    };

    let slow_completed = {
        let mut sink = ClusterSink {
            cluster: &mut cluster,
            controller: 2,
            host_send: 3_000_000,
        };
        let receipt = slow_session.begin(&mut sink, &mut slow_clock_host).unwrap();
        let response = sink
            .cluster
            .receive_due(2, 3_000_000)
            .unwrap()
            .expect("response is due");
        let completed = slow_session
            .accept_response(response.as_bytes(), &mut slow_clock_host)
            .unwrap();
        assert_eq!(receipt.sequence, 1);
        completed
    };
    let slow_estimate = slow_completed.estimate.unwrap();
    assert_eq!(slow_completed.sample.host_send, HostTick(3_000_000));
    assert_eq!(slow_completed.sample.host_receive, HostTick(3_000_000));
    assert_eq!(
        slow_completed.sample.controller_receive,
        slow_clock.at(3_000_000).unwrap()
    );
    assert_eq!(slow_completed.sample.controller_send, slow_clock.at(3_000_000).unwrap());
    assert_eq!(slow_estimate.drift.ppb, -500);
    assert_eq!(slow_estimate.drift.min_ppb, -500);
    assert_eq!(slow_estimate.drift.max_ppb, -500);
}
