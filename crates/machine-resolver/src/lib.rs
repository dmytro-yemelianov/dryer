//! Deterministic Machine Graph resolution (spec §11), v0.1 slice.
//!
//! Implements the resolver's explicit phase structure (§11.2) over the
//! Phase 0 models. Phases 1–2 delegate to `forge-machine-parser`; phases
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
//! Deliberately NOT here yet: timing validation, firmware partitioning,
//! graph expansion (templates), transitive dependency solving, and
//! `SourceSpan` tracking. Each is a later slice; lockfile generation
//! lives in `forge-machine-lock`.

use forge_machine_schema::{Diagnostic, Dimension, MachineDoc, Quantity, Severity};
use forge_package_model::{board::BoardPackageFile, LocalRegistry, PackageRef};
use forge_resource_model::ResourceId;
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
}

/// The resolved graph, v0.1: deterministic assignments keyed by component.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ResolvedGraph {
    pub assignments: BTreeMap<String, Vec<Assignment>>,
}

impl ResolvedGraph {
    /// Explain one component's assignments (the CLI `explain` seed, §11.5).
    pub fn explain(&self, component: &str) -> Option<String> {
        let list = self.assignments.get(component)?;
        let mut s = String::new();
        for a in list {
            s.push_str(&format!(
                "{} --{}--> {} (kind {})\n  candidates: {}\n  constraints: {}\n",
                a.requested_by,
                a.via,
                a.resource.0,
                a.connector_kind,
                a.candidates_considered.join(", "),
                a.constraints_applied.join("; "),
            ));
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

/// Resolve a machine manifest source against a local package registry.
///
/// Determinism (§11.4): all iteration is over `BTreeMap`s, so identical
/// inputs produce identical outcomes including diagnostic order.
pub fn resolve_source(source: &str, registry: &LocalRegistry) -> ResolveOutcome {
    let mut phases_run = vec![Phase::Parse, Phase::SchemaValidation];
    let parsed = forge_machine_parser::parse_str(source);
    let mut diagnostics = parsed.diagnostics;
    let Some(doc) = parsed.doc else {
        return ResolveOutcome {
            resolved: None,
            diagnostics,
            phases_run,
        };
    };
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return ResolveOutcome {
            resolved: None,
            diagnostics,
            phases_run,
        };
    }
    resolve_doc(&doc, registry, &mut diagnostics, &mut phases_run)
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

    // --- Phase 3: package dependency resolution ------------------------
    phases_run.push(Phase::PackageDependencies);
    let mut pinned: BTreeMap<String, PackageRef> = BTreeMap::new();
    for pkg in &doc.packages {
        // syntax already validated by the parser
        if let Ok(r) = PackageRef::parse(pkg) {
            pinned.insert(format!("{}/{}", r.namespace, r.name), r);
        }
    }
    for (path, r) in &pinned {
        match registry.find(&r.namespace, &r.name) {
            None => diagnostics.push(
                Diagnostic::error("E1100", format!("package '{path}' is not in the registry"))
                    .at("packages"),
            ),
            Some(found) if found.reference.version != r.version => diagnostics.push(
                Diagnostic::error(
                    "E1101",
                    format!(
                        "package '{path}' pinned at {} but the registry has {}",
                        r.version, found.reference.version
                    ),
                )
                .at("packages"),
            ),
            Some(found) => {
                // dependency ranges of the loaded package must be satisfied
                // by other pins (no transitive solving in v0.1 — flat pins).
                for (dep, d) in &found.manifest.dependencies {
                    let Ok(req) = d.requirement() else { continue };
                    match pinned.get(dep) {
                        None => diagnostics.push(Diagnostic::warning(
                            "E1102",
                            format!(
                                "'{path}' depends on '{dep}' ({req}) which is not pinned in this machine"
                            ),
                        )),
                        Some(dep_pin) if !req.matches(&dep_pin.version) => {
                            diagnostics.push(Diagnostic::error(
                                "E1103",
                                format!(
                                    "'{path}' requires '{dep}' {req} but the machine pins {}",
                                    dep_pin.version
                                ),
                            ))
                        }
                        Some(_) => {}
                    }
                }
            }
        }
    }
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

    // --- Phase 4: package loading (board payloads per controller) ------
    phases_run.push(Phase::PackageLoading);
    let mut boards: BTreeMap<String, BoardPackageFile> = BTreeMap::new();
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
        match registry.find(ns, name) {
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
                    boards.insert(cname.clone(), payload);
                }
                Err(errs) => diagnostics.extend(errs),
            },
        }
    }
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

    // --- Phase 5: graph expansion (recorded no-op in v0.1) -------------
    // No package templates exist yet (§5.5 'expanded graph'); recording the
    // phase keeps `phases_run` an honest trace once expansion arrives.
    phases_run.push(Phase::GraphExpansion);

    // --- Phase 6+7: capability matching & explicit-claim allocation ----
    phases_run.push(Phase::CapabilityMatching);
    phases_run.push(Phase::ResourceAllocation);

    let mut resolved = ResolvedGraph::default();
    // connector -> first claimant, for exclusivity conflicts
    let mut claims: BTreeMap<String, String> = BTreeMap::new();

    for (cname, comp) in &doc.components {
        for (attr, val) in &comp.attributes {
            let Some(target) = val.as_str() else { continue };
            let Some(expected_kind) = claim_kind(attr) else {
                continue;
            };
            // parser already guaranteed 'controller.port' shape + controller exists
            let Some((ctrl, port)) = target.split_once('.') else {
                continue;
            };
            let Some(board) = boards.get(ctrl) else {
                continue;
            };

            let Some(connector) = board.connectors.get(port) else {
                let known: Vec<&String> = board.connectors.keys().collect();
                diagnostics.push(
                    Diagnostic::error(
                        "E1201",
                        format!(
                            "component '{cname}': controller '{ctrl}' has no connector '{port}'"
                        ),
                    )
                    .at(format!("components.{cname}.{attr}"))
                    .suggest(format!("available connectors: {known:?}")),
                );
                continue;
            };

            if connector.kind != expected_kind {
                diagnostics.push(
                    Diagnostic::error(
                        "E1202",
                        format!(
                            "component '{cname}': '{target}' is a {} connector, but '{attr}' requires {expected_kind}",
                            connector.kind
                        ),
                    )
                    .at(format!("components.{cname}.{attr}")),
                );
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
                let mut d = Diagnostic::error(
                    "E1200",
                    format!(
                        "connector conflict: '{target}' is claimed by both '{prev}' and '{cname}'"
                    ),
                )
                .at(format!("components.{cname}.{attr}"));
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

            claims.insert(target.to_string(), cname.clone());
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
                    constraints_applied: vec![
                        format!("explicit claim of '{target}'"),
                        format!("connector kind == {expected_kind}"),
                        "exclusive ownership".to_string(),
                    ],
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
        let Some(dev) = registry.find("devices", &comp.kind) else {
            continue; // no device package for this type — nothing to search for
        };
        let Ok(payload) = dev.device_payload() else {
            continue; // payload errors already surface when explicitly used
        };
        let Some(required_kind) = payload.requires.and_then(|r| r.connector) else {
            continue;
        };
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
        let chosen = candidates.iter().find(|t| !claims.contains_key(*t));
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
        claims.insert(target.clone(), cname.clone());
        resolved
            .assignments
            .entry(cname.clone())
            .or_default()
            .push(Assignment {
                requested_by: cname.clone(),
                via: format!("requires.connector ({})", dev.reference),
                resource: ResourceId(target.clone()),
                connector_kind: required_kind.clone(),
                candidates_considered: candidates.clone(),
                constraints_applied: vec![
                    format!("connector kind == {required_kind}"),
                    "first free candidate in stable connector order".to_string(),
                    "exclusive ownership".to_string(),
                ],
            });
    }

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

    // --- Phase 8: electrical validation (first check) -------------------
    // A component declaring its draw (`current: "3 A"`) must fit the
    // connector's `max_current`. Voltage-domain and timing checks are
    // later slices of this phase.
    phases_run.push(Phase::ElectricalValidation);
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
    // must belong to a class the profile covers (§18.2). Classes may add
    // structural requirements (requires_sensor, §18.3). Compiling safe
    // states into firmware artifacts is a later phase; this one guarantees
    // no hazardous output escapes policy — the §30 "no unresolved safety
    // defaults" gate.
    phases_run.push(Phase::SafetyValidation);
    let Some((s_ns, s_name)) = doc.safety.profile.split_once('/') else {
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
    };
    match registry.find(s_ns, s_name) {
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
        }
        Some(pkg) => match pkg.safety_profile_payload() {
            Err(errs) => diagnostics.extend(errs),
            Ok(profile) => {
                for (cname, comp) in &doc.components {
                    let hazardous = resolved
                        .assignments
                        .get(cname)
                        .into_iter()
                        .flatten()
                        .any(|a| a.connector_kind == "power_output");
                    let policy = profile.classes.get(&comp.kind);
                    if hazardous && policy.is_none() {
                        diagnostics.push(
                            Diagnostic::error(
                                "E1501",
                                format!(
                                    "component '{cname}' drives a power output but class '{}' has no policy in '{}'",
                                    comp.kind, doc.safety.profile
                                ),
                            )
                            .at(format!("components.{cname}"))
                            .suggest(format!(
                                "add a '{}' class to the safety profile or use a covered class",
                                comp.kind
                            )),
                        );
                    }
                    if let Some(policy) = policy {
                        if policy.requires_sensor && !comp.attributes.contains_key("sensor") {
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
                        }
                    }
                }
            }
        },
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
        assert_eq!(o.phases_run.len(), 9, "all nine phases ran");
        assert_eq!(*o.phases_run.last().unwrap(), Phase::SafetyValidation);
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
        let with_z = fixture().replace(
            "kinematics:",
            "  z_driver:\n    type: tmc2209\n\nkinematics:",
        );
        let o = resolve_source(&with_z, &registry());
        assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
        let g = o.resolved.unwrap();
        let z = &g.assignments["z_driver"][0];
        assert_eq!(
            z.resource.0, "mainboard.motor1",
            "motor0 is already claimed"
        );
        assert_eq!(
            z.candidates_considered,
            vec![
                "mainboard.motor0".to_string(),
                "mainboard.motor1".to_string()
            ],
            "all kind-compatible connectors are recorded"
        );
        assert!(z.via.contains("devices/tmc2209"));
    }

    #[test]
    fn exhausting_the_sockets_is_a_typed_error() {
        let with_two = fixture().replace(
            "kinematics:",
            "  z_driver:\n    type: tmc2209\n\n  e_driver:\n    type: tmc2209\n\nkinematics:",
        );
        let o = resolve_source(&with_two, &registry());
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

    #[test]
    fn a_missing_safety_profile_fails_resolution() {
        let bad = fixture().replace("safety-profiles/desktop-fdm", "safety-profiles/ghost");
        let o = resolve_source(&bad, &registry());
        assert!(!o.is_ok());
        assert!(o.diagnostics.iter().any(|d| d.code == "E1500"));
        assert_eq!(*o.phases_run.last().unwrap(), Phase::SafetyValidation);
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
