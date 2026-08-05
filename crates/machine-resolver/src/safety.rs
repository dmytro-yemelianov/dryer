use crate::capability::{
    is_sensor_connector_kind, safety_target_resources, sensor_resource_on_controller,
};
use crate::model::{ControllerSafeState, ResolvedGraph};
use crate::packages::PackageSelection;
use dryer_machine_schema::{Diagnostic, MachineDoc};
use dryer_package_model::safety::SafetyProfileFile;
use dryer_resource_model::ResourceId;
use std::collections::BTreeMap;

pub(super) fn validate(
    doc: &MachineDoc,
    resolved: &ResolvedGraph,
    packages: &PackageSelection<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<SafetyProfileFile> {
    if !dryer_machine_schema::valid_package_ref_name(&doc.safety.profile) {
        diagnostics.push(
            Diagnostic::error(
                "E1500",
                format!(
                    "safety profile '{}' must be 'namespace/name'",
                    doc.safety.profile
                ),
            )
            .at("safety.profile"),
        );
        return None;
    }
    let profile = match packages.select(&doc.safety.profile) {
        None => {
            diagnostics.push(
                Diagnostic::error(
                    "E1500",
                    format!(
                        "safety profile '{}' is not in the registry",
                        doc.safety.profile
                    ),
                )
                .at("safety.profile"),
            );
            return None;
        }
        Some(pkg) => match pkg.safety_profile_payload() {
            Err(errs) => {
                diagnostics.extend(errs);
                return None;
            }
            Ok(profile) => profile,
        },
    };
    for (cname, comp) in &doc.components {
        let hazardous = resolved
            .assignments
            .get(cname)
            .into_iter()
            .flatten()
            .any(|assignment| assignment.connector_kind == "power_output");
        let targets = match safety_target_resources(resolved, cname, comp) {
            Ok(targets) => targets,
            Err(message) => {
                diagnostics.push(
                    Diagnostic::error("E1506", message).at(format!("components.{cname}.driver")),
                );
                continue;
            }
        };
        let driver_backed = resolved.assignments.get(cname).map_or(true, Vec::is_empty)
            && comp
                .attributes
                .get("driver")
                .and_then(|value| value.as_str())
                .is_some();
        let policy = profile.classes.get(&comp.kind);
        if (hazardous || driver_backed) && policy.is_none() {
            let output = if driver_backed {
                "delegates an actuator output to a driver"
            } else {
                "drives a power output"
            };
            diagnostics.push(
                Diagnostic::error(
                    "E1501",
                    format!(
                        "component '{cname}' {output} but class '{}' has no policy in '{}'",
                        comp.kind, doc.safety.profile,
                    ),
                )
                .at(format!("components.{cname}"))
                .suggest(format!(
                    "add a '{}' class to the safety profile or use a covered class",
                    comp.kind
                )),
            );
        }
        let Some(policy) = policy else { continue };
        if targets.is_empty() {
            diagnostics.push(
                Diagnostic::error(
                    "E1505",
                    format!(
                        "component '{cname}' has class '{}' safety policy but no concrete controller resource",
                        comp.kind
                    ),
                )
                .at(format!("components.{cname}")),
            );
            continue;
        }
        if !policy.requires_sensor {
            continue;
        }
        let Some(sensor_name) = comp
            .attributes
            .get("sensor")
            .and_then(|value| value.as_str())
            .filter(|name| !name.trim().is_empty())
        else {
            diagnostics.push(
                Diagnostic::error(
                    "E1502",
                    format!(
                        "class '{}' requires a sensor, but component '{cname}' declares none",
                        comp.kind
                    ),
                )
                .at(format!("components.{cname}"))
                .suggest("add 'sensor: <component>' referencing a sensor component"),
            );
            continue;
        };
        let sensor_assignments = resolved.assignments.get(sensor_name);
        if sensor_assignments.map_or(true, Vec::is_empty) {
            diagnostics.push(
                Diagnostic::error(
                    "E1503",
                    format!(
                        "component '{cname}' requires sensor '{sensor_name}', but that sensor has no resolved controller resource"
                    ),
                )
                .at(format!("components.{cname}.sensor")),
            );
            continue;
        }
        if !sensor_assignments
            .into_iter()
            .flatten()
            .any(|assignment| is_sensor_connector_kind(&assignment.connector_kind))
        {
            diagnostics.push(
                Diagnostic::error(
                    "E1507",
                    format!(
                        "component '{cname}' requires sensor '{sensor_name}', but its resolved resource is not a sensor input"
                    ),
                )
                .at(format!("components.{cname}.sensor")),
            );
            continue;
        }
        let mut checked_controllers = std::collections::BTreeSet::new();
        for target in &targets {
            let Some((controller, _)) = target.0.split_once('.') else {
                continue;
            };
            if checked_controllers.insert(controller)
                && sensor_resource_on_controller(resolved, comp, controller).is_none()
            {
                diagnostics.push(
                    Diagnostic::error(
                        "E1504",
                        format!(
                            "component '{cname}' and required sensor '{sensor_name}' must resolve on the same controller as '{}'",
                            target.0
                        ),
                    )
                    .at(format!("components.{cname}.sensor")),
                );
            }
        }
    }
    Some(profile)
}

pub(super) fn partition(
    doc: &MachineDoc,
    resolved: &mut ResolvedGraph,
    profile: Option<&SafetyProfileFile>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(profile) = profile else { return };
    let mut safety_owners: BTreeMap<ResourceId, String> = BTreeMap::new();
    for (cname, comp) in &doc.components {
        let Some(policy) = profile.classes.get(&comp.kind) else {
            continue;
        };
        let Ok(resources) = safety_target_resources(resolved, cname, comp) else {
            continue;
        };
        for resource in resources {
            let Some((controller, _)) = resource.0.split_once('.') else {
                continue;
            };
            if let Some(existing) = safety_owners.get(&resource) {
                diagnostics.push(
                    Diagnostic::error(
                        "E1508",
                        format!(
                            "components '{existing}' and '{cname}' both define safety actions for physical resource '{}'",
                            resource.0
                        ),
                    )
                    .at(format!("components.{cname}")),
                );
                continue;
            }
            safety_owners.insert(resource.clone(), cname.clone());
            let sensor = policy
                .requires_sensor
                .then(|| sensor_resource_on_controller(resolved, comp, controller))
                .flatten();
            resolved
                .controller_safety
                .entry(controller.to_string())
                .or_default()
                .push(ControllerSafeState {
                    component: cname.clone(),
                    class: comp.kind.clone(),
                    resource,
                    state: policy.safe_state,
                    heartbeat_timeout_us: policy.heartbeat_timeout_us(),
                    sensor,
                });
        }
    }
    for bindings in resolved.controller_safety.values_mut() {
        bindings.sort_by(|left, right| {
            (&left.component, &left.resource.0).cmp(&(&right.component, &right.resource.0))
        });
    }
}
