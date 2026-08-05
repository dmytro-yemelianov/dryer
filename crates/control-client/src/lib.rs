//! Synchronous host-side sender for the bounded Dryer control protocol.
//!
//! The client owns one fixed-size frame buffer. A sequence number is consumed
//! once a command has encoded successfully, even if the transport rejects the
//! resulting frame. Encoding failures do not consume a sequence number.

use core::fmt;

use dryer_control_protocol::{
    encode_command, CommandEnvelope, CommandFrame, EncodeError, MAX_FRAME_LEN,
};
pub use dryer_control_protocol::{Command, Tick};

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
    use dryer_control_protocol::{decode_command, MAX_STRING_LEN};

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
}
