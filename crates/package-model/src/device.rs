//! Device package payload (spec §9): a reusable component's declared class
//! and requirements. v0.1 carries the single requirement the resolver's
//! search-based allocator consumes — the connector kind the device plugs
//! into. Bus/signal requirements (§9 `requires.bus`, `requires.signals`)
//! land with the capability-matching slice.

use forge_machine_schema::Diagnostic;
use serde::Deserialize;

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
        serde_yaml::from_str(&text).map_err(|e| {
            vec![Diagnostic::error(
                "E0622",
                format!("{}: device payload does not parse: {e}", self.reference),
            )]
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

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
}
