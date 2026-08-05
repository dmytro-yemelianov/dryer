#![no_std]
#![forbid(unsafe_code)]

//! Allocation-free host/controller clock estimation.
//!
//! A sample contains the four timestamps from one synchronization exchange.
//! Host and controller ticks remain distinct clock domains, but both domains
//! must already be normalized to the same tick duration and resolution (one
//! microsecond in the current system). This crate estimates offset and drift;
//! it does not perform unit conversion.
//!
//! The estimator keeps a conservative offset interval, derives signed drift
//! bounds from consecutive samples, combines them with an explicit maximum
//! slew bound, and projects those bounds using checked integer arithmetic only.

use core::fmt;

const PARTS_PER_BILLION: i128 = 1_000_000_000;
const MIDPOINT_SLEW_DENOMINATOR: i128 = 2_000_000_000;

/// A monotonic tick in the host clock domain, normalized to the shared tick
/// duration (currently one microsecond).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostTick(pub u64);

/// A monotonic tick in one controller's local clock domain, normalized to the
/// same tick duration as [`HostTick`] (currently one microsecond).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ControllerTick(pub u64);

/// Four timestamps captured by one round-trip synchronization exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockSample {
    pub host_send: HostTick,
    pub controller_receive: ControllerTick,
    pub controller_send: ControllerTick,
    pub host_receive: HostTick,
}

/// An offset interval established by one sample.
///
/// Offsets are `controller - host`. `offset_ticks` is the floor midpoint of
/// the inclusive interval bounded by `offset_min_ticks` and
/// `offset_max_ticks`. [`observe`] returns the nominal raw interval;
/// [`ClockSync::push`] widens it using the session's configured slew bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OffsetObservation {
    pub reference_host: HostTick,
    pub offset_ticks: i128,
    pub offset_min_ticks: i128,
    pub offset_max_ticks: i128,
    /// Timing span used for this interval. Standalone [`observe`] reports the
    /// nominal processing-excluded residual; bounded [`ClockSync`] estimates
    /// report the physically measured host exchange span instead.
    pub round_trip_ticks: u64,
}

/// Signed controller-clock drift relative to the host, in parts per billion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriftEstimate {
    pub ppb: i128,
    pub min_ppb: i128,
    pub max_ppb: i128,
    pub baseline_ticks: u64,
}

/// Latest offset anchor and drift derived from two consecutive observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockEstimate {
    pub anchor: OffsetObservation,
    pub drift: DriftEstimate,
    pub sampled_at: HostTick,
}

/// Conservative projection of a host tick into controller-local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerWindow {
    pub earliest: ControllerTick,
    pub estimate: ControllerTick,
    pub latest: ControllerTick,
}

/// Deterministic validation or arithmetic failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockSyncError {
    InvalidSlewBound,
    HostRegressed,
    ControllerRegressed,
    ControllerProcessingExceedsRoundTrip,
    InsufficientSeparation,
    ArithmeticOverflow,
    EstimateUnavailable,
    HostTickBeforeSession,
}

impl fmt::Display for ClockSyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidSlewBound => "maximum clock slew must be nonnegative",
            Self::HostRegressed => "host clock regressed",
            Self::ControllerRegressed => "controller clock regressed",
            Self::ControllerProcessingExceedsRoundTrip => {
                "controller timing is inconsistent with the host exchange and slew bound"
            }
            Self::InsufficientSeparation => "clock samples have no host-time separation",
            Self::ArithmeticOverflow => "clock synchronization arithmetic overflowed",
            Self::EstimateUnavailable => "clock estimate is unavailable",
            Self::HostTickBeforeSession => "host tick predates the clock-sync session",
        };
        formatter.write_str(message)
    }
}

/// Validate a four-timestamp sample and derive its nominal NTP-style offset
/// interval.
///
/// This standalone result does not account for relative clock slew during the
/// exchange. Scheduling code must use [`ClockSync`], which widens the interval
/// by its explicit bound before estimating or projecting it.
pub fn observe(sample: ClockSample) -> Result<OffsetObservation, ClockSyncError> {
    let host_elapsed = sample
        .host_receive
        .0
        .checked_sub(sample.host_send.0)
        .ok_or(ClockSyncError::HostRegressed)?;
    let controller_processing = sample
        .controller_send
        .0
        .checked_sub(sample.controller_receive.0)
        .ok_or(ClockSyncError::ControllerRegressed)?;
    let round_trip_ticks = host_elapsed
        .checked_sub(controller_processing)
        .ok_or(ClockSyncError::ControllerProcessingExceedsRoundTrip)?;

    let low = i128::from(sample.controller_send.0)
        .checked_sub(i128::from(sample.host_receive.0))
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let high = i128::from(sample.controller_receive.0)
        .checked_sub(i128::from(sample.host_send.0))
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let width = high
        .checked_sub(low)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let midpoint = low
        .checked_add(width / 2)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let reference_host = sample
        .host_send
        .0
        .checked_add(host_elapsed / 2)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;

    Ok(OffsetObservation {
        reference_host: HostTick(reference_host),
        offset_ticks: midpoint,
        offset_min_ticks: low,
        offset_max_ticks: high,
        round_trip_ticks,
    })
}

/// O(1), allocation-free state for one host/controller clock session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockSync {
    max_slew_ppb: i128,
    session_start: Option<HostTick>,
    previous: Option<OffsetObservation>,
    estimate: Option<ClockEstimate>,
    last_raw: Option<ClockSample>,
}

impl ClockSync {
    /// Start a session with an explicit maximum absolute relative clock slew,
    /// in parts per billion.
    pub const fn new(max_slew_ppb: i128) -> Result<Self, ClockSyncError> {
        if max_slew_ppb < 0 {
            return Err(ClockSyncError::InvalidSlewBound);
        }
        Ok(Self {
            max_slew_ppb,
            session_start: None,
            previous: None,
            estimate: None,
            last_raw: None,
        })
    }

    /// Add one sample. The first accepted sample establishes an anchor; every
    /// subsequent accepted sample returns a drift-bearing estimate.
    ///
    /// Bounded ingestion uses the host-observed exchange span to widen the
    /// midpoint interval. It does not subtract a controller-domain processing
    /// duration from a host-domain duration.
    ///
    /// All validation and arithmetic complete before estimator state changes.
    pub fn push(&mut self, sample: ClockSample) -> Result<Option<&ClockEstimate>, ClockSyncError> {
        let observation = observe_bounded(sample, self.max_slew_ppb)?;

        if let Some(last) = self.last_raw {
            if sample.host_send < last.host_send || sample.host_receive < last.host_receive {
                return Err(ClockSyncError::HostRegressed);
            }
            if sample.controller_receive < last.controller_receive
                || sample.controller_send < last.controller_send
            {
                return Err(ClockSyncError::ControllerRegressed);
            }
        }

        let next_estimate = match self.previous {
            Some(previous) => {
                if observation.reference_host <= previous.reference_host {
                    return Err(ClockSyncError::InsufficientSeparation);
                }
                Some(ClockEstimate {
                    anchor: observation,
                    drift: estimate_drift(previous, observation)?,
                    sampled_at: sample.host_receive,
                })
            }
            None => None,
        };

        if self.session_start.is_none() {
            self.session_start = Some(observation.reference_host);
        }
        self.previous = Some(observation);
        self.last_raw = Some(sample);
        if let Some(estimate) = next_estimate {
            self.estimate = Some(estimate);
        }

        Ok(self.estimate.as_ref())
    }

    pub fn estimate(&self) -> Option<&ClockEstimate> {
        self.estimate.as_ref()
    }

    /// Project a same-duration host tick into the controller clock domain with
    /// conservative offset and drift bounds. This estimates a clock mapping;
    /// it does not convert between tick units.
    pub fn controller_window(&self, host: HostTick) -> Result<ControllerWindow, ClockSyncError> {
        let (low, midpoint, high) = self.project(host)?;
        Ok(ControllerWindow {
            earliest: ControllerTick(to_u64(low)?),
            estimate: ControllerTick(to_u64(midpoint)?),
            latest: ControllerTick(to_u64(high)?),
        })
    }

    /// Return the ceiling half-width of the projected controller window.
    pub fn uncertainty_at(&self, host: HostTick) -> Result<u64, ClockSyncError> {
        let (low, midpoint, high) = self.project(host)?;
        // Confidence is meaningful only when the entire projected controller
        // window is representable in the public u64 tick domain.
        to_u64(low)?;
        to_u64(midpoint)?;
        to_u64(high)?;
        let below = midpoint
            .checked_sub(low)
            .ok_or(ClockSyncError::ArithmeticOverflow)?;
        let above = high
            .checked_sub(midpoint)
            .ok_or(ClockSyncError::ArithmeticOverflow)?;
        to_u64(below.max(above))
    }

    /// Whether a synchronization exchange is needed at `host_now`.
    ///
    /// A session without a complete estimate is always due. A tick before the
    /// latest completed sample is not considered elapsed time.
    pub fn resync_due(&self, host_now: HostTick, interval_ticks: u64) -> bool {
        let Some(estimate) = self.estimate else {
            return true;
        };
        host_now
            .0
            .checked_sub(estimate.sampled_at.0)
            .is_some_and(|elapsed| elapsed >= interval_ticks)
    }

    fn project(&self, host: HostTick) -> Result<(i128, i128, i128), ClockSyncError> {
        let estimate = self
            .estimate
            .as_ref()
            .ok_or(ClockSyncError::EstimateUnavailable)?;
        let session_start = self
            .session_start
            .ok_or(ClockSyncError::EstimateUnavailable)?;
        if host < session_start {
            return Err(ClockSyncError::HostTickBeforeSession);
        }
        let elapsed = i128::from(host.0)
            .checked_sub(i128::from(estimate.anchor.reference_host.0))
            .ok_or(ClockSyncError::ArithmeticOverflow)?;
        let physical_min_ppb = self
            .max_slew_ppb
            .checked_neg()
            .ok_or(ClockSyncError::ArithmeticOverflow)?;
        let minimum_ppb = estimate.drift.min_ppb.min(physical_min_ppb);
        let maximum_ppb = estimate.drift.max_ppb.max(self.max_slew_ppb);
        let (low_ppb, high_ppb) = if elapsed < 0 {
            (maximum_ppb, minimum_ppb)
        } else {
            (minimum_ppb, maximum_ppb)
        };

        let low_drift = div_floor(
            elapsed
                .checked_mul(low_ppb)
                .ok_or(ClockSyncError::ArithmeticOverflow)?,
            PARTS_PER_BILLION,
        )?;
        let high_drift = div_ceil(
            elapsed
                .checked_mul(high_ppb)
                .ok_or(ClockSyncError::ArithmeticOverflow)?,
            PARTS_PER_BILLION,
        )?;
        let offset_low = estimate
            .anchor
            .offset_min_ticks
            .checked_add(low_drift)
            .ok_or(ClockSyncError::ArithmeticOverflow)?;
        let offset_high = estimate
            .anchor
            .offset_max_ticks
            .checked_add(high_drift)
            .ok_or(ClockSyncError::ArithmeticOverflow)?;
        let host = i128::from(host.0);
        let controller_low = host
            .checked_add(offset_low)
            .ok_or(ClockSyncError::ArithmeticOverflow)?;
        let controller_high = host
            .checked_add(offset_high)
            .ok_or(ClockSyncError::ArithmeticOverflow)?;
        let width = controller_high
            .checked_sub(controller_low)
            .ok_or(ClockSyncError::ArithmeticOverflow)?;
        let midpoint = controller_low
            .checked_add(width / 2)
            .ok_or(ClockSyncError::ArithmeticOverflow)?;
        Ok((controller_low, midpoint, controller_high))
    }
}

fn observe_bounded(
    sample: ClockSample,
    max_slew_ppb: i128,
) -> Result<OffsetObservation, ClockSyncError> {
    let host_elapsed = sample
        .host_receive
        .0
        .checked_sub(sample.host_send.0)
        .ok_or(ClockSyncError::HostRegressed)?;
    if sample.controller_send < sample.controller_receive {
        return Err(ClockSyncError::ControllerRegressed);
    }

    let allowance = div_ceil(
        i128::from(host_elapsed)
            .checked_mul(max_slew_ppb)
            .ok_or(ClockSyncError::ArithmeticOverflow)?,
        MIDPOINT_SLEW_DENOMINATOR,
    )?;
    let nominal_low = i128::from(sample.controller_send.0)
        .checked_sub(i128::from(sample.host_receive.0))
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let nominal_high = i128::from(sample.controller_receive.0)
        .checked_sub(i128::from(sample.host_send.0))
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let low = nominal_low
        .checked_sub(allowance)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let high = nominal_high
        .checked_add(allowance)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let width = high
        .checked_sub(low)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    if width < 0 {
        return Err(ClockSyncError::ControllerProcessingExceedsRoundTrip);
    }
    let midpoint = low
        .checked_add(width / 2)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let reference_host = sample
        .host_send
        .0
        .checked_add(host_elapsed / 2)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    Ok(OffsetObservation {
        reference_host: HostTick(reference_host),
        offset_ticks: midpoint,
        offset_min_ticks: low,
        offset_max_ticks: high,
        round_trip_ticks: host_elapsed,
    })
}

fn estimate_drift(
    previous: OffsetObservation,
    current: OffsetObservation,
) -> Result<DriftEstimate, ClockSyncError> {
    let baseline_ticks = current
        .reference_host
        .0
        .checked_sub(previous.reference_host.0)
        .filter(|baseline| *baseline != 0)
        .ok_or(ClockSyncError::InsufficientSeparation)?;
    let denominator = i128::from(baseline_ticks);

    let point_delta = current
        .offset_ticks
        .checked_sub(previous.offset_ticks)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let minimum_delta = current
        .offset_min_ticks
        .checked_sub(previous.offset_max_ticks)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let maximum_delta = current
        .offset_max_ticks
        .checked_sub(previous.offset_min_ticks)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;

    Ok(DriftEstimate {
        ppb: div_round_nearest_ties_away(
            point_delta
                .checked_mul(PARTS_PER_BILLION)
                .ok_or(ClockSyncError::ArithmeticOverflow)?,
            denominator,
        )?,
        min_ppb: div_floor(
            minimum_delta
                .checked_mul(PARTS_PER_BILLION)
                .ok_or(ClockSyncError::ArithmeticOverflow)?,
            denominator,
        )?,
        max_ppb: div_ceil(
            maximum_delta
                .checked_mul(PARTS_PER_BILLION)
                .ok_or(ClockSyncError::ArithmeticOverflow)?,
            denominator,
        )?,
        baseline_ticks,
    })
}

fn div_floor(numerator: i128, denominator: i128) -> Result<i128, ClockSyncError> {
    let quotient = numerator
        .checked_div(denominator)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let remainder = numerator
        .checked_rem(denominator)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    if remainder < 0 {
        quotient
            .checked_sub(1)
            .ok_or(ClockSyncError::ArithmeticOverflow)
    } else {
        Ok(quotient)
    }
}

fn div_ceil(numerator: i128, denominator: i128) -> Result<i128, ClockSyncError> {
    let quotient = numerator
        .checked_div(denominator)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let remainder = numerator
        .checked_rem(denominator)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    if remainder > 0 {
        quotient
            .checked_add(1)
            .ok_or(ClockSyncError::ArithmeticOverflow)
    } else {
        Ok(quotient)
    }
}

fn div_round_nearest_ties_away(numerator: i128, denominator: i128) -> Result<i128, ClockSyncError> {
    let quotient = numerator
        .checked_div(denominator)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let remainder = numerator
        .checked_rem(denominator)
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    let magnitude = remainder
        .checked_abs()
        .and_then(|value| value.checked_mul(2))
        .ok_or(ClockSyncError::ArithmeticOverflow)?;
    if magnitude < denominator {
        return Ok(quotient);
    }
    if numerator < 0 {
        quotient
            .checked_sub(1)
            .ok_or(ClockSyncError::ArithmeticOverflow)
    } else {
        quotient
            .checked_add(1)
            .ok_or(ClockSyncError::ArithmeticOverflow)
    }
}

fn to_u64(value: i128) -> Result<u64, ClockSyncError> {
    u64::try_from(value).map_err(|_| ClockSyncError::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t0: u64, c1: u64, c2: u64, t3: u64) -> ClockSample {
        ClockSample {
            host_send: HostTick(t0),
            controller_receive: ControllerTick(c1),
            controller_send: ControllerTick(c2),
            host_receive: HostTick(t3),
        }
    }

    #[test]
    fn observes_zero_and_signed_offsets() {
        let zero = observe(sample(100, 100, 110, 110)).unwrap();
        assert_eq!(zero.reference_host, HostTick(105));
        assert_eq!(zero.offset_ticks, 0);
        assert_eq!(zero.offset_min_ticks, 0);
        assert_eq!(zero.offset_max_ticks, 0);
        assert_eq!(zero.round_trip_ticks, 0);

        let positive = observe(sample(100, 160, 180, 140)).unwrap();
        assert_eq!(positive.offset_ticks, 50);
        assert_eq!(
            (positive.offset_min_ticks, positive.offset_max_ticks),
            (40, 60)
        );

        let negative = observe(sample(100, 80, 100, 140)).unwrap();
        assert_eq!(negative.offset_ticks, -30);
        assert_eq!(
            (negative.offset_min_ticks, negative.offset_max_ticks),
            (-40, -20)
        );
    }

    #[test]
    fn asymmetric_path_preserves_the_true_offset_inside_bounds() {
        let observation = observe(sample(100, 155, 175, 140)).unwrap();
        assert_eq!(observation.offset_ticks, 45);
        assert_eq!(observation.offset_min_ticks, 35);
        assert_eq!(observation.offset_max_ticks, 55);
        assert!(observation.offset_min_ticks <= 50 && 50 <= observation.offset_max_ticks);
    }

    #[test]
    fn controller_processing_is_excluded_from_round_trip() {
        let observation = observe(sample(100, 110, 140, 140)).unwrap();
        assert_eq!(observation.round_trip_ticks, 10);
        assert_eq!(
            (observation.offset_min_ticks, observation.offset_max_ticks),
            (0, 10)
        );
    }

    #[test]
    fn odd_interval_width_uses_floor_midpoint_and_ceiling_uncertainty() {
        let observation = observe(sample(0, 2, 2, 5)).unwrap();
        assert_eq!(observation.offset_ticks, -1);
        assert_eq!(
            (observation.offset_min_ticks, observation.offset_max_ticks),
            (-3, 2)
        );
        let uncertainty = (observation.offset_ticks - observation.offset_min_ticks)
            .max(observation.offset_max_ticks - observation.offset_ticks);
        assert_eq!(uncertainty, 3);
    }

    #[test]
    fn invalid_within_sample_ordering_is_rejected() {
        assert_eq!(
            observe(sample(10, 10, 10, 9)),
            Err(ClockSyncError::HostRegressed)
        );
        assert_eq!(
            observe(sample(0, 11, 10, 20)),
            Err(ClockSyncError::ControllerRegressed)
        );
        assert_eq!(
            observe(sample(0, 0, 11, 10)),
            Err(ClockSyncError::ControllerProcessingExceedsRoundTrip)
        );
    }

    #[test]
    fn observations_accept_both_u64_offset_endpoints_without_wrapping() {
        let positive = observe(sample(0, u64::MAX, u64::MAX, 0)).unwrap();
        assert_eq!(positive.offset_ticks, i128::from(u64::MAX));
        assert_eq!(positive.offset_min_ticks, i128::from(u64::MAX));
        assert_eq!(positive.offset_max_ticks, i128::from(u64::MAX));

        let negative = observe(sample(u64::MAX, 0, 0, u64::MAX)).unwrap();
        assert_eq!(negative.offset_ticks, -i128::from(u64::MAX));
        assert_eq!(negative.offset_min_ticks, -i128::from(u64::MAX));
        assert_eq!(negative.offset_max_ticks, -i128::from(u64::MAX));
    }

    #[test]
    fn negative_slew_bound_is_rejected() {
        assert_eq!(ClockSync::new(-1), Err(ClockSyncError::InvalidSlewBound));
        assert_eq!(ClockSync::new(0).unwrap().max_slew_ppb, 0);
    }

    #[test]
    fn first_sample_has_no_drift_or_projection() {
        let mut sync = ClockSync::new(0).unwrap();
        assert_eq!(sync.push(sample(10, 10, 10, 10)).unwrap(), None);
        assert_eq!(sync.estimate(), None);
        assert_eq!(
            sync.controller_window(HostTick(10)),
            Err(ClockSyncError::EstimateUnavailable)
        );
        assert_eq!(
            sync.uncertainty_at(HostTick(10)),
            Err(ClockSyncError::EstimateUnavailable)
        );
        assert!(sync.resync_due(HostTick(10), 100));
    }

    #[test]
    fn estimates_known_positive_and_negative_drift() {
        let mut positive = ClockSync::new(0).unwrap();
        positive.push(sample(1_000, 1_100, 1_100, 1_000)).unwrap();
        positive
            .push(sample(1_001_000, 1_001_200, 1_001_200, 1_001_000))
            .unwrap();
        assert_eq!(
            positive.estimate().unwrap().drift,
            DriftEstimate {
                ppb: 100_000,
                min_ppb: 100_000,
                max_ppb: 100_000,
                baseline_ticks: 1_000_000,
            }
        );

        let mut negative = ClockSync::new(0).unwrap();
        negative.push(sample(1_000, 1_100, 1_100, 1_000)).unwrap();
        negative
            .push(sample(1_001_000, 1_001_025, 1_001_025, 1_001_000))
            .unwrap();
        assert_eq!(negative.estimate().unwrap().drift.ppb, -75_000);
        assert_eq!(negative.estimate().unwrap().drift.min_ppb, -75_000);
        assert_eq!(negative.estimate().unwrap().drift.max_ppb, -75_000);
    }

    #[test]
    fn signed_drift_bounds_round_outward() {
        let previous = observe(sample(0, 1, 1, 2)).unwrap();
        let current = observe(sample(3, 4, 4, 5)).unwrap();
        let drift = estimate_drift(previous, current).unwrap();
        assert_eq!(drift.baseline_ticks, 3);
        assert_eq!(drift.ppb, 0);
        assert_eq!(drift.min_ppb, -666_666_667);
        assert_eq!(drift.max_ppb, 666_666_667);

        assert_eq!(div_floor(-2_000_000_000, 3).unwrap(), -666_666_667);
        assert_eq!(div_ceil(2_000_000_000, 3).unwrap(), 666_666_667);
        assert_eq!(div_round_nearest_ties_away(1, 2).unwrap(), 1);
        assert_eq!(div_round_nearest_ties_away(-1, 2).unwrap(), -1);
    }

    #[test]
    fn uncertainty_grows_under_drift_bounds() {
        let mut sync = ClockSync::new(0).unwrap();
        sync.push(sample(0, 1, 1, 2)).unwrap();
        sync.push(sample(100, 101, 101, 102)).unwrap();

        assert_eq!(sync.uncertainty_at(HostTick(101)).unwrap(), 1);
        assert_eq!(sync.uncertainty_at(HostTick(151)).unwrap(), 2);
        assert_eq!(
            sync.controller_window(HostTick(151)).unwrap(),
            ControllerWindow {
                earliest: ControllerTick(149),
                estimate: ControllerTick(151),
                latest: ControllerTick(153),
            }
        );
    }

    #[test]
    fn bounded_slew_contains_an_affine_controller_clock() {
        const MAX_SLEW_PPB: i128 = 100_000_000;
        let mut sync = ClockSync::new(MAX_SLEW_PPB).unwrap();

        sync.push(sample(0, 0, 0, 100)).unwrap();
        let estimate = *sync
            .push(sample(1_000, 1_100, 1_100, 1_100))
            .unwrap()
            .unwrap();

        // C = 1.1 H. The raw second-sample interval [0, 100] misses
        // its offset 105 at the host midpoint; the 5-tick half-exchange
        // slew allowance expands the accepted anchor to [-5, 105].
        assert_eq!(estimate.anchor.reference_host, HostTick(1_050));
        assert_eq!(estimate.anchor.offset_min_ticks, -5);
        assert_eq!(estimate.anchor.offset_max_ticks, 105);
        let at_anchor = sync.controller_window(HostTick(1_050)).unwrap();
        assert!(at_anchor.earliest.0 <= 1_155 && 1_155 <= at_anchor.latest.0);

        let future = sync.controller_window(HostTick(1_150)).unwrap();
        assert!(future.earliest.0 <= 1_265 && 1_265 <= future.latest.0);
        let past = sync.controller_window(HostTick(1_000)).unwrap();
        assert!(past.earliest.0 <= 1_100 && 1_100 <= past.latest.0);
    }

    #[test]
    fn bounded_ingestion_accepts_processing_measured_in_a_faster_clock() {
        const MAX_SLEW_PPB: i128 = 100_000_000;

        // C = 1.1 H, with 100 host ticks of return-path delay after a long
        // controller processing interval. The nominal cross-domain
        // subtraction gives a zero residual, but host-span widening produces
        // [45, 155] at H=1550 and contains C-H=155.
        let mut delayed = ClockSync::new(MAX_SLEW_PPB).unwrap();
        delayed.push(sample(0, 0, 0, 100)).unwrap();
        let delayed_estimate = *delayed
            .push(sample(1_000, 1_100, 2_200, 2_100))
            .unwrap()
            .unwrap();
        assert_eq!(delayed_estimate.anchor.reference_host, HostTick(1_550));
        assert_eq!(delayed_estimate.anchor.offset_min_ticks, 45);
        assert_eq!(delayed_estimate.anchor.offset_max_ticks, 155);
        assert_eq!(delayed_estimate.anchor.round_trip_ticks, 1_100);
        let delayed_window = delayed.controller_window(HostTick(1_550)).unwrap();
        assert!(delayed_window.earliest.0 <= 1_705 && 1_705 <= delayed_window.latest.0);

        // With no network delay, controller processing advances 1100 local
        // ticks while the host advances only 1000. This is valid at the
        // configured slew even though c2-c1 > t3-t0.
        let mut local = ClockSync::new(MAX_SLEW_PPB).unwrap();
        local.push(sample(0, 0, 0, 100)).unwrap();
        let local_estimate = *local
            .push(sample(1_000, 1_100, 2_200, 2_000))
            .unwrap()
            .unwrap();
        assert_eq!(local_estimate.anchor.reference_host, HostTick(1_500));
        assert_eq!(local_estimate.anchor.offset_min_ticks, 150);
        assert_eq!(local_estimate.anchor.offset_max_ticks, 150);
        assert_eq!(local_estimate.anchor.round_trip_ticks, 1_000);
        assert_eq!(
            local.controller_window(HostTick(1_500)).unwrap(),
            ControllerWindow {
                earliest: ControllerTick(1_650),
                estimate: ControllerTick(1_650),
                latest: ControllerTick(1_650),
            }
        );
    }

    #[test]
    fn past_projection_is_allowed_within_but_not_before_the_session() {
        let mut sync = ClockSync::new(0).unwrap();
        sync.push(sample(10, 10, 10, 10)).unwrap();
        sync.push(sample(20, 20, 20, 20)).unwrap();
        assert_eq!(
            sync.controller_window(HostTick(19)).unwrap(),
            ControllerWindow {
                earliest: ControllerTick(19),
                estimate: ControllerTick(19),
                latest: ControllerTick(19),
            }
        );
        assert_eq!(
            sync.controller_window(HostTick(9)),
            Err(ClockSyncError::HostTickBeforeSession)
        );
    }

    #[test]
    fn resync_is_due_at_the_exact_interval_boundary() {
        let mut sync = ClockSync::new(0).unwrap();
        sync.push(sample(10, 10, 10, 10)).unwrap();
        sync.push(sample(20, 20, 20, 20)).unwrap();
        assert!(!sync.resync_due(HostTick(119), 100));
        assert!(sync.resync_due(HostTick(120), 100));
        assert!(!sync.resync_due(HostTick(19), 0));
    }

    #[test]
    fn cross_sample_regressions_and_zero_separation_do_not_mutate_state() {
        let mut sync = ClockSync::new(0).unwrap();
        sync.push(sample(10, 10, 10, 10)).unwrap();
        sync.push(sample(20, 20, 20, 20)).unwrap();
        let before = sync;

        assert_eq!(
            sync.push(sample(19, 21, 21, 21)),
            Err(ClockSyncError::HostRegressed)
        );
        assert_eq!(sync, before);
        assert_eq!(
            sync.push(sample(21, 19, 19, 21)),
            Err(ClockSyncError::ControllerRegressed)
        );
        assert_eq!(sync, before);
        assert_eq!(
            sync.push(sample(20, 20, 20, 20)),
            Err(ClockSyncError::InsufficientSeparation)
        );
        assert_eq!(sync, before);
        assert_eq!(
            sync.push(sample(21, 21, 32, 31)),
            Err(ClockSyncError::ControllerProcessingExceedsRoundTrip)
        );
        assert_eq!(sync, before);
    }

    #[test]
    fn projected_controller_endpoints_are_checked() {
        let mut positive = ClockSync::new(0).unwrap();
        positive.push(sample(10, 11, 11, 10)).unwrap();
        positive.push(sample(20, 21, 21, 20)).unwrap();
        assert_eq!(
            positive.controller_window(HostTick(u64::MAX)),
            Err(ClockSyncError::ArithmeticOverflow)
        );
        assert_eq!(
            positive.uncertainty_at(HostTick(u64::MAX)),
            Err(ClockSyncError::ArithmeticOverflow)
        );

        let mut negative = ClockSync::new(0).unwrap();
        negative.push(sample(0, 0, 0, 2)).unwrap();
        negative.push(sample(2, 0, 0, 4)).unwrap();
        assert_eq!(
            negative.controller_window(HostTick(3)),
            Err(ClockSyncError::ArithmeticOverflow)
        );
        assert_eq!(
            negative.uncertainty_at(HostTick(3)),
            Err(ClockSyncError::ArithmeticOverflow)
        );
    }

    #[test]
    fn projection_multiplication_overflow_is_reported() {
        let mut sync = ClockSync::new(0).unwrap();
        sync.push(sample(0, 0, 0, 0)).unwrap();
        sync.push(sample(1, u64::MAX, u64::MAX, 1)).unwrap();
        assert_eq!(
            sync.controller_window(HostTick(u64::MAX)),
            Err(ClockSyncError::ArithmeticOverflow)
        );
    }
}
