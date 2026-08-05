use crate::packages::PackageSelection;
use dryer_machine_schema::{Diagnostic, MachineDoc};

pub(super) fn validate(
    doc: &MachineDoc,
    packages: &PackageSelection<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (name, workflow_ref) in &doc.workflows {
        let package_name = workflow_ref.package();
        let Some(pkg) = packages.select(package_name) else {
            diagnostics.push(
                Diagnostic::error(
                    "E1700",
                    format!("workflow '{name}' package '{package_name}' is not in the registry"),
                )
                .at(format!("workflows.{name}")),
            );
            continue;
        };
        let payload = match pkg.workflow_payload() {
            Ok(payload) => payload,
            Err(errs) => {
                diagnostics.extend(errs);
                continue;
            }
        };
        if let Some(params) = workflow_ref.parameters() {
            for param_name in params.keys() {
                if !payload.parameters.contains_key(param_name) {
                    diagnostics.push(
                        Diagnostic::error(
                            "E1701",
                            format!(
                                "workflow '{name}' passed parameter '{param_name}' which is not declared by '{}'",
                                pkg.reference
                            ),
                        )
                        .at(format!("workflows.{name}.parameters.{param_name}")),
                    );
                }
            }
        }
    }
}
