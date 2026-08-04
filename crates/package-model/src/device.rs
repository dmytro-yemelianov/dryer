//! Device package payload (spec §9): a reusable component's declared class
//! and requirements. Connector, voltage-domain, bus-frequency, DMA-routing,
//! and latency/jitter requirements are hard inputs to capability matching.

use dryer_machine_schema::{valid_identifier, Diagnostic, Dimension, Quantity};
use serde::Deserialize;
use std::collections::BTreeSet;

/// Full `package.yaml` view for a `kind: device` package.
/// Unknown fields are ignored (forward-compatible payloads).
#[derive(Debug, Clone, Deserialize)]
pub struct DevicePackageFile {
    pub package: crate::PackageIdentity,
    #[serde(default)]
    pub device: Option<DeviceInfo>,
    #[serde(default)]
    pub requires: Option<Requires>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceInfo {
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Requires {
    /// Connector kind this device occupies (`stepper_driver_socket`).
    /// A device package must not hardcode board pins (§9) — a *kind* is
    /// the most specific placement requirement it may state.
    #[serde(default)]
    pub connector: Option<String>,
    /// Acceptable connector voltage domains (§10.1 membership constraint).
    /// Empty = no requirement. A connector with NO declared domain does
    /// not satisfy a non-empty list — electrical compatibility is never
    /// assumed from silence.
    #[serde(default)]
    pub voltage_domains: Vec<String>,
    /// Bus requirement (§9 `requires.bus`): the connector's pins must
    /// carry functions of this bus kind, at at least this frequency.
    #[serde(default)]
    pub bus: Option<BusReq>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BusReq {
    /// Bus family (`spi`, `i2c`, `uart`) — matched against capability
    /// token prefixes (`spi1.sck` ⇒ family `spi`, instance `spi1`).
    pub kind: String,
    /// Quantity string (`1 MHz`). When set, the chip's bus instance must
    /// declare a `max_frequency` at least this high — an undeclared
    /// frequency does not satisfy a requirement.
    #[serde(default)]
    pub min_frequency: Option<String>,
    /// Peripheral signal names that require an explicit DMA route (`rx`,
    /// `tx`). Empty means the device does not require DMA.
    #[serde(default)]
    pub dma_signals: Vec<String>,
    /// Maximum acceptable measured worst-case bus latency (`50 us`).
    #[serde(default)]
    pub max_latency: Option<String>,
    /// Maximum acceptable measured worst-case jitter (`10 us`).
    #[serde(default)]
    pub max_jitter: Option<String>,
}

impl crate::LoadedPackage {
    /// Parse this package's device payload (`E062x` diagnostics).
    pub fn device_payload(&self) -> Result<DevicePackageFile, Vec<Diagnostic>> {
        if self.kind != crate::PackageKind::Device {
            return Err(vec![Diagnostic::error(
                "E0620",
                format!(
                    "{} is a {:?} package, not a device",
                    self.reference, self.kind
                ),
            )]);
        }
        let text = std::fs::read_to_string(self.dir.join("package.yaml")).map_err(|e| {
            vec![Diagnostic::error(
                "E0621",
                format!("{}: cannot read package.yaml: {e}", self.reference),
            )]
        })?;
        let dev: DevicePackageFile = serde_yaml::from_str(&text).map_err(|e| {
            vec![Diagnostic::error(
                "E0622",
                format!("{}: device payload does not parse: {e}", self.reference),
            )]
        })?;
        let mut diagnostics = Vec::new();
        if let Some(bus) = dev.requires.as_ref().and_then(|r| r.bus.as_ref()) {
            if let Some(fq) = &bus.min_frequency {
                if let Err(e) = Quantity::parse_as(fq, Dimension::Frequency) {
                    diagnostics.push(Diagnostic::error(
                        "E0623",
                        format!("{}: requires.bus.min_frequency: {e}", self.reference),
                    ));
                }
            }
            for (field, raw, code) in [
                ("max_latency", &bus.max_latency, "E0624"),
                ("max_jitter", &bus.max_jitter, "E0625"),
            ] {
                if let Some(raw) = raw {
                    match Quantity::parse_as(raw, Dimension::Time) {
                        Ok(quantity) if quantity.value >= 0.0 => {}
                        Ok(_) => diagnostics.push(Diagnostic::error(
                            code,
                            format!(
                                "{}: requires.bus.{field} must be non-negative",
                                self.reference
                            ),
                        )),
                        Err(e) => diagnostics.push(Diagnostic::error(
                            code,
                            format!("{}: requires.bus.{field}: {e}", self.reference),
                        )),
                    }
                }
            }
            let mut signals = BTreeSet::new();
            for signal in &bus.dma_signals {
                if !valid_identifier(signal) || !signals.insert(signal) {
                    diagnostics.push(Diagnostic::error(
                        "E0626",
                        format!(
                            "{}: requires.bus.dma_signals entries must be unique identifiers; got '{signal}'",
                            self.reference
                        ),
                    ));
                }
            }
        }
        if diagnostics.is_empty() {
            Ok(dev)
        } else {
            Err(diagnostics)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn temporary_registry(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "dryer-device-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn the_fixture_device_declares_its_connector_requirement() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = crate::LocalRegistry::load(&root);
        let dev = reg.find("devices", "tmc2209").unwrap();
        let payload = dev.device_payload().expect("payload parses");
        assert_eq!(
            payload.requires.unwrap().connector.as_deref(),
            Some("stepper_driver_socket")
        );
        assert_eq!(
            payload.device.unwrap().class.as_deref(),
            Some("stepper_driver")
        );
    }

    #[test]
    fn the_dma_fixture_declares_typed_timing_and_routes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = crate::LocalRegistry::load(&root);
        let dev = reg.find("devices", "dma-stream-sensor").unwrap();
        let payload = dev.device_payload().expect("payload parses");
        let bus = payload.requires.unwrap().bus.unwrap();
        assert_eq!(bus.dma_signals, vec!["rx"]);
        assert_eq!(bus.max_latency.as_deref(), Some("50 us"));
        assert_eq!(bus.max_jitter.as_deref(), Some("10 us"));
    }

    #[test]
    fn invalid_timing_and_duplicate_dma_signals_are_rejected() {
        let root = temporary_registry("invalid-budgets");
        let package = root.join("devices/bad-device");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("README.md"), "fixture\n").unwrap();
        std::fs::write(package.join("LICENSE"), "fixture\n").unwrap();
        std::fs::write(
            package.join("package.yaml"),
            r#"package:
  namespace: devices
  name: bad-device
  version: 1.0.0
  kind: device
requires:
  bus:
    kind: spi
    dma_signals: [rx, rx]
    max_latency: very-fast
"#,
        )
        .unwrap();
        let registry = crate::LocalRegistry::load(&root);
        let errors = registry
            .find("devices", "bad-device")
            .unwrap()
            .device_payload()
            .unwrap_err();
        for code in ["E0624", "E0626"] {
            assert!(errors.iter().any(|diagnostic| diagnostic.code == code));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
