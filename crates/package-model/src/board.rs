//! Board package payload (spec §8): the board-specific half of a
//! `kind: board` package.yaml — connectors, transports, hardware identity.
//!
//! Boards bind an MCU to a PCB. Per §8 they must NOT contain
//! printer-specific assignments ("X motor"); they offer *connectors*
//! that a machine's components claim.

use dryer_machine_schema::{Diagnostic, Dimension, Quantity};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Full `package.yaml` view for a `kind: board` package.
/// Unknown fields are ignored: board packages may carry sections
/// (flash, boot, limits) that later resolver phases will consume.
#[derive(Debug, Clone, Deserialize)]
pub struct BoardPackageFile {
    pub package: crate::PackageIdentity,
    /// Chip package reference (`chips/stm32f446re@1.0.0`); optional in
    /// Phase 0 fixtures, required once chip packages exist.
    #[serde(default)]
    pub chip: Option<String>,
    #[serde(default)]
    pub hardware: Option<Hardware>,
    #[serde(default)]
    pub connectors: BTreeMap<String, Connector>,
    #[serde(default)]
    pub transports: BTreeMap<String, Transport>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Hardware {
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Connector {
    /// Connector class (`stepper_driver_socket`, `power_output`,
    /// `analog_input`, ...). Open vocabulary; the resolver's
    /// claim-compatibility table interprets it.
    pub kind: String,
    /// Multi-pin connectors (`step`/`dir`/`enable`/`uart` → MCU pins).
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    /// Single-pin connectors.
    #[serde(default)]
    pub pin: Option<String>,
    #[serde(default)]
    pub voltage_domain: Option<String>,
    /// Quantity string, e.g. `"5 A"`.
    #[serde(default)]
    pub max_current: Option<String>,
    #[serde(default)]
    pub supply: Option<String>,
    /// Quantity string, e.g. `"4.7 kOhm"`.
    #[serde(default)]
    pub pullup: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Transport {
    #[serde(default)]
    pub peripheral: Option<String>,
}

impl crate::LoadedPackage {
    /// Parse this package's board payload. Errors (wrong kind, unparseable
    /// payload, invalid quantity strings) come back as diagnostics with
    /// `E061x` codes so the resolver can surface them uniformly.
    pub fn board_payload(&self) -> Result<BoardPackageFile, Vec<Diagnostic>> {
        if self.kind != crate::PackageKind::Board {
            return Err(vec![Diagnostic::error(
                "E0610",
                format!(
                    "{} is a {:?} package, not a board",
                    self.reference, self.kind
                ),
            )]);
        }
        let text = std::fs::read_to_string(self.dir.join("package.yaml")).map_err(|e| {
            vec![Diagnostic::error(
                "E0611",
                format!("{}: cannot read package.yaml: {e}", self.reference),
            )]
        })?;
        let board: BoardPackageFile = serde_yaml::from_str(&text).map_err(|e| {
            vec![Diagnostic::error(
                "E0612",
                format!("{}: board payload does not parse: {e}", self.reference),
            )]
        })?;
        let mut diags = Vec::new();
        for (cid, c) in &board.connectors {
            if !dryer_machine_schema::valid_identifier(cid) {
                diags.push(Diagnostic::error(
                    "E0613",
                    format!(
                        "{}: connector '{cid}' is not a valid identifier",
                        self.reference
                    ),
                ));
            }
            if let Some(mc) = &c.max_current {
                if let Err(e) = Quantity::parse_as(mc, Dimension::Current) {
                    diags.push(Diagnostic::error(
                        "E0614",
                        format!("{}: connector '{cid}' max_current: {e}", self.reference),
                    ));
                }
            }
            if let Some(p) = &c.pullup {
                if let Err(e) = Quantity::parse_as(p, Dimension::Resistance) {
                    diags.push(Diagnostic::error(
                        "E0614",
                        format!("{}: connector '{cid}' pullup: {e}", self.reference),
                    ));
                }
            }
        }
        if diags.is_empty() {
            Ok(board)
        } else {
            Err(diags)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn the_fixture_board_payload_parses_with_typed_quantities() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = crate::LocalRegistry::load(&root);
        let board = reg
            .find("boards", "example-mainboard")
            .expect("fixture board");
        let payload = board.board_payload().expect("payload parses");
        assert_eq!(payload.connectors["motor0"].kind, "stepper_driver_socket");
        assert_eq!(
            payload.connectors["heater0"].max_current.as_deref(),
            Some("5 A")
        );
        assert!(payload.transports.contains_key("usb"));
    }

    #[test]
    fn asking_a_device_for_a_board_payload_is_a_typed_error() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = crate::LocalRegistry::load(&root);
        let dev = reg.find("devices", "tmc2209").unwrap();
        let err = dev.board_payload().unwrap_err();
        assert_eq!(err[0].code, "E0610");
    }
}
