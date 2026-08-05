use dryer_control_protocol::{
    decode_command, decode_queue_status, encode_command, encode_queue_status, Command,
    CommandEnvelope, CommandFrame, DecodeError, EncodeError, QueueStatus, QueueStatusFrame,
    CHECKSUM_LEN, HEADER_LEN, MAX_FRAME_LEN, MAX_PAYLOAD_LEN, MAX_STRING_LEN,
    QUEUE_STATUS_FRAME_LEN, QUEUE_STATUS_MESSAGE_TYPE, QUEUE_STATUS_PAYLOAD_LEN,
};

const HEARTBEAT_GOLDEN: &[u8] = &[
    0x44, 0x52, 0x01, 0x01, 0x04, 0x03, 0x02, 0x01, 0x02, 0x00, 0x00, 0x00, 0x5d, 0x46, 0x57, 0x19,
];

const HEATER_GOLDEN: &[u8] = &[
    0x44, 0x52, 0x01, 0x01, 0xd4, 0xc3, 0xb2, 0xa1, 0x19, 0x00, 0x01, 0x08, 0x07, 0x06, 0x05, 0x04,
    0x03, 0x02, 0x01, 0x01, 0x06, 0x68, 0x6f, 0x74, 0x65, 0x6e, 0x64, 0xc7, 0xcf, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xf7, 0xe1, 0x8e, 0xac,
];

const HOME_GOLDEN: &[u8] = &[
    0x44, 0x52, 0x01, 0x01, 0x07, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x02, 0x01, 0x78, 0x10, 0x27,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xa3, 0x20, 0x8d, 0x52,
];

const MOVE_GOLDEN: &[u8] = &[
    0x44, 0x52, 0x01, 0x01, 0x09, 0x00, 0x00, 0x00, 0x1d, 0x00, 0x01, 0x40, 0x42, 0x0f, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x03, 0x02, 0x78, 0x79, 0x30, 0xf8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xc0,
    0xd4, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x57, 0xd6, 0x5e,
];

const QUEUE_STATUS_GOLDEN: &[u8] = &[
    0x44, 0x52, 0x01, 0x02, 0x0d, 0x0c, 0x0b, 0x0a, 0x16, 0x00, 0x00, 0x00, 0x04, 0x2a, 0x00, 0x08,
    0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, 0x01,
    0x4b, 0x2f, 0x8c, 0x44,
];

fn frame(sequence: u32, execute_at: Option<u64>, command: Command) -> CommandFrame {
    CommandFrame {
        sequence,
        envelope: CommandEnvelope {
            execute_at,
            command,
        },
    }
}

fn encoded(frame: &CommandFrame) -> Vec<u8> {
    let mut output = [0xa5; MAX_FRAME_LEN];
    let length = encode_command(frame, &mut output).expect("frame encodes");
    output[..length].to_vec()
}

fn encoded_queue_status(frame: &QueueStatusFrame) -> Vec<u8> {
    let mut output = [0xa5; QUEUE_STATUS_FRAME_LEN];
    let length = encode_queue_status(frame, &mut output).expect("queue status encodes");
    output[..length].to_vec()
}

fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

fn raw_frame(payload: &[u8]) -> Vec<u8> {
    raw_typed_frame(1, payload)
}

fn raw_typed_frame(message_type: u8, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= MAX_PAYLOAD_LEN);
    let mut bytes = Vec::with_capacity(HEADER_LEN + payload.len() + CHECKSUM_LEN);
    bytes.extend_from_slice(b"DR");
    bytes.extend_from_slice(&[1, message_type]);
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    bytes.extend_from_slice(payload);
    let checksum = crc32c(&bytes[2..]);
    bytes.extend_from_slice(&checksum.to_le_bytes());
    bytes
}

#[test]
fn queue_status_has_a_stable_v1_byte_golden() {
    let expected = QueueStatusFrame {
        sequence: 0x0a0b_0c0d,
        status: QueueStatus {
            capacity: 1024,
            fill: 42,
            earliest_accepted: 0x0102_0304_0506_0708,
            latest_accepted: 0x1112_1314_1516_1718,
            underrun: true,
        },
    };

    assert_eq!(QUEUE_STATUS_PAYLOAD_LEN, 22);
    assert_eq!(QUEUE_STATUS_FRAME_LEN, 36);
    assert_eq!(encoded_queue_status(&expected), QUEUE_STATUS_GOLDEN);
    assert_eq!(decode_queue_status(QUEUE_STATUS_GOLDEN), Ok(expected));
}

#[test]
fn queue_status_numeric_and_buffer_boundaries_are_exact() {
    for expected in [
        QueueStatusFrame {
            sequence: 0,
            status: QueueStatus {
                capacity: 0,
                fill: 0,
                earliest_accepted: 0,
                latest_accepted: 0,
                underrun: false,
            },
        },
        QueueStatusFrame {
            sequence: u32::MAX,
            status: QueueStatus {
                capacity: u16::MAX,
                fill: u16::MAX,
                earliest_accepted: u64::MAX,
                latest_accepted: u64::MAX,
                underrun: true,
            },
        },
    ] {
        assert_eq!(
            decode_queue_status(&encoded_queue_status(&expected)),
            Ok(expected)
        );
    }

    let frame = QueueStatusFrame {
        sequence: 1,
        status: QueueStatus {
            capacity: 1,
            fill: 1,
            earliest_accepted: 1,
            latest_accepted: 1,
            underrun: false,
        },
    };
    let mut short = [0xa5; QUEUE_STATUS_FRAME_LEN - 1];
    assert_eq!(
        encode_queue_status(&frame, &mut short),
        Err(EncodeError::BufferTooSmall {
            needed: QUEUE_STATUS_FRAME_LEN,
            available: QUEUE_STATUS_FRAME_LEN - 1,
        })
    );
    assert!(short.iter().all(|byte| *byte == 0xa5));
}

#[test]
fn every_queue_status_prefix_is_reported_as_truncated() {
    for prefix_len in 0..QUEUE_STATUS_GOLDEN.len() {
        assert!(
            matches!(
                decode_queue_status(&QUEUE_STATUS_GOLDEN[..prefix_len]),
                Err(DecodeError::Truncated { .. })
            ),
            "prefix {prefix_len}/{} returned {:?}",
            QUEUE_STATUS_GOLDEN.len(),
            decode_queue_status(&QUEUE_STATUS_GOLDEN[..prefix_len])
        );
    }
}

#[test]
fn queue_status_headers_follow_the_documented_validation_order() {
    assert_eq!(
        decode_queue_status(b"DX"),
        Err(DecodeError::InvalidMagic { found: *b"DX" })
    );

    let mut header = [0_u8; HEADER_LEN];
    header[..2].copy_from_slice(b"DR");
    header[2] = 2;
    header[3] = 99;
    header[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
    assert_eq!(
        decode_queue_status(&header),
        Err(DecodeError::UnsupportedVersion { version: 2 })
    );

    header[2] = 1;
    assert_eq!(
        decode_queue_status(&header),
        Err(DecodeError::UnsupportedMessageType { message_type: 99 })
    );

    header[3] = QUEUE_STATUS_MESSAGE_TYPE;
    assert_eq!(
        decode_queue_status(&header),
        Err(DecodeError::PayloadTooLong {
            length: usize::from(u16::MAX),
            maximum: MAX_PAYLOAD_LEN,
        })
    );

    let mut trailing = QUEUE_STATUS_GOLDEN.to_vec();
    trailing.push(0);
    trailing[HEADER_LEN] = 0xff;
    assert_eq!(
        decode_queue_status(&trailing),
        Err(DecodeError::TrailingFrameBytes { count: 1 })
    );
}

#[test]
fn queue_status_rejects_wrong_type_checksum_length_and_reserved_bits() {
    assert_eq!(
        decode_queue_status(HEARTBEAT_GOLDEN),
        Err(DecodeError::UnsupportedMessageType { message_type: 1 })
    );

    let mut corrupt = QUEUE_STATUS_GOLDEN.to_vec();
    corrupt[14] ^= 0xff;
    let computed = crc32c(&corrupt[2..corrupt.len() - CHECKSUM_LEN]);
    assert_eq!(
        decode_queue_status(&corrupt),
        Err(DecodeError::ChecksumMismatch {
            encoded: 0x448c_2f4b,
            computed,
        })
    );

    assert_eq!(
        decode_queue_status(&raw_typed_frame(
            QUEUE_STATUS_MESSAGE_TYPE,
            &[0; QUEUE_STATUS_PAYLOAD_LEN - 1]
        )),
        Err(DecodeError::InvalidPayloadLength {
            expected: QUEUE_STATUS_PAYLOAD_LEN,
            actual: QUEUE_STATUS_PAYLOAD_LEN - 1,
        })
    );
    assert_eq!(
        decode_queue_status(&raw_typed_frame(
            QUEUE_STATUS_MESSAGE_TYPE,
            &[0; QUEUE_STATUS_PAYLOAD_LEN + 1]
        )),
        Err(DecodeError::InvalidPayloadLength {
            expected: QUEUE_STATUS_PAYLOAD_LEN,
            actual: QUEUE_STATUS_PAYLOAD_LEN + 1,
        })
    );

    let mut flags = QUEUE_STATUS_GOLDEN.to_vec();
    flags[HEADER_LEN] = 1;
    refresh_checksum(&mut flags);
    assert_eq!(
        decode_queue_status(&flags),
        Err(DecodeError::InvalidFlags { flags: 1 })
    );

    let mut state_flags = QUEUE_STATUS_GOLDEN.to_vec();
    state_flags[HEADER_LEN + QUEUE_STATUS_PAYLOAD_LEN - 1] = 0x80;
    refresh_checksum(&mut state_flags);
    assert_eq!(
        decode_queue_status(&state_flags),
        Err(DecodeError::InvalidStateFlags { flags: 0x80 })
    );
}

fn refresh_checksum(bytes: &mut [u8]) {
    let checksum_offset = bytes.len() - CHECKSUM_LEN;
    let checksum = crc32c(&bytes[2..checksum_offset]);
    bytes[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
}

#[test]
fn every_command_has_a_stable_v1_byte_golden() {
    let cases = [
        (
            frame(0x0102_0304, None, Command::Heartbeat),
            HEARTBEAT_GOLDEN,
        ),
        (
            frame(
                0xa1b2_c3d4,
                Some(0x0102_0304_0506_0708),
                Command::SetHeaterTarget {
                    heater: "hotend".into(),
                    target_milli_c: -12_345,
                },
            ),
            HEATER_GOLDEN,
        ),
        (
            frame(
                7,
                None,
                Command::Home {
                    axis: "x".into(),
                    rate_um_s: 10_000,
                },
            ),
            HOME_GOLDEN,
        ),
        (
            frame(
                9,
                Some(1_000_000),
                Command::Move {
                    axis: "xy".into(),
                    distance_um: -2_000,
                    rate_um_s: 120_000,
                },
            ),
            MOVE_GOLDEN,
        ),
    ];

    for (expected_frame, golden) in cases {
        assert_eq!(encoded(&expected_frame), golden);
        assert_eq!(decode_command(golden), Ok(expected_frame));
    }
}

#[test]
fn scheduled_and_immediate_commands_round_trip() {
    let commands = [
        Command::Heartbeat,
        Command::SetHeaterTarget {
            heater: "bed".into(),
            target_milli_c: i64::MIN,
        },
        Command::Home {
            axis: "z".into(),
            rate_um_s: u64::MAX,
        },
        Command::Move {
            axis: "extruder".into(),
            distance_um: i64::MAX,
            rate_um_s: 1,
        },
    ];

    for (index, command) in commands.into_iter().enumerate() {
        for execute_at in [None, Some(0), Some(u64::MAX)] {
            let expected = frame(index as u32, execute_at, command.clone());
            assert_eq!(decode_command(&encoded(&expected)), Ok(expected));
        }
    }
}

#[test]
fn string_and_buffer_boundaries_are_enforced_before_writing() {
    assert_eq!(MAX_FRAME_LEN, 142);
    assert_eq!(MAX_PAYLOAD_LEN, 128);
    assert_eq!(MAX_STRING_LEN, 63);

    let maximum = frame(
        1,
        Some(u64::MAX),
        Command::Move {
            axis: "a".repeat(MAX_STRING_LEN),
            distance_um: i64::MIN,
            rate_um_s: u64::MAX,
        },
    );
    let bytes = encoded(&maximum);
    assert_eq!(decode_command(&bytes), Ok(maximum.clone()));

    let too_long = frame(
        2,
        None,
        Command::Home {
            axis: "a".repeat(MAX_STRING_LEN + 1),
            rate_um_s: 1,
        },
    );
    let mut empty = [];
    assert_eq!(
        encode_command(&too_long, &mut empty),
        Err(EncodeError::StringTooLong {
            length: 64,
            maximum: 63,
        })
    );

    let needed = bytes.len();
    let mut short = vec![0xa5; needed - 1];
    assert_eq!(
        encode_command(&maximum, &mut short),
        Err(EncodeError::BufferTooSmall {
            needed,
            available: needed - 1,
        })
    );
    assert!(short.iter().all(|byte| *byte == 0xa5));

    let oversized_name_payload = {
        let mut payload = vec![0, 2, 64];
        payload.extend(core::iter::repeat(b'a').take(64));
        payload.extend_from_slice(&1_u64.to_le_bytes());
        payload
    };
    assert_eq!(
        decode_command(&raw_frame(&oversized_name_payload)),
        Err(DecodeError::StringTooLong {
            length: 64,
            maximum: 63,
        })
    );
}

#[test]
fn every_proper_prefix_is_reported_as_truncated() {
    for golden in [HEARTBEAT_GOLDEN, HEATER_GOLDEN, HOME_GOLDEN, MOVE_GOLDEN] {
        for prefix_len in 0..golden.len() {
            assert!(
                matches!(
                    decode_command(&golden[..prefix_len]),
                    Err(DecodeError::Truncated { .. })
                ),
                "prefix {prefix_len}/{} returned {:?}",
                golden.len(),
                decode_command(&golden[..prefix_len])
            );
        }
    }
}

#[test]
fn malformed_headers_follow_the_documented_validation_order() {
    assert_eq!(
        decode_command(b"DX"),
        Err(DecodeError::InvalidMagic { found: *b"DX" })
    );

    let mut header = [0_u8; HEADER_LEN];
    header[..2].copy_from_slice(b"DR");
    header[2] = 2;
    header[3] = 99;
    header[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
    assert_eq!(
        decode_command(&header),
        Err(DecodeError::UnsupportedVersion { version: 2 })
    );

    header[2] = 1;
    assert_eq!(
        decode_command(&header),
        Err(DecodeError::UnsupportedMessageType { message_type: 99 })
    );

    header[3] = 1;
    assert_eq!(
        decode_command(&header),
        Err(DecodeError::PayloadTooLong {
            length: usize::from(u16::MAX),
            maximum: MAX_PAYLOAD_LEN,
        })
    );

    let mut trailing = HEARTBEAT_GOLDEN.to_vec();
    trailing.push(0);
    trailing[12] ^= 0xff;
    assert_eq!(
        decode_command(&trailing),
        Err(DecodeError::TrailingFrameBytes { count: 1 })
    );
}

#[test]
fn checksum_is_verified_before_payload_semantics() {
    let mut corrupt = HEARTBEAT_GOLDEN.to_vec();
    corrupt[11] = 0xff;
    let computed = crc32c(&corrupt[2..corrupt.len() - CHECKSUM_LEN]);
    assert_eq!(
        decode_command(&corrupt),
        Err(DecodeError::ChecksumMismatch {
            encoded: 0x1957_465d,
            computed,
        })
    );
}

#[test]
fn malformed_payloads_are_rejected_without_ambiguity() {
    let mut flags = HEARTBEAT_GOLDEN.to_vec();
    flags[10] = 0x80;
    refresh_checksum(&mut flags);
    assert_eq!(
        decode_command(&flags),
        Err(DecodeError::InvalidFlags { flags: 0x80 })
    );

    let mut tag = HEARTBEAT_GOLDEN.to_vec();
    tag[11] = 0xff;
    refresh_checksum(&mut tag);
    assert_eq!(
        decode_command(&tag),
        Err(DecodeError::UnknownCommandTag { tag: 0xff })
    );

    let mut invalid_utf8 = vec![0, 2, 1, 0xff];
    invalid_utf8.extend_from_slice(&1_u64.to_le_bytes());
    assert_eq!(
        decode_command(&raw_frame(&invalid_utf8)),
        Err(DecodeError::InvalidUtf8)
    );

    assert_eq!(
        decode_command(&raw_frame(&[0, 0, 0xff])),
        Err(DecodeError::TrailingPayloadBytes { count: 1 })
    );

    assert_eq!(
        decode_command(&raw_frame(&[1])),
        Err(DecodeError::Truncated {
            needed: 9,
            available: 1,
        })
    );
}
