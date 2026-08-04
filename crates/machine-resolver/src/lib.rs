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

mod capability;
mod diagnostics;
mod model;
#[cfg(test)]
mod tests;

use capability::{
    bus_satisfied, claim_kind, derive_pin_capabilities, is_sensor_connector_kind,
    safety_target_resources, sensor_resource_on_controller,
};
use diagnostics::{diagnostic_at_source, expanded_source, locate_diagnostics, related_to_claim};
use dryer_machine_parser::spans::SpanIndex;
use dryer_machine_schema::{Diagnostic, Dimension, MachineDoc, Quantity, Severity, SourceSpan};
use dryer_package_model::{board::BoardPackageFile, LocalRegistry, PackageRef};
use dryer_resource_model::ResourceId;
pub use model::{
    Assignment, ControllerBuildPlan, ControllerSafeState, Phase, ResolveOutcome, ResolvedGraph,
};
use model::{RequirementConstraint, ResourceClaim, TimerClaim};
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
    let mut pins: BTreeMap<String, semver::Version> = BTreeMap::new();
    let mut pin_paths: BTreeMap<String, String> = BTreeMap::new();
    for (index, pkg) in doc.packages.iter().enumerate() {
        // syntax already validated by the parser
        if let Ok(r) = PackageRef::parse(pkg) {
            let path = format!("{}/{}", r.namespace, r.name);
            pins.insert(path.clone(), r.version);
            pin_paths.insert(path, format!("packages[{index}]"));
        }
    }
    let mut implicit_roots: std::collections::BTreeSet<String> = doc
        .controllers
        .values()
        .map(|c| c.board.clone())
        .filter(|b| b.contains('/'))
        .collect();
    if doc.safety.profile.contains('/') {
        implicit_roots.insert(doc.safety.profile.clone());
    }

    let package_spans: BTreeMap<String, SpanIndex> = registry
        .packages
        .iter()
        .filter_map(|package| {
            let source = std::fs::read_to_string(package.dir.join("package.yaml")).ok()?;
            let reference = package.reference.to_string();
            let document = format!("package:{reference}/package.yaml");
            Some((reference, SpanIndex::build_named(&source, document)))
        })
        .collect();

    type Constraints = BTreeMap<String, Vec<RequirementConstraint>>;
    let mut constraints: Constraints = BTreeMap::new();
    let mut chosen: BTreeMap<String, semver::Version> = BTreeMap::new();
    let mut phase_errs: Vec<Diagnostic> = Vec::new();
    let mut phase_warns: Vec<Diagnostic> = Vec::new();
    let mut converged = false;
    for _round in 0..64 {
        let mut next: BTreeMap<String, semver::Version> = BTreeMap::new();
        let mut errs: Vec<Diagnostic> = Vec::new();
        let mut warns: Vec<Diagnostic> = Vec::new();
        let paths: std::collections::BTreeSet<String> = pins
            .keys()
            .chain(implicit_roots.iter())
            .chain(constraints.keys())
            .cloned()
            .collect();
        for path in &paths {
            let Some((ns, name)) = path.split_once('/') else {
                continue;
            };
            let available = registry.versions(ns, name);
            if available.is_empty() {
                errs.push(
                    Diagnostic::error("E1100", format!("package '{path}' is not in the registry"))
                        .at("packages"),
                );
                continue;
            }
            let reqs = constraints.get(path).cloned().unwrap_or_default();
            if let Some(pin) = pins.get(path) {
                if !available.contains(&pin) {
                    errs.push(
                        Diagnostic::error(
                            "E1101",
                            format!(
                                "package '{path}' pinned at {pin} but the registry has {}",
                                available
                                    .iter()
                                    .map(|v| v.to_string())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        )
                        .at(pin_paths
                            .get(path)
                            .cloned()
                            .unwrap_or_else(|| "packages".to_string())),
                    );
                    continue;
                }
                let mut excluded = false;
                for constraint in &reqs {
                    if !constraint.requirement.matches(pin) {
                        excluded = true;
                        let mut diagnostic = Diagnostic::error(
                            "E1103",
                            format!(
                                "'{}' requires '{path}' {} but the machine pins {pin}",
                                constraint.required_by, constraint.requirement
                            ),
                        )
                        .at(pin_paths
                            .get(path)
                            .cloned()
                            .unwrap_or_else(|| "packages".to_string()));
                        if let Some(source) = &constraint.source {
                            diagnostic = diagnostic.related_source(
                                format!(
                                    "requirement from '{}' declared here",
                                    constraint.required_by
                                ),
                                source.clone(),
                            );
                        }
                        errs.push(diagnostic);
                    }
                }
                if !excluded {
                    next.insert(path.clone(), pin.clone());
                }
            } else {
                let sat: Vec<&semver::Version> = available
                    .iter()
                    .copied()
                    .filter(|v| reqs.iter().all(|c| c.requirement.matches(v)))
                    .collect();
                match sat.last() {
                    Some(v) => {
                        if reqs.is_empty() && implicit_roots.contains(path) {
                            warns.push(Diagnostic::warning(
                                "E1106",
                                format!(
                                    "'{path}' is not version-pinned; selected {v} (highest available)"
                                ),
                            ));
                        }
                        next.insert(path.clone(), (*v).clone());
                    }
                    None => {
                        let mut diagnostic = Diagnostic::error(
                            "E1104",
                            format!(
                                "no version of '{path}' satisfies {}; available: {}",
                                reqs.iter()
                                    .map(|c| format!("'{}' ({})", c.required_by, c.requirement))
                                    .collect::<Vec<_>>()
                                    .join(" and "),
                                available
                                    .iter()
                                    .map(|v| v.to_string())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        );
                        let mut has_primary_source = false;
                        for constraint in &reqs {
                            let message = format!(
                                "'{}' requires '{path}' {}",
                                constraint.required_by, constraint.requirement
                            );
                            if let Some(source) = &constraint.source {
                                if has_primary_source {
                                    diagnostic = diagnostic.related_source(message, source.clone());
                                } else {
                                    diagnostic = diagnostic.with_source(source.clone());
                                    has_primary_source = true;
                                }
                            }
                        }
                        errs.push(diagnostic);
                    }
                }
            }
        }
        // Re-derive constraints from the manifests of the chosen set.
        let mut new_constraints: Constraints = BTreeMap::new();
        for (path, ver) in &next {
            let (ns, name) = path.split_once('/').expect("paths are ns/name");
            if let Some(p) = registry.find_version(ns, name, ver) {
                for (dep, d) in &p.manifest.dependencies {
                    if let Ok(req) = d.requirement() {
                        let source = package_spans
                            .get(&p.reference.to_string())
                            .and_then(|index| index.locate_span(&format!("dependencies.{dep}")));
                        new_constraints.entry(dep.clone()).or_default().push(
                            RequirementConstraint {
                                required_by: path.clone(),
                                requirement: req,
                                source,
                            },
                        );
                    }
                }
            }
        }
        let stable = next == chosen && new_constraints == constraints;
        chosen = next;
        constraints = new_constraints;
        phase_errs = errs;
        phase_warns = warns;
        if stable {
            converged = true;
            break;
        }
    }
    if !converged && phase_errs.is_empty() {
        phase_errs.push(Diagnostic::error(
            "E1107",
            "dependency resolution did not converge (cyclic version constraints?)",
        ));
    }
    diagnostics.extend(phase_warns);
    diagnostics.extend(phase_errs);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }
    // The closure: every package the machine uses, with its selected version.
    let select = |path: &str| -> Option<&dryer_package_model::LoadedPackage> {
        let (ns, name) = path.split_once('/')?;
        match chosen.get(path) {
            Some(v) => registry.find_version(ns, name, v),
            None => registry.find(ns, name),
        }
    };
    let closure_refs: Vec<String> = chosen
        .iter()
        .map(|(path, v)| format!("{path}@{v}"))
        .collect();

    // --- Phase 4: package loading (board payloads per controller) ------
    phases_run.push(Phase::PackageLoading);
    let mut boards: BTreeMap<String, BoardPackageFile> = BTreeMap::new();
    let mut chips: BTreeMap<String, dryer_package_model::chip::ChipPackageFile> = BTreeMap::new();
    let mut chip_refs: BTreeMap<String, String> = BTreeMap::new();
    for (cname, ctrl) in &doc.controllers {
        let Some((ns, name)) = ctrl.board.split_once('/') else {
            diagnostics.push(
                Diagnostic::error(
                    "E1110",
                    format!(
                        "controller '{cname}': board '{}' must be 'namespace/name'",
                        ctrl.board
                    ),
                )
                .at(format!("controllers.{cname}.board")),
            );
            continue;
        };
        let _ = (ns, name); // shape validated above; selection is closure-aware
        match select(&ctrl.board) {
            None => diagnostics.push(
                Diagnostic::error(
                    "E1111",
                    format!(
                        "controller '{cname}': board package '{}' is not in the registry",
                        ctrl.board
                    ),
                )
                .at(format!("controllers.{cname}.board")),
            ),
            Some(pkg) => match pkg.board_payload() {
                Ok(payload) => {
                    // transport must exist on the board
                    if !payload.transports.contains_key(&ctrl.transport.kind) {
                        diagnostics.push(
                            Diagnostic::error(
                                "E1120",
                                format!(
                                    "controller '{cname}': board '{}' has no '{}' transport",
                                    ctrl.board, ctrl.transport.kind
                                ),
                            )
                            .at(format!("controllers.{cname}.transport")),
                        );
                    }
                    // Peripheral mapping (docs/peripheral-mapping.md): join
                    // the board's pins to the chip's pin-function table.
                    if let Some(chip_ref) = payload.chip.clone() {
                        let chip_pkg = select(&chip_ref);
                        if let Some(chip_pkg) = chip_pkg {
                            if !chosen.contains_key(&chip_ref) {
                                diagnostics.push(Diagnostic::warning(
                                    "E1311",
                                    format!(
                                        "board '{}' chip '{chip_ref}' is not in the dependency closure; using {} (highest available)",
                                        ctrl.board, chip_pkg.reference.version
                                    ),
                                ));
                            }
                            match chip_pkg.chip_payload() {
                                Ok(chip) => {
                                    // A chip without a pin table disables the
                                    // wiring check — no data, no verdict.
                                    if !chip.pin_functions.is_empty() {
                                        for (cid, conn) in &payload.connectors {
                                            for pin in conn.pins.values().chain(conn.pin.iter()) {
                                                if !chip.pin_functions.contains_key(pin) {
                                                    diagnostics.push(Diagnostic::error(
                                                        "E1312",
                                                        format!(
                                                            "board '{}' connector '{cid}' wires pin {pin}, which chip '{chip_ref}' does not declare",
                                                            ctrl.board
                                                        ),
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                    chip_refs.insert(cname.clone(), chip_pkg.reference.to_string());
                                    chips.insert(cname.clone(), chip);
                                }
                                Err(errs) => diagnostics.extend(errs),
                            }
                        } else {
                            diagnostics.push(Diagnostic::error(
                                "E1313",
                                format!(
                                    "board '{}' references chip '{chip_ref}', which is not in the registry",
                                    ctrl.board
                                ),
                            ));
                        }
                    }
                    boards.insert(cname.clone(), payload);
                }
                Err(errs) => diagnostics.extend(errs),
            },
        }
    }
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
    let mut expanded = doc.clone();
    // Machine-style expanded paths -> exact package-template source. Keeping
    // this beside the expanded graph lets later phases report the document
    // that actually introduced a component instead of asking the machine's
    // SpanIndex to locate a path that never existed there.
    let mut expanded_sources: BTreeMap<String, SourceSpan> = BTreeMap::new();
    for path in chosen.keys() {
        let Some(pkg) = select(path) else { continue };
        if pkg.kind != dryer_package_model::PackageKind::Machine {
            continue;
        }
        let template = match pkg.machine_payload() {
            Ok(p) => match p.template {
                Some(t) => t,
                None => continue,
            },
            Err(errs) => {
                diagnostics.extend(errs);
                continue;
            }
        };
        for (cid, comp) in template.components {
            if !dryer_machine_schema::valid_identifier(&cid) {
                diagnostics.push(Diagnostic::error(
                    "E1131",
                    format!("template component '{cid}' from '{path}' is not a valid identifier"),
                ));
                continue;
            }
            match expanded.components.entry(cid.clone()) {
                std::collections::btree_map::Entry::Occupied(_) => {
                    diagnostics.push(Diagnostic::info(
                        "I1132",
                        format!("source component '{cid}' shadows the template from '{path}'"),
                    ));
                }
                std::collections::btree_map::Entry::Vacant(e) => {
                    diagnostics.push(Diagnostic::info(
                        "I1133",
                        format!("component '{cid}' expanded from '{path}'"),
                    ));
                    if let Some(index) = package_spans.get(&pkg.reference.to_string()) {
                        let template_path = format!("template.components.{cid}");
                        let expanded_path = format!("components.{cid}");
                        if let Some(source) = index.get_span(&template_path) {
                            expanded_sources.insert(expanded_path.clone(), source);
                        }
                        for attr in comp.attributes.keys() {
                            if let Some(source) = index.get_span(&format!("{template_path}.{attr}"))
                            {
                                expanded_sources.insert(format!("{expanded_path}.{attr}"), source);
                            }
                        }
                    }
                    e.insert(comp);
                }
            }
        }
        if let Some(k) = template.kinematics {
            if let Some(kind) = k.kind {
                if kind != expanded.kinematics.kind {
                    diagnostics.push(Diagnostic::warning(
                        "E1130",
                        format!(
                            "machine class '{path}' assumes {kind} kinematics but the source declares {}",
                            expanded.kinematics.kind
                        ),
                    ));
                }
            }
            for (limit, value) in k.limits {
                if let std::collections::btree_map::Entry::Vacant(e) =
                    expanded.kinematics.limits.entry(limit.clone())
                {
                    if Quantity::parse(&value).is_err() {
                        diagnostics.push(Diagnostic::error(
                            "E1134",
                            format!("template limit '{limit}' from '{path}': '{value}' is not a valid quantity"),
                        ));
                    } else {
                        diagnostics.push(Diagnostic::info(
                            "I1133",
                            format!("kinematics limit '{limit}' defaulted from '{path}'"),
                        ));
                        e.insert(value);
                    }
                }
            }
        }
    }
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }
    let doc = &expanded;

    // Device requirements (§9), resolved once per component type over the
    // EXPANDED graph. This is what replaces the v0 attribute→kind table
    // wherever a device package exists: compatibility comes from the
    // device, the attribute only supplies the claim syntax.
    struct DevReq {
        reference: String,
        connector: Option<String>,
        domains: Vec<String>,
        bus: Option<dryer_package_model::device::BusReq>,
    }
    let mut device_reqs: BTreeMap<String, DevReq> = BTreeMap::new();
    for comp in doc.components.values() {
        if device_reqs.contains_key(&comp.kind) {
            continue;
        }
        let Some(dev) = select(&format!("devices/{}", comp.kind)) else {
            continue;
        };
        let Ok(payload) = dev.device_payload() else {
            continue; // payload errors surface when the device is used
        };
        let r = payload.requires;
        device_reqs.insert(
            comp.kind.clone(),
            DevReq {
                reference: dev.reference.to_string(),
                connector: r.as_ref().and_then(|r| r.connector.clone()),
                domains: r
                    .as_ref()
                    .map(|r| r.voltage_domains.clone())
                    .unwrap_or_default(),
                bus: r.and_then(|r| r.bus),
            },
        );
    }

    // --- Phase 6+7: capability matching & explicit-claim allocation ----
    phases_run.push(Phase::CapabilityMatching);
    phases_run.push(Phase::ResourceAllocation);

    let mut resolved = ResolvedGraph {
        packages: closure_refs,
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
            let Some(target) = val.as_str() else { continue };
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
            let claim_source = expanded_source(&expanded_sources, &claim_path);
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
        let component_source = expanded_source(&expanded_sources, &component_path);
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
                        if candidates.is_empty() { "none of that kind".to_string() } else { candidates.join(", ") }
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

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

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
    let safety_profile = match select(&doc.safety.profile) {
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
        let Some(board_version) = chosen.get(&controller.board) else {
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
