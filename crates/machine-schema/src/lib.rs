//! Machine Graph v0.1: document types, typed physical quantities, identifier
//! rules, and the shared diagnostic type.
//!
//! This crate defines the *shape* of a machine manifest and the primitive
//! validation rules that belong to the schema itself (identifiers, units,
//! required fields). Cross-document semantics (package resolution, hardware
//! resource allocation) belong to the parser and the future resolver.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// The only Machine Graph API version this crate understands.
pub const API_VERSION: &str = "forge.machine/v0.1";
/// The only document kind this crate understands.
pub const KIND_MACHINE: &str = "Machine";

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// Structured diagnostic, shared by the parser and (later) the resolver.
///
/// v0.1 locates findings by a dotted document `path` plus a best-effort
/// `line`; the spec's richer `SourceSpan`/`related` model (§11.3) arrives
/// with the resolver, where multi-source decisions need it.
///
/// Code conventions (docs/implementation-roadmap.md):
/// `E01xx` parse · `E02xx` structure/required fields · `E03xx` identifiers ·
/// `E04xx` units/quantities · `E05xx` intra-document references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// 1-based column of the located key/item, when span tracking found it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Diagnostic {
    pub fn error(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            severity: Severity::Error,
            message: message.into(),
            path: None,
            line: None,
            column: None,
            suggestions: Vec::new(),
        }
    }

    pub fn warning(code: &str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(code, message)
        }
    }

    pub fn at(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn suggest(mut self, s: impl Into<String>) -> Self {
        self.suggestions.push(s.into());
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(p) = &self.path {
            write!(f, " (at {p}")?;
            if let Some(l) = self.line {
                write!(f, ", line {l}")?;
                if let Some(c) = self.column {
                    write!(f, ":{c}")?;
                }
            }
            write!(f, ")")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// Validate a machine-local identifier (spec §5.3): lowercase ASCII letters,
/// digits, `_` and `-`; must begin with a letter.
pub fn valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

// ---------------------------------------------------------------------------
// Typed quantities
// ---------------------------------------------------------------------------

/// Physical dimension of a quantity. One dimension per incompatible unit
/// family — mixing families is a schema error, never a silent conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    /// canonical: millimetres
    Length,
    /// canonical: millimetres per second
    Velocity,
    /// canonical: millimetres per second squared
    Acceleration,
    /// canonical: seconds
    Time,
    /// canonical: watts
    Power,
    /// canonical: volts
    Voltage,
    /// canonical: amperes
    Current,
    /// canonical: hertz
    Frequency,
    /// canonical: degrees Celsius. Kelvin is deliberately unsupported in
    /// v0.1: its conversion is an offset, not a scale, and admitting it
    /// would break the scale-only unit table below.
    Temperature,
    /// canonical: ohms
    Resistance,
    /// canonical: bytes
    Memory,
}

/// A parsed physical quantity: canonical value + dimension + the source text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quantity {
    /// Value in the dimension's canonical unit.
    pub value: f64,
    pub dimension: Dimension,
    /// The text this was parsed from, preserved for display and round-trips.
    pub raw: String,
}

/// `(unit token, dimension, scale to canonical)`. Scale-only by design; see
/// [`Dimension::Temperature`] for why offset units are excluded.
const UNIT_TABLE: &[(&str, Dimension, f64)] = &[
    ("mm", Dimension::Length, 1.0),
    ("m", Dimension::Length, 1000.0),
    ("um", Dimension::Length, 0.001),
    ("mm/s", Dimension::Velocity, 1.0),
    ("m/s", Dimension::Velocity, 1000.0),
    ("mm/s^2", Dimension::Acceleration, 1.0),
    ("m/s^2", Dimension::Acceleration, 1000.0),
    ("s", Dimension::Time, 1.0),
    ("ms", Dimension::Time, 0.001),
    ("us", Dimension::Time, 0.000_001),
    ("min", Dimension::Time, 60.0),
    ("W", Dimension::Power, 1.0),
    ("kW", Dimension::Power, 1000.0),
    ("V", Dimension::Voltage, 1.0),
    ("mV", Dimension::Voltage, 0.001),
    ("A", Dimension::Current, 1.0),
    ("mA", Dimension::Current, 0.001),
    ("Hz", Dimension::Frequency, 1.0),
    ("kHz", Dimension::Frequency, 1_000.0),
    ("MHz", Dimension::Frequency, 1_000_000.0),
    ("C", Dimension::Temperature, 1.0),
    ("Ohm", Dimension::Resistance, 1.0),
    ("kOhm", Dimension::Resistance, 1_000.0),
    ("B", Dimension::Memory, 1.0),
    ("KiB", Dimension::Memory, 1024.0),
    ("MiB", Dimension::Memory, 1_048_576.0),
];

/// Error from [`Quantity::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuantityError {
    /// No unit token — bare numbers are rejected where a unit is required (§5.4).
    MissingUnit,
    /// The numeric part did not parse or is not finite.
    BadNumber(String),
    UnknownUnit(String),
    /// Parsed fine but is not the dimension the field requires.
    WrongDimension {
        expected: Dimension,
        got: Dimension,
    },
}

impl fmt::Display for QuantityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QuantityError::MissingUnit => {
                f.write_str("quantity requires an explicit unit (e.g. \"300 mm/s\")")
            }
            QuantityError::BadNumber(s) => write!(f, "'{s}' is not a finite number"),
            QuantityError::UnknownUnit(u) => write!(f, "unknown unit '{u}'"),
            QuantityError::WrongDimension { expected, got } => {
                write!(f, "expected a {expected:?} quantity, got {got:?}")
            }
        }
    }
}

impl Quantity {
    /// Parse `"300 mm/s"` / `"4.7 kOhm"` / `"500 ms"` into a typed quantity.
    /// The unit may be separated by whitespace or directly attached.
    pub fn parse(raw: &str) -> Result<Self, QuantityError> {
        let raw_trimmed = raw.trim();
        let split = raw_trimmed
            .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
            .ok_or(QuantityError::MissingUnit)?;
        let (num, unit) = raw_trimmed.split_at(split);
        let unit = unit.trim();
        if num.is_empty() || unit.is_empty() {
            return Err(QuantityError::MissingUnit);
        }
        let value: f64 = num
            .parse()
            .map_err(|_| QuantityError::BadNumber(num.to_string()))?;
        if !value.is_finite() {
            return Err(QuantityError::BadNumber(num.to_string()));
        }
        let (_, dimension, scale) = UNIT_TABLE
            .iter()
            .find(|(u, _, _)| *u == unit)
            .ok_or_else(|| QuantityError::UnknownUnit(unit.to_string()))?;
        Ok(Quantity {
            value: value * scale,
            dimension: *dimension,
            raw: raw_trimmed.to_string(),
        })
    }

    /// Parse and require a specific dimension.
    pub fn parse_as(raw: &str, expected: Dimension) -> Result<Self, QuantityError> {
        let q = Self::parse(raw)?;
        if q.dimension != expected {
            return Err(QuantityError::WrongDimension {
                expected,
                got: q.dimension,
            });
        }
        Ok(q)
    }
}

// ---------------------------------------------------------------------------
// Machine Graph v0.1 document types
// ---------------------------------------------------------------------------

/// A source-graph machine manifest (spec §5.1). This is the *user-authored*
/// stage of the graph lifecycle (§5.5); expansion and resolution stages are
/// separate types owned by the future resolver.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineDoc {
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    /// Package references (`namespace/name@version`); syntax is validated by
    /// the parser via `forge-package-model` to keep this crate leaf-level.
    #[serde(default)]
    pub packages: Vec<String>,
    pub controllers: BTreeMap<String, Controller>,
    pub components: BTreeMap<String, Component>,
    pub kinematics: Kinematics,
    pub safety: Safety,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workflows: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibration: Option<Calibration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Controller {
    /// Board package reference without version (`boards/btt-octopus-pro`);
    /// the version is pinned through `packages` + the lockfile.
    pub board: String,
    pub transport: Transport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transport {
    #[serde(rename = "type")]
    pub kind: String,
    /// For child transports (e.g. CAN): `controller.port`, resolved by the parser.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// A component keeps its `type` plus open attributes: component vocabularies
/// grow with device packages, so the schema stays open here and semantic
/// validation happens against the package registry at resolve time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(flatten)]
    pub attributes: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kinematics {
    #[serde(rename = "type")]
    pub kind: String,
    /// Motion limits as quantity strings (`max_velocity: "300 mm/s"`),
    /// validated to typed quantities by the parser.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub limits: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Safety {
    /// Safety-profile package reference (`safety-profiles/desktop-fdm`).
    pub profile: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Calibration {
    /// Machine-local calibration file; never embedded in packages (§5.6).
    pub source: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_follow_the_spec_rules() {
        for ok in ["x", "x_motor", "hotend-sensor", "can0", "a1-b_c"] {
            assert!(valid_identifier(ok), "{ok} should be valid");
        }
        for bad in ["", "X", "1motor", "_x", "-x", "böard", "x motor", "x.y"] {
            assert!(!valid_identifier(bad), "{bad} should be invalid");
        }
    }

    #[test]
    fn quantities_parse_with_canonical_scaling() {
        let v = Quantity::parse("300 mm/s").unwrap();
        assert_eq!(v.dimension, Dimension::Velocity);
        assert_eq!(v.value, 300.0);

        let v = Quantity::parse("0.3 m/s").unwrap();
        assert!((v.value - 300.0).abs() < 1e-9);

        let t = Quantity::parse("500 ms").unwrap();
        assert_eq!(t.dimension, Dimension::Time);
        assert!((t.value - 0.5).abs() < 1e-12);

        let r = Quantity::parse("4.7 kOhm").unwrap();
        assert_eq!(r.dimension, Dimension::Resistance);
        assert!((r.value - 4700.0).abs() < 1e-9);

        // attached unit, no whitespace
        let f = Quantity::parse("8MHz").unwrap();
        assert_eq!(f.dimension, Dimension::Frequency);
        assert_eq!(f.value, 8_000_000.0);
    }

    #[test]
    fn bare_numbers_and_unknown_units_are_rejected() {
        assert_eq!(Quantity::parse("300"), Err(QuantityError::MissingUnit));
        assert!(matches!(
            Quantity::parse("300 furlongs"),
            Err(QuantityError::UnknownUnit(_))
        ));
        // 'nan' never reaches float parsing: the splitter finds no numeric
        // prefix, so this rejects as a missing number/unit rather than
        // producing a NaN quantity.
        assert!(Quantity::parse("nan mm").is_err());
        // A finite-looking literal that overflows f64 to infinity is the
        // live BadNumber case.
        let huge = format!("1{} mm", "0".repeat(400));
        assert!(matches!(
            Quantity::parse(&huge),
            Err(QuantityError::BadNumber(_))
        ));
        // Kelvin is deliberately unsupported (offset unit).
        assert!(matches!(
            Quantity::parse("300 K"),
            Err(QuantityError::UnknownUnit(_))
        ));
    }

    #[test]
    fn dimension_mixing_is_an_error_not_a_conversion() {
        let err = Quantity::parse_as("300 mm/s", Dimension::Acceleration).unwrap_err();
        assert_eq!(
            err,
            QuantityError::WrongDimension {
                expected: Dimension::Acceleration,
                got: Dimension::Velocity,
            }
        );
    }

    #[test]
    fn machine_doc_round_trips_through_yaml() {
        let yaml = r#"
api_version: forge.machine/v0.1
kind: Machine
metadata:
  name: test
controllers:
  mainboard:
    board: boards/example
    transport:
      type: usb
components:
  x_motor:
    type: stepper_motor
    role: axis.x
kinematics:
  type: cartesian
  limits:
    max_velocity: 300 mm/s
safety:
  profile: safety-profiles/desktop-fdm
"#;
        let doc: MachineDoc = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(doc.api_version, API_VERSION);
        let back = serde_yaml::to_string(&doc).unwrap();
        let doc2: MachineDoc = serde_yaml::from_str(&back).unwrap();
        assert_eq!(doc2.components["x_motor"].kind, "stepper_motor");
        assert_eq!(
            doc2.components["x_motor"].attributes["role"],
            serde_yaml::Value::String("axis.x".into())
        );
    }
}
