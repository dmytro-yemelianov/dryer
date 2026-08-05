#![no_std]
//! Bounded v1 wire framing for Dryer controller commands.
//!
//! A frame is `DR`, version, message type, sequence, payload length, payload,
//! and CRC-32C. Integers are little-endian. The checksum covers the version
//! byte through the end of the payload; the two magic bytes are excluded.

extern crate alloc;

use alloc::string::String;
use core::fmt;
use serde::{Deserialize, Serialize};

/// One microsecond of controller time.
pub type Tick = u64;

pub const MAGIC: [u8; 2] = *b"DR";
pub const PROTOCOL_VERSION: u8 = 1;
pub const COMMAND_MESSAGE_TYPE: u8 = 1;
pub const QUEUE_STATUS_MESSAGE_TYPE: u8 = 2;
pub const HEADER_LEN: usize = 10;
pub const CHECKSUM_LEN: usize = 4;
pub const MAX_STRING_LEN: usize = 63;
pub const MAX_PAYLOAD_LEN: usize = 128;
pub const MAX_FRAME_LEN: usize = HEADER_LEN + MAX_PAYLOAD_LEN + CHECKSUM_LEN;
pub const QUEUE_STATUS_PAYLOAD_LEN: usize = 22;
pub const QUEUE_STATUS_FRAME_LEN: usize = HEADER_LEN + QUEUE_STATUS_PAYLOAD_LEN + CHECKSUM_LEN;

const FLAG_EXECUTE_AT: u8 = 1 << 0;
const KNOWN_FLAGS: u8 = FLAG_EXECUTE_AT;
const QUEUE_STATUS_FLAG_UNDERRUN: u8 = 1 << 0;

const TAG_HEARTBEAT: u8 = 0;
const TAG_SET_HEATER_TARGET: u8 = 1;
const TAG_HOME: u8 = 2;
const TAG_MOVE: u8 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "cmd")]
pub enum Command {
    Heartbeat,
    SetHeaterTarget {
        heater: String,
        target_milli_c: i64,
    },
    Home {
        axis: String,
        rate_um_s: u64,
    },
    Move {
        axis: String,
        distance_um: i64,
        rate_um_s: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub execute_at: Option<Tick>,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandFrame {
    pub sequence: u32,
    pub envelope: CommandEnvelope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueStatus {
    pub capacity: u16,
    pub fill: u16,
    pub earliest_accepted: Tick,
    pub latest_accepted: Tick,
    pub underrun: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueStatusFrame {
    pub sequence: u32,
    pub status: QueueStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    StringTooLong { length: usize, maximum: usize },
    PayloadTooLong { length: usize, maximum: usize },
    BufferTooSmall { needed: usize, available: usize },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StringTooLong { length, maximum } => {
                write!(formatter, "string is {length} bytes; maximum is {maximum}")
            }
            Self::PayloadTooLong { length, maximum } => {
                write!(formatter, "payload is {length} bytes; maximum is {maximum}")
            }
            Self::BufferTooSmall { needed, available } => write!(
                formatter,
                "output buffer is {available} bytes; {needed} bytes are required"
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    Truncated { needed: usize, available: usize },
    InvalidMagic { found: [u8; 2] },
    UnsupportedVersion { version: u8 },
    UnsupportedMessageType { message_type: u8 },
    PayloadTooLong { length: usize, maximum: usize },
    TrailingFrameBytes { count: usize },
    ChecksumMismatch { encoded: u32, computed: u32 },
    InvalidPayloadLength { expected: usize, actual: usize },
    InvalidFlags { flags: u8 },
    InvalidStateFlags { flags: u8 },
    UnknownCommandTag { tag: u8 },
    StringTooLong { length: usize, maximum: usize },
    InvalidUtf8,
    TrailingPayloadBytes { count: usize },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { needed, available } => write!(
                formatter,
                "frame is truncated: {available} bytes available, {needed} required"
            ),
            Self::InvalidMagic { found } => write!(
                formatter,
                "invalid frame magic {:02x} {:02x}",
                found[0], found[1]
            ),
            Self::UnsupportedVersion { version } => {
                write!(formatter, "unsupported protocol version {version}")
            }
            Self::UnsupportedMessageType { message_type } => {
                write!(formatter, "unsupported message type {message_type}")
            }
            Self::PayloadTooLong { length, maximum } => {
                write!(formatter, "payload is {length} bytes; maximum is {maximum}")
            }
            Self::TrailingFrameBytes { count } => {
                write!(formatter, "frame has {count} trailing bytes")
            }
            Self::ChecksumMismatch { encoded, computed } => write!(
                formatter,
                "CRC-32C mismatch: encoded {encoded:08x}, computed {computed:08x}"
            ),
            Self::InvalidPayloadLength { expected, actual } => write!(
                formatter,
                "payload is {actual} bytes; exactly {expected} bytes are required"
            ),
            Self::InvalidFlags { flags } => write!(formatter, "invalid payload flags {flags:02x}"),
            Self::InvalidStateFlags { flags } => {
                write!(formatter, "invalid queue state flags {flags:02x}")
            }
            Self::UnknownCommandTag { tag } => write!(formatter, "unknown command tag {tag}"),
            Self::StringTooLong { length, maximum } => {
                write!(formatter, "string is {length} bytes; maximum is {maximum}")
            }
            Self::InvalidUtf8 => formatter.write_str("command name is not valid UTF-8"),
            Self::TrailingPayloadBytes { count } => {
                write!(formatter, "payload has {count} trailing bytes")
            }
        }
    }
}

/// Encode one command frame without allocating.
///
/// Validation is deterministic: semantic field bounds, payload bounds, then
/// output-buffer capacity are checked before any byte is written.
pub fn encode_command(frame: &CommandFrame, output: &mut [u8]) -> Result<usize, EncodeError> {
    let payload_len = encoded_payload_len(&frame.envelope)?;
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(EncodeError::PayloadTooLong {
            length: payload_len,
            maximum: MAX_PAYLOAD_LEN,
        });
    }
    let frame_len = HEADER_LEN + payload_len + CHECKSUM_LEN;
    if output.len() < frame_len {
        return Err(EncodeError::BufferTooSmall {
            needed: frame_len,
            available: output.len(),
        });
    }

    let mut writer = Writer::new(output);
    writer.write(&MAGIC)?;
    writer.write_u8(PROTOCOL_VERSION)?;
    writer.write_u8(COMMAND_MESSAGE_TYPE)?;
    writer.write(&frame.sequence.to_le_bytes())?;
    writer.write(&(payload_len as u16).to_le_bytes())?;
    encode_payload(&frame.envelope, &mut writer)?;
    let payload_end = writer.position();
    let checksum_input =
        writer
            .buffer()
            .get(2..payload_end)
            .ok_or(EncodeError::BufferTooSmall {
                needed: frame_len,
                available: frame_len,
            })?;
    let checksum = crc32c(checksum_input);
    writer.write(&checksum.to_le_bytes())?;
    Ok(writer.position())
}

/// Decode exactly one command frame.
///
/// Validation order is fixed: magic prefix, complete header, version/type,
/// payload bound, exact frame length, checksum, then payload semantics.
pub fn decode_command(input: &[u8]) -> Result<CommandFrame, DecodeError> {
    require_len(input, 2)?;
    let found = [input[0], input[1]];
    if found != MAGIC {
        return Err(DecodeError::InvalidMagic { found });
    }

    require_len(input, HEADER_LEN)?;
    let version = input[2];
    if version != PROTOCOL_VERSION {
        return Err(DecodeError::UnsupportedVersion { version });
    }
    let message_type = input[3];
    if message_type != COMMAND_MESSAGE_TYPE {
        return Err(DecodeError::UnsupportedMessageType { message_type });
    }
    let sequence = u32::from_le_bytes([input[4], input[5], input[6], input[7]]);
    let payload_len = u16::from_le_bytes([input[8], input[9]]) as usize;
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(DecodeError::PayloadTooLong {
            length: payload_len,
            maximum: MAX_PAYLOAD_LEN,
        });
    }

    let frame_len = HEADER_LEN + payload_len + CHECKSUM_LEN;
    require_len(input, frame_len)?;
    if input.len() > frame_len {
        return Err(DecodeError::TrailingFrameBytes {
            count: input.len() - frame_len,
        });
    }

    let payload_end = HEADER_LEN + payload_len;
    let encoded_checksum = read_u32_at(input, payload_end)?;
    let checksum_input = input.get(2..payload_end).ok_or(DecodeError::Truncated {
        needed: payload_end,
        available: input.len(),
    })?;
    let computed_checksum = crc32c(checksum_input);
    if encoded_checksum != computed_checksum {
        return Err(DecodeError::ChecksumMismatch {
            encoded: encoded_checksum,
            computed: computed_checksum,
        });
    }

    let payload = input
        .get(HEADER_LEN..payload_end)
        .ok_or(DecodeError::Truncated {
            needed: payload_end,
            available: input.len(),
        })?;
    let envelope = decode_payload(payload)?;
    Ok(CommandFrame { sequence, envelope })
}

/// Encode one queue-status frame without allocating.
///
/// The payload is fixed-width, so output-buffer capacity is validated before
/// any byte is written.
pub fn encode_queue_status(
    frame: &QueueStatusFrame,
    output: &mut [u8],
) -> Result<usize, EncodeError> {
    if output.len() < QUEUE_STATUS_FRAME_LEN {
        return Err(EncodeError::BufferTooSmall {
            needed: QUEUE_STATUS_FRAME_LEN,
            available: output.len(),
        });
    }

    let mut writer = Writer::new(output);
    writer.write(&MAGIC)?;
    writer.write_u8(PROTOCOL_VERSION)?;
    writer.write_u8(QUEUE_STATUS_MESSAGE_TYPE)?;
    writer.write(&frame.sequence.to_le_bytes())?;
    writer.write(&(QUEUE_STATUS_PAYLOAD_LEN as u16).to_le_bytes())?;
    writer.write_u8(0)?;
    writer.write(&frame.status.capacity.to_le_bytes())?;
    writer.write(&frame.status.fill.to_le_bytes())?;
    writer.write(&frame.status.earliest_accepted.to_le_bytes())?;
    writer.write(&frame.status.latest_accepted.to_le_bytes())?;
    let state_flags = if frame.status.underrun {
        QUEUE_STATUS_FLAG_UNDERRUN
    } else {
        0
    };
    writer.write_u8(state_flags)?;
    let payload_end = writer.position();
    let checksum_input =
        writer
            .buffer()
            .get(2..payload_end)
            .ok_or(EncodeError::BufferTooSmall {
                needed: QUEUE_STATUS_FRAME_LEN,
                available: QUEUE_STATUS_FRAME_LEN,
            })?;
    let checksum = crc32c(checksum_input);
    writer.write(&checksum.to_le_bytes())?;
    Ok(writer.position())
}

/// Decode exactly one queue-status frame.
///
/// Validation order is fixed: magic prefix, complete header, version/type,
/// payload bound, exact frame length, checksum, exact queue-status payload
/// length, then reserved payload and state flags.
pub fn decode_queue_status(input: &[u8]) -> Result<QueueStatusFrame, DecodeError> {
    require_len(input, 2)?;
    let found = [input[0], input[1]];
    if found != MAGIC {
        return Err(DecodeError::InvalidMagic { found });
    }

    require_len(input, HEADER_LEN)?;
    let version = input[2];
    if version != PROTOCOL_VERSION {
        return Err(DecodeError::UnsupportedVersion { version });
    }
    let message_type = input[3];
    if message_type != QUEUE_STATUS_MESSAGE_TYPE {
        return Err(DecodeError::UnsupportedMessageType { message_type });
    }
    let sequence = u32::from_le_bytes([input[4], input[5], input[6], input[7]]);
    let payload_len = u16::from_le_bytes([input[8], input[9]]) as usize;
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(DecodeError::PayloadTooLong {
            length: payload_len,
            maximum: MAX_PAYLOAD_LEN,
        });
    }

    let frame_len = HEADER_LEN + payload_len + CHECKSUM_LEN;
    require_len(input, frame_len)?;
    if input.len() > frame_len {
        return Err(DecodeError::TrailingFrameBytes {
            count: input.len() - frame_len,
        });
    }

    let payload_end = HEADER_LEN + payload_len;
    let encoded_checksum = read_u32_at(input, payload_end)?;
    let checksum_input = input.get(2..payload_end).ok_or(DecodeError::Truncated {
        needed: payload_end,
        available: input.len(),
    })?;
    let computed_checksum = crc32c(checksum_input);
    if encoded_checksum != computed_checksum {
        return Err(DecodeError::ChecksumMismatch {
            encoded: encoded_checksum,
            computed: computed_checksum,
        });
    }
    if payload_len != QUEUE_STATUS_PAYLOAD_LEN {
        return Err(DecodeError::InvalidPayloadLength {
            expected: QUEUE_STATUS_PAYLOAD_LEN,
            actual: payload_len,
        });
    }

    let payload = input
        .get(HEADER_LEN..payload_end)
        .ok_or(DecodeError::Truncated {
            needed: payload_end,
            available: input.len(),
        })?;
    let mut reader = Reader::new(payload);
    let flags = reader.read_u8()?;
    if flags != 0 {
        return Err(DecodeError::InvalidFlags { flags });
    }
    let capacity = reader.read_u16()?;
    let fill = reader.read_u16()?;
    let earliest_accepted = reader.read_u64()?;
    let latest_accepted = reader.read_u64()?;
    let state_flags = reader.read_u8()?;
    if state_flags & !QUEUE_STATUS_FLAG_UNDERRUN != 0 {
        return Err(DecodeError::InvalidStateFlags { flags: state_flags });
    }

    Ok(QueueStatusFrame {
        sequence,
        status: QueueStatus {
            capacity,
            fill,
            earliest_accepted,
            latest_accepted,
            underrun: state_flags & QUEUE_STATUS_FLAG_UNDERRUN != 0,
        },
    })
}

fn encoded_payload_len(envelope: &CommandEnvelope) -> Result<usize, EncodeError> {
    let schedule_len = if envelope.execute_at.is_some() { 8 } else { 0 };
    let command_len = match &envelope.command {
        Command::Heartbeat => 1,
        Command::SetHeaterTarget { heater, .. } => 1 + encoded_string_len(heater)? + 8,
        Command::Home { axis, .. } => 1 + encoded_string_len(axis)? + 8,
        Command::Move { axis, .. } => 1 + encoded_string_len(axis)? + 8 + 8,
    };
    Ok(1 + schedule_len + command_len)
}

fn encoded_string_len(value: &str) -> Result<usize, EncodeError> {
    let length = value.len();
    if length > MAX_STRING_LEN {
        return Err(EncodeError::StringTooLong {
            length,
            maximum: MAX_STRING_LEN,
        });
    }
    Ok(1 + length)
}

fn encode_payload(envelope: &CommandEnvelope, writer: &mut Writer<'_>) -> Result<(), EncodeError> {
    let flags = if envelope.execute_at.is_some() {
        FLAG_EXECUTE_AT
    } else {
        0
    };
    writer.write_u8(flags)?;
    if let Some(execute_at) = envelope.execute_at {
        writer.write(&execute_at.to_le_bytes())?;
    }
    match &envelope.command {
        Command::Heartbeat => writer.write_u8(TAG_HEARTBEAT),
        Command::SetHeaterTarget {
            heater,
            target_milli_c,
        } => {
            writer.write_u8(TAG_SET_HEATER_TARGET)?;
            writer.write_string(heater)?;
            writer.write(&target_milli_c.to_le_bytes())
        }
        Command::Home { axis, rate_um_s } => {
            writer.write_u8(TAG_HOME)?;
            writer.write_string(axis)?;
            writer.write(&rate_um_s.to_le_bytes())
        }
        Command::Move {
            axis,
            distance_um,
            rate_um_s,
        } => {
            writer.write_u8(TAG_MOVE)?;
            writer.write_string(axis)?;
            writer.write(&distance_um.to_le_bytes())?;
            writer.write(&rate_um_s.to_le_bytes())
        }
    }
}

fn decode_payload(payload: &[u8]) -> Result<CommandEnvelope, DecodeError> {
    let mut reader = Reader::new(payload);
    let flags = reader.read_u8()?;
    if flags & !KNOWN_FLAGS != 0 {
        return Err(DecodeError::InvalidFlags { flags });
    }
    let execute_at = if flags & FLAG_EXECUTE_AT != 0 {
        Some(reader.read_u64()?)
    } else {
        None
    };
    let tag = reader.read_u8()?;
    let command = match tag {
        TAG_HEARTBEAT => Command::Heartbeat,
        TAG_SET_HEATER_TARGET => Command::SetHeaterTarget {
            heater: reader.read_string()?,
            target_milli_c: reader.read_i64()?,
        },
        TAG_HOME => Command::Home {
            axis: reader.read_string()?,
            rate_um_s: reader.read_u64()?,
        },
        TAG_MOVE => Command::Move {
            axis: reader.read_string()?,
            distance_um: reader.read_i64()?,
            rate_um_s: reader.read_u64()?,
        },
        _ => return Err(DecodeError::UnknownCommandTag { tag }),
    };
    if reader.remaining() != 0 {
        return Err(DecodeError::TrailingPayloadBytes {
            count: reader.remaining(),
        });
    }
    Ok(CommandEnvelope {
        execute_at,
        command,
    })
}

fn require_len(input: &[u8], needed: usize) -> Result<(), DecodeError> {
    if input.len() < needed {
        return Err(DecodeError::Truncated {
            needed,
            available: input.len(),
        });
    }
    Ok(())
}

fn read_u32_at(input: &[u8], offset: usize) -> Result<u32, DecodeError> {
    let end = offset.saturating_add(4);
    let bytes = input.get(offset..end).ok_or(DecodeError::Truncated {
        needed: end,
        available: input.len(),
    })?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
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

struct Writer<'a> {
    output: &'a mut [u8],
    position: usize,
}

impl<'a> Writer<'a> {
    fn new(output: &'a mut [u8]) -> Self {
        Self {
            output,
            position: 0,
        }
    }

    fn write_u8(&mut self, value: u8) -> Result<(), EncodeError> {
        self.write(&[value])
    }

    fn write_string(&mut self, value: &str) -> Result<(), EncodeError> {
        self.write_u8(value.len() as u8)?;
        self.write(value.as_bytes())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        let end = self.position.saturating_add(bytes.len());
        let available = self.output.len();
        let target =
            self.output
                .get_mut(self.position..end)
                .ok_or(EncodeError::BufferTooSmall {
                    needed: end,
                    available,
                })?;
        target.copy_from_slice(bytes);
        self.position = end;
        Ok(())
    }

    fn position(&self) -> usize {
        self.position
    }

    fn buffer(&self) -> &[u8] {
        self.output
    }
}

struct Reader<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, position: 0 }
    }

    fn remaining(&self) -> usize {
        self.input.len().saturating_sub(self.position)
    }

    fn read_u8(&mut self) -> Result<u8, DecodeError> {
        let bytes = self.read(1)?;
        Ok(bytes[0])
    }

    fn read_u16(&mut self) -> Result<u16, DecodeError> {
        let bytes = self.read(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u64(&mut self) -> Result<u64, DecodeError> {
        let bytes = self.read(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_i64(&mut self) -> Result<i64, DecodeError> {
        let bytes = self.read(8)?;
        Ok(i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_string(&mut self) -> Result<String, DecodeError> {
        let length = usize::from(self.read_u8()?);
        if length > MAX_STRING_LEN {
            return Err(DecodeError::StringTooLong {
                length,
                maximum: MAX_STRING_LEN,
            });
        }
        let bytes = self.read(length)?;
        let value = core::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidUtf8)?;
        Ok(String::from(value))
    }

    fn read(&mut self, length: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.position.saturating_add(length);
        let bytes = self
            .input
            .get(self.position..end)
            .ok_or(DecodeError::Truncated {
                needed: end,
                available: self.input.len(),
            })?;
        self.position = end;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::crc32c;

    #[test]
    fn crc32c_matches_the_standard_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }
}
