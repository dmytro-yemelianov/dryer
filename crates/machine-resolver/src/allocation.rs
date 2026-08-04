use crate::capability::{bus_satisfied, claim_kind, derive_pin_capabilities};
use crate::diagnostics::{diagnostic_at_source, expanded_source, related_to_claim};
use crate::model::{Assignment, Phase, ResolvedGraph};
use crate::packages::PackageSelection;
use crate::requirements::DeviceRequirement;
use dryer_machine_schema::{Diagnostic, Dimension, MachineDoc, Quantity, Severity, SourceSpan};
use dryer_package_model::{board::BoardPackageFile, chip::ChipPackageFile};
use dryer_resource_model::ResourceId;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
struct ResourceClaim {
    component: String,
    path: String,
    source: Option<SourceSpan>,
}

#[derive(Debug, Clone)]
struct TimerClaim {
    component: String,
    pin: String,
    path: String,
    source: Option<SourceSpan>,
}

/// Run capability matching and resource allocation as one ordered phase.
/// Explicit claims and search allocation intentionally share claim state so
/// later candidates observe all earlier resource and timer reservations.
pub(super) struct Inputs<'a, 'registry> {
    pub(super) doc: &'a MachineDoc,
    pub(super) expanded_sources: &'a BTreeMap<String, SourceSpan>,
    pub(super) device_reqs: &'a BTreeMap<String, DeviceRequirement>,
    pub(super) boards: &'a BTreeMap<String, BoardPackageFile>,
    pub(super) chips: &'a BTreeMap<String, ChipPackageFile>,
    pub(super) packages: &'a PackageSelection<'registry>,
}

pub(super) fn allocate(
    inputs: Inputs<'_, '_>,
    diagnostics: &mut Vec<Diagnostic>,
    phases_run: &mut Vec<Phase>,
) -> Option<ResolvedGraph> {
    let Inputs {
        doc,
        expanded_sources,
        device_reqs,
        boards,
        chips,
        packages,
    } = inputs;
    // --- Phase 6+7: capability matching & explicit-claim allocation ----
    phases_run.push(Phase::CapabilityMatching);
    phases_run.push(Phase::ResourceAllocation);

    let mut resolved = ResolvedGraph {
        packages: packages.closure_refs().to_vec(),
        ..ResolvedGraph::default()
    };
    // connector -> first claimant, for exclusivity conflicts
    let mut claims: BTreeMap<String, ResourceClaim> = BTreeMap::new();
    // Step-timing (docs/peripheral-mapping.md): when the machine declares a
    // step-rate budget, every stepper socket's step pin must sit on a timer
    // channel, and channels are exclusive. Reservations interleave with
    // allocation (explicit pass first, then search) so the search allocator
    // can steer around reserved channels — which is why this check lives
    // here rather than in the electrical phase.
    let max_step_rate = doc
        .kinematics
        .limits
        .get("max_step_rate")
        .and_then(|v| Quantity::parse_as(v, Dimension::Frequency).ok());
    // "ctrl.timN.chM" -> (claiming component, step pin)
    let mut timer_claims: BTreeMap<String, TimerClaim> = BTreeMap::new();

    for (cname, comp) in &doc.components {
        for (attr, val) in &comp.attributes {
            let Some(target) = val.as_str() else {
                continue;
            };
            // The component's device package decides the connector kind
            // when it has one; the attr table is the documented fallback.
            let expected_kind: String = match claim_kind(attr) {
                None => continue, // not a claiming attribute
                Some(table_kind) => device_reqs
                    .get(&comp.kind)
                    .and_then(|d| d.connector.clone())
                    .unwrap_or_else(|| table_kind.to_string()),
            };
            let claim_path = format!("components.{cname}.{attr}");
            let claim_source = expanded_source(expanded_sources, &claim_path);
            // The parser guarantees shape and controller existence for
            // SOURCE components; template-expanded components bypass it,
            // so both are re-checked here rather than silently skipped.
            let Some((ctrl, port)) = target.split_once('.') else {
                diagnostics.push(diagnostic_at_source(
                    Diagnostic::error(
                        "E1206",
                        format!(
                            "component '{cname}': '{target}' must name a controller port as 'controller.port'"
                        ),
                    ),
                    &claim_path,
                    claim_source.as_ref(),
                ));
                continue;
            };
            let Some(board) = boards.get(ctrl) else {
                diagnostics.push(diagnostic_at_source(
                    Diagnostic::error(
                        "E1206",
                        format!("component '{cname}': unknown controller '{ctrl}' in '{target}'"),
                    ),
                    &claim_path,
                    claim_source.as_ref(),
                ));
                continue;
            };

            let Some(connector) = board.connectors.get(port) else {
                let known: Vec<&String> = board.connectors.keys().collect();
                diagnostics.push(diagnostic_at_source(
                    Diagnostic::error(
                        "E1201",
                        format!(
                            "component '{cname}': controller '{ctrl}' has no connector '{port}'"
                        ),
                    )
                    .suggest(format!("available connectors: {known:?}")),
                    &claim_path,
                    claim_source.as_ref(),
                ));
                continue;
            };

            if connector.kind != expected_kind {
                diagnostics.push(diagnostic_at_source(
                    Diagnostic::error(
                        "E1202",
                        format!(
                            "component '{cname}': '{target}' is a {} connector, but '{attr}' requires {expected_kind}",
                            connector.kind
                        ),
                    ),
                    &claim_path,
                    claim_source.as_ref(),
                ));
                continue;
            }

            if let Some(prev) = claims.get(target) {
                // §11.3-style conflict with actionable suggestions:
                // list free connectors of the same kind on the same board.
                let free: Vec<String> = board
                    .connectors
                    .iter()
                    .filter(|(id, c)| {
                        c.kind == expected_kind && !claims.contains_key(&format!("{ctrl}.{id}"))
                    })
                    .map(|(id, _)| format!("{ctrl}.{id}"))
                    .collect();
                let mut d = diagnostic_at_source(
                    Diagnostic::error(
                        "E1200",
                        format!(
                            "connector conflict: '{target}' is claimed by both '{prev}' and '{cname}'",
                            prev = prev.component
                        ),
                    ),
                    &claim_path,
                    claim_source.as_ref(),
                );
                d = related_to_claim(
                    d,
                    format!("'{}' first claimed '{target}' here", prev.component),
                    &prev.path,
                    prev.source.as_ref(),
                );
                if free.is_empty() {
                    d = d.suggest(format!(
                        "no free {expected_kind} connectors remain on '{ctrl}'"
                    ));
                } else {
                    d = d.suggest(format!("move '{cname}' to one of: {}", free.join(", ")));
                }
                diagnostics.push(d);
                continue;
            }

            let caps = derive_pin_capabilities(chips.get(ctrl), connector);
            let mut constraints = vec![
                format!("explicit claim of '{target}'"),
                format!("connector kind == {expected_kind}"),
                "exclusive ownership".to_string(),
            ];
            if let Some(bus) = device_reqs
                .get(&comp.kind)
                .and_then(|requirement| requirement.bus.as_ref())
            {
                match bus_satisfied(chips.get(ctrl), &caps, bus) {
                    Ok(matched) => constraints.extend(matched.constraints(bus)),
                    Err(reason) => {
                        diagnostics.push(diagnostic_at_source(
                            Diagnostic::error(
                                "E1315",
                                format!(
                                    "component '{cname}': device requires a {} bus on '{target}' — {reason}",
                                    bus.kind
                                ),
                            ),
                            &claim_path,
                            claim_source.as_ref(),
                        ));
                        continue;
                    }
                }
            }
            if max_step_rate.is_some() && expected_kind == "stepper_driver_socket" {
                if let Some(step_funcs) = caps.get("step") {
                    let step_pin = connector.pins.get("step").cloned().unwrap_or_default();
                    match step_funcs.iter().find(|f| f.starts_with("tim")) {
                        None => {
                            diagnostics.push(diagnostic_at_source(
                                Diagnostic::error(
                                    "E1310",
                                    format!(
                                        "component '{cname}': max_step_rate is declared, but '{target}' step pin {step_pin} has no timer function (capabilities: {})",
                                        step_funcs.join(", ")
                                    ),
                                ),
                                &claim_path,
                                claim_source.as_ref(),
                            ));
                            continue;
                        }
                        Some(tok) => {
                            let key = format!("{ctrl}.{tok}");
                            if let Some(other) = timer_claims.get(&key) {
                                let d = diagnostic_at_source(
                                    Diagnostic::error(
                                        "E1314",
                                        format!(
                                            "timer conflict: '{cname}' (pin {step_pin}) and '{}' (pin {}) both need {tok} on '{ctrl}'",
                                            other.component, other.pin
                                        ),
                                    ),
                                    &claim_path,
                                    claim_source.as_ref(),
                                );
                                diagnostics.push(related_to_claim(
                                    d,
                                    format!("'{}' reserved {tok} here", other.component),
                                    &other.path,
                                    other.source.as_ref(),
                                ));
                                continue;
                            }
                            timer_claims.insert(
                                key,
                                TimerClaim {
                                    component: cname.clone(),
                                    pin: step_pin,
                                    path: claim_path.clone(),
                                    source: claim_source.clone(),
                                },
                            );
                            constraints.push(format!(
                                "step pin on free timer channel {tok} (max_step_rate)"
                            ));
                        }
                    }
                }
            }
            claims.insert(
                target.to_string(),
                ResourceClaim {
                    component: cname.clone(),
                    path: claim_path,
                    source: claim_source,
                },
            );
            resolved
                .assignments
                .entry(cname.clone())
                .or_default()
                .push(Assignment {
                    requested_by: cname.clone(),
                    via: attr.clone(),
                    resource: ResourceId(target.to_string()),
                    connector_kind: connector.kind.clone(),
                    candidates_considered: vec![target.to_string()],
                    constraints_applied: constraints,
                    pin_capabilities: caps,
                });
        }
    }

    // Search-based allocation: a component with no explicit claim, whose
    // type names a device package that declares `requires.connector`, gets
    // the first free connector of that kind — §11.4 stable ordering: the
    // component iteration is BTreeMap order, and candidates are scanned in
    // BTreeMap connector-id order. All kind-compatible connectors land in
    // `candidates_considered` (§11.5), free or not.
    for (cname, comp) in &doc.components {
        if comp
            .attributes
            .iter()
            .any(|(attr, v)| claim_kind(attr).is_some() && v.as_str().is_some())
        {
            continue; // explicitly claimed above
        }
        let Some(dreq) = device_reqs.get(&comp.kind) else {
            continue; // no device package for this type — nothing to search for
        };
        let Some(required_kind) = dreq.connector.clone() else {
            continue;
        };
        let component_path = format!("components.{cname}");
        let component_source = expanded_source(expanded_sources, &component_path);
        let required_domains = dreq.domains.clone();
        let required_bus = dreq.bus.clone();
        // Which controller to search: an explicit `controller:` attribute,
        // or the only controller when the machine has exactly one.
        let ctrl_name = match comp.attributes.get("controller").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None if doc.controllers.len() == 1 => doc.controllers.keys().next().unwrap().clone(),
            None => {
                diagnostics.push(
                    Diagnostic::error(
                        "E1203",
                        format!(
                            "component '{cname}' needs a {required_kind} but the machine has {} controllers — add 'controller: <name>' to disambiguate",
                            doc.controllers.len()
                        ),
                    )
                    .at(format!("components.{cname}")),
                );
                continue;
            }
        };
        let Some(board) = boards.get(&ctrl_name) else {
            diagnostics.push(
                Diagnostic::error(
                    "E1204",
                    format!("component '{cname}': unknown controller '{ctrl_name}'"),
                )
                .at(format!("components.{cname}.controller")),
            );
            continue;
        };
        let candidates: Vec<String> = board
            .connectors
            .iter()
            .filter(|(_, c)| c.kind == required_kind)
            .map(|(id, _)| format!("{ctrl_name}.{id}"))
            .collect();
        // Voltage domains are a HARD constraint (§10.1): an eligible
        // candidate must carry one of the required domains; a connector
        // with no declared domain never satisfies a non-empty requirement.
        let domain_ok = |target: &str| -> bool {
            required_domains.is_empty()
                || target
                    .split_once('.')
                    .and_then(|(_, port)| board.connectors.get(port))
                    .and_then(|c| c.voltage_domain.as_deref())
                    .is_some_and(|d| required_domains.iter().any(|r| r == d))
        };
        // Step-timing as a hard search filter: with a declared rate, only
        // sockets whose step pin carries an UNRESERVED timer channel are
        // eligible. No chip table ⇒ no filtering (no data, no check).
        let timing_ok = |target: &str| -> bool {
            if max_step_rate.is_none() || required_kind != "stepper_driver_socket" {
                return true;
            }
            let Some((c, port)) = target.split_once('.') else {
                return true;
            };
            let Some(connector) = board.connectors.get(port) else {
                return true;
            };
            let caps = derive_pin_capabilities(chips.get(c), connector);
            match caps.get("step") {
                None => true,
                Some(funcs) => funcs
                    .iter()
                    .find(|f| f.starts_with("tim"))
                    .is_some_and(|tok| !timer_claims.contains_key(&format!("{c}.{tok}"))),
            }
        };
        // §9 bus requirement as a hard search filter, like domains/timing.
        let bus_ok = |target: &str| -> bool {
            let Some(bus) = &required_bus else {
                return true;
            };
            target
                .split_once('.')
                .and_then(|(c, port)| {
                    board
                        .connectors
                        .get(port)
                        .map(|conn| (c, derive_pin_capabilities(chips.get(c), conn)))
                })
                .is_some_and(|(c, caps)| bus_satisfied(chips.get(c), &caps, bus).is_ok())
        };
        let chosen = candidates
            .iter()
            .find(|t| !claims.contains_key(*t) && domain_ok(t) && timing_ok(t) && bus_ok(t));
        let Some(target) = chosen else {
            diagnostics.push(
                Diagnostic::error(
                    "E1205",
                    format!(
                        "component '{cname}': no free {required_kind} connector on '{ctrl_name}' (considered: {})",
                        if candidates.is_empty() {
                            "none of that kind".to_string()
                        } else {
                            candidates.join(", ")
                        }
                    ),
                )
                .at(format!("components.{cname}")),
            );
            continue;
        };
        claims.insert(
            target.clone(),
            ResourceClaim {
                component: cname.clone(),
                path: component_path.clone(),
                source: component_source.clone(),
            },
        );
        let target_caps = target
            .split_once('.')
            .and_then(|(c, port)| {
                board
                    .connectors
                    .get(port)
                    .map(|conn| derive_pin_capabilities(chips.get(c), conn))
            })
            .unwrap_or_default();
        let mut constraints = vec![
            format!("connector kind == {required_kind}"),
            "first free candidate in stable connector order".to_string(),
            "exclusive ownership".to_string(),
        ];
        if !required_domains.is_empty() {
            constraints.push(format!(
                "voltage domain in [{}]",
                required_domains.join(", ")
            ));
        }
        if let Some(bus) = &required_bus {
            if let Ok(matched) = bus_satisfied(chips.get(&ctrl_name as &str), &target_caps, bus) {
                constraints.extend(matched.constraints(bus));
            }
        }
        if max_step_rate.is_some() && required_kind == "stepper_driver_socket" {
            if let Some(tok) = target_caps
                .get("step")
                .and_then(|fs| fs.iter().find(|f| f.starts_with("tim")))
            {
                let (c, port) = target.split_once('.').expect("controller.port shape");
                let step_pin = board
                    .connectors
                    .get(port)
                    .and_then(|conn| conn.pins.get("step").cloned())
                    .unwrap_or_default();
                timer_claims.insert(
                    format!("{c}.{tok}"),
                    TimerClaim {
                        component: cname.clone(),
                        pin: step_pin,
                        path: component_path,
                        source: component_source,
                    },
                );
                constraints.push(format!(
                    "step pin on free timer channel {tok} (max_step_rate)"
                ));
            }
        }
        resolved
            .assignments
            .entry(cname.clone())
            .or_default()
            .push(Assignment {
                requested_by: cname.clone(),
                via: format!("requires.connector ({})", dreq.reference),
                resource: ResourceId(target.clone()),
                connector_kind: required_kind.clone(),
                candidates_considered: candidates.clone(),
                constraints_applied: constraints,
                pin_capabilities: target_caps,
            });
    }

    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return None;
    }
    Some(resolved)
}
