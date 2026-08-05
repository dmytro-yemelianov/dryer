use dryer_control_protocol::{
    decode_clock_response, encode_clock_request, ClockRequestFrame, CLOCK_REQUEST_FRAME_LEN,
};
use dryer_simulator::{
    ClockTransportConfig, ControllerClock, SimClockCluster, SimClockClusterError,
};

fn request(sequence: u32) -> [u8; CLOCK_REQUEST_FRAME_LEN] {
    let mut bytes = [0; CLOCK_REQUEST_FRAME_LEN];
    encode_clock_request(&ClockRequestFrame { sequence }, &mut bytes).unwrap();
    bytes
}

#[test]
fn routes_independent_controller_clocks_without_wire_identity() {
    let mut cluster = SimClockCluster::new([
        (
            7,
            ClockTransportConfig {
                controller_clock: ControllerClock::new(0, 10_000, 1_000).unwrap(),
                ..ClockTransportConfig::default()
            },
        ),
        (
            3,
            ClockTransportConfig {
                controller_clock: ControllerClock::new(0, 20_000, -1_000).unwrap(),
                ..ClockTransportConfig::default()
            },
        ),
    ])
    .unwrap();
    assert_eq!(cluster.controller_ids().collect::<Vec<_>>(), vec![3, 7]);
    cluster.send_request(7, 0, &request(1)).unwrap();
    cluster.send_request(3, 0, &request(1)).unwrap();
    let seven = decode_clock_response(
        cluster
            .receive_due(7, u64::MAX)
            .unwrap()
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    let three = decode_clock_response(
        cluster
            .receive_due(3, u64::MAX)
            .unwrap()
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    assert_eq!(
        (seven.sequence, seven.response.controller_receive),
        (1, 10_200)
    );
    assert_eq!(
        (three.sequence, three.response.controller_receive),
        (1, 20_199)
    );
}

#[test]
fn one_controller_fault_does_not_contaminate_another() {
    let mut cluster = SimClockCluster::new([
        (1, ClockTransportConfig::default()),
        (2, ClockTransportConfig::default()),
    ])
    .unwrap();
    cluster.drop_link(1).unwrap();
    cluster.send_request(1, 0, &request(10)).unwrap();
    cluster.send_request(2, 0, &request(20)).unwrap();
    assert!(cluster.receive_due(1, u64::MAX).unwrap().is_none());
    assert!(cluster.receive_due(2, u64::MAX).unwrap().is_some());
}

#[test]
fn cluster_rejects_duplicate_and_unknown_controllers() {
    assert_eq!(
        SimClockCluster::new([
            (1, ClockTransportConfig::default()),
            (1, ClockTransportConfig::default())
        ])
        .unwrap_err(),
        SimClockClusterError::DuplicateController { controller: 1 }
    );
    let cluster = SimClockCluster::new([(1, ClockTransportConfig::default())]).unwrap();
    assert_eq!(
        cluster.pending_len(9),
        Err(SimClockClusterError::UnknownController { controller: 9 })
    );
}
