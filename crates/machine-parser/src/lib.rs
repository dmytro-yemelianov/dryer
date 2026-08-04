//! YAML → validated Machine Graph (spec §29 step 4).
//!
//! Output is always a [`ParseOutcome`]: a document (when the YAML at least
//! deserializes) plus every diagnostic found — the parser never stops at the
//! first problem, because resolver-style "list everything wrong" output is a
//! stated project value (§11.3).
//!
//! Source location: diagnostics carry a dotted `path` plus the exact range
//! of the named key or sequence item, from a marked YAML event
//! walk ([`spans::SpanIndex`]). A path with no exact entry (e.g. a missing
//! key being reported) locates at its nearest recorded ancestor.

pub mod spans;

use dryer_machine_schema::{
    valid_identifier, Diagnostic, Dimension, MachineDoc, Quantity, SourceSpan, API_VERSION,
    KIND_MACHINE,
};
use dryer_package_model::PackageRef;
use spans::SpanIndex;

/// Result of parsing + validating one machine manifest.
#[derive(Debug)]
pub struct ParseOutcome {
    /// Present when the YAML deserialized into the document shape at all;
    /// may still be accompanied by error diagnostics.
    pub doc: Option<MachineDoc>,
    pub diagnostics: Vec<Diagnostic>,
    /// Reusable source map for later resolver diagnostics.
    pub spans: SpanIndex,
}

impl ParseOutcome {
    pub fn is_valid(&self) -> bool {
        self.doc.is_some()
            && !self
                .diagnostics
                .iter()
                .any(|d| d.severity == dryer_machine_schema::Severity::Error)
    }
}

/// Parse and validate a machine manifest from YAML text.
pub fn parse_str(source: &str) -> ParseOutcome {
    parse_str_with_document(source, None)
}

fn parse_str_with_document(source: &str, document: Option<&str>) -> ParseOutcome {
    let mut diagnostics = Vec::new();
    let spans = match document {
        Some(document) => SpanIndex::build_named(source, document),
        None => SpanIndex::build(source),
    };

    let doc: MachineDoc = match serde_yaml::from_str(source) {
        Ok(d) => d,
        Err(e) => {
            let mut d = Diagnostic::error("E0100", format!("machine manifest does not parse: {e}"));
            if let Some(location) = e.location() {
                let mut source = SourceSpan::point(location.line(), location.column());
                if let Some(document) = document {
                    source = source.in_document(document);
                }
                d = d.with_source(source);
            }
            return ParseOutcome {
                doc: None,
                diagnostics: vec![d],
                spans,
            };
        }
    };

    validate(&doc, &spans, &mut diagnostics);
    ParseOutcome {
        doc: Some(doc),
        diagnostics,
        spans,
    }
}

/// Parse and validate a machine manifest from a file.
pub fn parse_file(path: &std::path::Path) -> ParseOutcome {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_str_with_document(&text, Some(&path.display().to_string())),
        Err(e) => ParseOutcome {
            doc: None,
            diagnostics: vec![Diagnostic::error(
                "E0101",
                format!("cannot read {}: {e}", path.display()),
            )],
            spans: SpanIndex::default(),
        },
    }
}

fn validate(doc: &MachineDoc, index: &SpanIndex, out: &mut Vec<Diagnostic>) {
    let mut push = |d: Diagnostic| {
        let d = locate(d, index);
        out.push(d);
    };

    // --- version / kind (E02xx) ---
    if doc.api_version != API_VERSION {
        push(
            Diagnostic::error(
                "E0200",
                format!(
                    "api_version '{}' is not supported (expected '{API_VERSION}')",
                    doc.api_version
                ),
            )
            .at("api_version"),
        );
    }
    if doc.kind != KIND_MACHINE {
        push(
            Diagnostic::error(
                "E0201",
                format!(
                    "kind '{}' is not supported (expected '{KIND_MACHINE}')",
                    doc.kind
                ),
            )
            .at("kind"),
        );
    }
    if doc.controllers.is_empty() {
        push(
            Diagnostic::error("E0202", "a machine needs at least one controller").at("controllers"),
        );
    }

    // --- identifiers (E03xx) ---
    for (section, keys) in [
        ("controllers", doc.controllers.keys().collect::<Vec<_>>()),
        ("components", doc.components.keys().collect::<Vec<_>>()),
        ("workflows", doc.workflows.keys().collect::<Vec<_>>()),
    ] {
        for key in keys {
            if !valid_identifier(key) {
                push(
                    Diagnostic::error(
                        "E0300",
                        format!(
                            "'{key}' is not a valid identifier (lowercase ASCII, digits, '_', '-', starting with a letter)"
                        ),
                    )
                    .at(format!("{section}.{key}")),
                );
            }
        }
    }

    // --- package references (E05xx) ---
    for (i, pkg) in doc.packages.iter().enumerate() {
        if let Err(e) = PackageRef::parse(pkg) {
            push(Diagnostic::error("E0500", e.to_string()).at(format!("packages[{i}]")));
        }
    }

    // --- kinematics limits are typed quantities (E04xx) ---
    for (name, value) in &doc.kinematics.limits {
        let expected = expected_limit_dimension(name);
        let parsed = match expected {
            Some(dim) => Quantity::parse_as(value, dim),
            None => Quantity::parse(value),
        };
        if let Err(e) = parsed {
            push(
                Diagnostic::error("E0400", format!("limit '{name}': {e}"))
                    .at(format!("kinematics.limits.{name}"))
                    .suggest("write quantities with explicit units, e.g. \"300 mm/s\""),
            );
        }
    }

    // --- intra-document references (E05xx) ---
    for (cname, comp) in &doc.components {
        for (attr, val) in &comp.attributes {
            let Some(target) = val.as_str() else { continue };
            match attr.as_str() {
                // component → component references
                "driver" | "sensor" if !doc.components.contains_key(target) => {
                    push(
                        Diagnostic::error(
                            "E0501",
                            format!("component '{cname}' references unknown component '{target}'"),
                        )
                        .at(format!("components.{cname}.{attr}")),
                    );
                }
                // component → controller.port references
                "connected_to" | "output" | "input" => {
                    if let Some((ctrl, _port)) = target.split_once('.') {
                        if !doc.controllers.contains_key(ctrl) {
                            push(
                                Diagnostic::error(
                                    "E0502",
                                    format!(
                                        "component '{cname}' references unknown controller '{ctrl}' in '{target}'"
                                    ),
                                )
                                .at(format!("components.{cname}.{attr}")),
                            );
                        }
                    } else {
                        push(
                            Diagnostic::error(
                                "E0503",
                                format!(
                                    "'{target}' must name a controller port as 'controller.port'"
                                ),
                            )
                            .at(format!("components.{cname}.{attr}")),
                        );
                    }
                }
                _ => {}
            }
        }
    }

    // controller transport parents
    for (name, ctrl) in &doc.controllers {
        if let Some(parent) = &ctrl.transport.parent {
            let Some((pctrl, _port)) = parent.split_once('.') else {
                push(
                    Diagnostic::error(
                        "E0503",
                        format!("'{parent}' must name a controller port as 'controller.port'"),
                    )
                    .at(format!("controllers.{name}.transport.parent")),
                );
                continue;
            };
            if !doc.controllers.contains_key(pctrl) {
                push(
                    Diagnostic::error(
                        "E0502",
                        format!("transport parent references unknown controller '{pctrl}'"),
                    )
                    .at(format!("controllers.{name}.transport.parent")),
                );
            }
        }
    }
}

/// Which dimension a well-known kinematics limit must carry. Unknown limit
/// names still require *some* valid unit, but their dimension is free.
fn expected_limit_dimension(name: &str) -> Option<Dimension> {
    match name {
        "max_velocity" | "max_z_velocity" => Some(Dimension::Velocity),
        "max_acceleration" => Some(Dimension::Acceleration),
        "max_step_rate" => Some(Dimension::Frequency),
        _ => None,
    }
}

/// Attach the exact source location of the diagnostic's path (or its
/// nearest recorded ancestor) from the span index.
fn locate(mut d: Diagnostic, index: &SpanIndex) -> Diagnostic {
    if d.source.is_some() {
        return d;
    }
    let Some(path) = d.path.as_deref() else {
        return d;
    };
    if let Some(source) = index.locate_span(path) {
        d = d.with_source(source);
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use dryer_machine_schema::Severity;

    fn errors(o: &ParseOutcome) -> Vec<String> {
        o.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.code.clone())
            .collect()
    }

    #[test]
    fn the_committed_example_is_valid() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/minimal-cartesian/machine.yaml");
        let o = parse_file(&path);
        assert!(o.is_valid(), "diagnostics: {:#?}", o.diagnostics);
    }

    #[test]
    fn bad_identifier_bad_unit_and_dangling_reference_all_reported_together() {
        let yaml = r#"
api_version: dryer.machine/v0.1
kind: Machine
metadata:
  name: broken
packages:
  - boards/example@not-a-version
controllers:
  mainboard:
    board: boards/example
    transport:
      type: usb
components:
  X_Motor:
    type: stepper_motor
  y_motor:
    type: stepper_motor
    driver: missing_driver
kinematics:
  type: cartesian
  limits:
    max_velocity: "300"
safety:
  profile: safety-profiles/desktop-fdm
"#;
        let o = parse_str(yaml);
        let codes = errors(&o);
        assert!(
            codes.contains(&"E0300".to_string()),
            "identifier: {codes:?}"
        );
        assert!(codes.contains(&"E0400".to_string()), "unit: {codes:?}");
        assert!(codes.contains(&"E0500".to_string()), "pkg ref: {codes:?}");
        assert!(codes.contains(&"E0501".to_string()), "reference: {codes:?}");
        assert_eq!(
            codes.len(),
            4,
            "exactly the four seeded problems: {codes:?}"
        );
    }

    #[test]
    fn wrong_dimension_on_a_known_limit_is_an_error() {
        let yaml = r#"
api_version: dryer.machine/v0.1
kind: Machine
metadata:
  name: t
controllers:
  mainboard:
    board: boards/example
    transport: { type: usb }
components: {}
kinematics:
  type: cartesian
  limits:
    max_velocity: 8000 mm/s^2
safety:
  profile: safety-profiles/desktop-fdm
"#;
        let o = parse_str(yaml);
        assert_eq!(errors(&o), vec!["E0400".to_string()]);
    }

    #[test]
    fn unknown_top_level_fields_are_rejected() {
        let yaml = "api_version: dryer.machine/v0.1\nkind: Machine\nmagic: true\n";
        let o = parse_str(yaml);
        assert!(o.doc.is_none());
        assert_eq!(o.diagnostics[0].code, "E0100");
    }

    #[test]
    fn diagnostics_carry_paths_and_exact_locations() {
        let yaml = r#"
api_version: dryer.machine/v0.1
kind: Machine
metadata:
  name: t
controllers:
  mainboard:
    board: boards/example
    transport: { type: usb }
components: {}
kinematics:
  type: cartesian
  limits:
    max_velocity: fast
safety:
  profile: safety-profiles/desktop-fdm
"#;
        let o = parse_str(yaml);
        let d = &o.diagnostics[0];
        assert_eq!(d.path.as_deref(), Some("kinematics.limits.max_velocity"));
        assert_eq!(d.line, Some(14), "exact key line from the span index");
        assert_eq!(d.column, Some(5), "1-based column of the key");
        let span = d.source.as_ref().expect("structured source span");
        assert_eq!(span.path.as_deref(), Some("kinematics.limits.max_velocity"));
        assert_eq!(span.start.line, 14);
        assert_eq!(span.start.column, 5);
        assert_eq!(span.end.line, 14);
        assert_eq!(span.end.column, 17, "exclusive end of max_velocity");
    }

    #[test]
    fn can_transport_parent_must_reference_a_known_controller() {
        let yaml = r#"
api_version: dryer.machine/v0.1
kind: Machine
metadata:
  name: t
controllers:
  toolhead:
    board: boards/ebb36
    transport:
      type: can
      parent: ghost.can0
components: {}
kinematics:
  type: corexy
safety:
  profile: safety-profiles/desktop-fdm
"#;
        let o = parse_str(yaml);
        assert_eq!(errors(&o), vec!["E0502".to_string()]);
    }
}
