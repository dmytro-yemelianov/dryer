//! Workflow package payload (spec §17). This is a registry-local transport for
//! workflow source references and compile-time metadata used by future workflow
//! compilation. The resolver in this phase stores only registry and type shape,
//! not executable semantics.

use dryer_machine_schema::{valid_identifier, Diagnostic};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Full `package.yaml` view for a `kind: workflow` package.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowPackageFile {
    pub package: crate::PackageIdentity,
    /// Optional workflow API version (for future schema gating).
    #[serde(default)]
    pub api_version: Option<String>,
    /// Workflow parameter schema by name.
    #[serde(default)]
    pub parameters: BTreeMap<String, WorkflowParameter>,
    /// Workflow requires block: capability-level permissions.
    #[serde(default)]
    pub requires: Option<WorkflowRequires>,
    /// Named resource locks required while running.
    #[serde(default)]
    pub locks: Vec<String>,
    /// The primary ordered step list (informational in this phase).
    #[serde(default)]
    pub steps: Vec<WorkflowStep>,
    /// Safe-exit handlers on cancellation.
    #[serde(rename = "on_cancel", default)]
    pub on_cancel: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkflowParameter {
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkflowRequires {
    /// Action capabilities required by the workflow implementation.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowStep {
    #[serde(default)]
    pub call: Option<String>,
    #[serde(default, rename = "with")]
    pub with_arguments: BTreeMap<String, serde_yaml::Value>,
}

impl crate::LoadedPackage {
    /// Parse this package's workflow payload (`E065x` diagnostics).
    pub fn workflow_payload(&self) -> Result<WorkflowPackageFile, Vec<Diagnostic>> {
        if self.kind != crate::PackageKind::Workflow {
            return Err(vec![Diagnostic::error(
                "E0650",
                format!(
                    "{} is a {:?} package, not a workflow",
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
        let workflow: WorkflowPackageFile = serde_yaml::from_str(&text).map_err(|e| {
            vec![Diagnostic::error(
                "E0652",
                format!("{}: workflow payload does not parse: {e}", self.reference),
            )]
        })?;
        let mut diagnostics = Vec::new();

        if let Some(api_version) = &workflow.api_version {
            if !api_version.starts_with("forge.workflow/v") {
                diagnostics.push(Diagnostic::error(
                    "E0653",
                    format!(
                        "{}: workflow api_version '{api_version}' is not forge.workflow/vN",
                        self.reference
                    ),
                ));
            }
        }

        for name in workflow.parameters.keys() {
            if !valid_identifier(name) {
                diagnostics.push(Diagnostic::error(
                    "E0654",
                    format!(
                        "{}: workflow parameter '{name}' is not a valid identifier",
                        self.reference
                    ),
                ));
            }
        }

        for step in &workflow.steps {
            validate_step(
                self,
                &step.call,
                &step.with_arguments,
                "steps",
                &mut diagnostics,
            );
        }
        for step in &workflow.on_cancel {
            validate_step(
                self,
                &step.call,
                &step.with_arguments,
                "on_cancel",
                &mut diagnostics,
            );
        }

        if let Some(req) = &workflow.requires {
            for capability in &req.capabilities {
                if !valid_action_name(capability) {
                    diagnostics.push(Diagnostic::error(
                        "E0655",
                        format!(
                            "{}: workflow requires capability '{capability}' is not a valid identifier",
                            self.reference
                        ),
                    ));
                }
            }
        }
        for lock in &workflow.locks {
            if !valid_identifier(lock) {
                diagnostics.push(Diagnostic::error(
                    "E0656",
                    format!(
                        "{}: workflow lock '{lock}' is not a valid identifier",
                        self.reference
                    ),
                ));
            }
        }

        if diagnostics.is_empty() {
            Ok(workflow)
        } else {
            Err(diagnostics)
        }
    }
}

fn valid_action_name(raw: &str) -> bool {
    if raw.is_empty() || raw.ends_with('.') || raw.starts_with('.') {
        return false;
    }
    raw.split('.').all(valid_identifier)
}

fn validate_step(
    package: &crate::LoadedPackage,
    call: &Option<String>,
    with_map: &BTreeMap<String, serde_yaml::Value>,
    context: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match call {
        Some(call) => {
            if !valid_action_name(call) {
                diagnostics.push(Diagnostic::error(
                    "E0657",
                    format!(
                        "{}: workflow {context} call '{call}' is not a valid action name",
                        package.reference
                    ),
                ));
            }
        }
        None => {
            diagnostics.push(Diagnostic::error(
                "E0658",
                format!(
                    "{}: workflow {context} entry is missing 'call'",
                    package.reference
                ),
            ));
        }
    }

    for argument in with_map.keys() {
        if !valid_identifier(argument) {
            diagnostics.push(Diagnostic::error(
                "E0659",
                format!(
                    "{}: workflow {context} argument '{argument}' is not a valid identifier",
                    package.reference
                ),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    fn temporary_registry(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "dryer-workflow-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn workflow_parameters_and_steps_validate() {
        let root = temporary_registry("valid-workflow");
        let package = root.join("workflows/print-start");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("README.md"), "fixture\n").unwrap();
        std::fs::write(package.join("LICENSE"), "fixture\n").unwrap();
        std::fs::write(
            package.join("package.yaml"),
            r#"package:
  namespace: workflows
  name: print-start
  version: 1.0.0
  kind: workflow
api_version: forge.workflow/v0.1
parameters:
  bed_temperature:
    type: temperature
steps:
  - call: heater.set_target
    with:
      bed: true
on_cancel:
  - call: motion.stop
    with:
      immediate: true
requires:
  capabilities:
    - heater.set_target
    - motion.home
"#,
        )
        .unwrap();
        let reg = crate::LocalRegistry::load(&root);
        let payload = reg
            .find("workflows", "print-start")
            .unwrap()
            .workflow_payload()
            .expect("workflow payload parses");
        assert!(payload.parameters.contains_key("bed_temperature"));
        assert_eq!(payload.steps.len(), 1);
        assert_eq!(payload.steps[0].call.as_deref(), Some("heater.set_target"));
        assert_eq!(payload.on_cancel[0].call.as_deref(), Some("motion.stop"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_workflow_steps_and_steps_are_rejected() {
        let root = temporary_registry("invalid-workflow");
        let package = root.join("workflows/invalid");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("README.md"), "fixture\n").unwrap();
        std::fs::write(package.join("LICENSE"), "fixture\n").unwrap();
        std::fs::write(
            package.join("package.yaml"),
            r#"package:
  namespace: workflows
  name: invalid
  version: 1.0.0
  kind: workflow
api_version: bad.version
parameters:
  bed temperature:
    type: temperature
steps:
  - with:
      bed temp: 250
"#,
        )
        .unwrap();

        let reg = crate::LocalRegistry::load(&root);
        let errors = reg
            .find("workflows", "invalid")
            .unwrap()
            .workflow_payload()
            .unwrap_err();
        for code in ["E0653", "E0654", "E0658", "E0659"] {
            assert!(
                errors.iter().any(|diagnostic| diagnostic.code == code),
                "missing {code}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn non_workflow_package_rejects_workflow_payload() {
        let root = temporary_registry("wrong-kind");
        let package = root.join("boards/invalid-workflow");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("README.md"), "fixture\n").unwrap();
        std::fs::write(package.join("LICENSE"), "fixture\n").unwrap();
        std::fs::write(
            package.join("package.yaml"),
            r#"package:
  namespace: boards
  name: invalid-workflow
  version: 1.0.0
  kind: board
"#,
        )
        .unwrap();

        let reg = crate::LocalRegistry::load(&root);
        let errors = reg
            .find("boards", "invalid-workflow")
            .unwrap()
            .workflow_payload()
            .unwrap_err();
        assert_eq!(errors[0].code, "E0650");
        std::fs::remove_dir_all(root).unwrap();
    }
}
