//! Safety-profile package payload (spec §18.2): per component-class safe
//! states and edge-enforcement parameters. This is *policy data* — the
//! resolver checks coverage and partitions concrete bindings into controller
//! safety configuration (§18.2 "must not rely solely on host initialization").

use dryer_machine_schema::{Diagnostic, Dimension, Quantity};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Full `package.yaml` view for a `kind: safety-profile` package.
#[derive(Debug, Clone, Deserialize)]
pub struct SafetyProfileFile {
    pub package: crate::PackageIdentity,
    /// Component class (the component's `type`) → policy.
    #[serde(default)]
    pub classes: BTreeMap<String, ClassPolicy>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClassPolicy {
    /// The state a controller must force on fault/boot/heartbeat loss
    /// (`off` or `disabled`). This is deliberately a closed vocabulary:
    /// controller artifacts must never carry an uninterpreted safety action.
    pub safe_state: SafeState,
    /// Quantity string (`500 ms`); loss of host heartbeat for longer
    /// than this forces the safe state at the edge (§18.1).
    #[serde(default)]
    pub heartbeat_timeout: Option<String>,
    /// The class is only safe with a sensor attached (heaters: thermal
    /// runaway detection needs one, §18.3).
    #[serde(default)]
    pub requires_sensor: bool,
}

/// Safety actions the current controller artifact ABI can compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeState {
    Off,
    Disabled,
}

impl SafeState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Disabled => "disabled",
        }
    }
}

impl ClassPolicy {
    /// Convert a validated timeout to the controller's 1 us time quantum.
    pub fn heartbeat_timeout_us(&self) -> Option<u64> {
        self.heartbeat_timeout.as_deref().and_then(|raw| {
            let quantity = Quantity::parse_as(raw, Dimension::Time).ok()?;
            let micros = quantity.value * 1_000_000.0;
            let rounded = micros.round();
            (quantity.value > 0.0
                && rounded >= 1.0
                && rounded <= u64::MAX as f64
                && (micros - rounded).abs() <= 1e-6)
                .then_some(rounded as u64)
        })
    }
}

impl crate::LoadedPackage {
    /// Parse this package's safety-profile payload (`E063x` diagnostics).
    pub fn safety_profile_payload(&self) -> Result<SafetyProfileFile, Vec<Diagnostic>> {
        if self.kind != crate::PackageKind::SafetyProfile {
            return Err(vec![Diagnostic::error(
                "E0630",
                format!(
                    "{} is a {:?} package, not a safety-profile",
                    self.reference, self.kind
                ),
            )]);
        }
        let text = std::fs::read_to_string(self.dir.join("package.yaml")).map_err(|e| {
            vec![Diagnostic::error(
                "E0631",
                format!("{}: cannot read package.yaml: {e}", self.reference),
            )]
        })?;
        let profile: SafetyProfileFile = serde_yaml::from_str(&text).map_err(|e| {
            vec![Diagnostic::error(
                "E0632",
                format!("{}: safety payload does not parse: {e}", self.reference),
            )]
        })?;
        let mut diags = Vec::new();
        for (class, policy) in &profile.classes {
            if let Some(hb) = &policy.heartbeat_timeout {
                match Quantity::parse_as(hb, Dimension::Time) {
                    Err(e) => diags.push(Diagnostic::error(
                        "E0633",
                        format!("{}: class '{class}' heartbeat_timeout: {e}", self.reference),
                    )),
                    Ok(quantity) if quantity.value <= 0.0 => {
                        diags.push(Diagnostic::error(
                            "E0634",
                            format!(
                                "{}: class '{class}' heartbeat_timeout must be positive",
                                self.reference
                            ),
                        ));
                    }
                    Ok(quantity) => {
                        let micros = quantity.value * 1_000_000.0;
                        let rounded = micros.round();
                        if rounded < 1.0
                            || rounded > u64::MAX as f64
                            || (micros - rounded).abs() > 1e-6
                        {
                            diags.push(Diagnostic::error(
                                "E0635",
                                format!(
                                    "{}: class '{class}' heartbeat_timeout must fit the 1 us controller time quantum",
                                    self.reference
                                ),
                            ));
                        }
                    }
                }
            }
        }
        if diags.is_empty() {
            Ok(profile)
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
        std::env::temp_dir().join(format!(
            "dryer-safety-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn the_fixture_profile_parses_with_typed_timeouts() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = crate::LocalRegistry::load(&root);
        let profile = reg
            .find("safety-profiles", "desktop-fdm")
            .expect("fixture profile")
            .safety_profile_payload()
            .expect("payload parses");
        let heater = &profile.classes["heater"];
        assert_eq!(heater.safe_state, super::SafeState::Off);
        assert!(heater.requires_sensor);
        assert_eq!(heater.heartbeat_timeout.as_deref(), Some("500 ms"));
        assert_eq!(heater.heartbeat_timeout_us(), Some(500_000));
        assert!(!profile.classes["fan"].requires_sensor);
    }

    #[test]
    fn controller_safety_actions_are_a_closed_vocabulary() {
        let root = temporary_registry("unsupported-state");
        let package = root.join("safety-profiles/invalid");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("README.md"), "fixture\n").unwrap();
        std::fs::write(package.join("LICENSE"), "fixture\n").unwrap();
        std::fs::write(
            package.join("package.yaml"),
            r#"package:
  namespace: safety-profiles
  name: invalid
  version: 1.0.0
  kind: safety-profile
classes:
  heater:
    safe_state: maybe
"#,
        )
        .unwrap();
        let registry = crate::LocalRegistry::load(&root);
        let errors = registry
            .find("safety-profiles", "invalid")
            .unwrap()
            .safety_profile_payload()
            .unwrap_err();
        assert!(errors.iter().any(|diagnostic| diagnostic.code == "E0632"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn heartbeat_timeouts_must_be_positive_controller_ticks() {
        let root = temporary_registry("invalid-timeouts");
        let package = root.join("safety-profiles/invalid");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("README.md"), "fixture\n").unwrap();
        std::fs::write(package.join("LICENSE"), "fixture\n").unwrap();
        std::fs::write(
            package.join("package.yaml"),
            r#"package:
  namespace: safety-profiles
  name: invalid
  version: 1.0.0
  kind: safety-profile
classes:
  heater:
    safe_state: off
    heartbeat_timeout: -1 ms
  fan:
    safe_state: off
    heartbeat_timeout: 0.5 us
  stepper_motor:
    safe_state: disabled
    heartbeat_timeout: 0.0000001 us
"#,
        )
        .unwrap();
        let registry = crate::LocalRegistry::load(&root);
        let errors = registry
            .find("safety-profiles", "invalid")
            .unwrap()
            .safety_profile_payload()
            .unwrap_err();
        for code in ["E0634", "E0635"] {
            assert!(errors.iter().any(|diagnostic| diagnostic.code == code));
        }
        let tiny = super::ClassPolicy {
            safe_state: super::SafeState::Disabled,
            heartbeat_timeout: Some("0.0000001 us".to_string()),
            requires_sensor: false,
        };
        assert_eq!(tiny.heartbeat_timeout_us(), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
