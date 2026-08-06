//! `dryer-transport-adapter`
//!
//! Stream transport adapters, frame delimitation, and daemon reader integration for Dryer.

use std::collections::VecDeque;
use std::fmt;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

pub use dryer_control_client::{decode_clock_response_frame, decode_queue_status_frame, FrameSink};
pub use dryer_control_protocol::{
    decode_clock_request, decode_command, ClockRequestFrame, ClockResponseFrame, CommandFrame,
    DecodeError, QueueStatus, QueueStatusFrame, CLOCK_REQUEST_MESSAGE_TYPE,
    CLOCK_RESPONSE_MESSAGE_TYPE, COMMAND_MESSAGE_TYPE, HEADER_LEN, MAX_FRAME_LEN, MAX_PAYLOAD_LEN,
    PROTOCOL_VERSION, QUEUE_STATUS_MESSAGE_TYPE,
};
pub use dryer_controller_daemon::ControllerDaemon;

/// CRC-32C implementation matching `dryer-control-protocol`.
pub fn crc32c(bytes: &[u8]) -> u32 {
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

/// Abstract byte-stream transport for Dryer frame communication.
pub trait StreamTransport {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Write raw bytes to the transport.
    fn write_bytes(&mut self, data: &[u8]) -> Result<(), Self::Error>;

    /// Read available bytes into `buf`, returning the number of bytes read.
    fn read_bytes(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error>;

    /// Flush any buffered bytes.
    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Adapter wrapping any `StreamTransport` reference to implement `FrameSink` for `CommandClient`.
pub struct TransportSink<'a, T: StreamTransport>(pub &'a mut T);

impl<'a, T: StreamTransport> FrameSink for TransportSink<'a, T> {
    type Error = T::Error;

    fn send_frame(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        self.0.write_bytes(frame)?;
        self.0.flush()
    }
}

impl FrameSink for MemoryTransport {
    type Error = MemoryTransportError;

    fn send_frame(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        self.write_bytes(frame)?;
        self.flush()
    }
}

impl FrameSink for ChannelTransport {
    type Error = ChannelTransportError;

    fn send_frame(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        self.write_bytes(frame)?;
        self.flush()
    }
}

/// Serial transport configuration specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialTransportSpec {
    pub port_path: String,
    pub baud_rate: u32,
    pub flow_control: FlowControl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlowControl {
    None,
    Software,
    Hardware,
}

impl SerialTransportSpec {
    pub fn new(port_path: impl Into<String>, baud_rate: u32, flow_control: FlowControl) -> Self {
        Self {
            port_path: port_path.into(),
            baud_rate,
            flow_control,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.port_path.trim().is_empty() {
            return Err("port_path cannot be empty".into());
        }
        if self.baud_rate == 0 {
            return Err("baud_rate must be greater than zero".into());
        }
        Ok(())
    }
}

impl Default for SerialTransportSpec {
    fn default() -> Self {
        Self {
            port_path: String::from("/dev/ttyUSB0"),
            baud_rate: 115_200,
            flow_control: FlowControl::None,
        }
    }
}

/// Error type for `MemoryTransport`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryTransportError {
    Closed,
}

impl fmt::Display for MemoryTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "memory transport is closed"),
        }
    }
}

impl std::error::Error for MemoryTransportError {}

/// In-memory stream transport for unit testing and local thread framing.
#[derive(Debug, Clone)]
pub struct MemoryTransport {
    incoming: Arc<Mutex<VecDeque<u8>>>,
    outgoing: Arc<Mutex<VecDeque<u8>>>,
}

impl MemoryTransport {
    /// Create a connected pair of `MemoryTransport` endpoints (A, B).
    /// Bytes written to A will be read from B, and vice-versa.
    pub fn pair() -> (Self, Self) {
        let q1 = Arc::new(Mutex::new(VecDeque::new()));
        let q2 = Arc::new(Mutex::new(VecDeque::new()));
        let a = Self {
            incoming: Arc::clone(&q1),
            outgoing: Arc::clone(&q2),
        };
        let b = Self {
            incoming: Arc::clone(&q2),
            outgoing: Arc::clone(&q1),
        };
        (a, b)
    }

    pub fn new() -> Self {
        let (a, _) = Self::pair();
        a
    }

    /// Inject bytes directly into incoming buffer for testing.
    pub fn inject_incoming(&self, bytes: &[u8]) {
        let mut inc = self.incoming.lock().unwrap();
        inc.extend(bytes);
    }

    /// Read raw outgoing bytes directly for testing.
    pub fn take_outgoing(&self) -> Vec<u8> {
        let mut out = self.outgoing.lock().unwrap();
        out.drain(..).collect()
    }
}

impl Default for MemoryTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamTransport for MemoryTransport {
    type Error = MemoryTransportError;

    fn write_bytes(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        let mut out = self.outgoing.lock().unwrap();
        out.extend(data);
        Ok(())
    }

    fn read_bytes(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let mut inc = self.incoming.lock().unwrap();
        let count = buf.len().min(inc.len());
        for slot in buf.iter_mut().take(count) {
            *slot = inc.pop_front().unwrap();
        }
        Ok(count)
    }
}

/// Error type for `ChannelTransport`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelTransportError {
    SendFailed,
    Disconnected,
}

impl fmt::Display for ChannelTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SendFailed => write!(f, "failed to send bytes across thread channel"),
            Self::Disconnected => write!(f, "channel transport thread disconnected"),
        }
    }
}

impl std::error::Error for ChannelTransportError {}

/// Thread-channel based stream transport for cross-thread messaging.
pub struct ChannelTransport {
    sender: mpsc::Sender<Vec<u8>>,
    receiver: mpsc::Receiver<Vec<u8>>,
    read_buf: VecDeque<u8>,
}

impl ChannelTransport {
    /// Create a connected pair of `ChannelTransport` endpoints.
    pub fn pair() -> (Self, Self) {
        let (tx1, rx1) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();

        let endpoint1 = Self {
            sender: tx1,
            receiver: rx2,
            read_buf: VecDeque::new(),
        };
        let endpoint2 = Self {
            sender: tx2,
            receiver: rx1,
            read_buf: VecDeque::new(),
        };

        (endpoint1, endpoint2)
    }
}

impl StreamTransport for ChannelTransport {
    type Error = ChannelTransportError;

    fn write_bytes(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.sender
            .send(data.to_vec())
            .map_err(|_| ChannelTransportError::SendFailed)
    }

    fn read_bytes(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if self.read_buf.is_empty() {
            match self.receiver.try_recv() {
                Ok(chunk) => self.read_buf.extend(chunk),
                Err(mpsc::TryRecvError::Empty) => return Ok(0),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(ChannelTransportError::Disconnected)
                }
            }
        }

        let count = buf.len().min(self.read_buf.len());
        for slot in buf.iter_mut().take(count) {
            *slot = self.read_buf.pop_front().unwrap();
        }
        Ok(count)
    }
}

/// Errors emitted during frame delimiting and codec parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameCodecError {
    BufferOverflow { capacity: usize },
}

impl fmt::Display for FrameCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferOverflow { capacity } => {
                write!(f, "frame codec buffer overflowed capacity {capacity}")
            }
        }
    }
}

impl std::error::Error for FrameCodecError {}

pub const DEFAULT_CODEC_CAPACITY: usize = 4096;

/// Delimits length-prefixed protocol frames matching `MAX_FRAME_LEN` and `DR` magic.
#[derive(Debug, Clone)]
pub struct FrameCodec {
    buffer: Vec<u8>,
    max_capacity: usize,
}

impl FrameCodec {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CODEC_CAPACITY)
    }

    pub fn with_capacity(max_capacity: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_capacity,
        }
    }

    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    /// Feed incoming bytes into the codec buffer.
    pub fn feed(&mut self, data: &[u8]) -> Result<(), FrameCodecError> {
        if self.buffer.len().saturating_add(data.len()) > self.max_capacity {
            return Err(FrameCodecError::BufferOverflow {
                capacity: self.max_capacity,
            });
        }
        self.buffer.extend_from_slice(data);
        Ok(())
    }

    /// Extract the next complete, CRC-validated frame from the buffer if available.
    /// Performs stream resynchronization and loss recovery on corrupted bytes.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameCodecError> {
        loop {
            // Find position of magic bytes "DR"
            let magic_pos = self
                .buffer
                .windows(2)
                .position(|win| win == dryer_control_protocol::MAGIC);

            match magic_pos {
                None => {
                    // No magic found. Keep last byte if it might be 'D', drop rest.
                    if self.buffer.last() == Some(&b'D') {
                        let len = self.buffer.len();
                        self.buffer.drain(..len - 1);
                    } else {
                        self.buffer.clear();
                    }
                    return Ok(None);
                }
                Some(pos) if pos > 0 => {
                    // Discard noise bytes leading up to "DR" (loss recovery resync)
                    self.buffer.drain(..pos);
                }
                Some(_) => {
                    // Buffer starts with "DR"
                    if self.buffer.len() < HEADER_LEN {
                        return Ok(None);
                    }

                    let version = self.buffer[2];
                    let msg_type = self.buffer[3];
                    let payload_len = u16::from_le_bytes([self.buffer[8], self.buffer[9]]) as usize;

                    // Validate header bounds
                    let valid_header = version == PROTOCOL_VERSION
                        && (1..=4).contains(&msg_type)
                        && payload_len <= MAX_PAYLOAD_LEN;

                    if !valid_header {
                        // Invalid header candidate: discard magic byte 'D' to search for next "DR"
                        self.buffer.drain(..1);
                        continue;
                    }

                    let frame_len = HEADER_LEN + payload_len + 4; // HEADER + PAYLOAD + CHECKSUM
                    if frame_len > MAX_FRAME_LEN {
                        self.buffer.drain(..1);
                        continue;
                    }

                    if self.buffer.len() < frame_len {
                        // Incomplete frame, wait for more data
                        return Ok(None);
                    }

                    // Verify CRC-32C checksum
                    let payload_end = HEADER_LEN + payload_len;
                    let encoded_checksum = u32::from_le_bytes([
                        self.buffer[payload_end],
                        self.buffer[payload_end + 1],
                        self.buffer[payload_end + 2],
                        self.buffer[payload_end + 3],
                    ]);

                    let computed_checksum = crc32c(&self.buffer[2..payload_end]);

                    if encoded_checksum != computed_checksum {
                        // CRC mismatch / corruption: discard magic byte 'D' and attempt resync
                        self.buffer.drain(..1);
                        continue;
                    }

                    // Valid frame delimited!
                    let frame = self.buffer.drain(..frame_len).collect();
                    return Ok(Some(frame));
                }
            }
        }
    }
}

impl Default for FrameCodec {
    fn default() -> Self {
        Self::new()
    }
}

/// Dispatched frame event types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchedFrame {
    QueueStatus(QueueStatusFrame),
    ClockRequest(ClockRequestFrame),
    ClockResponse(ClockResponseFrame),
    Command(CommandFrame),
    Raw { message_type: u8, bytes: Vec<u8> },
}

/// Adapter error for transport, framing codec, or daemon frame updates.
#[derive(Debug)]
pub enum TransportAdapterError<E> {
    Transport(E),
    Codec(FrameCodecError),
    Decode(DecodeError),
    DaemonUpdate(DecodeError),
}

impl<E: fmt::Display> fmt::Display for TransportAdapterError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(err) => write!(f, "transport error: {err}"),
            Self::Codec(err) => write!(f, "frame codec error: {err}"),
            Self::Decode(err) => write!(f, "frame decode error: {err}"),
            Self::DaemonUpdate(err) => write!(f, "daemon queue status update error: {err}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for TransportAdapterError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(err) => Some(err),
            Self::Codec(err) => Some(err),
            Self::Decode(_) => None,
            Self::DaemonUpdate(_) => None,
        }
    }
}

/// Transport stream reader integrating frame delimitation with `ControllerDaemon`.
#[derive(Debug, Clone)]
pub struct TransportStreamReader {
    codec: FrameCodec,
    read_buf: [u8; MAX_FRAME_LEN * 4],
}

impl TransportStreamReader {
    pub fn new() -> Self {
        Self::with_codec(FrameCodec::new())
    }

    pub fn with_codec(codec: FrameCodec) -> Self {
        Self {
            codec,
            read_buf: [0; MAX_FRAME_LEN * 4],
        }
    }

    pub fn codec(&self) -> &FrameCodec {
        &self.codec
    }

    pub fn codec_mut(&mut self) -> &mut FrameCodec {
        &mut self.codec
    }

    /// Read bytes from transport, delimit frames, update daemon state, and return dispatched frames.
    pub fn read_and_dispatch<T: StreamTransport>(
        &mut self,
        transport: &mut T,
        daemon: &mut ControllerDaemon,
        controller_id: &str,
        current_host_us: u64,
    ) -> Result<Vec<DispatchedFrame>, TransportAdapterError<T::Error>> {
        let n = transport
            .read_bytes(&mut self.read_buf)
            .map_err(TransportAdapterError::Transport)?;

        if n > 0 {
            self.codec
                .feed(&self.read_buf[..n])
                .map_err(TransportAdapterError::Codec)?;
        }

        let mut dispatched = Vec::new();
        while let Some(frame_bytes) = self
            .codec
            .next_frame()
            .map_err(TransportAdapterError::Codec)?
        {
            let frame =
                self.dispatch_frame(&frame_bytes, daemon, controller_id, current_host_us)?;
            dispatched.push(frame);
        }

        Ok(dispatched)
    }

    /// Dispatch a single validated raw frame, integrating with `ControllerDaemon::update_queue_status` for queue status messages.
    pub fn dispatch_frame<E>(
        &mut self,
        frame_bytes: &[u8],
        daemon: &mut ControllerDaemon,
        controller_id: &str,
        current_host_us: u64,
    ) -> Result<DispatchedFrame, TransportAdapterError<E>> {
        if frame_bytes.len() < HEADER_LEN {
            return Err(TransportAdapterError::Decode(DecodeError::Truncated {
                needed: HEADER_LEN,
                available: frame_bytes.len(),
            }));
        }

        let msg_type = frame_bytes[3];
        match msg_type {
            QUEUE_STATUS_MESSAGE_TYPE => {
                let _status = daemon
                    .update_queue_status(controller_id, frame_bytes, current_host_us)
                    .map_err(TransportAdapterError::DaemonUpdate)?;
                let qs_frame = decode_queue_status_frame(frame_bytes)
                    .map_err(TransportAdapterError::Decode)?;
                Ok(DispatchedFrame::QueueStatus(qs_frame))
            }
            CLOCK_RESPONSE_MESSAGE_TYPE => {
                let cr_frame = decode_clock_response_frame(frame_bytes)
                    .map_err(TransportAdapterError::Decode)?;
                Ok(DispatchedFrame::ClockResponse(cr_frame))
            }
            CLOCK_REQUEST_MESSAGE_TYPE => {
                let req_frame =
                    decode_clock_request(frame_bytes).map_err(TransportAdapterError::Decode)?;
                Ok(DispatchedFrame::ClockRequest(req_frame))
            }
            COMMAND_MESSAGE_TYPE => {
                let cmd_frame =
                    decode_command(frame_bytes).map_err(TransportAdapterError::Decode)?;
                Ok(DispatchedFrame::Command(cmd_frame))
            }
            other => Ok(DispatchedFrame::Raw {
                message_type: other,
                bytes: frame_bytes.to_vec(),
            }),
        }
    }
}

impl Default for TransportStreamReader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dryer_control_protocol::encode_queue_status;

    #[test]
    fn serial_transport_spec_validates_correctly() {
        let spec = SerialTransportSpec::default();
        assert!(spec.validate().is_ok());

        let invalid_path = SerialTransportSpec::new("  ", 115200, FlowControl::None);
        assert_eq!(
            invalid_path.validate(),
            Err("port_path cannot be empty".into())
        );

        let invalid_baud = SerialTransportSpec::new("/dev/ttyUSB0", 0, FlowControl::None);
        assert_eq!(
            invalid_baud.validate(),
            Err("baud_rate must be greater than zero".into())
        );
    }

    #[test]
    fn memory_transport_pair_transfers_bytes_bidirectionally() {
        let (mut host, mut mcu) = MemoryTransport::pair();

        host.write_bytes(b"hello mcu").unwrap();
        let mut buf = [0u8; 32];
        let read = mcu.read_bytes(&mut buf).unwrap();
        assert_eq!(&buf[..read], b"hello mcu");

        mcu.write_bytes(b"hello host").unwrap();
        let read = host.read_bytes(&mut buf).unwrap();
        assert_eq!(&buf[..read], b"hello host");
    }

    #[test]
    fn channel_transport_pair_transfers_bytes() {
        let (mut host, mut mcu) = ChannelTransport::pair();

        host.write_bytes(b"ping").unwrap();
        let mut buf = [0u8; 16];
        let read = mcu.read_bytes(&mut buf).unwrap();
        assert_eq!(&buf[..read], b"ping");
    }

    #[test]
    fn frame_codec_delimits_and_resynchronizes_over_noise() {
        let mut codec = FrameCodec::new();

        let frame = QueueStatusFrame {
            sequence: 42,
            status: QueueStatus {
                capacity: 100,
                fill: 10,
                earliest_accepted: 1000,
                latest_accepted: 2000,
                underrun: false,
            },
        };
        let mut encoded = [0u8; MAX_FRAME_LEN];
        let len = encode_queue_status(&frame, &mut encoded).unwrap();

        // Feed garbage noise followed by valid frame
        let mut stream = vec![0xFF, 0xAA, 0x55, 0x00, b'D'];
        stream.extend_from_slice(&encoded[..len]);
        stream.extend_from_slice(&[0x11, 0x22]);

        codec.feed(&stream).unwrap();
        let delimited = codec.next_frame().unwrap().expect("frame delimited");
        assert_eq!(delimited, &encoded[..len]);
        assert_eq!(codec.next_frame().unwrap(), None);
    }

    #[test]
    fn reader_dispatches_queue_status_to_daemon() {
        let mut daemon = ControllerDaemon::new();
        daemon.register_controller("mcu1", 50_000);

        let (mut host, mut mcu) = MemoryTransport::pair();

        let frame = QueueStatusFrame {
            sequence: 1,
            status: QueueStatus {
                capacity: 64,
                fill: 5,
                earliest_accepted: 500,
                latest_accepted: 1500,
                underrun: false,
            },
        };
        let mut encoded = [0u8; MAX_FRAME_LEN];
        let len = encode_queue_status(&frame, &mut encoded).unwrap();
        mcu.write_bytes(&encoded[..len]).unwrap();

        let mut reader = TransportStreamReader::new();
        let dispatched = reader
            .read_and_dispatch(&mut host, &mut daemon, "mcu1", 10_000)
            .unwrap();

        assert_eq!(dispatched.len(), 1);
        match &dispatched[0] {
            DispatchedFrame::QueueStatus(qs) => {
                assert_eq!(qs.sequence, 1);
                assert_eq!(qs.status.fill, 5);
            }
            _ => panic!("expected QueueStatus frame"),
        }

        let session_status = daemon.session_status("mcu1", 10_000).unwrap();
        assert_eq!(session_status.queue_fill, 5);
        assert_eq!(session_status.queue_capacity, 64);
        assert_eq!(session_status.last_seen_host_us, 10_000);
    }
}
