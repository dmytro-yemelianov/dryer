use crate::model::ResolvedGraph;
use crate::packages::PackageSelection;
use dryer_machine_schema::{Diagnostic, MachineDoc};
use std::collections::BTreeSet;

pub(super) fn validate(
    doc: &MachineDoc,
    resolved: &ResolvedGraph,
    packages: &PackageSelection<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let resolved_resources: BTreeSet<&str> = resolved
        .assignments
        .values()
        .flatten()
        .map(|assignment| assignment.resource.0.as_str())
        .collect();

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

        if let Some(req) = &payload.requires {
            for cap in &req.capabilities {
                let family = cap.split('.').next().unwrap_or(cap.as_str());
                if !machine_supports_capability_family(doc, family) {
                    diagnostics.push(
                        Diagnostic::error(
                            "E1702",
                            format!(
                                "workflow '{name}' requires capability '{cap}', but the machine provides no matching '{family}' component"
                            ),
                        )
                        .at(format!("workflows.{name}")),
                    );
                }
            }
        }

        for lock in &payload.locks {
            let lock_name = lock.as_str();
            let is_resolved_resource = resolved_resources.contains(lock_name);
            let is_component = doc.components.contains_key(lock_name);
            let is_controller = doc.controllers.contains_key(lock_name);
            if !is_resolved_resource && !is_component && !is_controller {
                diagnostics.push(
                    Diagnostic::error(
                        "E1703",
                        format!(
                            "workflow '{name}' declares lock '{lock_name}' which is not a resolved resource, component, or controller"
                        ),
                    )
                    .at(format!("workflows.{name}")),
                );
            }
        }
    }
}

fn machine_supports_capability_family(doc: &MachineDoc, family: &str) -> bool {
    doc.components.values().any(|comp| {
        comp.kind == family
            || comp.kind.starts_with(family)
            || comp.kind.ends_with(family)
            || (family == "motion" && comp.kind == "stepper_motor")
    })
}
