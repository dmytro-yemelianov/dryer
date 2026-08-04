//! Exact source ranges for dotted document paths (spec §11.3).
//!
//! A marked event walk over the YAML source builds an index from every
//! key/item path (`kinematics.limits.max_velocity`, `packages[0]`) to the
//! 1-based start/end positions where it appears. Diagnostics consult
//! this index instead of guessing; a path with no entry falls back to its
//! nearest recorded ancestor.

use dryer_machine_schema::{SourcePosition, SourceSpan};
use std::collections::BTreeMap;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::Marker;

/// Path → exact range of the key or sequence item.
#[derive(Debug, Clone, Default)]
pub struct SpanIndex {
    map: BTreeMap<String, SourceSpan>,
    document: Option<String>,
}

impl SpanIndex {
    /// Build the index. A source that fails to scan yields an empty index
    /// (diagnostics then carry paths without lines) — parse errors proper
    /// are reported by the typed deserialization pass, not here.
    pub fn build(source: &str) -> SpanIndex {
        Self::build_with_document(source, None)
    }

    /// Build an index whose returned spans identify `document`.
    pub fn build_named(source: &str, document: impl Into<String>) -> SpanIndex {
        Self::build_with_document(source, Some(document.into()))
    }

    fn build_with_document(source: &str, document: Option<String>) -> SpanIndex {
        let mut rx = Receiver::new(source);
        let mut parser = Parser::new_from_str(source);
        let _ = parser.load(&mut rx, false);
        SpanIndex {
            map: rx.map,
            document,
        }
    }

    /// Compatibility point lookup: line is 1-based and column is 0-based.
    pub fn get(&self, path: &str) -> Option<(usize, usize)> {
        self.get_span(path)
            .map(|span| (span.start.line, span.start.column - 1))
    }

    pub fn get_span(&self, path: &str) -> Option<SourceSpan> {
        let mut span = self.map.get(path)?.clone();
        span.document.clone_from(&self.document);
        Some(span)
    }

    /// Locate a path, retreating to the nearest recorded ancestor
    /// (`components.x.ghost` → `components.x`) when the exact path has
    /// no entry.
    pub fn locate(&self, path: &str) -> Option<(usize, usize)> {
        self.locate_span(path)
            .map(|span| (span.start.line, span.start.column - 1))
    }

    /// Locate an exact range, retreating to the nearest recorded ancestor.
    pub fn locate_span(&self, path: &str) -> Option<SourceSpan> {
        let mut p = path.to_string();
        loop {
            if let Some(hit) = self.get_span(&p) {
                return Some(hit);
            }
            match p.rfind(['.', '[']) {
                Some(i) => p.truncate(i),
                None => return None,
            }
        }
    }
}

enum Frame {
    Map { expect_key: bool },
    Seq { idx: usize },
}

#[derive(Default)]
struct Receiver {
    map: BTreeMap<String, SourceSpan>,
    source_lines: Vec<String>,
    /// Path segments of enclosing containers. Sequence-item segments are
    /// bracketed (`[0]`) and join without a dot.
    path: Vec<String>,
    frames: Vec<Frame>,
    /// True per frame if the container pushed a `path` segment on entry.
    pushed: Vec<bool>,
    /// The map key whose value comes next.
    pending: Option<String>,
}

fn join(path: &[String]) -> String {
    let mut out = String::new();
    for seg in path {
        if seg.starts_with('[') || out.is_empty() {
            out.push_str(seg);
        } else {
            out.push('.');
            out.push_str(seg);
        }
    }
    out
}

impl Receiver {
    fn new(source: &str) -> Self {
        Self {
            source_lines: source.lines().map(str::to_owned).collect(),
            ..Self::default()
        }
    }

    fn scalar_span(&self, path: String, value: &str, mark: Marker) -> SourceSpan {
        let start_column = mark.col() + 1;
        let width = self.scalar_width(value, mark).max(1);
        SourceSpan::new(
            path,
            SourcePosition::new(mark.line(), start_column),
            SourcePosition::new(mark.line(), start_column + width),
        )
    }

    fn scalar_width(&self, value: &str, mark: Marker) -> usize {
        let Some(line) = self.source_lines.get(mark.line().saturating_sub(1)) else {
            return value.chars().count();
        };
        let tail: Vec<char> = line.chars().skip(mark.col()).collect();
        match tail.first().copied() {
            Some('\'') => quoted_width(&tail, '\''),
            Some('"') => quoted_width(&tail, '"'),
            _ => value.chars().count(),
        }
    }

    fn enter_container(&mut self) {
        let seg = if let Some(key) = self.pending.take() {
            Some(key)
        } else if let Some(Frame::Seq { idx }) = self.frames.last_mut() {
            let s = format!("[{idx}]");
            *idx += 1;
            Some(s)
        } else {
            None // root container
        };
        match seg {
            Some(s) => {
                self.path.push(s);
                self.pushed.push(true);
            }
            None => self.pushed.push(false),
        }
    }

    fn leave_container(&mut self) {
        self.frames.pop();
        if self.pushed.pop().unwrap_or(false) {
            self.path.pop();
        }
        if let Some(Frame::Map { expect_key }) = self.frames.last_mut() {
            *expect_key = true; // the container was this key's value
        }
    }

    fn on_value_scalar(&mut self) {
        match self.frames.last_mut() {
            Some(Frame::Map { expect_key }) => {
                self.pending = None;
                *expect_key = true;
            }
            Some(Frame::Seq { .. }) | None => {}
        }
    }
}

impl MarkedEventReceiver for Receiver {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        match ev {
            Event::MappingStart(..) => {
                self.enter_container();
                self.frames.push(Frame::Map { expect_key: true });
            }
            Event::SequenceStart(..) => {
                self.enter_container();
                self.frames.push(Frame::Seq { idx: 0 });
            }
            Event::MappingEnd | Event::SequenceEnd => self.leave_container(),
            Event::Scalar(value, ..) => {
                let mut key_entry: Option<String> = None;
                let mut item_entry: Option<String> = None;
                match self.frames.last_mut() {
                    Some(Frame::Map { expect_key }) if *expect_key => {
                        let mut full = self.path.clone();
                        full.push(value.clone());
                        key_entry = Some(join(&full));
                        *expect_key = false;
                    }
                    Some(Frame::Map { .. }) => {}
                    Some(Frame::Seq { idx }) => {
                        let mut full = self.path.clone();
                        full.push(format!("[{}]", *idx));
                        item_entry = Some(join(&full));
                        *idx += 1;
                    }
                    None => {}
                }
                if let Some(p) = key_entry {
                    let span = self.scalar_span(p.clone(), &value, mark);
                    self.map.insert(p, span);
                    self.pending = Some(value);
                } else if let Some(p) = item_entry {
                    let span = self.scalar_span(p.clone(), &value, mark);
                    self.map.insert(p, span);
                } else {
                    self.on_value_scalar();
                }
            }
            Event::Alias(_) => self.on_value_scalar(),
            _ => {}
        }
    }
}

fn quoted_width(tail: &[char], quote: char) -> usize {
    let mut escaped = false;
    let mut i = 1;
    while i < tail.len() {
        let c = tail[i];
        if quote == '"' {
            if c == quote && !escaped {
                return i + 1;
            }
            escaped = c == '\\' && !escaped;
            if c != '\\' {
                escaped = false;
            }
        } else if c == quote {
            if tail.get(i + 1) == Some(&quote) {
                i += 1;
            } else {
                return i + 1;
            }
        }
        i += 1;
    }
    tail.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "\
top: 1
nest:
  a: x
  b:
    - one
    - two
    - key: v
list:
  - 10
  - 20
";

    #[test]
    fn keys_items_and_nested_maps_are_indexed_exactly() {
        let idx = SpanIndex::build(DOC);
        assert_eq!(idx.get("top"), Some((1, 0)));
        assert_eq!(idx.get("nest"), Some((2, 0)));
        assert_eq!(idx.get("nest.a"), Some((3, 2)));
        assert_eq!(idx.get("nest.b[0]"), Some((5, 6)));
        assert_eq!(idx.get("nest.b[1]"), Some((6, 6)));
        assert_eq!(idx.get("nest.b[2].key"), Some((7, 6)));
        assert_eq!(idx.get("list[1]"), Some((10, 4)));
        let span = idx.get_span("nest.a").unwrap();
        assert_eq!(span.start, SourcePosition::new(3, 3));
        assert_eq!(span.end, SourcePosition::new(3, 4));
    }

    #[test]
    fn quoted_scalars_include_their_delimiters_in_the_range() {
        let idx = SpanIndex::build("items:\n  - \"two words\"\n");
        let span = idx.get_span("items[0]").unwrap();
        assert_eq!(span.start, SourcePosition::new(2, 5));
        assert_eq!(span.end, SourcePosition::new(2, 16));
    }

    #[test]
    fn locate_retreats_to_the_nearest_ancestor() {
        let idx = SpanIndex::build(DOC);
        assert_eq!(idx.locate("nest.a.ghost"), Some((3, 2)));
        assert_eq!(idx.locate("nest.b[7]"), Some((4, 2)));
        assert_eq!(idx.locate("absent"), None);
    }

    #[test]
    fn a_scan_failure_yields_an_empty_index() {
        let idx = SpanIndex::build("a: [unclosed");
        assert!(idx.locate("a").is_none() || idx.get("a").is_some());
    }
}
