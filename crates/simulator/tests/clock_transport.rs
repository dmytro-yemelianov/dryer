use dryer_control_protocol::{
    decode_clock_response, encode_clock_request, ClockRequestFrame, DecodeError,
    CLOCK_REQUEST_FRAME_LEN,
};
use dryer_simulator::{
    ClockLinkConfig, ClockTransportConfig, ClockTransportConfigError, ControllerClock,
    ControllerClockError, SimClockTransport, SimClockTransportError,
};

fn request(sequence: u32) -> [u8; CLOCK_REQUEST_FRAME_LEN] {
    let mut bytes = [0; CLOCK_REQUEST_FRAME_LEN];
    encode_clock_request(&ClockRequestFrame { sequence }, &mut bytes).unwrap();
    bytes
}

fn lossless(latency_ticks: u64) -> ClockLinkConfig {
    ClockLinkConfig {
        latency_ticks,
        jitter_ticks: 0,
        loss_per_mille: 0,
        dup_per_mille: 0,
    }
}

#[test]
fn response_echoes_sequence_and_captures_exact_controller_events() {
    let mut transport = SimClockTransport::new(ClockTransportConfig {
        request: lossless(200),
        response: lossless(300),
        processing_ticks: 25,
        controller_clock: ControllerClock::new(1_000, 11_000, 0).unwrap(),
        ..ClockTransportConfig::default()
    })
    .unwrap();

    transport.send_request(1_000, &request(17)).unwrap();
    assert!(transport.receive_due(1_524).is_none());

    let frame = transport.receive_due(1_525).unwrap();
    assert_eq!(frame.delivered_at, 1_525);
    let response = decode_clock_response(frame.as_bytes()).unwrap();
    assert_eq!(response.sequence, 17);
    assert_eq!(response.response.controller_receive, 11_200);
    assert_eq!(response.response.controller_send, 11_225);
    assert_eq!(transport.pending_len(), 0);
}

#[test]
fn controller_clock_supports_signed_offset_and_checked_rate() {
    let behind = ControllerClock::new(10_000, 0, -1_000).unwrap();
    assert_eq!(behind.at(20_000), Ok(9_999));
    assert_eq!(behind.host_epoch(), 10_000);
    assert_eq!(behind.controller_epoch(), 0);
    assert_eq!(behind.rate_ppb(), -1_000);
    assert_eq!(
        behind.at(9_999),
        Err(ControllerClockError::HostBeforeEpoch {
            host_tick: 9_999,
            host_epoch: 10_000,
        })
    );
    assert_eq!(
        ControllerClock::new(0, 0, -1_000_000_000),
        Err(ControllerClockError::NonPositiveRate {
            rate_ppb: -1_000_000_000,
        })
    );
}

#[test]
fn jitter_is_deterministic_for_a_fixed_seed() {
    let cfg = ClockTransportConfig {
        request: ClockLinkConfig {
            latency_ticks: 10,
            jitter_ticks: 20,
            ..lossless(0)
        },
        response: ClockLinkConfig {
            latency_ticks: 30,
            jitter_ticks: 40,
            ..lossless(0)
        },
        processing_ticks: 5,
        seed: 77,
        ..ClockTransportConfig::default()
    };
    let mut left = SimClockTransport::new(cfg).unwrap();
    let mut right = SimClockTransport::new(cfg).unwrap();

    for sequence in 0..4 {
        left.send_request(1_000 + u64::from(sequence), &request(sequence))
            .unwrap();
        right
            .send_request(1_000 + u64::from(sequence), &request(sequence))
            .unwrap();
    }

    let mut left_frames = Vec::new();
    let mut right_frames = Vec::new();
    while let Some(frame) = left.receive_due(u64::MAX) {
        left_frames.push((frame.delivered_at, frame.as_bytes().to_vec()));
    }
    while let Some(frame) = right.receive_due(u64::MAX) {
        right_frames.push((frame.delivered_at, frame.as_bytes().to_vec()));
    }
    assert_eq!(left_frames, right_frames);
}

#[test]
fn loss_and_link_faults_are_silent_and_recoverable() {
    let mut lost = SimClockTransport::new(ClockTransportConfig {
        request: ClockLinkConfig {
            loss_per_mille: 1000,
            ..lossless(0)
        },
        ..ClockTransportConfig::default()
    })
    .unwrap();
    lost.send_request(0, &request(1)).unwrap();
    assert!(lost.receive_due(u64::MAX).is_none());

    let mut transport = SimClockTransport::new(ClockTransportConfig::default()).unwrap();
    transport.send_request(0, &request(2)).unwrap();
    transport.drop_link();
    assert_eq!(transport.pending_len(), 0);
    transport.send_request(1_000, &request(3)).unwrap();
    assert!(transport.receive_due(u64::MAX).is_none());
    transport.restore_link();
    transport.send_request(2_000, &request(4)).unwrap();
    assert!(transport.receive_due(u64::MAX).is_some());
}

#[test]
fn duplication_is_bounded_and_queue_capacity_is_atomic() {
    let duplicate = ClockLinkConfig {
        dup_per_mille: 1000,
        ..lossless(0)
    };
    let mut transport = SimClockTransport::new(ClockTransportConfig {
        request: duplicate,
        response: duplicate,
        max_pending: 4,
        ..ClockTransportConfig::default()
    })
    .unwrap();
    transport.send_request(100, &request(5)).unwrap();
    assert_eq!(transport.pending_len(), 4);
    let mut frames = Vec::new();
    while let Some(frame) = transport.receive_due(u64::MAX) {
        frames.push(decode_clock_response(frame.as_bytes()).unwrap());
    }
    assert_eq!(frames.len(), 4);
    assert!(frames.iter().all(|frame| frame.sequence == 5));

    let mut bounded = SimClockTransport::new(ClockTransportConfig {
        request: lossless(0),
        response: lossless(0),
        max_pending: 1,
        ..ClockTransportConfig::default()
    })
    .unwrap();
    bounded.send_request(0, &request(1)).unwrap();
    assert_eq!(
        bounded.send_request(0, &request(2)),
        Err(SimClockTransportError::QueueFull { maximum: 1 })
    );
    assert_eq!(bounded.pending_len(), 1);
    assert_eq!(
        decode_clock_response(bounded.receive_due(0).unwrap().as_bytes())
            .unwrap()
            .sequence,
        1
    );
}

#[test]
fn configuration_and_frames_are_strictly_validated() {
    assert_eq!(
        SimClockTransport::new(ClockTransportConfig {
            max_pending: 0,
            ..ClockTransportConfig::default()
        })
        .unwrap_err(),
        ClockTransportConfigError::ZeroPendingCapacity
    );
    assert_eq!(
        SimClockTransport::new(ClockTransportConfig {
            response: ClockLinkConfig {
                loss_per_mille: 1001,
                ..lossless(0)
            },
            ..ClockTransportConfig::default()
        })
        .unwrap_err(),
        ClockTransportConfigError::InvalidLossRate {
            direction: "response",
            value: 1001,
        }
    );

    let mut transport = SimClockTransport::new(ClockTransportConfig::default()).unwrap();
    let error = transport.send_request(0, b"DX").unwrap_err();
    assert_eq!(
        error,
        SimClockTransportError::Decode(DecodeError::InvalidMagic { found: *b"DX" })
    );
}

#[test]
fn timestamp_overflow_is_reported_without_enqueuing_a_response() {
    let mut transport = SimClockTransport::new(ClockTransportConfig {
        request: lossless(1),
        response: lossless(0),
        ..ClockTransportConfig::default()
    })
    .unwrap();
    assert_eq!(
        transport.send_request(u64::MAX, &request(1)),
        Err(SimClockTransportError::TimeOverflow)
    );
    assert_eq!(transport.pending_len(), 0);

    let mut jitter_overflow = SimClockTransport::new(ClockTransportConfig {
        request: ClockLinkConfig {
            jitter_ticks: u64::MAX,
            ..lossless(0)
        },
        ..ClockTransportConfig::default()
    })
    .unwrap();
    assert_eq!(
        jitter_overflow.send_request(0, &request(2)),
        Err(SimClockTransportError::TimeOverflow)
    );
    assert_eq!(jitter_overflow.pending_len(), 0);
}

#[test]
fn rejected_send_does_not_change_future_seeded_delivery() {
    let cfg = ClockTransportConfig {
        request: lossless(10),
        response: lossless(20),
        max_pending: 1,
        seed: 91,
        ..ClockTransportConfig::default()
    };
    let mut failed = SimClockTransport::new(cfg).unwrap();
    let mut clean = SimClockTransport::new(cfg).unwrap();
    failed.send_request(0, &request(1)).unwrap();
    assert_eq!(
        failed.send_request(1, &request(2)),
        Err(SimClockTransportError::QueueFull { maximum: 1 })
    );
    clean.send_request(0, &request(1)).unwrap();
    assert_eq!(
        failed.receive_due(u64::MAX).unwrap().as_bytes(),
        clean.receive_due(u64::MAX).unwrap().as_bytes()
    );
    failed.send_request(100, &request(3)).unwrap();
    clean.send_request(100, &request(3)).unwrap();
    let failed_frame = failed.receive_due(u64::MAX).unwrap();
    let clean_frame = clean.receive_due(u64::MAX).unwrap();
    assert_eq!(failed_frame.delivered_at, clean_frame.delivered_at);
    assert_eq!(failed_frame.as_bytes(), clean_frame.as_bytes());
}
