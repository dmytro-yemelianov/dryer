//! Deterministic Machine Graph resolution (spec §11), v0.1 slice.
//!
//! Implements the resolver's explicit phase structure (§11.2) over the
//! Phase 0 models. Phases 1–2 delegate to `dryer-machine-parser`; phases
//! 3–4 check and load packages; phase 5 is a recorded no-op (no package
//! templates exist yet); phases 6–7 validate and allocate **explicit
//! connector claims** — a component saying `connected_to: mainboard.motor0`
//! claims that connector, and the resolver checks existence, kind
//! compatibility, and exclusivity, then records an explainable assignment
//! (§11.5).
//!
//! Slice 2 added: **search-based allocation** — a component with no
//! explicit claim whose type names a device package with a
//! `requires.connector` payload (§9) gets the first free kind-compatible
//! connector in stable order, with every candidate recorded — and the
//! first **electrical validation** check (§11.2 phase 8): a component's
//! declared `current` draw must fit the connector's `max_current`.
//!
//! Slice 3 added the **safety phase** (profile coverage, §18); slice 5
//! rebuilt phase 3 as **transitive closure resolution**: roots are the
//! machine's pins plus implicit references (boards, safety profile),
//! dependency ranges intersect per package, and each package resolves to
//! the highest satisfying version at a fixpoint — machine pins are
//! absolute. The closure is published on `ResolvedGraph::packages` and is
//! what the lockfile pins.
//!
//! Slice 6 implemented **graph expansion** (machine-class templates,
//! §5.5); slice 7 added **voltage-domain validation**; slice 8 implemented
//! **peripheral mapping** (docs/peripheral-mapping.md): board pins join the
//! chip's pin-function table into per-assignment `pin_capabilities`, board
//! wiring is checked against the chip (E1312), and a declared
//! `max_step_rate` makes step pins require exclusive timer channels —
//! E1310 for gpio-only pins, E1314 for channel conflicts, with search
//! allocation steering around both.
//!
//! Slice 9 completed peripheral mapping with **bus/signal matching**
//! (§9 `requires.bus`): a device's bus family must appear among the
//! connector's derived capabilities and the chip's bus instance must
//! declare a sufficient `max_frequency` (E1315; silence never satisfies a
//! minimum). Device packages now also drive the expected connector kind
//! for explicit claims, retiring the attribute table wherever a device
//! exists.
//!
//! Slice 12 added **DMA routing and measured timing budgets**: device bus
//! requirements may name DMA signals plus maximum worst-case latency and
//! jitter; chip targets publish explicit routes and measured bounds. All are
//! hard search/claim constraints, and accepted evidence is recorded on the
//! assignment. DMA channel ownership/exclusivity remains a firmware concern.
//!
//! Slice 13 compiles validated class policy into concrete, controller-local
//! safe-state bindings (phase 11). Lockfile generation and versioned artifact
//! encoding remain separate downstream boundaries. Slice 14 adds phase 12
//! artifact planning: exact board/chip/native-driver inputs plus validated
//! target, toolchain, memory, feature, protocol, and ABI metadata.

mod allocation;
mod capability;
mod diagnostics;
mod expansion;
mod model;
mod packages;
mod requirements;
mod targets;
#[cfg(test)]
mod tests;

use capability::{
    bus_satisfied, is_sensor_connector_kind, safety_target_resources, sensor_resource_on_controller,
};
use diagnostics::locate_diagnostics;
use dryer_machine_schema::{Diagnostic, Dimension, MachineDoc, Quantity, Severity};
use dryer_package_model::LocalRegistry;
use dryer_resource_model::ResourceId;
pub use model::{
    Assignment, ControllerBuildPlan, ControllerSafeState, Phase, ResolveOutcome, ResolvedGraph,
};
use std::collections::BTreeMap;

/// Resolve a machine manifest source against a local package registry.
///
/// Determinism (§11.4): all iteration is over `BTreeMap`s, so identical
/// inputs produce identical outcomes including diagnostic order.
pub fn resolve_source(source: &str, registry: &LocalRegistry) -> ResolveOutcome {
    let mut phases_run = vec![Phase::Parse, Phase::SchemaValidation];
    let dryer_machine_parser::ParseOutcome {
        doc,
        mut diagnostics,
        spans,
    } = dryer_machine_parser::parse_str(source);
    let Some(doc) = doc else {
        let mut outcome = ResolveOutcome {
            resolved: None,
            diagnostics,
            phases_run,
        };
        locate_diagnostics(&mut outcome.diagnostics, &spans);
        return outcome;
    };
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        let mut outcome = ResolveOutcome {
            resolved: None,
            diagnostics,
            phases_run,
        };
        locate_diagnostics(&mut outcome.diagnostics, &spans);
        return outcome;
    }
    let mut outcome = resolve_doc(&doc, registry, &mut diagnostics, &mut phases_run);
    locate_diagnostics(&mut outcome.diagnostics, &spans);
    outcome
}

fn resolve_doc(
    doc: &MachineDoc,
    registry: &LocalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
    phases_run: &mut Vec<Phase>,
) -> ResolveOutcome {
    let fail = |diagnostics: Vec<Diagnostic>, phases_run: Vec<Phase>| ResolveOutcome {
        resolved: None,
        diagnostics,
        phases_run,
    };

    // --- Phase 3: package dependency resolution (transitive closure) ---
    //
    // Roots are the machine's explicit pins plus its implicit package
    // references (controller boards, the safety profile). Dependencies are
    // resolved to a fixpoint: each package's version is the HIGHEST
    // registry version satisfying the intersection of every requirer's
    // range (§11.4 stable selection rule); a machine pin is absolute and
    // conflicts with it are errors, never silent overrides.
    phases_run.push(Phase::PackageDependencies);
    let packages = packages::PackageSelection::resolve(doc, registry, diagnostics);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

    // --- Phase 4: package loading (board payloads per controller) ------
    phases_run.push(Phase::PackageLoading);
    let targets::ControllerTargets {
        boards,
        chips,
        chip_refs,
    } = targets::load(doc, &packages, diagnostics);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

    // --- Phase 5: graph expansion (§5.5) --------------------------------
    // Every machine-kind package in the closure contributes its template,
    // in sorted package order. The SOURCE graph is never mutated: expansion
    // produces `expanded`, and all later phases read that. Source wins —
    // a template never overrides a user declaration; every contribution
    // and shadowing is surfaced as an Info diagnostic so the expanded
    // graph stays explainable.
    phases_run.push(Phase::GraphExpansion);
    let expansion::ExpandedGraph {
        doc: expanded,
        sources: expanded_sources,
    } = expansion::expand(doc, &packages, diagnostics);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }
    let doc = &expanded;

    // Device requirements (§9), resolved once per component type over the
    // EXPANDED graph. This is what replaces the v0 attribute→kind table
    // wherever a device package exists: compatibility comes from the
    // device, the attribute only supplies the claim syntax.
    let device_reqs = requirements::collect(doc, &packages);

    let Some(mut resolved) = allocation::allocate(
        allocation::Inputs {
            doc,
            expanded_sources: &expanded_sources,
            device_reqs: &device_reqs,
            boards: &boards,
            chips: &chips,
            packages: &packages,
        },
        diagnostics,
        phases_run,
    ) else {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    };

    // --- Phase 8: electrical validation ---------------------------------
    // (a) a component's declared `current` draw must fit the connector's
    //     `max_current`; (b) a device's required voltage domains must
    //     include the assigned connector's; (c) bus frequency, measured
    //     latency/jitter, and DMA routes must satisfy the device package.
    //     Silence never satisfies a declared requirement.
    phases_run.push(Phase::ElectricalValidation);
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

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

    // --- Phase 10: safety validation (coverage check) --------------------
    // The profile must exist as a safety-profile package, and every
    // component that resolved a hazardous output (a power_output connector)
    // or delegates an actuator output to a driver must belong to a class the
    // profile covers (§18.2). Delegated resources must be real driver sockets.
    // Classes may add structural requirements (requires_sensor, §18.3). A
    // required sensor must resolve through an input connector on the same
    // controller so edge enforcement never depends on a host or link.
    phases_run.push(Phase::SafetyValidation);
    if !doc.safety.profile.contains('/') {
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
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }
    let safety_profile = match packages.select(&doc.safety.profile) {
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
            None
        }
        Some(pkg) => match pkg.safety_profile_payload() {
            Err(errs) => {
                diagnostics.extend(errs);
                None
            }
            Ok(profile) => Some(profile),
        },
    };
    if let Some(profile) = &safety_profile {
        for (cname, comp) in &expanded.components {
            let hazardous = resolved
                .assignments
                .get(cname)
                .into_iter()
                .flatten()
                .any(|assignment| assignment.connector_kind == "power_output");
            let targets = match safety_target_resources(&resolved, cname, comp) {
                Ok(targets) => targets,
                Err(message) => {
                    diagnostics.push(
                        Diagnostic::error("E1506", message)
                            .at(format!("components.{cname}.driver")),
                    );
                    continue;
                }
            };
            let driver_backed = resolved.assignments.get(cname).map_or(true, Vec::is_empty)
                && comp
                    .attributes
                    .get("driver")
                    .and_then(|value| value.as_str())
                    .is_some()
                && !targets.is_empty();
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
            for target in targets {
                let controller = target.0.split_once('.').map(|(name, _)| name);
                if controller.is_some_and(|controller| {
                    sensor_resource_on_controller(&resolved, comp, controller).is_none()
                }) {
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
    }

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

    // --- Phase 11: firmware partitioning --------------------------------
    // Convert policy strings/quantities into concrete controller-local
    // resources and integer controller time. `machine-lock` pins this
    // projection and `firmware-build` wraps it in a versioned artifact.
    phases_run.push(Phase::FirmwarePartitioning);
    if let Some(profile) = safety_profile {
        let mut safety_owners: BTreeMap<ResourceId, String> = BTreeMap::new();
        for (cname, comp) in &expanded.components {
            let Some(policy) = profile.classes.get(&comp.kind) else {
                continue;
            };
            let Ok(resources) = safety_target_resources(&resolved, cname, comp) else {
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
                    .then(|| sensor_resource_on_controller(&resolved, comp, controller))
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
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

    // --- Phase 12: artifact planning ------------------------------------
    // Select exact board/chip packages and compile target quantities before
    // lock generation. Firmware-build consumes only this locked projection;
    // it never guesses a target or rereads package metadata.
    phases_run.push(Phase::ArtifactPlanning);
    for (controller_name, controller) in &doc.controllers {
        let Some(chip) = chips.get(controller_name) else {
            diagnostics.push(
                Diagnostic::error(
                    "E1600",
                    format!(
                        "controller '{controller_name}' has no resolved chip for firmware artifact planning"
                    ),
                )
                .at(format!("controllers.{controller_name}.board")),
            );
            continue;
        };
        let (Some(memory), Some(boot), Some(firmware), Some(chip_reference)) = (
            chip.memory.as_ref(),
            chip.boot.as_ref(),
            chip.firmware.as_ref(),
            chip_refs.get(controller_name),
        ) else {
            diagnostics.push(
                Diagnostic::error(
                    "E1600",
                    format!(
                        "controller '{controller_name}' chip package lacks memory, boot, or firmware target metadata"
                    ),
                )
                .at(format!("controllers.{controller_name}.board")),
            );
            continue;
        };
        let (Some(flash_bytes), Some(ram_bytes)) = (memory.flash_bytes(), memory.ram_bytes())
        else {
            diagnostics.push(
                Diagnostic::error(
                    "E1601",
                    format!(
                        "controller '{controller_name}' chip memory cannot compile to whole bytes"
                    ),
                )
                .at(format!("controllers.{controller_name}.board")),
            );
            continue;
        };
        let Some(board_version) = packages.version(&controller.board) else {
            diagnostics.push(
                Diagnostic::error(
                    "E1602",
                    format!(
                        "controller '{controller_name}' board '{}' has no exact selected version",
                        controller.board
                    ),
                )
                .at(format!("controllers.{controller_name}.board")),
            );
            continue;
        };

        let mut features = firmware.features.clone();
        features.sort();
        features.dedup();
        let native_drivers: std::collections::BTreeSet<String> = doc
            .components
            .iter()
            .filter(|(component_name, _)| {
                resolved
                    .assignments
                    .get(*component_name)
                    .into_iter()
                    .flatten()
                    .any(|assignment| {
                        assignment
                            .resource
                            .0
                            .split_once('.')
                            .is_some_and(|(candidate, _)| candidate == controller_name)
                    })
            })
            .filter_map(|(_, component)| {
                device_reqs
                    .get(&component.kind)
                    .map(|requirement| requirement.reference.clone())
            })
            .collect();

        resolved.controller_build_plans.insert(
            controller_name.clone(),
            ControllerBuildPlan {
                board: format!("{}@{board_version}", controller.board),
                chip: chip_reference.clone(),
                target_triple: firmware.target_triple.clone(),
                toolchain: firmware.toolchain.clone(),
                build_profile: firmware.build_profile.clone(),
                protocol_version: firmware.protocol_version.clone(),
                abi_version: firmware.abi_version.clone(),
                flash_bytes,
                ram_bytes,
                bootloader_offset_bytes: boot.default_bootloader_offset,
                features,
                native_drivers: native_drivers.into_iter().collect(),
            },
        );
    }
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }
    ResolveOutcome {
        resolved: Some(resolved),
        diagnostics: std::mem::take(diagnostics),
        phases_run: phases_run.clone(),
    }
}
