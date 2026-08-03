//! Safety-profile package payload (spec §18.2): per component-class safe
//! states and edge-enforcement parameters. This is *policy data* — the
//! resolver checks coverage (every hazardous output belongs to a covered
//! class); compiling safe states into controller artifacts is a firmware-
//! phase concern (§18.2 "must not rely solely on host initialization").

use forge_machine_schema::{Diagnostic, Dimension, Quantity};
use serde::Deserialize;
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
    /// (`off`, `disabled`, ...). Open vocabulary until the firmware
    /// phase defines the closed set it can actually compile.
    pub safe_state: String,
    /// Quantity string (`500 ms`); loss of host heartbeat for longer
    /// than this forces the safe state at the edge (§18.1).
    #[serde(default)]
    pub heartbeat_timeout: Option<String>,
    /// The class is only safe with a sensor attached (heaters: thermal
    /// runaway detection needs one, §18.3).
    #[serde(default)]
    pub requires_sensor: bool,
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
                if let Err(e) = Quantity::parse_as(hb, Dimension::Time) {
                    diags.push(Diagnostic::error(
                        "E0633",
                        format!("{}: class '{class}' heartbeat_timeout: {e}", self.reference),
                    ));
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
        assert_eq!(heater.safe_state, "off");
        assert!(heater.requires_sensor);
        assert_eq!(heater.heartbeat_timeout.as_deref(), Some("500 ms"));
        assert!(!profile.classes["fan"].requires_sensor);
    }
}
