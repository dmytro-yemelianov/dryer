//! Synchronous host-side sender for the bounded Dryer control protocol.
//!
//! The client owns one fixed-size frame buffer. A sequence number is consumed
//! once a command has encoded successfully, even if the transport rejects the
//! resulting frame. Encoding failures do not consume a sequence number.

extern crate alloc;

use core::fmt;

use dryer_clock_sync::ControllerTick;
pub use dryer_clock_sync::{ClockEstimate, ClockSample, ClockSync, ClockSyncError, HostTick};
pub use dryer_control_protocol::{
    decode_clock_response, decode_queue_status, ClockRequestFrame, ClockResponse,
    ClockResponseFrame, Command, DecodeError, QueueStatus, QueueStatusFrame, Tick,
};
use dryer_control_protocol::{encode_clock_request, CLOCK_REQUEST_FRAME_LEN};
use dryer_control_protocol::{
    encode_command, CommandEnvelope, CommandFrame, EncodeError, MAX_FRAME_LEN,
};

/// Decode and validate one complete controller queue-status observation.
///
/// This function is transport-agnostic: callers remain responsible for
/// obtaining exactly one frame from their transport.
pub fn decode_queue_status_frame(input: &[u8]) -> Result<QueueStatusFrame, DecodeError> {
    decode_queue_status(input)
}

/// Decode and validate one complete controller clock-sync response.
pub fn decode_clock_response_frame(input: &[u8]) -> Result<ClockResponseFrame, DecodeError> {
    decode_clock_response(input)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockExchangeError {
    ExchangePending,
    NoExchangePending,
    SequenceMismatch { expected: u32, received: u32 },
    Decode(DecodeError),
    Sync(ClockSyncError),
}

pub trait HostClock {
    fn now(&mut self) -> HostTick;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockRequestReceipt {
    pub sequence: u32,
    pub frame_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockExchangeReceipt {
    pub sequence: u32,
    pub sample: ClockSample,
    pub estimate: Option<ClockEstimate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockTimeout {
    pub sequence: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockSessionError<E> {
    Busy { sequence: u32 },
    SequenceExhausted,
    Encode(EncodeError),
    Transport(E),
    Sync(ClockSyncError),
    Decode(DecodeError),
    NoPending,
    UnexpectedSequence { expected: u32, received: u32 },
    TimedOut { sequence: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingClockExchange {
    sequence: u32,
    host_send: HostTick,
}

/// Event-driven clock exchange state. Framing, I/O, and timeout scheduling
/// remain with the caller; this type owns timestamp ordering and correlation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockSession {
    sync: ClockSync,
    next_sequence: u32,
    exhausted: bool,
    timeout_ticks: u64,
    pending: Option<PendingClockExchange>,
    request: [u8; CLOCK_REQUEST_FRAME_LEN],
}

impl ClockSession {
    pub fn new(max_slew_ppb: i128, timeout_ticks: u64) -> Result<Self, ClockSyncError> {
        Ok(Self {
            sync: ClockSync::new(max_slew_ppb)?,
            next_sequence: 0,
            exhausted: false,
            timeout_ticks,
            pending: None,
            request: [0; CLOCK_REQUEST_FRAME_LEN],
        })
    }

    pub fn begin<S: FrameSink, C: HostClock>(
        &mut self,
        sink: &mut S,
        clock: &mut C,
    ) -> Result<ClockRequestReceipt, ClockSessionError<S::Error>> {
        if let Some(pending) = self.pending {
            return Err(ClockSessionError::Busy {
                sequence: pending.sequence,
            });
        }
        if self.exhausted {
            return Err(ClockSessionError::SequenceExhausted);
        }
        let sequence = self.next_sequence;
        let frame_len = encode_clock_request(&ClockRequestFrame { sequence }, &mut self.request)
            .map_err(ClockSessionError::Encode)?;
        self.exhausted = sequence == u32::MAX;
        self.next_sequence = sequence.wrapping_add(1);
        let host_send = clock.now();
        sink.send_frame(&self.request[..frame_len])
            .map_err(ClockSessionError::Transport)?;
        self.pending = Some(PendingClockExchange {
            sequence,
            host_send,
        });
        Ok(ClockRequestReceipt {
            sequence,
            frame_len,
        })
    }

    pub fn accept_response<C: HostClock>(
        &mut self,
        bytes: &[u8],
        clock: &mut C,
    ) -> Result<ClockExchangeReceipt, ClockSessionError<core::convert::Infallible>> {
        let host_receive = clock.now();
        let response = decode_clock_response_frame(bytes).map_err(ClockSessionError::Decode)?;
        let pending = self.pending.ok_or(ClockSessionError::NoPending)?;
        if response.sequence != pending.sequence {
            return Err(ClockSessionError::UnexpectedSequence {
                expected: pending.sequence,
                received: response.sequence,
            });
        }
        if host_receive.0.saturating_sub(pending.host_send.0) >= self.timeout_ticks {
            self.pending = None;
            return Err(ClockSessionError::TimedOut {
                sequence: pending.sequence,
            });
        }
        let sample = ClockSample {
            host_send: pending.host_send,
            controller_receive: ControllerTick(response.response.controller_receive),
            controller_send: ControllerTick(response.response.controller_send),
            host_receive,
        };
        // A matching response retires the outstanding exchange even when the
        // estimator rejects its sample; callers must start a fresh exchange.
        self.pending = None;
        let estimate = self
            .sync
            .push(sample)
            .map_err(ClockSessionError::Sync)?
            .copied();
        Ok(ClockExchangeReceipt {
            sequence: pending.sequence,
            sample,
            estimate,
        })
    }

    pub fn expire(&mut self, now: HostTick) -> Option<ClockTimeout> {
        let pending = self.pending?;
        if now.0.saturating_sub(pending.host_send.0) >= self.timeout_ticks {
            self.pending = None;
            Some(ClockTimeout {
                sequence: pending.sequence,
            })
        } else {
            None
        }
    }

    pub fn synchronizer(&self) -> &ClockSync {
        &self.sync
    }
}

/// Multi-controller clock synchronization state (§16.5).
///
/// Tracks independent `ClockSession` instances for each controller in a cluster,
/// enabling coordinated multi-controller scheduling and cross-controller synchronization
/// verification.
#[derive(Debug)]
pub struct MultiControllerClockSync {
    sessions: alloc::collections::BTreeMap<alloc::string::String, ClockSession>,
    max_slew_ppb: i128,
    timeout_ticks: u64,
}

impl MultiControllerClockSync {
    pub fn new(max_slew_ppb: i128, timeout_ticks: u64) -> Self {
        Self {
            sessions: alloc::collections::BTreeMap::new(),
            max_slew_ppb,
            timeout_ticks,
        }
    }

    pub fn add_controller(&mut self, name: &str) -> Result<(), ClockSyncError> {
        let session = ClockSession::new(self.max_slew_ppb, self.timeout_ticks)?;
        self.sessions
            .insert(alloc::string::String::from(name), session);
        Ok(())
    }

    pub fn session_mut(&mut self, name: &str) -> Option<&mut ClockSession> {
        self.sessions.get_mut(name)
    }

    pub fn session(&self, name: &str) -> Option<&ClockSession> {
        self.sessions.get(name)
    }

    pub fn is_cluster_synchronized(&self) -> bool {
        !self.sessions.is_empty()
            && self
                .sessions
                .values()
                .all(|session| session.synchronizer().estimate().is_some())
    }

    pub fn controller_count(&self) -> usize {
        self.sessions.len()
    }
}

/// Transport-agnostic state for one outstanding clock exchange.
///
/// The transport boundary must call `begin` with a timestamp captured
/// immediately before handing the encoded request to the transport, then call
/// `complete` with a timestamp captured immediately after receiving the full
/// response frame. Only one sequence may be outstanding at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockExchange {
    sync: ClockSync,
    next_sequence: u32,
    pending: Option<(u32, HostTick)>,
}

impl ClockExchange {
    pub fn new(max_slew_ppb: i128) -> Result<Self, ClockSyncError> {
        Ok(Self {
            sync: ClockSync::new(max_slew_ppb)?,
            next_sequence: 0,
            pending: None,
        })
    }

    pub fn begin(&mut self, host_send: HostTick) -> Result<ClockRequestFrame, ClockExchangeError> {
        if self.pending.is_some() {
            return Err(ClockExchangeError::ExchangePending);
        }
        let sequence = self.next_sequence;
        self.next_sequence = sequence.wrapping_add(1);
        self.pending = Some((sequence, host_send));
        Ok(ClockRequestFrame { sequence })
    }

    pub fn complete(
        &mut self,
        host_receive: HostTick,
        response: ClockResponseFrame,
    ) -> Result<Option<&ClockEstimate>, ClockExchangeError> {
        let (expected, host_send) = self.pending.ok_or(ClockExchangeError::NoExchangePending)?;
        if response.sequence != expected {
            return Err(ClockExchangeError::SequenceMismatch {
                expected,
                received: response.sequence,
            });
        }
        let sample = ClockSample {
            host_send,
            controller_receive: dryer_clock_sync::ControllerTick(
                response.response.controller_receive,
            ),
            controller_send: dryer_clock_sync::ControllerTick(response.response.controller_send),
            host_receive,
        };
        let estimate = self.sync.push(sample).map_err(ClockExchangeError::Sync)?;
        self.pending = None;
        Ok(estimate)
    }

    pub fn complete_frame(
        &mut self,
        host_receive: HostTick,
        input: &[u8],
    ) -> Result<Option<&ClockEstimate>, ClockExchangeError> {
        let response = decode_clock_response_frame(input).map_err(ClockExchangeError::Decode)?;
        self.complete(host_receive, response)
    }

    pub fn estimate(&self) -> Option<&ClockEstimate> {
        self.sync.estimate()
    }
    pub fn next_sequence(&self) -> u32 {
        self.next_sequence
    }
    pub fn pending_sequence(&self) -> Option<u32> {
        self.pending.map(|(sequence, _)| sequence)
    }
}

/// A synchronous destination for one complete encoded command frame.
pub trait FrameSink {
    type Error;

    fn send_frame(&mut self, frame: &[u8]) -> Result<(), Self::Error>;
}

/// Metadata for a command frame accepted by the sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendReceipt {
    pub sequence: u32,
    pub frame_len: usize,
}

/// A command that could not be encoded or delivered to the sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError<E> {
    Encode(EncodeError),
    Transport(E),
}

impl<E: fmt::Display> fmt::Display for SendError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(error) => write!(formatter, "failed to encode command frame: {error}"),
            Self::Transport(error) => write!(formatter, "failed to send command frame: {error}"),
        }
    }
}

impl<E> std::error::Error for SendError<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Encode(_) => None,
            Self::Transport(error) => Some(error),
        }
    }
}

/// A synchronous command sender backed by one fixed-size protocol buffer.
#[derive(Debug)]
pub struct CommandClient<S> {
    sink: S,
    next_sequence: u32,
    frame: [u8; MAX_FRAME_LEN],
}

impl<S> CommandClient<S> {
    /// Create a client whose first frame uses sequence zero.
    pub fn new(sink: S) -> Self {
        Self::with_initial_sequence(sink, 0)
    }

    /// Create a client whose first frame uses `next_sequence`.
    pub fn with_initial_sequence(sink: S, next_sequence: u32) -> Self {
        Self {
            sink,
            next_sequence,
            frame: [0; MAX_FRAME_LEN],
        }
    }

    pub fn next_sequence(&self) -> u32 {
        self.next_sequence
    }

    pub fn sink(&self) -> &S {
        &self.sink
    }

    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    pub fn into_sink(self) -> S {
        self.sink
    }
}

impl<S: FrameSink> CommandClient<S> {
    /// Encode and synchronously send an immediate command.
    pub fn send(&mut self, command: Command) -> Result<SendReceipt, SendError<S::Error>> {
        self.send_inner(None, command)
    }

    /// Encode and synchronously send a command carrying a controller timestamp.
    pub fn send_scheduled(
        &mut self,
        execute_at: Tick,
        command: Command,
    ) -> Result<SendReceipt, SendError<S::Error>> {
        self.send_inner(Some(execute_at), command)
    }

    fn send_inner(
        &mut self,
        execute_at: Option<Tick>,
        command: Command,
    ) -> Result<SendReceipt, SendError<S::Error>> {
        let sequence = self.next_sequence;
        let frame = CommandFrame {
            sequence,
            envelope: CommandEnvelope {
                execute_at,
                command,
            },
        };
        let frame_len = encode_command(&frame, &mut self.frame).map_err(SendError::Encode)?;

        // Encoding commits the sequence. Delivery may be ambiguous after a
        // transport error, so retries must use a new sequence number.
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.sink
            .send_frame(&self.frame[..frame_len])
            .map_err(SendError::Transport)?;

        Ok(SendReceipt {
            sequence,
            frame_len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dryer_control_protocol::{
        decode_command, encode_clock_response, encode_queue_status, ClockResponse,
        ClockResponseFrame, CLOCK_RESPONSE_FRAME_LEN, MAX_STRING_LEN, QUEUE_STATUS_FRAME_LEN,
    };

    #[derive(Debug, Default)]
    struct RecordingSink {
        frames: Vec<Vec<u8>>,
    }

    impl FrameSink for RecordingSink {
        type Error = core::convert::Infallible;

        fn send_frame(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
            self.frames.push(frame.to_vec());
            Ok(())
        }
    }

    #[derive(Debug)]
    struct ScriptClock {
        ticks: Vec<HostTick>,
    }

    impl HostClock for ScriptClock {
        fn now(&mut self) -> HostTick {
            self.ticks.remove(0)
        }
    }

    #[test]
    fn immediate_and_scheduled_commands_use_consecutive_sequences() {
        let mut client = CommandClient::with_initial_sequence(RecordingSink::default(), 41);

        let immediate = client
            .send(Command::Heartbeat)
            .expect("immediate command sends");
        let scheduled = client
            .send_scheduled(
                500_000,
                Command::Home {
                    axis: "x".into(),
                    rate_um_s: 10_000,
                },
            )
            .expect("scheduled command sends");

        assert_eq!(immediate.sequence, 41);
        assert_eq!(scheduled.sequence, 42);
        assert_eq!(client.next_sequence(), 43);
        assert!(immediate.frame_len <= MAX_FRAME_LEN);
        assert!(scheduled.frame_len <= MAX_FRAME_LEN);

        let frames = &client.sink().frames;
        let decoded_immediate = decode_command(&frames[0]).expect("immediate frame decodes");
        let decoded_scheduled = decode_command(&frames[1]).expect("scheduled frame decodes");
        assert_eq!(decoded_immediate.sequence, 41);
        assert_eq!(decoded_immediate.envelope.execute_at, None);
        assert_eq!(decoded_scheduled.sequence, 42);
        assert_eq!(decoded_scheduled.envelope.execute_at, Some(500_000));
    }

    #[test]
    fn fixed_buffer_accepts_the_maximum_string_length() {
        let mut client = CommandClient::new(RecordingSink::default());
        let axis = "a".repeat(MAX_STRING_LEN);

        let receipt = client
            .send_scheduled(
                Tick::MAX,
                Command::Move {
                    axis: axis.clone(),
                    distance_um: i64::MIN,
                    rate_um_s: u64::MAX,
                },
            )
            .expect("maximum command fits the fixed frame buffer");

        assert!(receipt.frame_len <= MAX_FRAME_LEN);
        let decoded = decode_command(&client.sink().frames[0]).expect("frame decodes");
        assert_eq!(
            decoded.envelope.command,
            Command::Move {
                axis,
                distance_um: i64::MIN,
                rate_um_s: u64::MAX,
            }
        );
    }

    #[test]
    fn encode_failure_does_not_call_sink_or_advance_sequence() {
        let mut client = CommandClient::with_initial_sequence(RecordingSink::default(), 7);

        let error = client
            .send(Command::Home {
                axis: "a".repeat(MAX_STRING_LEN + 1),
                rate_um_s: 1,
            })
            .expect_err("overlong string is rejected");

        assert_eq!(
            error,
            SendError::Encode(EncodeError::StringTooLong {
                length: MAX_STRING_LEN + 1,
                maximum: MAX_STRING_LEN,
            })
        );
        assert_eq!(client.next_sequence(), 7);
        assert!(client.sink().frames.is_empty());
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct SinkFailure;

    impl fmt::Display for SinkFailure {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("sink unavailable")
        }
    }

    impl std::error::Error for SinkFailure {}

    #[derive(Debug, Default)]
    struct FailingSink {
        calls: usize,
    }

    impl FrameSink for FailingSink {
        type Error = SinkFailure;

        fn send_frame(&mut self, _frame: &[u8]) -> Result<(), Self::Error> {
            self.calls += 1;
            Err(SinkFailure)
        }
    }

    #[test]
    fn transport_failure_consumes_sequence() {
        let mut client = CommandClient::with_initial_sequence(FailingSink::default(), 9);

        let error = client
            .send(Command::Heartbeat)
            .expect_err("sink rejects frame");

        assert_eq!(error, SendError::Transport(SinkFailure));
        assert_eq!(
            error.to_string(),
            "failed to send command frame: sink unavailable"
        );
        assert!(std::error::Error::source(&error).is_some());
        assert_eq!(client.next_sequence(), 10);
        assert_eq!(client.sink().calls, 1);
    }

    #[test]
    fn sequence_wraps_after_u32_max() {
        let mut client = CommandClient::with_initial_sequence(RecordingSink::default(), u32::MAX);

        let last = client
            .send(Command::Heartbeat)
            .expect("last sequence sends");
        let first = client
            .send(Command::Heartbeat)
            .expect("wrapped sequence sends");

        assert_eq!(last.sequence, u32::MAX);
        assert_eq!(first.sequence, 0);
        assert_eq!(client.next_sequence(), 1);
    }

    #[test]
    fn sink_accessors_preserve_ownership() {
        let mut client = CommandClient::new(RecordingSink::default());
        client.sink_mut().frames.push(vec![1, 2, 3]);
        let sink = client.into_sink();
        assert_eq!(sink.frames, [vec![1, 2, 3]]);
    }

    #[test]
    fn queue_status_observation_decodes_through_client_api() {
        let expected = QueueStatusFrame {
            sequence: 17,
            status: QueueStatus {
                capacity: 64,
                fill: 12,
                earliest_accepted: 120_000,
                latest_accepted: 240_000,
                underrun: true,
            },
        };
        let mut encoded = [0; QUEUE_STATUS_FRAME_LEN];
        let length = encode_queue_status(&expected, &mut encoded).expect("queue status encodes");

        assert_eq!(decode_queue_status_frame(&encoded[..length]), Ok(expected));
    }

    #[test]
    fn clock_response_decodes_through_client_api() {
        let expected = ClockResponseFrame {
            sequence: 9,
            response: ClockResponse {
                controller_receive: 100,
                controller_send: 125,
            },
        };
        let mut encoded = [0; CLOCK_RESPONSE_FRAME_LEN];
        let length =
            encode_clock_response(&expected, &mut encoded).expect("clock response encodes");
        assert_eq!(
            decode_clock_response_frame(&encoded[..length]),
            Ok(expected)
        );
    }

    #[test]
    fn clock_exchange_correlates_sequence_and_feeds_estimator() {
        let mut exchange = ClockExchange::new(0).expect("valid slew bound");
        let request = exchange.begin(HostTick(100)).expect("request begins");
        assert_eq!(request.sequence, 0);
        let response = ClockResponseFrame {
            sequence: request.sequence,
            response: ClockResponse {
                controller_receive: 150,
                controller_send: 160,
            },
        };
        assert!(exchange
            .complete(HostTick(200), response)
            .expect("response completes")
            .is_none());
        assert_eq!(exchange.pending_sequence(), None);
    }

    #[test]
    fn clock_exchange_rejects_unsolicited_and_mismatched_responses() {
        let mut exchange = ClockExchange::new(0).expect("valid slew bound");
        assert_eq!(
            exchange.complete(
                HostTick(1),
                ClockResponseFrame {
                    sequence: 1,
                    response: ClockResponse {
                        controller_receive: 1,
                        controller_send: 1
                    },
                }
            ),
            Err(ClockExchangeError::NoExchangePending)
        );
        exchange.begin(HostTick(10)).expect("request begins");
        assert_eq!(
            exchange.complete(
                HostTick(20),
                ClockResponseFrame {
                    sequence: 99,
                    response: ClockResponse {
                        controller_receive: 11,
                        controller_send: 12
                    },
                }
            ),
            Err(ClockExchangeError::SequenceMismatch {
                expected: 0,
                received: 99
            })
        );
        assert_eq!(exchange.pending_sequence(), Some(0));
    }

    #[test]
    fn clock_session_captures_receive_before_decode_and_keeps_pending_on_mismatch() {
        let mut session = ClockSession::new(0, 100).expect("valid session");
        let mut sink = RecordingSink::default();
        let mut clock = ScriptClock {
            ticks: vec![HostTick(10), HostTick(20)],
        };
        let request = session
            .begin(&mut sink, &mut clock)
            .expect("request begins");
        assert_eq!(request.sequence, 0);
        assert!(matches!(
            session.accept_response(b"DX", &mut clock),
            Err(ClockSessionError::Decode(DecodeError::InvalidMagic { .. }))
        ));
        assert_eq!(session.synchronizer().estimate(), None);
        assert_eq!(session.expire(HostTick(109)), None);
        assert_eq!(
            session.expire(HostTick(110)),
            Some(ClockTimeout { sequence: 0 })
        );
    }

    #[test]
    fn clock_session_retires_matching_sample_when_estimator_rejects_it() {
        let mut session = ClockSession::new(0, 100).expect("valid session");
        let mut sink = RecordingSink::default();
        let mut clock = ScriptClock {
            ticks: vec![HostTick(10), HostTick(20)],
        };
        let request = session
            .begin(&mut sink, &mut clock)
            .expect("request begins");
        let response = ClockResponseFrame {
            sequence: request.sequence,
            response: ClockResponse {
                controller_receive: 30,
                controller_send: 20,
            },
        };
        assert!(matches!(
            session.accept_response(&encoded_clock_response(&response), &mut clock),
            Err(ClockSessionError::Sync(ClockSyncError::ControllerRegressed))
        ));
        assert_eq!(session.expire(HostTick(100)), None);
    }

    fn encoded_clock_response(frame: &ClockResponseFrame) -> Vec<u8> {
        let mut bytes = [0; CLOCK_RESPONSE_FRAME_LEN];
        let length = encode_clock_response(frame, &mut bytes).expect("response encodes");
        bytes[..length].to_vec()
    }

    #[test]
    fn queue_status_decode_errors_propagate_unchanged() {
        assert_eq!(
            decode_queue_status_frame(b"DX"),
            Err(DecodeError::InvalidMagic { found: *b"DX" })
        );

        let frame = QueueStatusFrame {
            sequence: 1,
            status: QueueStatus {
                capacity: 8,
                fill: 2,
                earliest_accepted: 10,
                latest_accepted: 20,
                underrun: false,
            },
        };
        let mut encoded = [0; QUEUE_STATUS_FRAME_LEN];
        encode_queue_status(&frame, &mut encoded).expect("queue status encodes");
        encoded[10] ^= 0x80;

        assert!(matches!(
            decode_queue_status_frame(&encoded),
            Err(DecodeError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn multi_controller_clock_sync_manages_multiple_controller_sessions() {
        let mut cluster = MultiControllerClockSync::new(100_000, 5_000);
        cluster.add_controller("mainboard").unwrap();
        cluster.add_controller("toolboard").unwrap();

        assert_eq!(cluster.controller_count(), 2);
        assert!(!cluster.is_cluster_synchronized());

        let mut sink1 = RecordingSink::default();
        let mut sink2 = RecordingSink::default();
        let mut clock1 = ScriptClock {
            ticks: vec![HostTick(100), HostTick(200), HostTick(300), HostTick(400)],
        };
        let mut clock2 = ScriptClock {
            ticks: vec![HostTick(150), HostTick(250), HostTick(350), HostTick(450)],
        };

        // Complete 2 exchanges for mainboard
        let req1_1 = cluster
            .session_mut("mainboard")
            .unwrap()
            .begin(&mut sink1, &mut clock1)
            .unwrap();
        let resp1_1 = ClockResponseFrame {
            sequence: req1_1.sequence,
            response: ClockResponse {
                controller_receive: 120,
                controller_send: 180,
            },
        };
        cluster
            .session_mut("mainboard")
            .unwrap()
            .accept_response(&encoded_clock_response(&resp1_1), &mut clock1)
            .unwrap();

        let req1_2 = cluster
            .session_mut("mainboard")
            .unwrap()
            .begin(&mut sink1, &mut clock1)
            .unwrap();
        let resp1_2 = ClockResponseFrame {
            sequence: req1_2.sequence,
            response: ClockResponse {
                controller_receive: 320,
                controller_send: 380,
            },
        };
        cluster
            .session_mut("mainboard")
            .unwrap()
            .accept_response(&encoded_clock_response(&resp1_2), &mut clock1)
            .unwrap();

        assert!(!cluster.is_cluster_synchronized());

        // Complete 2 exchanges for toolboard
        let req2_1 = cluster
            .session_mut("toolboard")
            .unwrap()
            .begin(&mut sink2, &mut clock2)
            .unwrap();
        let resp2_1 = ClockResponseFrame {
            sequence: req2_1.sequence,
            response: ClockResponse {
                controller_receive: 170,
                controller_send: 230,
            },
        };
        cluster
            .session_mut("toolboard")
            .unwrap()
            .accept_response(&encoded_clock_response(&resp2_1), &mut clock2)
            .unwrap();

        let req2_2 = cluster
            .session_mut("toolboard")
            .unwrap()
            .begin(&mut sink2, &mut clock2)
            .unwrap();
        let resp2_2 = ClockResponseFrame {
            sequence: req2_2.sequence,
            response: ClockResponse {
                controller_receive: 370,
                controller_send: 430,
            },
        };
        cluster
            .session_mut("toolboard")
            .unwrap()
            .accept_response(&encoded_clock_response(&resp2_2), &mut clock2)
            .unwrap();

        assert!(cluster.is_cluster_synchronized());
    }
}
