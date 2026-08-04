//! Chip package payload (spec §7 + docs/peripheral-mapping.md): the MCU's
//! peripheral inventory and the pin→capability table the resolver joins
//! against board connector pins. Capability, never wiring.

use dryer_machine_schema::{valid_identifier, Diagnostic, Dimension, Quantity};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

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
    pub uart: Vec<Bus>,
    #[serde(default)]
    pub adc: Vec<Adc>,
    /// Explicit DMA channels and the peripheral signal routes they support.
    #[serde(default)]
    pub dma: Vec<DmaChannel>,
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
    /// Measured upper bound for this validated target (`20 us`).
    #[serde(default)]
    pub worst_case_latency: Option<String>,
    /// Measured upper bound for variation around that latency (`3 us`).
    #[serde(default)]
    pub worst_case_jitter: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DmaChannel {
    /// Stable channel identifier (`dma1.ch0`).
    pub id: String,
    /// Routable peripheral signals (`spi1.rx`, `spi1.tx`).
    #[serde(default)]
    pub routes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Adc {
    pub id: String,
    #[serde(default)]
    pub channels: Option<u32>,
}

impl ChipPackageFile {
    /// Bus metadata across every currently supported bus family.
    pub fn buses(&self) -> impl Iterator<Item = &Bus> {
        self.peripherals
            .spi
            .iter()
            .chain(&self.peripherals.i2c)
            .chain(&self.peripherals.uart)
    }

    /// Deterministically select the lowest-id DMA channel for one route.
    pub fn dma_channel_for_route(&self, route: &str) -> Option<&DmaChannel> {
        self.peripherals
            .dma
            .iter()
            .filter(|channel| channel.routes.iter().any(|candidate| candidate == route))
            .min_by(|left, right| left.id.cmp(&right.id))
    }

    /// Every peripheral id this chip declares.
    fn declared_ids(&self) -> Vec<&str> {
        let p = &self.peripherals;
        p.timers
            .iter()
            .map(|t| t.id.as_str())
            .chain(p.spi.iter().map(|b| b.id.as_str()))
            .chain(p.i2c.iter().map(|b| b.id.as_str()))
            .chain(p.uart.iter().map(|b| b.id.as_str()))
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
        for b in chip.buses() {
            if let Some(fq) = &b.max_frequency {
                if let Err(e) = Quantity::parse_as(fq, Dimension::Frequency) {
                    diags.push(Diagnostic::error(
                        "E0654",
                        format!("{}: bus '{}' max_frequency: {e}", self.reference, b.id),
                    ));
                }
            }
            for (field, raw, code) in [
                ("worst_case_latency", &b.worst_case_latency, "E0655"),
                ("worst_case_jitter", &b.worst_case_jitter, "E0656"),
            ] {
                if let Some(raw) = raw {
                    match Quantity::parse_as(raw, Dimension::Time) {
                        Ok(quantity) if quantity.value >= 0.0 => {}
                        Ok(_) => diags.push(Diagnostic::error(
                            code,
                            format!(
                                "{}: bus '{}' {field} must be non-negative",
                                self.reference, b.id
                            ),
                        )),
                        Err(e) => diags.push(Diagnostic::error(
                            code,
                            format!("{}: bus '{}' {field}: {e}", self.reference, b.id),
                        )),
                    }
                }
            }
        }
        let bus_ids: BTreeSet<&str> = chip.buses().map(|bus| bus.id.as_str()).collect();
        let mut dma_ids = BTreeSet::new();
        for channel in &chip.peripherals.dma {
            let valid_id = channel.id.split_once('.').is_some_and(|(controller, sub)| {
                valid_identifier(controller) && valid_identifier(sub)
            });
            if !valid_id || !dma_ids.insert(channel.id.as_str()) {
                diags.push(Diagnostic::error(
                    "E0657",
                    format!(
                        "{}: DMA channel id '{}' must be a unique '<controller>.<channel>' identifier",
                        self.reference, channel.id
                    ),
                ));
            }
            for route in &channel.routes {
                let valid = route
                    .split_once('.')
                    .is_some_and(|(bus, signal)| bus_ids.contains(bus) && valid_identifier(signal));
                if !valid {
                    diags.push(Diagnostic::error(
                        "E0658",
                        format!(
                            "{}: DMA channel '{}' route '{}' must name a declared bus signal",
                            self.reference, channel.id, route
                        ),
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

    fn temporary_registry(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("dryer-chip-{label}-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn the_chip_fixture_parses_and_validates_tokens() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = crate::LocalRegistry::load(&root);
        let chip = reg.find("chips", "generic-mcu").expect("fixture chip");
        assert_eq!(chip.reference.version, semver::Version::new(1, 5, 0));
        let payload = chip.chip_payload().expect("payload parses");
        assert_eq!(payload.pin_functions["PE11"], vec!["tim1.ch2", "gpio"]);
        assert_eq!(payload.peripherals.timers.len(), 2);
        assert_eq!(payload.peripherals.dma.len(), 2);
        assert_eq!(
            payload.dma_channel_for_route("spi1.rx").unwrap().id,
            "dma1.ch0"
        );
        assert_eq!(
            payload
                .buses()
                .find(|bus| bus.id == "spi1")
                .unwrap()
                .worst_case_latency
                .as_deref(),
            Some("20 us")
        );
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
        assert!(payload.peripherals.dma.is_empty());
    }

    #[test]
    fn invalid_timing_and_dma_metadata_is_rejected() {
        let root = temporary_registry("invalid-budgets");
        let package = root.join("chips/bad-chip");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("README.md"), "fixture\n").unwrap();
        std::fs::write(package.join("LICENSE"), "fixture\n").unwrap();
        std::fs::write(
            package.join("package.yaml"),
            r#"package:
  namespace: chips
  name: bad-chip
  version: 1.0.0
  kind: chip
peripherals:
  spi:
    - id: spi1
      worst_case_latency: 20 MHz
  dma:
    - id: dma1.ch0
      routes: [ghost.rx]
    - id: dma1.ch0
"#,
        )
        .unwrap();
        let registry = crate::LocalRegistry::load(&root);
        let errors = registry
            .find("chips", "bad-chip")
            .unwrap()
            .chip_payload()
            .unwrap_err();
        for code in ["E0655", "E0657", "E0658"] {
            assert!(errors.iter().any(|diagnostic| diagnostic.code == code));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
