use crate::packages::PackageSelection;
use dryer_machine_schema::{Diagnostic, MachineDoc, Quantity, SourceSpan};
use dryer_package_model::PackageKind;
use std::collections::BTreeMap;

pub(super) struct ExpandedGraph {
    pub(super) doc: MachineDoc,
    pub(super) sources: BTreeMap<String, SourceSpan>,
}

pub(super) fn expand(
    doc: &MachineDoc,
    packages: &PackageSelection<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ExpandedGraph {
    let mut expanded = doc.clone();
    // Machine-style expanded paths -> exact package-template source. Keeping
    // this beside the expanded graph lets later phases report the document
    // that actually introduced a component instead of asking the machine's
    // SpanIndex to locate a path that never existed there.
    let mut sources = BTreeMap::new();
    for path in packages.paths() {
        let Some(package) = packages.select(path) else {
            continue;
        };
        if package.kind != PackageKind::Machine {
            continue;
        }
        let template = match package.machine_payload() {
            Ok(payload) => match payload.template {
                Some(template) => template,
                None => continue,
            },
            Err(errors) => {
                diagnostics.extend(errors);
                continue;
            }
        };
        for (component_id, component) in template.components {
            if !dryer_machine_schema::valid_identifier(&component_id) {
                diagnostics.push(Diagnostic::error(
                    "E1131",
                    format!(
                        "template component '{component_id}' from '{path}' is not a valid identifier"
                    ),
                ));
                continue;
            }
            match expanded.components.entry(component_id.clone()) {
                std::collections::btree_map::Entry::Occupied(_) => {
                    diagnostics.push(Diagnostic::info(
                        "I1132",
                        format!(
                            "source component '{component_id}' shadows the template from '{path}'"
                        ),
                    ));
                }
                std::collections::btree_map::Entry::Vacant(entry) => {
                    diagnostics.push(Diagnostic::info(
                        "I1133",
                        format!("component '{component_id}' expanded from '{path}'"),
                    ));
                    if let Some(index) = packages.span(&package.reference.to_string()) {
                        let template_path = format!("template.components.{component_id}");
                        let expanded_path = format!("components.{component_id}");
                        if let Some(source) = index.get_span(&template_path) {
                            sources.insert(expanded_path.clone(), source);
                        }
                        for attribute in component.attributes.keys() {
                            if let Some(source) =
                                index.get_span(&format!("{template_path}.{attribute}"))
                            {
                                sources.insert(format!("{expanded_path}.{attribute}"), source);
                            }
                        }
                    }
                    entry.insert(component);
                }
            }
        }
        if let Some(kinematics) = template.kinematics {
            if let Some(kind) = kinematics.kind {
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
            for (limit, value) in kinematics.limits {
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    expanded.kinematics.limits.entry(limit.clone())
                {
                    if Quantity::parse(&value).is_err() {
                        diagnostics.push(Diagnostic::error(
                            "E1134",
                            format!(
                                "template limit '{limit}' from '{path}': '{value}' is not a valid quantity"
                            ),
                        ));
                    } else {
                        diagnostics.push(Diagnostic::info(
                            "I1133",
                            format!("kinematics limit '{limit}' defaulted from '{path}'"),
                        ));
                        entry.insert(value);
                    }
                }
            }
        }
    }
    ExpandedGraph {
        doc: expanded,
        sources,
    }
}
