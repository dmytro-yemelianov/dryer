//! Generic hardware resource model (spec §10): the vocabulary the resolver
//! uses to match device requirements against board/chip capabilities.
//!
//! Everything here is data. Allocation logic lives in the future
//! `machine-resolver`; keeping the model pure keeps it serializable into
//! lockfiles and `explain` output.

use serde::{Deserialize, Serialize};

/// Stable identity of a concrete resource instance, machine-scoped
/// (e.g. `mainboard.tim1.ch1`, `toolhead.gpio.pa2`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceId(pub String);

/// The initial resource type vocabulary (spec §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    GpioInput,
    GpioOutput,
    GpioAlternate,
    AdcChannel,
    PwmChannel,
    Timer,
    TimerChannel,
    DmaChannel,
    SpiBus,
    I2cBus,
    Uart,
    Can,
    UsbEndpoint,
    InterruptLine,
    MemoryRegion,
    FlashRegion,
    ClockSource,
    PowerOutput,
    VoltageDomain,
    Connector,
}

/// Whether a resource can be shared once claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exclusivity {
    /// One owner; any second claim is a conflict.
    Exclusive,
    /// Multiple owners up to the stated limit (0 = unlimited).
    Shared { max_owners: u32 },
}

/// Hard constraints (spec §10.1). A candidate violating any hard constraint
/// is excluded; preferences never override these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "constraint")]
pub enum Constraint {
    /// Attribute must equal a value (e.g. `pin == PE11`).
    Equality { key: String, value: String },
    /// Attribute must be one of the listed values.
    Membership { key: String, values: Vec<String> },
    /// Electrical: candidate voltage domain must lie in `[min_v, max_v]`.
    VoltageRange { min_v: f64, max_v: f64 },
    /// Electrical: candidate must supply at least `min_a` amperes.
    CurrentAtLeast { min_a: f64 },
    /// Bus/clock frequency window in hertz.
    FrequencyRange { min_hz: f64, max_hz: f64 },
    /// Requires coupling to a specific timer (step generation, PWM pairs).
    TimerCoupling { timer: String },
    /// Requires a DMA route to the named peripheral.
    DmaRoute { peripheral: String },
    /// Claims a whole physical connector (occupancy).
    ConnectorOccupancy { connector: String },
    /// Latency budget in nanoseconds for scheduled operations.
    LatencyBudgetNs { max_ns: u64 },
    /// Memory/flash budget in bytes.
    MemoryBudget { max_bytes: u64 },
    /// Ownership mode this requirement insists on.
    Ownership { mode: Exclusivity },
}

/// Soft preference: influences ranking among candidates that satisfy every
/// hard constraint. Higher weight = more preferred.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preference {
    pub weight: i32,
    pub description: String,
}

/// One resource requirement emitted by a device/component (spec §10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceRequirement {
    /// Which component/role asked for this (for `explain` provenance).
    pub requested_by: String,
    pub kind: ResourceKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<Constraint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferred: Vec<Preference>,
}

/// A concrete resource a board/chip package offers to the resolver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceOffer {
    pub id: ResourceId,
    pub kind: ResourceKind,
    pub exclusivity: Exclusivity,
    /// Open attribute map matched by `Equality`/`Membership` constraints
    /// (e.g. `pin: PE11`, `voltage_domain: logic_3v3`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<(String, String)>,
}

impl ResourceOffer {
    pub fn attribute(&self, key: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Check the constraints this pure data model can decide locally
    /// (attribute and ownership constraints). Electrical/timing/routing
    /// constraints need registry context and are the resolver's job.
    pub fn satisfies_locally(&self, c: &Constraint) -> Option<bool> {
        match c {
            Constraint::Equality { key, value } => Some(self.attribute(key) == Some(value)),
            Constraint::Membership { key, values } => Some(
                self.attribute(key)
                    .map(|v| values.iter().any(|x| x == v))
                    .unwrap_or(false),
            ),
            Constraint::Ownership { mode } => Some(match (mode, &self.exclusivity) {
                (Exclusivity::Exclusive, Exclusivity::Exclusive) => true,
                (Exclusivity::Exclusive, Exclusivity::Shared { .. }) => false,
                (Exclusivity::Shared { .. }, _) => true,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer() -> ResourceOffer {
        ResourceOffer {
            id: ResourceId("mainboard.motor0.step".into()),
            kind: ResourceKind::GpioOutput,
            exclusivity: Exclusivity::Exclusive,
            attributes: vec![
                ("pin".into(), "PE11".into()),
                ("voltage_domain".into(), "logic_3v3".into()),
            ],
        }
    }

    #[test]
    fn attribute_constraints_evaluate_locally() {
        let o = offer();
        assert_eq!(
            o.satisfies_locally(&Constraint::Equality {
                key: "pin".into(),
                value: "PE11".into()
            }),
            Some(true)
        );
        assert_eq!(
            o.satisfies_locally(&Constraint::Membership {
                key: "voltage_domain".into(),
                values: vec!["logic_5v".into()]
            }),
            Some(false)
        );
        // electrical constraints are deferred to the resolver
        assert_eq!(
            o.satisfies_locally(&Constraint::VoltageRange {
                min_v: 3.0,
                max_v: 3.6
            }),
            None
        );
    }

    #[test]
    fn requirement_round_trips_through_json_for_lockfiles() {
        let req = ResourceRequirement {
            requested_by: "x_motor".into(),
            kind: ResourceKind::TimerChannel,
            constraints: vec![Constraint::TimerCoupling {
                timer: "tim1".into(),
            }],
            preferred: vec![Preference {
                weight: 10,
                description: "prefer advanced timer".into(),
            }],
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ResourceRequirement = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }
}
