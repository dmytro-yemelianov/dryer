use crate::model::RequirementConstraint;
use dryer_machine_parser::spans::SpanIndex;
use dryer_machine_schema::{Diagnostic, MachineDoc};
use dryer_package_model::{LoadedPackage, LocalRegistry, PackageRef};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct PackageSelection<'a> {
    registry: &'a LocalRegistry,
    chosen: BTreeMap<String, semver::Version>,
    package_spans: BTreeMap<String, SpanIndex>,
    closure_refs: Vec<String>,
}

impl<'a> PackageSelection<'a> {
    pub(super) fn resolve(
        doc: &MachineDoc,
        registry: &'a LocalRegistry,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Self {
        let mut pins: BTreeMap<String, semver::Version> = BTreeMap::new();
        let mut pin_paths: BTreeMap<String, String> = BTreeMap::new();
        for (index, pkg) in doc.packages.iter().enumerate() {
            // Syntax is already validated by the parser.
            if let Ok(reference) = PackageRef::parse(pkg) {
                let path = format!("{}/{}", reference.namespace, reference.name);
                pins.insert(path.clone(), reference.version);
                pin_paths.insert(path, format!("packages[{index}]"));
            }
        }
        let mut implicit_roots: BTreeSet<String> = doc
            .controllers
            .values()
            .map(|controller| controller.board.clone())
            .filter(|board| board.contains('/'))
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
            let paths: BTreeSet<String> = pins
                .keys()
                .chain(implicit_roots.iter())
                .chain(constraints.keys())
                .cloned()
                .collect();
            for path in &paths {
                let Some((namespace, name)) = path.split_once('/') else {
                    continue;
                };
                let available = registry.versions(namespace, name);
                if available.is_empty() {
                    errs.push(
                        Diagnostic::error(
                            "E1100",
                            format!("package '{path}' is not in the registry"),
                        )
                        .at("packages"),
                    );
                    continue;
                }
                let requirements = constraints.get(path).cloned().unwrap_or_default();
                if let Some(pin) = pins.get(path) {
                    if !available.contains(&pin) {
                        errs.push(
                            Diagnostic::error(
                                "E1101",
                                format!(
                                    "package '{path}' pinned at {pin} but the registry has {}",
                                    available
                                        .iter()
                                        .map(|version| version.to_string())
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
                    for constraint in &requirements {
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
                    let satisfying: Vec<&semver::Version> = available
                        .iter()
                        .copied()
                        .filter(|version| {
                            requirements
                                .iter()
                                .all(|constraint| constraint.requirement.matches(version))
                        })
                        .collect();
                    match satisfying.last() {
                        Some(version) => {
                            if requirements.is_empty() && implicit_roots.contains(path) {
                                warns.push(Diagnostic::warning(
                                    "E1106",
                                    format!(
                                        "'{path}' is not version-pinned; selected {version} (highest available)"
                                    ),
                                ));
                            }
                            next.insert(path.clone(), (*version).clone());
                        }
                        None => {
                            let mut diagnostic = Diagnostic::error(
                                "E1104",
                                format!(
                                    "no version of '{path}' satisfies {}; available: {}",
                                    requirements
                                        .iter()
                                        .map(|constraint| format!(
                                            "'{}' ({})",
                                            constraint.required_by, constraint.requirement
                                        ))
                                        .collect::<Vec<_>>()
                                        .join(" and "),
                                    available
                                        .iter()
                                        .map(|version| version.to_string())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            );
                            let mut has_primary_source = false;
                            for constraint in &requirements {
                                let message = format!(
                                    "'{}' requires '{path}' {}",
                                    constraint.required_by, constraint.requirement
                                );
                                if let Some(source) = &constraint.source {
                                    if has_primary_source {
                                        diagnostic =
                                            diagnostic.related_source(message, source.clone());
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
            for (path, version) in &next {
                let (namespace, name) = path.split_once('/').expect("paths are ns/name");
                if let Some(package) = registry.find_version(namespace, name, version) {
                    for (dependency, declaration) in &package.manifest.dependencies {
                        if let Ok(requirement) = declaration.requirement() {
                            let source = package_spans
                                .get(&package.reference.to_string())
                                .and_then(|index| {
                                    index.locate_span(&format!("dependencies.{dependency}"))
                                });
                            new_constraints.entry(dependency.clone()).or_default().push(
                                RequirementConstraint {
                                    required_by: path.clone(),
                                    requirement,
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

        let closure_refs = chosen
            .iter()
            .map(|(path, version)| format!("{path}@{version}"))
            .collect();
        Self {
            registry,
            chosen,
            package_spans,
            closure_refs,
        }
    }

    pub(super) fn select(&self, path: &str) -> Option<&'a LoadedPackage> {
        let (namespace, name) = path.split_once('/')?;
        match self.chosen.get(path) {
            Some(version) => self.registry.find_version(namespace, name, version),
            None => self.registry.find(namespace, name),
        }
    }

    pub(super) fn paths(&self) -> impl Iterator<Item = &String> {
        self.chosen.keys()
    }

    pub(super) fn contains(&self, path: &str) -> bool {
        self.chosen.contains_key(path)
    }

    pub(super) fn version(&self, path: &str) -> Option<&semver::Version> {
        self.chosen.get(path)
    }

    pub(super) fn closure_refs(&self) -> &[String] {
        &self.closure_refs
    }

    pub(super) fn span(&self, reference: &str) -> Option<&SpanIndex> {
        self.package_spans.get(reference)
    }
}
