use dryer_control_protocol::{
    decode_clock_request, encode_clock_response, ClockResponse, ClockResponseFrame, DecodeError,
    EncodeError, Tick, CLOCK_RESPONSE_FRAME_LEN,
};
use std::{collections::VecDeque, fmt};

const PARTS_PER_BILLION: i128 = 1_000_000_000;

/// One deterministic direction of the simulated clock-sync link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockLinkConfig {
    pub latency_ticks: Tick,
    pub jitter_ticks: Tick,
    pub loss_per_mille: u16,
    pub dup_per_mille: u16,
}

impl Default for ClockLinkConfig {
    fn default() -> Self {
        Self {
            latency_ticks: 200,
            jitter_ticks: 0,
            loss_per_mille: 0,
            dup_per_mille: 0,
        }
    }
}

/// An integer-only controller clock anchored to one host-clock epoch.
///
/// `rate_ppb` is the controller's signed rate difference from host time. A
/// value of `1_000` means the controller advances 1,000 parts per billion
/// faster than the host. Conversion rounds down to a whole controller tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControllerClock {
    host_epoch: Tick,
    controller_epoch: Tick,
    rate_ppb: i64,
}

impl ControllerClock {
    pub fn new(
        host_epoch: Tick,
        controller_epoch: Tick,
        rate_ppb: i64,
    ) -> Result<Self, ControllerClockError> {
        if i128::from(rate_ppb) <= -PARTS_PER_BILLION {
            return Err(ControllerClockError::NonPositiveRate { rate_ppb });
        }
        Ok(Self {
            host_epoch,
            controller_epoch,
            rate_ppb,
        })
    }

    pub fn host_epoch(&self) -> Tick {
        self.host_epoch
    }

    pub fn controller_epoch(&self) -> Tick {
        self.controller_epoch
    }

    pub fn rate_ppb(&self) -> i64 {
        self.rate_ppb
    }

    pub fn at(&self, host_tick: Tick) -> Result<Tick, ControllerClockError> {
        let elapsed = host_tick.checked_sub(self.host_epoch).ok_or(
            ControllerClockError::HostBeforeEpoch {
                host_tick,
                host_epoch: self.host_epoch,
            },
        )?;
        let rate = PARTS_PER_BILLION
            .checked_add(i128::from(self.rate_ppb))
            .ok_or(ControllerClockError::ArithmeticOverflow)?;
        let scaled = i128::from(elapsed)
            .checked_mul(rate)
            .ok_or(ControllerClockError::ArithmeticOverflow)?
            / PARTS_PER_BILLION;
        let scaled = u64::try_from(scaled).map_err(|_| ControllerClockError::ArithmeticOverflow)?;
        self.controller_epoch
            .checked_add(scaled)
            .ok_or(ControllerClockError::ArithmeticOverflow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerClockError {
    NonPositiveRate { rate_ppb: i64 },
    HostBeforeEpoch { host_tick: Tick, host_epoch: Tick },
    ArithmeticOverflow,
}

impl fmt::Display for ControllerClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonPositiveRate { rate_ppb } => {
                write!(
                    formatter,
                    "controller clock rate {rate_ppb} ppb is not positive"
                )
            }
            Self::HostBeforeEpoch {
                host_tick,
                host_epoch,
            } => write!(
                formatter,
                "host tick {host_tick} is before controller clock epoch {host_epoch}"
            ),
            Self::ArithmeticOverflow => formatter.write_str("controller clock arithmetic overflow"),
        }
    }
}

impl std::error::Error for ControllerClockError {}

/// Deterministic link and endpoint configuration for clock-sync frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockTransportConfig {
    pub request: ClockLinkConfig,
    pub response: ClockLinkConfig,
    /// Physical processing time between controller receive and send events.
    pub processing_ticks: Tick,
    /// Maximum number of complete response frames awaiting host receipt.
    pub max_pending: usize,
    /// PRNG seed; this is part of the deterministic test definition.
    pub seed: u64,
    pub controller_clock: ControllerClock,
}

impl Default for ClockTransportConfig {
    fn default() -> Self {
        Self {
            request: ClockLinkConfig::default(),
            response: ClockLinkConfig::default(),
            processing_ticks: 0,
            max_pending: 8,
            seed: 0,
            controller_clock: ControllerClock::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockTransportConfigError {
    InvalidLossRate { direction: &'static str, value: u16 },
    InvalidDuplicationRate { direction: &'static str, value: u16 },
    ZeroPendingCapacity,
}

impl fmt::Display for ClockTransportConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLossRate { direction, value } => write!(
                formatter,
                "{direction} loss rate {value} per mille exceeds 1000"
            ),
            Self::InvalidDuplicationRate { direction, value } => write!(
                formatter,
                "{direction} duplication rate {value} per mille exceeds 1000"
            ),
            Self::ZeroPendingCapacity => {
                formatter.write_str("clock transport pending capacity must be positive")
            }
        }
    }
}

impl std::error::Error for ClockTransportConfigError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimClockTransportError {
    Decode(DecodeError),
    Encode(EncodeError),
    Clock(ControllerClockError),
    TimeOverflow,
    QueueFull { maximum: usize },
}

impl fmt::Display for SimClockTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(error) => write!(formatter, "invalid clock request frame: {error}"),
            Self::Encode(error) => write!(formatter, "failed to encode clock response: {error}"),
            Self::Clock(error) => write!(formatter, "failed to timestamp clock response: {error}"),
            Self::TimeOverflow => formatter.write_str("clock transport timestamp overflow"),
            Self::QueueFull { maximum } => {
                write!(
                    formatter,
                    "clock response queue is full (maximum {maximum})"
                )
            }
        }
    }
}

impl std::error::Error for SimClockTransportError {}

/// One complete response frame and the host-domain tick when it became due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimClockFrame {
    pub delivered_at: Tick,
    bytes: [u8; CLOCK_RESPONSE_FRAME_LEN],
}

impl SimClockFrame {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScheduledResponse {
    delivered_at: Tick,
    insertion_order: u64,
    bytes: [u8; CLOCK_RESPONSE_FRAME_LEN],
}

/// Deterministic, bounded clock-sync wire endpoint for event-loop tests.
///
/// Requests are validated at handoff. Accepted frames traverse the simulated
/// request link, are timestamped in the controller domain, and return through
/// the simulated response link. The host drives time explicitly by passing its
/// current tick to `send_request` and `receive_due`; no wall clock or I/O is
/// consulted.
#[derive(Debug, Clone)]
pub struct SimClockTransport {
    cfg: ClockTransportConfig,
    rng: u64,
    next_insertion_order: u64,
    pending: VecDeque<ScheduledResponse>,
    link_up: bool,
}

impl SimClockTransport {
    pub fn new(cfg: ClockTransportConfig) -> Result<Self, ClockTransportConfigError> {
        validate_link("request", cfg.request)?;
        validate_link("response", cfg.response)?;
        if cfg.max_pending == 0 {
            return Err(ClockTransportConfigError::ZeroPendingCapacity);
        }
        Ok(Self {
            rng: cfg.seed.wrapping_mul(2).wrapping_add(1),
            cfg,
            next_insertion_order: 0,
            pending: VecDeque::new(),
            link_up: true,
        })
    }

    pub fn config(&self) -> &ClockTransportConfig {
        &self.cfg
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Hand one complete type-3 request frame to the simulated link.
    ///
    /// A down link and configured packet loss are intentionally silent: the
    /// host observes them through its normal exchange timeout.
    pub fn send_request(
        &mut self,
        host_send: Tick,
        frame: &[u8],
    ) -> Result<(), SimClockTransportError> {
        let mut candidate = self.clone();
        candidate.send_request_mut(host_send, frame)?;
        *self = candidate;
        Ok(())
    }

    fn send_request_mut(
        &mut self,
        host_send: Tick,
        frame: &[u8],
    ) -> Result<(), SimClockTransportError> {
        let request = decode_clock_request(frame).map_err(SimClockTransportError::Decode)?;
        if !self.link_up {
            return Ok(());
        }

        let request_link = self.cfg.request;
        let request_arrivals = self.deliveries(host_send, request_link)?;
        let mut scheduled = Vec::with_capacity(4);
        for controller_arrival_host in request_arrivals {
            let controller_send_host = controller_arrival_host
                .checked_add(self.cfg.processing_ticks)
                .ok_or(SimClockTransportError::TimeOverflow)?;
            let response = ClockResponseFrame {
                sequence: request.sequence,
                response: ClockResponse {
                    controller_receive: self
                        .cfg
                        .controller_clock
                        .at(controller_arrival_host)
                        .map_err(SimClockTransportError::Clock)?,
                    controller_send: self
                        .cfg
                        .controller_clock
                        .at(controller_send_host)
                        .map_err(SimClockTransportError::Clock)?,
                },
            };
            let mut bytes = [0; CLOCK_RESPONSE_FRAME_LEN];
            encode_clock_response(&response, &mut bytes).map_err(SimClockTransportError::Encode)?;

            let response_link = self.cfg.response;
            for delivered_at in self.deliveries(controller_send_host, response_link)? {
                scheduled.push(ScheduledResponse {
                    delivered_at,
                    insertion_order: self.next_insertion_order,
                    bytes,
                });
                self.next_insertion_order = self.next_insertion_order.wrapping_add(1);
            }
        }

        if self.pending.len().saturating_add(scheduled.len()) > self.cfg.max_pending {
            return Err(SimClockTransportError::QueueFull {
                maximum: self.cfg.max_pending,
            });
        }
        self.pending.extend(scheduled);
        Ok(())
    }

    /// Return the earliest response whose delivery tick is not later than
    /// `host_now`. Equal-tick responses retain deterministic insertion order.
    pub fn receive_due(&mut self, host_now: Tick) -> Option<SimClockFrame> {
        let index = self
            .pending
            .iter()
            .enumerate()
            .filter(|(_, response)| response.delivered_at <= host_now)
            .min_by_key(|(_, response)| (response.delivered_at, response.insertion_order))
            .map(|(index, _)| index)?;
        let response = self.pending.remove(index)?;
        Some(SimClockFrame {
            delivered_at: response.delivered_at,
            bytes: response.bytes,
        })
    }

    /// Sever the link and discard responses already in flight.
    pub fn drop_link(&mut self) {
        self.link_up = false;
        self.pending.clear();
    }

    pub fn restore_link(&mut self) {
        self.link_up = true;
    }

    fn deliveries(
        &mut self,
        sent_at: Tick,
        link: ClockLinkConfig,
    ) -> Result<Vec<Tick>, SimClockTransportError> {
        if link.loss_per_mille > 0 && self.next_rand() % 1000 < u64::from(link.loss_per_mille) {
            return Ok(Vec::new());
        }
        let jitter = if link.jitter_ticks == 0 {
            0
        } else {
            let choices = link
                .jitter_ticks
                .checked_add(1)
                .ok_or(SimClockTransportError::TimeOverflow)?;
            self.next_rand() % choices
        };
        let delivered_at = sent_at
            .checked_add(link.latency_ticks)
            .and_then(|tick| tick.checked_add(jitter))
            .ok_or(SimClockTransportError::TimeOverflow)?;
        let duplicate =
            link.dup_per_mille > 0 && self.next_rand() % 1000 < u64::from(link.dup_per_mille);
        if duplicate {
            Ok(vec![
                delivered_at,
                delivered_at
                    .checked_add(1)
                    .ok_or(SimClockTransportError::TimeOverflow)?,
            ])
        } else {
            Ok(vec![delivered_at])
        }
    }

    fn next_rand(&mut self) -> u64 {
        self.rng = self
            .rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.rng >> 33
    }
}

fn validate_link(
    direction: &'static str,
    link: ClockLinkConfig,
) -> Result<(), ClockTransportConfigError> {
    if link.loss_per_mille > 1000 {
        return Err(ClockTransportConfigError::InvalidLossRate {
            direction,
            value: link.loss_per_mille,
        });
    }
    if link.dup_per_mille > 1000 {
        return Err(ClockTransportConfigError::InvalidDuplicationRate {
            direction,
            value: link.dup_per_mille,
        });
    }
    Ok(())
}
