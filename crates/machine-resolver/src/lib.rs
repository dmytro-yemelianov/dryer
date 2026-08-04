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
//! encoding remain separate downstream boundaries.

use dryer_machine_parser::spans::SpanIndex;
use dryer_machine_schema::{
    Component, Diagnostic, Dimension, MachineDoc, Quantity, Severity, SourceSpan,
};
use dryer_package_model::{board::BoardPackageFile, LocalRegistry, PackageRef};
use dryer_resource_model::ResourceId;
use serde::Serialize;
use std::collections::BTreeMap;

/// Resolver phases (§11.2). `phases_run` in the outcome records how far
/// resolution progressed before stopping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Parse,
    SchemaValidation,
    PackageDependencies,
    PackageLoading,
    GraphExpansion,
    CapabilityMatching,
    ResourceAllocation,
    ElectricalValidation,
    SafetyValidation,
    FirmwarePartitioning,
}

/// One explainable assignment (§11.5): which requirement asked, what was
/// considered, what was chosen.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Assignment {
    /// The component that claimed the resource.
    pub requested_by: String,
    /// The attribute the claim came from (`connected_to`, `output`, `input`).
    pub via: String,
    /// The chosen concrete resource (`controller.connector`).
    pub resource: ResourceId,
    /// Connector kind of the chosen resource.
    pub connector_kind: String,
    /// Candidates considered. Explicit claims consider exactly one; the
    /// future search-based allocator will list every kind-compatible
    /// connector here.
    pub candidates_considered: Vec<String>,
    /// Human-readable constraints applied to accept the claim.
    pub constraints_applied: Vec<String>,
    /// Signal → chip capability tokens for the connector's pins, derived by
    /// joining board wiring with the chip's pin-function table
    /// (docs/peripheral-mapping.md). Empty when the board names no chip or
    /// the chip carries no table.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pin_capabilities: BTreeMap<String, Vec<String>>,
}

/// One concrete edge-enforced safety action assigned to a controller during
/// firmware partitioning (§11.2 phase 11, §18.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ControllerSafeState {
    pub component: String,
    pub class: String,
    /// Concrete controller resource (`controller.connector`).
    pub resource: ResourceId,
    pub state: dryer_package_model::safety::SafeState,
    /// Compiled to the controller's 1 us time quantum.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_timeout_us: Option<u64>,
    /// Concrete sensor resource on the same controller when policy requires it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sensor: Option<ResourceId>,
}

/// The resolved graph, v0.1: deterministic assignments keyed by component,
/// plus the full package closure the machine uses.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ResolvedGraph {
    pub assignments: BTreeMap<String, Vec<Assignment>>,
    /// Controller id → deterministic local safety configuration.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub controller_safety: BTreeMap<String, Vec<ControllerSafeState>>,
    /// Every package the resolution selected — explicit pins, implicit
    /// roots (boards, safety profile) and transitive dependencies — as
    /// `namespace/name@version`, sorted. This is what the lockfile pins.
    pub packages: Vec<String>,
}

impl ResolvedGraph {
    /// Explain one component's assignments (the CLI `explain` seed, §11.5).
    pub fn explain(&self, component: &str) -> Option<String> {
        let list = self.assignments.get(component);
        let safety: Vec<&ControllerSafeState> = self
            .controller_safety
            .values()
            .flatten()
            .filter(|binding| binding.component == component)
            .collect();
        if list.is_none() && safety.is_empty() {
            return None;
        }
        let mut s = String::new();
        for a in list.into_iter().flatten() {
            s.push_str(&format!(
                "{} --{}--> {} (kind {})\n  candidates: {}\n  constraints: {}\n",
                a.requested_by,
                a.via,
                a.resource.0,
                a.connector_kind,
                a.candidates_considered.join(", "),
                a.constraints_applied.join("; "),
            ));
            for (signal, funcs) in &a.pin_capabilities {
                s.push_str(&format!("  {signal}: {}\n", funcs.join(" | ")));
            }
        }
        for binding in safety {
            s.push_str(&format!(
                "{} --safe_state--> {} = {}",
                binding.component,
                binding.resource.0,
                binding.state.as_str()
            ));
            if let Some(timeout) = binding.heartbeat_timeout_us {
                s.push_str(&format!(" (heartbeat {} us)", timeout));
            }
            if let Some(sensor) = &binding.sensor {
                s.push_str(&format!(" (sensor {})", sensor.0));
            }
            s.push('\n');
        }
        Some(s)
    }
}

/// Everything a resolution run produces (§11.1, v0.1 subset).
#[derive(Debug)]
pub struct ResolveOutcome {
    pub resolved: Option<ResolvedGraph>,
    pub diagnostics: Vec<Diagnostic>,
    pub phases_run: Vec<Phase>,
}

impl ResolveOutcome {
    pub fn is_ok(&self) -> bool {
        self.resolved.is_some()
            && !self
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequirementConstraint {
    required_by: String,
    requirement: semver::VersionReq,
    source: Option<SourceSpan>,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct BusMatch {
    instance: String,
    latency: Option<String>,
    jitter: Option<String>,
    dma_routes: BTreeMap<String, String>,
}

impl BusMatch {
    fn constraints(&self, requirement: &dryer_package_model::device::BusReq) -> Vec<String> {
        let mut constraints = vec![format!(
            "{} bus via {}{}",
            requirement.kind,
            self.instance,
            requirement
                .min_frequency
                .as_deref()
                .map(|frequency| format!(" (>= {frequency})"))
                .unwrap_or_default()
        )];
        if let (Some(actual), Some(limit)) = (&self.latency, &requirement.max_latency) {
            constraints.push(format!("worst-case bus latency {actual} <= {limit}"));
        }
        if let (Some(actual), Some(limit)) = (&self.jitter, &requirement.max_jitter) {
            constraints.push(format!("worst-case bus jitter {actual} <= {limit}"));
        }
        constraints.extend(self.dma_routes.iter().map(|(signal, channel)| {
            format!("DMA route {}.{signal} via {channel}", self.instance)
        }));
        constraints
    }
}

/// Resolve the physical resources governed by a component's safety policy.
/// Most components own resources directly; logical actuators such as a
/// `stepper_motor` inherit the connector assigned to their declared driver.
fn safety_target_resources(
    resolved: &ResolvedGraph,
    component_name: &str,
    component: &Component,
) -> Result<Vec<ResourceId>, String> {
    let direct = resolved
        .assignments
        .get(component_name)
        .filter(|assignments| !assignments.is_empty());
    let assignments = match direct {
        Some(assignments) => assignments,
        None => {
            let Some(driver) = component
                .attributes
                .get("driver")
                .and_then(|value| value.as_str())
            else {
                return Ok(Vec::new());
            };
            let Some(assignments) = resolved
                .assignments
                .get(driver)
                .filter(|assignments| !assignments.is_empty())
            else {
                return Ok(Vec::new());
            };
            if let Some(assignment) = assignments
                .iter()
                .find(|assignment| assignment.connector_kind != "stepper_driver_socket")
            {
                return Err(format!(
                    "component '{component_name}' names '{driver}' as its driver, but '{}' is a '{}' connector rather than a stepper driver socket",
                    assignment.resource.0, assignment.connector_kind
                ));
            }
            assignments
        }
    };
    let mut resources: Vec<ResourceId> = assignments
        .iter()
        .map(|assignment| assignment.resource.clone())
        .collect();
    resources.sort_by(|left, right| left.0.cmp(&right.0));
    resources.dedup();
    Ok(resources)
}

fn is_sensor_connector_kind(connector_kind: &str) -> bool {
    matches!(connector_kind, "analog_input" | "digital_input")
}

fn sensor_resource_on_controller(
    resolved: &ResolvedGraph,
    component: &Component,
    controller: &str,
) -> Option<ResourceId> {
    let sensor = component
        .attributes
        .get("sensor")
        .and_then(|value| value.as_str())?;
    resolved
        .assignments
        .get(sensor)?
        .iter()
        .filter(|assignment| is_sensor_connector_kind(&assignment.connector_kind))
        .find_map(|assignment| {
            assignment
                .resource
                .0
                .split_once('.')
                .filter(|(candidate, _)| *candidate == controller)
                .map(|_| assignment.resource.clone())
        })
}

fn expanded_source(sources: &BTreeMap<String, SourceSpan>, path: &str) -> Option<SourceSpan> {
    let mut candidate = path.to_string();
    loop {
        if let Some(source) = sources.get(&candidate) {
            return Some(source.clone());
        }
        let i = candidate.rfind(['.', '['])?;
        candidate.truncate(i);
    }
}

fn diagnostic_at_source(
    diagnostic: Diagnostic,
    path: &str,
    source: Option<&SourceSpan>,
) -> Diagnostic {
    let diagnostic = diagnostic.at(path);
    match source {
        Some(source) => diagnostic.with_source(source.clone()),
        None => diagnostic,
    }
}

fn related_to_claim(
    diagnostic: Diagnostic,
    message: String,
    path: &str,
    source: Option<&SourceSpan>,
) -> Diagnostic {
    match source {
        Some(source) => diagnostic.related_source(message, source.clone()),
        None => diagnostic.related_at(message, path),
    }
}

fn locate_diagnostics(diagnostics: &mut [Diagnostic], spans: &SpanIndex) {
    for diagnostic in diagnostics {
        if diagnostic.source.is_none() {
            if let Some(path) = diagnostic.path.as_deref() {
                if let Some(source) = spans.locate_span(path) {
                    diagnostic.line = Some(source.start.line);
                    diagnostic.column = Some(source.start.column);
                    diagnostic.source = Some(source);
                }
            }
        }
        for related in &mut diagnostic.related {
            if related.source.is_none() {
                if let Some(path) = related.path.as_deref() {
                    related.source = spans.locate_span(path);
                }
            }
        }
    }
}

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
    ResolveOutcome {
        resolved: Some(resolved),
        diagnostics: std::mem::take(diagnostics),
        phases_run: phases_run.clone(),
    }
}

/// Join a connector's pins to the chip's pin-function table
/// (docs/peripheral-mapping.md). No chip / no table ⇒ empty: absence of
/// data disables capability checks, it never fakes them.
fn derive_pin_capabilities(
    chip: Option<&dryer_package_model::chip::ChipPackageFile>,
    connector: &dryer_package_model::board::Connector,
) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let Some(chip) = chip else { return out };
    if chip.pin_functions.is_empty() {
        return out;
    }
    for (signal, pin) in &connector.pins {
        if let Some(f) = chip.pin_functions.get(pin) {
            out.insert(signal.clone(), f.clone());
        }
    }
    if let Some(pin) = &connector.pin {
        if let Some(f) = chip.pin_functions.get(pin) {
            out.insert("pin".to_string(), f.clone());
        }
    }
    out
}

/// Does the connector (via its derived capabilities) satisfy a §9 bus
/// requirement? Returns the matched bus instance and its verified evidence,
/// or the reason it fails. Undeclared frequency, timing, or DMA data never
/// satisfies a corresponding hard requirement — silence is not compatibility.
fn bus_satisfied(
    chip: Option<&dryer_package_model::chip::ChipPackageFile>,
    caps: &BTreeMap<String, Vec<String>>,
    req: &dryer_package_model::device::BusReq,
) -> Result<BusMatch, String> {
    let instance = caps.values().flatten().find_map(|tok| {
        let inst = tok.split('.').next().unwrap_or(tok);
        let family = inst.trim_end_matches(|c: char| c.is_ascii_digit());
        (family == req.kind).then(|| inst.to_string())
    });
    let Some(instance) = instance else {
        return Err(format!("no {} function on any connector pin", req.kind));
    };
    let needs_metadata = req.min_frequency.is_some()
        || req.max_latency.is_some()
        || req.max_jitter.is_some()
        || !req.dma_signals.is_empty();
    let declared = chip.and_then(|chip| chip.buses().find(|bus| bus.id == instance));
    if needs_metadata && declared.is_none() {
        return Err(format!(
            "bus '{instance}' has no declared capability metadata"
        ));
    }

    if let Some(min_raw) = &req.min_frequency {
        let min = Quantity::parse_as(min_raw, Dimension::Frequency).map_err(|e| e.to_string())?;
        let frequency = declared.and_then(|bus| bus.max_frequency.as_ref());
        let Some(frequency) = frequency else {
            return Err(format!(
                "bus '{instance}' declares no max_frequency (required >= {min_raw})"
            ));
        };
        let actual =
            Quantity::parse_as(frequency, Dimension::Frequency).map_err(|e| e.to_string())?;
        if actual.value < min.value {
            return Err(format!(
                "bus '{instance}' max_frequency {frequency} < required {min_raw}"
            ));
        }
    }

    let check_time = |field: &str,
                      actual: Option<&String>,
                      limit: Option<&String>|
     -> Result<Option<String>, String> {
        let Some(limit) = limit else { return Ok(None) };
        let Some(actual) = actual else {
            return Err(format!(
                "bus '{instance}' declares no {field} (required <= {limit})"
            ));
        };
        let actual_quantity =
            Quantity::parse_as(actual, Dimension::Time).map_err(|e| e.to_string())?;
        let limit_quantity =
            Quantity::parse_as(limit, Dimension::Time).map_err(|e| e.to_string())?;
        if actual_quantity.value > limit_quantity.value {
            return Err(format!(
                "bus '{instance}' {field} {actual} > required maximum {limit}"
            ));
        }
        Ok(Some(actual.clone()))
    };
    let latency = check_time(
        "worst_case_latency",
        declared.and_then(|bus| bus.worst_case_latency.as_ref()),
        req.max_latency.as_ref(),
    )?;
    let jitter = check_time(
        "worst_case_jitter",
        declared.and_then(|bus| bus.worst_case_jitter.as_ref()),
        req.max_jitter.as_ref(),
    )?;

    let mut dma_routes = BTreeMap::new();
    if !req.dma_signals.is_empty() {
        let chip = chip.ok_or_else(|| "no chip table to verify DMA routing against".to_string())?;
        for signal in &req.dma_signals {
            let route = format!("{instance}.{signal}");
            let Some(channel) = chip.dma_channel_for_route(&route) else {
                return Err(format!("no DMA channel routes '{route}'"));
            };
            dma_routes.insert(signal.clone(), channel.id.clone());
        }
    }
    Ok(BusMatch {
        instance,
        latency,
        jitter,
        dma_routes,
    })
}

/// v0.1 claim-compatibility table: which connector kind each claiming
/// attribute requires. This is a deliberate stopgap — once device packages
/// carry requirement payloads (§9), compatibility comes from the claimed
/// component's device package, not from the attribute name.
fn claim_kind(attr: &str) -> Option<&'static str> {
    match attr {
        "connected_to" => Some("stepper_driver_socket"),
        "output" => Some("power_output"),
        "input" => Some("analog_input"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn registry() -> LocalRegistry {
        LocalRegistry::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages"))
    }

    fn registry_with_conflicting_template() -> LocalRegistry {
        let mut registry = registry();
        let package = registry
            .packages
            .iter_mut()
            .find(|package| package.reference.to_string() == "machines/cartesian-basic@1.0.0")
            .expect("cartesian template package");
        package.dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/template-conflict");
        registry
    }

    fn registry_with_safety_fixture(fixture: &str) -> LocalRegistry {
        let mut registry = registry();
        let package = registry
            .packages
            .iter_mut()
            .find(|package| package.reference.to_string() == "safety-profiles/desktop-fdm@1.0.0")
            .expect("desktop FDM safety profile");
        package.dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture);
        registry
    }

    fn fixture() -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/minimal-cartesian/machine.yaml"),
        )
        .unwrap()
    }

    #[test]
    fn the_fixture_machine_resolves_with_expected_assignments() {
        let o = resolve_source(&fixture(), &registry());
        assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
        assert_eq!(o.phases_run.len(), 10, "all implemented phases ran");
        assert_eq!(*o.phases_run.last().unwrap(), Phase::FirmwarePartitioning);
        let g = o.resolved.unwrap();
        let x = &g.assignments["x_driver"][0];
        assert_eq!(x.resource.0, "mainboard.motor0");
        assert_eq!(x.connector_kind, "stepper_driver_socket");
        let h = &g.assignments["hotend_heater"][0];
        assert_eq!(h.resource.0, "mainboard.heater0");
        let s = &g.assignments["hotend_sensor"][0];
        assert_eq!(s.resource.0, "mainboard.thermistor0");
        assert!(g.explain("x_driver").unwrap().contains("explicit claim"));
    }

    #[test]
    fn resolution_is_deterministic() {
        let a = resolve_source(&fixture(), &registry());
        let b = resolve_source(&fixture(), &registry());
        assert_eq!(a.resolved, b.resolved);
        assert_eq!(
            serde_json::to_string(&a.resolved).unwrap(),
            serde_json::to_string(&b.resolved).unwrap()
        );
        assert_eq!(
            serde_json::to_string(&a.diagnostics).unwrap(),
            serde_json::to_string(&b.diagnostics).unwrap(),
            "diagnostic ranges and related locations are deterministic"
        );
    }

    #[test]
    fn a_double_claim_is_a_conflict_with_actionable_suggestions() {
        let doubled = fixture().replace(
            "  x_driver:\n    type: tmc2209\n    connected_to: mainboard.motor0",
            "  x_driver:\n    type: tmc2209\n    connected_to: mainboard.motor0\n\n  y_driver:\n    type: tmc2209\n    connected_to: mainboard.motor0",
        );
        assert_ne!(doubled, fixture(), "replacement must apply");
        let o = resolve_source(&doubled, &registry());
        assert!(!o.is_ok());
        let conflict = o
            .diagnostics
            .iter()
            .find(|d| d.code == "E1200")
            .expect("conflict diagnostic");
        assert!(conflict.message.contains("x_driver") && conflict.message.contains("y_driver"));
        assert!(
            conflict.suggestions[0].contains("mainboard.motor1"),
            "should suggest the free socket: {:?}",
            conflict.suggestions
        );
        let source = conflict.source.as_ref().expect("second claim source");
        assert_eq!(
            source.path.as_deref(),
            Some("components.y_driver.connected_to")
        );
        assert_eq!((source.start.column, source.end.column), (5, 17));
        assert_eq!(conflict.related.len(), 1);
        let first = conflict.related[0]
            .source
            .as_ref()
            .expect("first claim source");
        assert_eq!(
            first.path.as_deref(),
            Some("components.x_driver.connected_to")
        );
        assert_eq!((first.start.column, first.end.column), (5, 17));
    }

    #[test]
    fn template_claim_conflicts_retain_package_source_ranges() {
        let o = resolve_source(&with_rate(), &registry_with_conflicting_template());
        assert!(!o.is_ok());

        let connector = o
            .diagnostics
            .iter()
            .find(|d| d.code == "E1200")
            .expect("connector conflict");
        let template_claim = connector.related[0]
            .source
            .as_ref()
            .expect("template connector source");
        assert_eq!(
            template_claim.document.as_deref(),
            Some("package:machines/cartesian-basic@1.0.0/package.yaml")
        );
        assert_eq!(
            template_claim.path.as_deref(),
            Some("template.components.a_template_driver.connected_to")
        );

        let timer = o
            .diagnostics
            .iter()
            .find(|d| d.code == "E1314")
            .expect("timer conflict");
        assert_eq!(
            timer
                .source
                .as_ref()
                .and_then(|source| source.path.as_deref()),
            Some("template.components.b_template_driver.connected_to")
        );
        assert_eq!(
            timer.related[0]
                .source
                .as_ref()
                .and_then(|source| source.path.as_deref()),
            Some("template.components.a_template_driver.connected_to")
        );
        assert!(timer.source.as_ref().unwrap().document.is_some());
        assert!(timer.related[0].source.as_ref().unwrap().document.is_some());
    }

    #[test]
    fn wrong_connector_kind_is_rejected() {
        let wrong = fixture().replace("output: mainboard.heater0", "output: mainboard.thermistor0");
        let o = resolve_source(&wrong, &registry());
        assert!(!o.is_ok());
        assert!(o.diagnostics.iter().any(|d| d.code == "E1202"));
    }

    #[test]
    fn unknown_connector_lists_available_ones() {
        let wrong = fixture().replace(
            "connected_to: mainboard.motor0",
            "connected_to: mainboard.motor9",
        );
        let o = resolve_source(&wrong, &registry());
        let d = o.diagnostics.iter().find(|d| d.code == "E1201").unwrap();
        assert!(d.suggestions[0].contains("motor0"));
    }

    #[test]
    fn missing_package_and_version_mismatch_stop_resolution_in_phase_3() {
        let missing =
            fixture().replace("boards/example-mainboard@1.0.0", "boards/ghost-board@1.0.0");
        let o = resolve_source(&missing, &registry());
        assert!(o.diagnostics.iter().any(|d| d.code == "E1100"));
        assert_eq!(*o.phases_run.last().unwrap(), Phase::PackageDependencies);

        let mismatched = fixture().replace("devices/tmc2209@2.1.0", "devices/tmc2209@9.9.9");
        let o = resolve_source(&mismatched, &registry());
        assert!(o.diagnostics.iter().any(|d| d.code == "E1101"));
    }

    #[test]
    fn unknown_transport_on_the_board_is_an_error() {
        let bad = fixture().replace("type: usb", "type: ethernet");
        let o = resolve_source(&bad, &registry());
        assert!(o.diagnostics.iter().any(|d| d.code == "E1120"));
    }

    /// Search-based allocation: a tmc2209 with no explicit claim gets the
    /// first FREE stepper socket (motor0 is claimed by x_driver), and its
    /// provenance lists every kind-compatible candidate.
    #[test]
    fn an_unclaimed_device_is_search_allocated_with_full_candidates() {
        // The template's y_driver is itself the search-allocated component:
        // motor0 is explicitly claimed, so it lands on motor1 with every
        // kind-compatible connector recorded.
        let o = resolve_source(&fixture(), &registry());
        assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
        let g = o.resolved.unwrap();
        let y = &g.assignments["y_driver"][0];
        assert_eq!(
            y.resource.0, "mainboard.motor1",
            "motor0 is already claimed"
        );
        assert_eq!(
            y.candidates_considered,
            vec![
                "mainboard.motor0".to_string(),
                "mainboard.motor1".to_string(),
                "mainboard.motor2".to_string(),
                "mainboard.motor3".to_string()
            ],
            "all kind-compatible connectors are recorded"
        );
        assert!(y.via.contains("devices/tmc2209"));
    }

    #[test]
    fn exhausting_the_sockets_is_a_typed_error() {
        // four sockets: x explicit + template y + z + e fill them; w exhausts
        let with_three = fixture().replace(
            "kinematics:",
            "  z_driver:\n    type: tmc2209\n\n  e_driver:\n    type: tmc2209\n\n  w_driver:\n    type: tmc2209\n\nkinematics:",
        );
        let o = resolve_source(&with_three, &registry());
        assert!(!o.is_ok());
        let d = o.diagnostics.iter().find(|d| d.code == "E1205").unwrap();
        assert!(d.message.contains("no free stepper_driver_socket"));
    }

    /// The fixture heater declares `current: 2 A` against a 5 A connector
    /// and passes; raising the draw beyond the connector limit fails.
    #[test]
    fn electrical_validation_compares_draw_against_connector_limit() {
        let over = fixture().replace("current: 2 A", "current: 6 A");
        assert_ne!(over, fixture());
        let o = resolve_source(&over, &registry());
        assert!(!o.is_ok());
        let d = o.diagnostics.iter().find(|d| d.code == "E1300").unwrap();
        assert!(d.message.contains("6 A") && d.message.contains("5 A"));
        assert_eq!(*o.phases_run.last().unwrap(), Phase::ElectricalValidation);
    }

    #[test]
    fn a_malformed_current_declaration_is_its_own_error() {
        let bad = fixture().replace("current: 2 A", "current: quite a lot");
        let o = resolve_source(&bad, &registry());
        assert!(o.diagnostics.iter().any(|d| d.code == "E1301"));
    }

    /// The safety profile is an implicit dependency root, so a missing one
    /// now fails in phase 3 (E1100) — before any allocation work — rather
    /// than surviving to the safety phase.
    #[test]
    fn a_missing_safety_profile_fails_resolution_at_the_package_phase() {
        let bad = fixture().replace("safety-profiles/desktop-fdm", "safety-profiles/ghost");
        let o = resolve_source(&bad, &registry());
        assert!(!o.is_ok());
        let d = o.diagnostics.iter().find(|d| d.code == "E1100").unwrap();
        assert!(d.message.contains("safety-profiles/ghost"));
        assert_eq!(*o.phases_run.last().unwrap(), Phase::PackageDependencies);
    }

    /// A component driving a power output whose class the profile does not
    /// cover is the §30 "unresolved safety default" — a hard error.
    #[test]
    fn an_uncovered_hazardous_output_is_rejected() {
        let laser = fixture().replace("    type: heater", "    type: laser");
        assert_ne!(laser, fixture());
        let o = resolve_source(&laser, &registry());
        assert!(!o.is_ok());
        let d = o.diagnostics.iter().find(|d| d.code == "E1501").unwrap();
        assert!(d.message.contains("laser"));
    }

    /// Graph expansion: the pinned machine class contributes `y_driver`
    /// (search-allocated to the free socket) and a default limit, each
    /// surfaced as an Info diagnostic; the source graph is never mutated.
    #[test]
    fn the_machine_template_expands_components_and_default_limits() {
        let o = resolve_source(&fixture(), &registry());
        assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
        let g = o.resolved.as_ref().unwrap();
        let y = &g.assignments["y_driver"][0];
        assert_eq!(y.resource.0, "mainboard.motor1");
        assert!(y.via.contains("devices/tmc2209"));
        let infos: Vec<&str> = o
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Info)
            .map(|d| d.message.as_str())
            .collect();
        assert!(
            infos.iter().any(|m| m.contains("y_driver")),
            "component contribution surfaced: {infos:?}"
        );
        assert!(
            infos.iter().any(|m| m.contains("max_z_velocity")),
            "limit default surfaced: {infos:?}"
        );
    }

    /// Source-wins: a source component with the template's name shadows it.
    #[test]
    fn a_source_component_shadows_the_template() {
        let with_own_y = fixture().replace(
            "kinematics:",
            "  y_driver:\n    type: tmc2209\n    connected_to: mainboard.motor1\n\nkinematics:",
        );
        let o = resolve_source(&with_own_y, &registry());
        assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
        let g = o.resolved.as_ref().unwrap();
        let y = &g.assignments["y_driver"][0];
        assert_eq!(y.via, "connected_to", "the explicit source claim won");
        assert!(o
            .diagnostics
            .iter()
            .any(|d| d.code == "I1132" && d.message.contains("y_driver")));
    }

    /// A kinematics-type mismatch between source and machine class is
    /// surfaced as a warning, never silently overridden.
    #[test]
    fn a_kinematics_mismatch_with_the_machine_class_warns() {
        let corexy = fixture().replace("  type: cartesian", "  type: corexy");
        let o = resolve_source(&corexy, &registry());
        assert!(o.is_ok(), "warning, not error: {:#?}", o.diagnostics);
        let d = o.diagnostics.iter().find(|d| d.code == "E1130").unwrap();
        assert!(d.message.contains("cartesian") && d.message.contains("corexy"));
    }

    /// §9 bus matching: the adxl345 (spi >= 1 MHz, logic_3v3) is
    /// search-allocated to accel0, whose pins reach spi1 (42 MHz); the
    /// gpio-only accel1 is skipped as a hard filter.
    #[test]
    fn a_bus_requirement_steers_search_to_a_capable_socket() {
        let with_accel =
            fixture().replace("kinematics:", "  imu:\n    type: adxl345\n\nkinematics:");
        let o = resolve_source(&with_accel, &registry());
        assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
        let g = o.resolved.unwrap();
        let imu = &g.assignments["imu"][0];
        assert_eq!(imu.resource.0, "mainboard.accel0");
        assert!(
            imu.constraints_applied
                .iter()
                .any(|c| c.contains("spi bus via spi1")),
            "constraints: {:?}",
            imu.constraints_applied
        );
    }

    /// An explicit claim onto a socket with no SPI function is E1315.
    #[test]
    fn an_explicit_claim_without_the_required_bus_is_rejected() {
        let bad = fixture().replace(
            "kinematics:",
            "  imu:\n    type: adxl345\n    connected_to: mainboard.accel1\n\nkinematics:",
        );
        let o = resolve_source(&bad, &registry());
        assert!(!o.is_ok());
        let d = o.diagnostics.iter().find(|d| d.code == "E1315").unwrap();
        assert!(d.message.contains("no spi function"), "{}", d.message);
    }

    /// A bus-frequency demand above what the chip declares fails with
    /// both numbers in the message.
    #[test]
    fn a_bus_frequency_above_the_chips_ceiling_is_rejected() {
        let bad = fixture().replace(
            "kinematics:",
            "  cam:\n    type: fast-cam\n    connected_to: mainboard.accel0\n\nkinematics:",
        );
        let o = resolve_source(&bad, &registry());
        assert!(!o.is_ok());
        let d = o.diagnostics.iter().find(|d| d.code == "E1315").unwrap();
        assert!(
            d.message.contains("42 MHz") && d.message.contains("80 MHz"),
            "{}",
            d.message
        );
    }

    /// DMA routes and measured timing bounds are hard requirements and
    /// become human-readable assignment provenance when accepted.
    #[test]
    fn dma_and_timing_budgets_are_matched_and_recorded() {
        let source = fixture().replace(
            "kinematics:",
            "  stream_sensor:\n    type: dma-stream-sensor\n\nkinematics:",
        );
        let outcome = resolve_source(&source, &registry());
        assert!(outcome.is_ok(), "diagnostics: {:#?}", outcome.diagnostics);
        let graph = outcome.resolved.unwrap();
        let assignment = &graph.assignments["stream_sensor"][0];
        assert_eq!(assignment.resource.0, "mainboard.accel0");
        for expected in [
            "worst-case bus latency 20 us <= 50 us",
            "worst-case bus jitter 3 us <= 10 us",
            "DMA route spi1.rx via dma1.ch0",
        ] {
            assert!(
                assignment
                    .constraints_applied
                    .iter()
                    .any(|constraint| constraint == expected),
                "missing '{expected}' in {:?}",
                assignment.constraints_applied
            );
        }
    }

    #[test]
    fn latency_jitter_and_dma_failures_name_the_missing_budget() {
        let registry = registry();
        let chip = registry
            .find("chips", "generic-mcu")
            .unwrap()
            .chip_payload()
            .unwrap();
        let caps = BTreeMap::from([
            ("sck".to_string(), vec!["spi2.sck".to_string()]),
            ("miso".to_string(), vec!["spi2.miso".to_string()]),
        ]);
        let requirement =
            |max_latency, max_jitter, dma_signals| dryer_package_model::device::BusReq {
                kind: "spi".to_string(),
                min_frequency: None,
                dma_signals,
                max_latency,
                max_jitter,
            };

        let latency = bus_satisfied(
            Some(&chip),
            &caps,
            &requirement(Some("50 us".to_string()), None, Vec::new()),
        )
        .unwrap_err();
        assert!(latency.contains("worst_case_latency 80 us"), "{latency}");

        let jitter = bus_satisfied(
            Some(&chip),
            &caps,
            &requirement(None, Some("10 us".to_string()), Vec::new()),
        )
        .unwrap_err();
        assert!(jitter.contains("worst_case_jitter 20 us"), "{jitter}");

        let dma = bus_satisfied(
            Some(&chip),
            &caps,
            &requirement(None, None, vec!["rx".to_string()]),
        )
        .unwrap_err();
        assert!(dma.contains("no DMA channel routes 'spi2.rx'"), "{dma}");
    }

    /// Pin capabilities ride every assignment: the explicit x_driver claim
    /// carries the chip functions behind motor0's pins.
    #[test]
    fn assignments_carry_derived_pin_capabilities() {
        let o = resolve_source(&fixture(), &registry());
        assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
        let g = o.resolved.unwrap();
        let x = &g.assignments["x_driver"][0];
        assert_eq!(x.pin_capabilities["step"], vec!["tim1.ch2", "gpio"]);
        assert_eq!(x.pin_capabilities["dir"], vec!["gpio"]);
        assert!(g.explain("x_driver").unwrap().contains("tim1.ch2"));
    }

    fn with_rate() -> String {
        fixture().replace("  limits:", "  limits:\n    max_step_rate: 100 kHz")
    }

    /// With a step-rate budget, search allocation steers around sockets
    /// whose step pin lacks a timer and reserves the chosen channel.
    #[test]
    fn a_step_rate_budget_steers_search_to_timer_backed_sockets() {
        let o = resolve_source(&with_rate(), &registry());
        assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
        let g = o.resolved.unwrap();
        let y = &g.assignments["y_driver"][0];
        assert_eq!(
            y.resource.0, "mainboard.motor1",
            "motor1's PD5 carries tim3.ch3; motor2 shares x's tim1.ch2"
        );
        assert!(y.constraints_applied.iter().any(|c| c.contains("tim3.ch3")));
    }

    /// An explicit claim onto a gpio-only step pin violates the budget.
    #[test]
    fn a_declared_step_rate_rejects_gpio_only_step_pins() {
        let bad = with_rate().replace(
            "kinematics:",
            "  z_driver:\n    type: tmc2209\n    connected_to: mainboard.motor3\n\nkinematics:",
        );
        let o = resolve_source(&bad, &registry());
        assert!(!o.is_ok());
        let d = o.diagnostics.iter().find(|d| d.code == "E1310").unwrap();
        assert!(d.message.contains("PD7"), "{}", d.message);
    }

    /// Two sockets multiplexed onto one timer channel cannot both step:
    /// the spec's own E1204-style conflict, now with real operands.
    #[test]
    fn a_shared_timer_channel_is_a_conflict_naming_both_components() {
        let bad = with_rate().replace(
            "kinematics:",
            "  z_driver:\n    type: tmc2209\n    connected_to: mainboard.motor2\n\nkinematics:",
        );
        let o = resolve_source(&bad, &registry());
        assert!(!o.is_ok());
        let d = o.diagnostics.iter().find(|d| d.code == "E1314").unwrap();
        assert!(
            d.message.contains("x_driver") && d.message.contains("tim1.ch2"),
            "{}",
            d.message
        );
        assert_eq!(d.related.len(), 1);
        assert_eq!(
            d.source.as_ref().and_then(|s| s.path.as_deref()),
            Some("components.z_driver.connected_to")
        );
        assert_eq!(
            d.related[0].source.as_ref().and_then(|s| s.path.as_deref()),
            Some("components.x_driver.connected_to")
        );
    }

    /// Voltage domains are checked on explicit claims: legacy-probe
    /// requires logic_5v, and thermistor0 declares no domain — silence
    /// never satisfies an electrical requirement.
    #[test]
    fn a_voltage_domain_requirement_rejects_an_undeclared_connector() {
        let probed = fixture().replace(
            "  hotend_sensor:\n    type: thermistor\n    model: generic-3950\n    input: mainboard.thermistor0",
            "  hotend_sensor:\n    type: thermistor\n    model: generic-3950\n\n  z_probe:\n    type: legacy-probe\n    input: mainboard.thermistor0",
        );
        assert_ne!(probed, fixture(), "replacement must apply");
        let o = resolve_source(&probed, &registry());
        assert!(!o.is_ok());
        let d = o.diagnostics.iter().find(|d| d.code == "E1302").unwrap();
        assert!(
            d.message.contains("logic_5v") && d.message.contains("none"),
            "{}",
            d.message
        );
    }

    /// And on search allocation the domain is a hard candidate filter:
    /// the tmc2209 requirement (logic_3v3) matches the fixture sockets,
    /// and the constraint is recorded in the assignment provenance.
    #[test]
    fn search_allocation_records_the_voltage_domain_constraint() {
        let o = resolve_source(&fixture(), &registry());
        assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
        let g = o.resolved.unwrap();
        let y = &g.assignments["y_driver"][0];
        assert!(
            y.constraints_applied
                .iter()
                .any(|c| c.contains("logic_3v3")),
            "constraints: {:?}",
            y.constraints_applied
        );
    }

    /// The closure pulls tmc2209's chip dependency at the highest version
    /// satisfying its range, and records implicit roots (board, safety
    /// profile) alongside explicit pins.
    #[test]
    fn the_closure_selects_transitive_dependencies_at_max_satisfying_version() {
        let o = resolve_source(&fixture(), &registry());
        assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
        let pkgs = o.resolved.unwrap().packages;
        assert!(
            pkgs.contains(&"chips/generic-mcu@1.5.0".to_string()),
            "transitive dep at max satisfying (1.5.0 supersedes 1.4.0): {pkgs:?}"
        );
        assert!(pkgs.contains(&"boards/example-mainboard@1.0.0".to_string()));
        assert!(pkgs.contains(&"safety-profiles/desktop-fdm@1.0.0".to_string()));
        // the unpinned implicit safety profile carries a warning, not an error
        assert!(o.diagnostics.iter().any(|d| d.code == "E1106"));
    }

    /// A machine pin that a dependency range excludes is an error — pins
    /// are absolute, never silently overridden (§11.4).
    #[test]
    fn a_machine_pin_excluded_by_a_dependency_range_is_rejected() {
        let pinned_old = fixture().replace(
            "  - devices/tmc2209@2.1.0",
            "  - devices/tmc2209@2.1.0\n  - chips/generic-mcu@1.0.0",
        );
        let o = resolve_source(&pinned_old, &registry());
        assert!(!o.is_ok());
        let d = o.diagnostics.iter().find(|d| d.code == "E1103").unwrap();
        assert!(d.message.contains("devices/tmc2209") && d.message.contains("1.0.0"));
        assert_eq!(
            d.source.as_ref().and_then(|s| s.path.as_deref()),
            Some("packages[2]")
        );
        assert_eq!(d.related.len(), 1);
        let requirement = d.related[0]
            .source
            .as_ref()
            .expect("dependency requirement source");
        assert_eq!(
            requirement.document.as_deref(),
            Some("package:devices/tmc2209@2.1.0/package.yaml")
        );
        assert_eq!(
            requirement.path.as_deref(),
            Some("dependencies.chips/generic-mcu")
        );
    }

    /// Two requirers with disjoint ranges on one package: no version can
    /// satisfy the intersection — a conflict naming both requirers.
    #[test]
    fn disjoint_dependency_ranges_are_a_conflict_naming_both_requirers() {
        let both = fixture().replace(
            "  - devices/tmc2209@2.1.0",
            "  - devices/tmc2209@2.1.0\n  - devices/legacy-probe@1.0.0",
        );
        let o = resolve_source(&both, &registry());
        assert!(!o.is_ok());
        let d = o.diagnostics.iter().find(|d| d.code == "E1104").unwrap();
        assert!(
            d.message.contains("tmc2209") && d.message.contains("legacy-probe"),
            "both requirers named: {}",
            d.message
        );
        assert!(d.message.contains("available: 1.0.0, 1.2.0"));
        let mut sources: Vec<_> = d
            .source
            .iter()
            .chain(d.related.iter().filter_map(|r| r.source.as_ref()))
            .map(|source| {
                (
                    source.document.clone().unwrap(),
                    source.path.clone().unwrap(),
                )
            })
            .collect();
        sources.sort();
        assert_eq!(
            sources,
            vec![
                (
                    "package:devices/legacy-probe@1.0.0/package.yaml".to_string(),
                    "dependencies.chips/generic-mcu".to_string(),
                ),
                (
                    "package:devices/tmc2209@2.1.0/package.yaml".to_string(),
                    "dependencies.chips/generic-mcu".to_string(),
                ),
            ]
        );
        let repeated = resolve_source(&both, &registry());
        assert_eq!(
            serde_json::to_string(&o.diagnostics).unwrap(),
            serde_json::to_string(&repeated.diagnostics).unwrap(),
            "multi-source conflict output must be byte-stable"
        );
    }

    #[test]
    fn safe_states_are_partitioned_to_concrete_controller_resources() {
        let outcome = resolve_source(&fixture(), &registry());
        assert!(outcome.is_ok(), "diagnostics: {:#?}", outcome.diagnostics);
        assert_eq!(
            outcome.phases_run.last(),
            Some(&Phase::FirmwarePartitioning)
        );
        let graph = outcome.resolved.unwrap();
        let safety = &graph.controller_safety["mainboard"];
        assert_eq!(safety.len(), 2, "{safety:#?}");

        let heater = safety
            .iter()
            .find(|binding| binding.component == "hotend_heater")
            .unwrap();
        assert_eq!(heater.resource.0, "mainboard.heater0");
        assert_eq!(heater.state, dryer_package_model::safety::SafeState::Off);
        assert_eq!(heater.heartbeat_timeout_us, Some(500_000));
        assert_eq!(
            heater.sensor.as_ref().map(|resource| resource.0.as_str()),
            Some("mainboard.thermistor0")
        );

        let motor = safety
            .iter()
            .find(|binding| binding.component == "x_motor")
            .unwrap();
        assert_eq!(motor.resource.0, "mainboard.motor0");
        assert_eq!(
            motor.state,
            dryer_package_model::safety::SafeState::Disabled
        );
        assert!(graph
            .explain("x_motor")
            .unwrap()
            .contains("safe_state--> mainboard.motor0 = disabled"));
    }

    #[test]
    fn an_unresolved_required_sensor_cannot_enter_controller_firmware() {
        let bad = fixture().replace("    input: mainboard.thermistor0\n", "");
        assert_ne!(bad, fixture(), "replacement must apply");
        let outcome = resolve_source(&bad, &registry());
        assert!(!outcome.is_ok());
        let diagnostic = outcome
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "E1503")
            .unwrap();
        assert!(
            diagnostic.message.contains("hotend_sensor"),
            "{}",
            diagnostic.message
        );
    }

    #[test]
    fn a_driver_backed_actuator_requires_profile_coverage() {
        let outcome = resolve_source(
            &fixture(),
            &registry_with_safety_fixture("safety-no-stepper"),
        );
        assert!(!outcome.is_ok());
        let diagnostic = outcome
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "E1501")
            .unwrap();
        assert!(
            diagnostic.message.contains("x_motor") && diagnostic.message.contains("stepper_motor"),
            "{}",
            diagnostic.message
        );
    }

    #[test]
    fn an_actuator_driver_must_resolve_to_a_driver_socket() {
        let bad = fixture().replace("    driver: x_driver\n", "    driver: hotend_sensor\n");
        assert_ne!(bad, fixture(), "replacement must apply");
        let outcome = resolve_source(&bad, &registry());
        assert!(!outcome.is_ok());
        let diagnostic = outcome
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "E1506")
            .unwrap();
        assert!(
            diagnostic.message.contains("thermistor0")
                && diagnostic.message.contains("analog_input"),
            "{}",
            diagnostic.message
        );
    }

    #[test]
    fn a_required_sensor_must_resolve_to_an_input_connector() {
        let bad = fixture().replace("    sensor: hotend_sensor\n", "    sensor: x_driver\n");
        assert_ne!(bad, fixture(), "replacement must apply");
        let outcome = resolve_source(&bad, &registry());
        assert!(!outcome.is_ok());
        let diagnostic = outcome
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "E1507")
            .unwrap();
        assert!(
            diagnostic.message.contains("x_driver"),
            "{}",
            diagnostic.message
        );
    }

    #[test]
    fn one_physical_resource_cannot_receive_two_safety_actions() {
        let outcome = resolve_source(
            &fixture(),
            &registry_with_safety_fixture("safety-driver-conflict"),
        );
        assert!(!outcome.is_ok());
        let diagnostic = outcome
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "E1508")
            .unwrap();
        assert!(
            diagnostic.message.contains("x_motor")
                && diagnostic.message.contains("x_driver")
                && diagnostic.message.contains("mainboard.motor0"),
            "{}",
            diagnostic.message
        );
    }

    #[test]
    fn a_safe_actuator_without_a_controller_resource_is_rejected() {
        let bad = fixture().replace("    driver: x_driver\n", "");
        assert_ne!(bad, fixture(), "replacement must apply");
        let outcome = resolve_source(&bad, &registry());
        assert!(!outcome.is_ok());
        let diagnostic = outcome
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "E1505")
            .unwrap();
        assert!(
            diagnostic.message.contains("x_motor"),
            "{}",
            diagnostic.message
        );
    }

    #[test]
    fn a_sensorless_heater_violates_the_profile() {
        let sensorless = fixture().replace("    sensor: hotend_sensor\n", "");
        assert_ne!(sensorless, fixture());
        let o = resolve_source(&sensorless, &registry());
        assert!(!o.is_ok());
        let d = o.diagnostics.iter().find(|d| d.code == "E1502").unwrap();
        assert!(d.message.contains("hotend_heater"));
    }
}
