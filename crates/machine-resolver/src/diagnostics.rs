use dryer_machine_parser::spans::SpanIndex;
use dryer_machine_schema::{Diagnostic, SourceSpan};
use std::collections::BTreeMap;

pub(super) fn expanded_source(
    sources: &BTreeMap<String, SourceSpan>,
    path: &str,
) -> Option<SourceSpan> {
    let mut candidate = path.to_string();
    loop {
        if let Some(source) = sources.get(&candidate) {
            return Some(source.clone());
        }
        let i = candidate.rfind(['.', '['])?;
        candidate.truncate(i);
    }
}

pub(super) fn diagnostic_at_source(
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

pub(super) fn related_to_claim(
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

pub(super) fn locate_diagnostics(diagnostics: &mut [Diagnostic], spans: &SpanIndex) {
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
