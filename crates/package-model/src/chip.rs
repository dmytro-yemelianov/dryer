//! Chip package payload (spec §7 + docs/peripheral-mapping.md): the MCU's
//! peripheral inventory and the pin→capability table the resolver joins
//! against board connector pins. Capability, never wiring.

use dryer_machine_schema::{Diagnostic, Dimension, Quantity};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Full `package.yaml` view for a `kind: chip` package.
#[derive(Debug, Clone, Deserialize)]
pub struct ChipPackageFile {
    pub package: crate::PackageIdentity,
    #[serde(default)]
    pub peripherals: Peripherals,
    /// Pin → capability tokens (`gpio` or `<peripheral>.<sub>`).
    #[serde(default)]
    pub pin_functions: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Peripherals {
    #[serde(default)]
    pub timers: Vec<Timer>,
    #[serde(default)]
    pub spi: Vec<Bus>,
    #[serde(default)]
    pub i2c: Vec<Bus>,
    #[serde(default)]
    pub uart: Vec<IdOnly>,
    #[serde(default)]
    pub adc: Vec<Adc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Timer {
    pub id: String,
    pub channels: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Bus {
    pub id: String,
    /// Quantity string (`42 MHz`).
    #[serde(default)]
    pub max_frequency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IdOnly {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Adc {
    pub id: String,
    #[serde(default)]
    pub channels: Option<u32>,
}

impl ChipPackageFile {
    /// Every peripheral id this chip declares.
    fn declared_ids(&self) -> Vec<&str> {
        let p = &self.peripherals;
        p.timers
            .iter()
            .map(|t| t.id.as_str())
            .chain(p.spi.iter().map(|b| b.id.as_str()))
            .chain(p.i2c.iter().map(|b| b.id.as_str()))
            .chain(p.uart.iter().map(|u| u.id.as_str()))
            .chain(p.adc.iter().map(|a| a.id.as_str()))
            .collect()
    }
}

impl crate::LoadedPackage {
    /// Parse this package's chip payload (`E065x` diagnostics). Validates
    /// that every pin-function token names a declared peripheral (or
    /// `gpio`) and that bus frequencies are typed.
    pub fn chip_payload(&self) -> Result<ChipPackageFile, Vec<Diagnostic>> {
        if self.kind != crate::PackageKind::Chip {
            return Err(vec![Diagnostic::error(
                "E0650",
                format!(
                    "{} is a {:?} package, not a chip",
                    self.reference, self.kind
                ),
            )]);
        }
        let text = std::fs::read_to_string(self.dir.join("package.yaml")).map_err(|e| {
            vec![Diagnostic::error(
                "E0651",
                format!("{}: cannot read package.yaml: {e}", self.reference),
            )]
        })?;
        let chip: ChipPackageFile = serde_yaml::from_str(&text).map_err(|e| {
            vec![Diagnostic::error(
                "E0652",
                format!("{}: chip payload does not parse: {e}", self.reference),
            )]
        })?;
        let mut diags = Vec::new();
        let ids = chip.declared_ids();
        for (pin, funcs) in &chip.pin_functions {
            for f in funcs {
                let base = f.split('.').next().unwrap_or(f);
                if base != "gpio" && !ids.contains(&base) {
                    diags.push(Diagnostic::error(
                        "E0653",
                        format!(
                            "{}: pin {pin} claims '{f}' but no peripheral '{base}' is declared",
                            self.reference
                        ),
                    ));
                }
            }
        }
        for b in chip.peripherals.spi.iter().chain(&chip.peripherals.i2c) {
            if let Some(fq) = &b.max_frequency {
                if let Err(e) = Quantity::parse_as(fq, Dimension::Frequency) {
                    diags.push(Diagnostic::error(
                        "E0654",
                        format!("{}: bus '{}' max_frequency: {e}", self.reference, b.id),
                    ));
                }
            }
        }
        if diags.is_empty() {
            Ok(chip)
        } else {
            Err(diags)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn the_chip_fixture_parses_and_validates_tokens() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = crate::LocalRegistry::load(&root);
        let chip = reg.find("chips", "generic-mcu").expect("fixture chip");
        assert_eq!(chip.reference.version, semver::Version::new(1, 4, 0));
        let payload = chip.chip_payload().expect("payload parses");
        assert_eq!(payload.pin_functions["PE11"], vec!["tim1.ch2", "gpio"]);
        assert_eq!(payload.peripherals.timers.len(), 2);
    }

    #[test]
    fn older_chip_versions_without_the_table_still_parse() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = crate::LocalRegistry::load(&root);
        let old = reg
            .find_version("chips", "generic-mcu", &semver::Version::new(1, 2, 0))
            .unwrap();
        let payload = old.chip_payload().expect("older payload parses");
        assert!(payload.pin_functions.is_empty());
    }
}
