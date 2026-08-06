//! Board package payload (spec §8): the board-specific half of a
//! `kind: board` package.yaml — connectors, transports, hardware identity.
//!
//! Boards bind an MCU to a PCB. Per §8 they must NOT contain
//! printer-specific assignments ("X motor"); they offer *connectors*
//! that a machine's components claim.

use dryer_machine_schema::{Diagnostic, Dimension, Quantity};
use serde::{de, Deserialize, Deserializer};
use std::collections::BTreeMap;

/// Full `package.yaml` view for a `kind: board` package.
/// Unknown fields are ignored: board packages may carry sections
/// (boot, limits) that later resolver phases will consume.
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
    /// Ports this board offers to *child* controllers (e.g. a mainboard's
    /// CAN bus that a toolhead board attaches to via
    /// `transport: { parent: mainboard.can0 }`). Distinct from `transports`,
    /// which describes the uplink this board's own controller uses: two
    /// child controllers may legitimately share one downlink (CAN is
    /// multi-drop), which an exclusive connector claim could not express.
    #[serde(default)]
    pub downlinks: BTreeMap<String, Downlink>,
    /// Board-specific flashing recipes. These describe how to select a
    /// bootloader device and how an operator can recover it; they never
    /// contain a machine-specific controller identity.
    #[serde(default)]
    pub flash: Option<FlashConfig>,
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

#[derive(Debug, Clone, Deserialize)]
pub struct Downlink {
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FlashConfig {
    pub default_method: String,
    #[serde(default)]
    pub methods: BTreeMap<String, FlashMethod>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FlashMethod {
    /// Transport used by the flashing protocol. Step 10 initially supports
    /// `usb`; the open string keeps the package format extensible.
    pub transport: String,
    /// Identity of the device while it is in flashing/bootloader mode.
    pub select: UsbSelector,
    /// Human-executed transition instructions. The dry-run planner reports
    /// these verbatim but never executes them.
    #[serde(default)]
    pub enter_bootloader: Vec<String>,
    /// Verification vocabulary. Only `sha256` is accepted in v0.
    pub verify: String,
    /// Board-specific recovery instructions surfaced by every plan.
    #[serde(default)]
    pub recovery: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UsbSelector {
    #[serde(deserialize_with = "deserialize_usb_id")]
    pub usb_vid: u16,
    #[serde(deserialize_with = "deserialize_usb_id")]
    pub usb_pid: u16,
    #[serde(default)]
    pub serial_number: Option<String>,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
}

/// Accept either YAML integers or the conventional quoted `0x1234` form.
fn deserialize_usb_id<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Integer(u64),
        String(String),
    }

    let value = match Repr::deserialize(deserializer)? {
        Repr::Integer(value) => value,
        Repr::String(value) => {
            let trimmed = value.trim();
            if let Some(hex) = trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
            {
                u64::from_str_radix(hex, 16).map_err(de::Error::custom)?
            } else {
                trimmed.parse::<u64>().map_err(de::Error::custom)?
            }
        }
    };
    u16::try_from(value).map_err(|_| de::Error::custom("USB id must fit in 16 bits"))
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
        if let Some(flash) = &board.flash {
            validate_flash(&self.reference.to_string(), flash, &mut diags);
        }
        if diags.is_empty() {
            Ok(board)
        } else {
            Err(diags)
        }
    }
}

fn validate_flash(reference: &str, flash: &FlashConfig, diags: &mut Vec<Diagnostic>) {
    if !dryer_machine_schema::valid_identifier(&flash.default_method) {
        diags.push(Diagnostic::error(
            "E0615",
            format!(
                "{reference}: flash.default_method '{}' is not a valid identifier",
                flash.default_method
            ),
        ));
    }
    if !flash.methods.contains_key(&flash.default_method) {
        diags.push(Diagnostic::error(
            "E0616",
            format!(
                "{reference}: flash.default_method '{}' has no matching method",
                flash.default_method
            ),
        ));
    }
    for (method_id, method) in &flash.methods {
        if !dryer_machine_schema::valid_identifier(method_id) {
            diags.push(Diagnostic::error(
                "E0615",
                format!("{reference}: flash method '{method_id}' is not a valid identifier"),
            ));
        }
        if method.transport != "usb" {
            diags.push(Diagnostic::error(
                "E0617",
                format!(
                    "{reference}: flash method '{method_id}' uses unsupported transport '{}' (v0 supports usb)",
                    method.transport
                ),
            ));
        }
        if method.verify != "sha256" {
            diags.push(Diagnostic::error(
                "E0618",
                format!(
                    "{reference}: flash method '{method_id}' uses unsupported verification '{}' (v0 supports sha256)",
                    method.verify
                ),
            ));
        }
        for (field, value) in [
            ("serial_number", method.select.serial_number.as_deref()),
            ("manufacturer", method.select.manufacturer.as_deref()),
            ("product", method.select.product.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                diags.push(Diagnostic::error(
                    "E0619",
                    format!(
                        "{reference}: flash method '{method_id}' select.{field} must not be empty"
                    ),
                ));
            }
        }
        if method
            .enter_bootloader
            .iter()
            .any(|step| step.trim().is_empty())
            || method.recovery.iter().any(|step| step.trim().is_empty())
        {
            diags.push(Diagnostic::error(
                "E0619",
                format!("{reference}: flash method '{method_id}' instructions must not be empty"),
            ));
        }
        if method.recovery.is_empty() {
            diags.push(Diagnostic::error(
                "E0619",
                format!(
                    "{reference}: flash method '{method_id}' must provide recovery instructions"
                ),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_flash, BoardPackageFile};
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
        let flash = payload.flash.expect("fixture flash metadata");
        assert_eq!(flash.default_method, "dfu");
        assert_eq!(flash.methods["dfu"].select.usb_vid, 0x1209);
        assert_eq!(flash.methods["dfu"].select.usb_pid, 0xd003);
    }

    #[test]
    fn asking_a_device_for_a_board_payload_is_a_typed_error() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = crate::LocalRegistry::load(&root);
        let dev = reg.find("devices", "tmc2209").unwrap();
        let err = dev.board_payload().unwrap_err();
        assert_eq!(err[0].code, "E0610");
    }

    #[test]
    fn invalid_flash_recipes_collect_structural_diagnostics() {
        let board: BoardPackageFile = serde_yaml::from_str(
            r#"
package:
  namespace: boards
  name: invalid-flash
  version: 1.0.0
  kind: board
flash:
  default_method: missing
  methods:
    serial_method:
      transport: serial
      select:
        usb_vid: 4617
        usb_pid: 53251
        product: ""
      verify: crc32
      recovery: []
"#,
        )
        .unwrap();
        let mut diagnostics = Vec::new();
        validate_flash(
            "boards/invalid-flash@1.0.0",
            board.flash.as_ref().unwrap(),
            &mut diagnostics,
        );
        let codes: Vec<_> = diagnostics.iter().map(|diag| diag.code.as_str()).collect();
        assert_eq!(codes, ["E0616", "E0617", "E0618", "E0619", "E0619"]);
    }

    #[test]
    fn downlinks_parse_when_declared() {
        let board: BoardPackageFile = serde_yaml::from_str(
            r#"
package:
  namespace: boards
  name: with-downlink
  version: 1.0.0
  kind: board
downlinks:
  can0:
    type: can
"#,
        )
        .unwrap();
        assert_eq!(board.downlinks.len(), 1);
        assert_eq!(board.downlinks["can0"].kind, "can");
    }

    #[test]
    fn downlinks_default_to_empty_when_absent() {
        let board: BoardPackageFile = serde_yaml::from_str(
            r#"
package:
  namespace: boards
  name: no-downlink
  version: 1.0.0
  kind: board
"#,
        )
        .unwrap();
        assert!(board.downlinks.is_empty());
    }
}
