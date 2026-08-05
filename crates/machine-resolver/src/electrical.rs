use crate::capability::bus_satisfied;
use crate::model::ResolvedGraph;
use crate::requirements::DeviceRequirement;
use dryer_machine_schema::{Diagnostic, Dimension, MachineDoc, Quantity};
use dryer_package_model::{board::BoardPackageFile, chip::ChipPackageFile};
use std::collections::BTreeMap;

pub(super) struct Inputs<'a> {
    pub(super) doc: &'a MachineDoc,
    pub(super) device_reqs: &'a BTreeMap<String, DeviceRequirement>,
    pub(super) boards: &'a BTreeMap<String, BoardPackageFile>,
    pub(super) chips: &'a BTreeMap<String, ChipPackageFile>,
}

pub(super) fn validate(
    inputs: Inputs<'_>,
    resolved: &ResolvedGraph,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Inputs {
        doc,
        device_reqs,
        boards,
        chips,
    } = inputs;

    for (cname, comp) in &doc.components {
        let Some(dreq) = device_reqs.get(&comp.kind) else {
            continue;
        };
        for assignment in resolved.assignments.get(cname).into_iter().flatten() {
            let Some((ctrl, port)) = assignment.resource.0.split_once('.') else {
                continue;
            };
            if !dreq.domains.is_empty() {
                let connector_domain = boards
                    .get(ctrl)
                    .and_then(|b| b.connectors.get(port))
                    .and_then(|c| c.voltage_domain.clone());
                let ok = connector_domain
                    .as_deref()
                    .is_some_and(|d| dreq.domains.iter().any(|r| r == d));
                if !ok {
                    diagnostics.push(
                        Diagnostic::error(
                            "E1302",
                            format!(
                                "component '{cname}': device '{}' requires voltage domain [{}] but '{}' declares {}",
                                dreq.reference,
                                dreq.domains.join(", "),
                                assignment.resource.0,
                                connector_domain.as_deref().unwrap_or("none"),
                            ),
                        )
                        .at(format!("components.{cname}")),
                    );
                }
            }
            if let Some(bus) = &dreq.bus {
                if let Err(reason) =
                    bus_satisfied(chips.get(ctrl), &assignment.pin_capabilities, bus)
                {
                    diagnostics.push(
                        Diagnostic::error(
                            "E1315",
                            format!(
                                "component '{cname}': device '{}' requires a {} bus{} on '{}' — {reason}",
                                dreq.reference,
                                bus.kind,
                                bus.min_frequency
                                    .as_deref()
                                    .map(|f| format!(" (>= {f})"))
                                    .unwrap_or_default(),
                                assignment.resource.0,
                            ),
                        )
                        .at(format!("components.{cname}")),
                    );
                }
            }
        }
    }
    for (cname, comp) in &doc.components {
        let Some(draw_raw) = comp.attributes.get("current").and_then(|v| v.as_str()) else {
            continue;
        };
        let draw = match Quantity::parse_as(draw_raw, Dimension::Current) {
            Ok(q) => q,
            Err(e) => {
                diagnostics.push(
                    Diagnostic::error("E1301", format!("component '{cname}' current: {e}"))
                        .at(format!("components.{cname}.current")),
                );
                continue;
            }
        };
        for assignment in resolved.assignments.get(cname).into_iter().flatten() {
            let Some((ctrl, port)) = assignment.resource.0.split_once('.') else {
                continue;
            };
            let Some(limit_raw) = boards
                .get(ctrl)
                .and_then(|b| b.connectors.get(port))
                .and_then(|c| c.max_current.clone())
            else {
                continue;
            };
            // board payloads validated this quantity at load time
            let Ok(limit) = Quantity::parse_as(&limit_raw, Dimension::Current) else {
                continue;
            };
            if draw.value > limit.value {
                diagnostics.push(
                    Diagnostic::error(
                        "E1300",
                        format!(
                            "component '{cname}' draws {draw_raw} but '{}' allows at most {limit_raw}",
                            assignment.resource.0
                        ),
                    )
                    .at(format!("components.{cname}.current")),
                );
            }
        }
    }
}
