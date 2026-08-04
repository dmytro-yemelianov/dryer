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
use yaml_rust2::scanner::{Marker, TScalarStyle};

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
        if parser.load(&mut rx, false).is_err() {
            rx.map.clear();
        }
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
            let i = p.rfind(['.', '['])?;
            p.truncate(i);
        }
    }
}

enum Frame {
    Map { expect_key: bool, flow: bool },
    Seq { idx: usize, flow: bool },
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

    fn scalar_span(
        &self,
        path: String,
        value: &str,
        style: TScalarStyle,
        mark: Marker,
        flow: bool,
    ) -> SourceSpan {
        let start_column = mark.col() + 1;
        let end = self.scalar_end(value, style, mark, flow);
        SourceSpan::new(path, SourcePosition::new(mark.line(), start_column), end)
    }

    fn scalar_end(
        &self,
        value: &str,
        style: TScalarStyle,
        mark: Marker,
        flow: bool,
    ) -> SourcePosition {
        match style {
            TScalarStyle::SingleQuoted => self.quoted_scalar_end(mark, '\''),
            TScalarStyle::DoubleQuoted => self.quoted_scalar_end(mark, '"'),
            TScalarStyle::Literal | TScalarStyle::Folded => self.block_scalar_end(mark),
            TScalarStyle::Plain => self.plain_scalar_end(value, mark, flow),
        }
    }

    fn quoted_scalar_end(&self, mark: Marker, quote: char) -> SourcePosition {
        let start_line = mark.line().saturating_sub(1);
        for (line_idx, line) in self.source_lines.iter().enumerate().skip(start_line) {
            let chars: Vec<char> = line.chars().collect();
            let start = if line_idx == start_line {
                mark.col() + 1
            } else {
                0
            };
            let mut i = start;
            while i < chars.len() {
                if chars[i] == quote {
                    if quote == '\'' && chars.get(i + 1) == Some(&quote) {
                        i += 2;
                        continue;
                    }
                    if quote == '"' && is_escaped(&chars, i) {
                        i += 1;
                        continue;
                    }
                    return SourcePosition::new(line_idx + 1, i + 2);
                }
                i += 1;
            }
        }
        self.source_end(mark)
    }

    fn block_scalar_end(&self, mark: Marker) -> SourcePosition {
        let start_idx = mark.line().saturating_sub(1);
        let Some(start_line) = self.source_lines.get(start_idx) else {
            return SourcePosition::new(mark.line(), mark.col() + 2);
        };
        let starts_at_header = matches!(start_line.chars().nth(mark.col()), Some('|' | '>'));
        let base_indent = if starts_at_header {
            leading_spaces(start_line)
        } else {
            mark.col()
        };
        let mut content_indent = (!starts_at_header).then_some(base_indent);
        let mut last_line = start_idx;

        for (line_idx, line) in self.source_lines.iter().enumerate().skip(start_idx + 1) {
            if line.trim().is_empty() {
                if content_indent.is_some() {
                    last_line = line_idx;
                }
                continue;
            }
            let indent = leading_spaces(line);
            let required = match content_indent {
                Some(required) => required,
                None if indent > base_indent => {
                    content_indent = Some(indent);
                    indent
                }
                None => break,
            };
            if indent < required {
                break;
            }
            last_line = line_idx;
        }

        let end_line = &self.source_lines[last_line];
        SourcePosition::new(last_line + 1, end_line.chars().count() + 1)
    }

    fn plain_scalar_end(&self, value: &str, mark: Marker, flow: bool) -> SourcePosition {
        let Some(line) = self.source_lines.get(mark.line().saturating_sub(1)) else {
            return SourcePosition::new(mark.line(), mark.col() + value.chars().count() + 1);
        };
        let chars: Vec<char> = line.chars().collect();
        let (raw_first_end, terminated) = plain_line_end(&chars, mark.col(), flow);
        let first_end = raw_first_end.min(mark.col() + value.chars().count());
        if terminated {
            return SourcePosition::new(mark.line(), first_end + 1);
        }

        let base_indent = leading_spaces(line);
        let mut last_line = mark.line().saturating_sub(1);
        let mut last_end = first_end.max(mark.col() + value.chars().count());
        for (line_idx, continuation) in self.source_lines.iter().enumerate().skip(mark.line()) {
            if continuation.trim().is_empty() {
                continue;
            }
            let indent = leading_spaces(continuation);
            if indent <= base_indent {
                break;
            }
            let continuation_chars: Vec<char> = continuation.chars().collect();
            let (end, stopped) = plain_line_end(&continuation_chars, indent, flow);
            last_line = line_idx;
            last_end = end;
            if stopped {
                break;
            }
        }
        SourcePosition::new(last_line + 1, last_end + 1)
    }

    fn alias_span(&self, path: String, mark: Marker) -> SourceSpan {
        let end = self
            .source_lines
            .get(mark.line().saturating_sub(1))
            .map(|line| {
                let chars: Vec<char> = line.chars().collect();
                let mut i = mark.col();
                while i < chars.len()
                    && !chars[i].is_whitespace()
                    && !matches!(chars[i], ',' | '[' | ']' | '{' | '}')
                {
                    i += 1;
                }
                SourcePosition::new(mark.line(), i + 1)
            })
            .unwrap_or_else(|| SourcePosition::new(mark.line(), mark.col() + 2));
        SourceSpan::new(path, SourcePosition::new(mark.line(), mark.col() + 1), end)
    }

    fn source_end(&self, mark: Marker) -> SourcePosition {
        self.source_lines
            .last()
            .map(|line| SourcePosition::new(self.source_lines.len(), line.chars().count() + 1))
            .unwrap_or_else(|| SourcePosition::new(mark.line(), mark.col() + 2))
    }

    fn starts_with(&self, mark: Marker, expected: char) -> bool {
        self.source_lines
            .get(mark.line().saturating_sub(1))
            .and_then(|line| line.chars().nth(mark.col()))
            == Some(expected)
    }

    fn enter_container(&mut self) {
        let seg = if let Some(key) = self.pending.take() {
            Some(key)
        } else if let Some(Frame::Seq { idx, .. }) = self.frames.last_mut() {
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
        if let Some(Frame::Map { expect_key, .. }) = self.frames.last_mut() {
            *expect_key = true; // the container was this key's value
        }
    }

    fn on_value_scalar(&mut self) {
        match self.frames.last_mut() {
            Some(Frame::Map { expect_key, .. }) => {
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
                let flow = self.starts_with(mark, '{');
                self.enter_container();
                self.frames.push(Frame::Map {
                    expect_key: true,
                    flow,
                });
            }
            Event::SequenceStart(..) => {
                let flow = self.starts_with(mark, '[');
                self.enter_container();
                self.frames.push(Frame::Seq { idx: 0, flow });
            }
            Event::MappingEnd | Event::SequenceEnd => self.leave_container(),
            Event::Scalar(value, style, ..) => {
                let mut key_entry: Option<String> = None;
                let mut item_entry: Option<String> = None;
                let mut flow_context = false;
                match self.frames.last_mut() {
                    Some(Frame::Map { expect_key, flow }) if *expect_key => {
                        let mut full = self.path.clone();
                        full.push(value.clone());
                        key_entry = Some(join(&full));
                        flow_context = *flow;
                        *expect_key = false;
                    }
                    Some(Frame::Map { .. }) => {}
                    Some(Frame::Seq { idx, flow }) => {
                        let mut full = self.path.clone();
                        full.push(format!("[{}]", *idx));
                        item_entry = Some(join(&full));
                        flow_context = *flow;
                        *idx += 1;
                    }
                    None => {}
                }
                if let Some(p) = key_entry {
                    let span = self.scalar_span(p.clone(), &value, style, mark, flow_context);
                    self.map.insert(p, span);
                    self.pending = Some(value);
                } else if let Some(p) = item_entry {
                    let span = self.scalar_span(p.clone(), &value, style, mark, flow_context);
                    self.map.insert(p, span);
                } else {
                    self.on_value_scalar();
                }
            }
            Event::Alias(_) => {
                let item_entry = match self.frames.last_mut() {
                    Some(Frame::Seq { idx, .. }) => {
                        let mut full = self.path.clone();
                        full.push(format!("[{}]", *idx));
                        *idx += 1;
                        Some(join(&full))
                    }
                    _ => None,
                };
                if let Some(p) = item_entry {
                    let span = self.alias_span(p.clone(), mark);
                    self.map.insert(p, span);
                } else {
                    self.on_value_scalar();
                }
            }
            _ => {}
        }
    }
}

fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

fn is_escaped(chars: &[char], index: usize) -> bool {
    chars[..index]
        .iter()
        .rev()
        .take_while(|c| **c == '\\')
        .count()
        % 2
        == 1
}

fn plain_line_end(chars: &[char], start: usize, flow: bool) -> (usize, bool) {
    let mut end = chars.len();
    let mut terminated = false;
    for i in start..chars.len() {
        let c = chars[i];
        let preceded_by_space = i == start || chars[i.saturating_sub(1)].is_whitespace();
        let followed_by_space = chars.get(i + 1).map_or(true, |next| next.is_whitespace());
        if (flow && matches!(c, ',' | ']' | '}'))
            || (c == '#' && preceded_by_space)
            || (c == ':' && followed_by_space)
        {
            end = i;
            terminated = true;
            break;
        }
    }
    while end > start && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    (end, terminated)
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
    fn multiline_scalars_end_at_their_physical_closing_position() {
        let idx = SpanIndex::build("items:\n  - |\n    first\n    second\n  - \"line\n    two\"\n");
        assert_eq!(
            idx.get_span("items[0]").unwrap(),
            SourceSpan::new(
                "items[0]",
                SourcePosition::new(3, 5),
                SourcePosition::new(4, 11),
            )
        );
        assert_eq!(
            idx.get_span("items[1]").unwrap(),
            SourceSpan::new(
                "items[1]",
                SourcePosition::new(5, 5),
                SourcePosition::new(6, 9),
            )
        );
    }

    #[test]
    fn plain_scalar_terminators_respect_block_and_flow_context() {
        let block_line = "  - hello, world ] ok }";
        let block = SpanIndex::build(&format!("items:\n{block_line}\n"));
        assert_eq!(
            block.get_span("items[0]").unwrap().end,
            SourcePosition::new(2, block_line.chars().count() + 1)
        );

        let multiline = SpanIndex::build("items:\n  - first   \n    second\n");
        assert_eq!(
            multiline.get_span("items[0]").unwrap().end,
            SourcePosition::new(3, 11)
        );

        let flow = SpanIndex::build("items: [one, two]\n");
        assert_eq!(
            flow.get_span("items[0]").unwrap().end,
            SourcePosition::new(1, 12)
        );
        assert_eq!(
            flow.get_span("items[1]").unwrap().end,
            SourcePosition::new(1, 17)
        );
    }

    #[test]
    fn aliases_occupy_a_sequence_index() {
        let idx = SpanIndex::build("items:\n  - &base one\n  - *base\n  - two\n");
        let alias = idx.get_span("items[1]").unwrap();
        assert_eq!(alias.start, SourcePosition::new(3, 5));
        assert_eq!(alias.end, SourcePosition::new(3, 10));
        assert_eq!(
            idx.get_span("items[2]").unwrap().start,
            SourcePosition::new(4, 5)
        );
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
        assert!(idx.map.is_empty());
    }
}
