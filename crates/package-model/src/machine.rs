//! Machine package payload: the **template** a `kind: machine` package
//! contributes to graph expansion (§5.5 "Package templates expand it").
//!
//! A template is deliberately board-agnostic: it may add *components*
//! (typically without explicit connector claims, so the resolver's
//! search-based allocation places them per board) and *kinematics
//! defaults*. Expansion semantics live in the resolver; the rule there is
//! source-wins — a user's manifest always shadows a template.

use dryer_machine_schema::{Component, Diagnostic};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Full `package.yaml` view for a `kind: machine` package.
#[derive(Debug, Clone, Deserialize)]
pub struct MachinePackageFile {
    pub package: crate::PackageIdentity,
    #[serde(default)]
    pub template: Option<Template>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Template {
    /// Components this machine class contributes (source-shadowable).
    #[serde(default)]
    pub components: BTreeMap<String, Component>,
    #[serde(default)]
    pub kinematics: Option<TemplateKinematics>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TemplateKinematics {
    /// The kinematics type this machine class assumes; a differing source
    /// declaration is surfaced, never overridden.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// Default limits, contributed only where the source declares none.
    #[serde(default)]
    pub limits: BTreeMap<String, String>,
}

impl crate::LoadedPackage {
    /// Parse this package's machine payload (`E064x` diagnostics).
    pub fn machine_payload(&self) -> Result<MachinePackageFile, Vec<Diagnostic>> {
        if self.kind != crate::PackageKind::Machine {
            return Err(vec![Diagnostic::error(
                "E0640",
                format!(
                    "{} is a {:?} package, not a machine",
                    self.reference, self.kind
                ),
            )]);
        }
        let text = std::fs::read_to_string(self.dir.join("package.yaml")).map_err(|e| {
            vec![Diagnostic::error(
                "E0641",
                format!("{}: cannot read package.yaml: {e}", self.reference),
            )]
        })?;
        serde_yaml::from_str(&text).map_err(|e| {
            vec![Diagnostic::error(
                "E0642",
                format!("{}: machine payload does not parse: {e}", self.reference),
            )]
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn the_fixture_machine_template_parses() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = crate::LocalRegistry::load(&root);
        let pkg = reg.find("machines", "cartesian-basic").expect("fixture");
        let payload = pkg.machine_payload().expect("payload parses");
        let t = payload.template.expect("has a template");
        assert!(t.components.contains_key("y_driver"));
        assert_eq!(t.kinematics.unwrap().kind.as_deref(), Some("cartesian"));
    }
}
