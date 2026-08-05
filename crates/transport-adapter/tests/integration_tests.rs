use dryer_control_client::CommandClient;
use dryer_control_protocol::{
    encode_clock_request, encode_clock_response, encode_command, encode_queue_status,
    ClockRequestFrame, ClockResponse, ClockResponseFrame, Command, CommandEnvelope, CommandFrame,
    QueueStatus, QueueStatusFrame, MAX_FRAME_LEN,
};
use dryer_controller_daemon::ControllerDaemon;
use dryer_transport_adapter::{
    ChannelTransport, DispatchedFrame, FlowControl, FrameCodec, MemoryTransport,
    SerialTransportSpec, StreamTransport, TransportStreamReader,
};

#[test]
fn serial_transport_spec_serialization_and_validation() {
    let spec = SerialTransportSpec {
        port_path: "/dev/ttyACM0".into(),
        baud_rate: 250_000,
        flow_control: FlowControl::Hardware,
    };
    assert!(spec.validate().is_ok());

    let json = serde_json::to_string(&spec).unwrap();
    let deserialized: SerialTransportSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(spec, deserialized);
}

#[test]
fn framed_data_delivery_all_frame_types_memory_transport() {
    let (mut host, mut mcu) = MemoryTransport::pair();
    let mut daemon = ControllerDaemon::new();
    daemon.register_controller("mcu-1", 100_000);

    // 1. Encode CommandFrame (MCU <- Host)
    let cmd_frame = CommandFrame {
        sequence: 1,
        envelope: CommandEnvelope {
            execute_at: Some(50_000),
            command: Command::Move {
                axis: "x".into(),
                distance_um: 5000,
                rate_um_s: 1000,
            },
        },
    };
    let mut cmd_buf = [0u8; MAX_FRAME_LEN];
    let cmd_len = encode_command(&cmd_frame, &mut cmd_buf).unwrap();

    // Send via CommandClient using StreamTransport as FrameSink
    let mut client = CommandClient::new(host.clone());
    client
        .send_scheduled(
            50_000,
            Command::Move {
                axis: "x".into(),
                distance_um: 5000,
                rate_um_s: 1000,
            },
        )
        .unwrap();

    let mcu_received_bytes = mcu.read_bytes(&mut [0u8; 256]).unwrap();
    assert_eq!(mcu_received_bytes, cmd_len);

    // 2. Encode ClockRequestFrame
    let req_frame = ClockRequestFrame { sequence: 10 };
    let mut req_buf = [0u8; MAX_FRAME_LEN];
    let req_len = encode_clock_request(&req_frame, &mut req_buf).unwrap();
    mcu.write_bytes(&req_buf[..req_len]).unwrap();

    // 3. Encode ClockResponseFrame
    let resp_frame = ClockResponseFrame {
        sequence: 10,
        response: ClockResponse {
            controller_receive: 1234,
            controller_send: 5678,
        },
    };
    let mut resp_buf = [0u8; MAX_FRAME_LEN];
    let resp_len = encode_clock_response(&resp_frame, &mut resp_buf).unwrap();
    mcu.write_bytes(&resp_buf[..resp_len]).unwrap();

    // 4. Encode QueueStatusFrame
    let qs_frame = QueueStatusFrame {
        sequence: 100,
        status: QueueStatus {
            capacity: 32,
            fill: 8,
            earliest_accepted: 10_000,
            latest_accepted: 60_000,
            underrun: false,
        },
    };
    let mut qs_buf = [0u8; MAX_FRAME_LEN];
    let qs_len = encode_queue_status(&qs_frame, &mut qs_buf).unwrap();
    mcu.write_bytes(&qs_buf[..qs_len]).unwrap();

    // Host reads and dispatches
    let mut reader = TransportStreamReader::new();
    let dispatched = reader
        .read_and_dispatch(&mut host, &mut daemon, "mcu-1", 15_000)
        .unwrap();

    assert_eq!(dispatched.len(), 3);
    assert_eq!(
        dispatched[0],
        DispatchedFrame::ClockRequest(ClockRequestFrame { sequence: 10 })
    );
    assert_eq!(
        dispatched[1],
        DispatchedFrame::ClockResponse(ClockResponseFrame {
            sequence: 10,
            response: ClockResponse {
                controller_receive: 1234,
                controller_send: 5678,
            }
        })
    );
    match &dispatched[2] {
        DispatchedFrame::QueueStatus(qs) => {
            assert_eq!(qs.sequence, 100);
            assert_eq!(qs.status.fill, 8);
        }
        _ => panic!("expected QueueStatus frame"),
    }

    // Verify Daemon updated session
    let status = daemon.session_status("mcu-1", 15_000).unwrap();
    assert_eq!(status.queue_capacity, 32);
    assert_eq!(status.queue_fill, 8);
    assert_eq!(status.last_seen_host_us, 15_000);
    assert!(status.heartbeat_ok);
}

#[test]
fn loss_and_corruption_recovery_on_stream_reader() {
    let mut codec = FrameCodec::new();

    // Valid Frame 1
    let frame1 = ClockRequestFrame { sequence: 1 };
    let mut buf1 = [0u8; MAX_FRAME_LEN];
    let len1 = encode_clock_request(&frame1, &mut buf1).unwrap();

    // Valid Frame 2
    let frame2 = ClockRequestFrame { sequence: 2 };
    let mut buf2 = [0u8; MAX_FRAME_LEN];
    let len2 = encode_clock_request(&frame2, &mut buf2).unwrap();

    // Construct stream with noise -> corrupted frame -> noise -> valid frame 1 -> partial noise -> valid frame 2
    let mut stream = Vec::new();
    stream.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // Random noise

    // Corrupted magic and CRC
    stream.extend_from_slice(b"DR\x01\x01\x00\x00\x00\x00\x05\x00corruptBADCRC");
    stream.extend_from_slice(&[0x12, 0x34, 0x56]); // More noise

    // Valid frame 1
    stream.extend_from_slice(&buf1[..len1]);

    // Truncated header candidate "DR" with invalid length
    stream.extend_from_slice(b"DR\x01\x02\x00\x00\x00\x00\xFF\xFF");

    // Valid frame 2
    stream.extend_from_slice(&buf2[..len2]);

    codec.feed(&stream).unwrap();

    // First delimited frame should be valid frame 1
    let f1 = codec
        .next_frame()
        .unwrap()
        .expect("should recover valid frame 1");
    assert_eq!(f1, &buf1[..len1]);

    // Second delimited frame should be valid frame 2
    let f2 = codec
        .next_frame()
        .unwrap()
        .expect("should recover valid frame 2");
    assert_eq!(f2, &buf2[..len2]);

    // No more frames
    assert_eq!(codec.next_frame().unwrap(), None);
}

#[test]
fn channel_transport_integration_with_daemon_reader() {
    let (mut host_transport, mut mcu_transport) = ChannelTransport::pair();
    let mut daemon = ControllerDaemon::new();
    daemon.register_controller("mcu-channel", 50_000);

    let qs = QueueStatusFrame {
        sequence: 42,
        status: QueueStatus {
            capacity: 128,
            fill: 16,
            earliest_accepted: 100,
            latest_accepted: 200,
            underrun: true,
        },
    };
    let mut qs_buf = [0u8; MAX_FRAME_LEN];
    let qs_len = encode_queue_status(&qs, &mut qs_buf).unwrap();

    // Send across channel transport
    mcu_transport.write_bytes(&qs_buf[..qs_len]).unwrap();

    let mut reader = TransportStreamReader::new();
    let dispatched = reader
        .read_and_dispatch(&mut host_transport, &mut daemon, "mcu-channel", 2000)
        .unwrap();

    assert_eq!(dispatched.len(), 1);
    let session = daemon.session_status("mcu-channel", 2000).unwrap();
    assert_eq!(session.queue_fill, 16);
    assert_eq!(session.queue_capacity, 128);
    assert!(session.underrun);
}
