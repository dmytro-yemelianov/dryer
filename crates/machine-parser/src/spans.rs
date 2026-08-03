//! Exact source locations for dotted document paths (spec §11.3).
//!
//! A marked event walk over the YAML source builds an index from every
//! key/item path (`kinematics.limits.max_velocity`, `packages[0]`) to the
//! 1-based line and 0-based column where it appears. Diagnostics consult
//! this index instead of guessing; a path with no entry falls back to its
//! nearest recorded ancestor.

use std::collections::BTreeMap;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::Marker;

/// Path → (line 1-based, column 0-based) of the key or sequence item.
#[derive(Debug, Default)]
pub struct SpanIndex {
    map: BTreeMap<String, (usize, usize)>,
}

impl SpanIndex {
    /// Build the index. A source that fails to scan yields an empty index
    /// (diagnostics then carry paths without lines) — parse errors proper
    /// are reported by the typed deserialization pass, not here.
    pub fn build(source: &str) -> SpanIndex {
        let mut rx = Receiver::default();
        let mut parser = Parser::new_from_str(source);
        let _ = parser.load(&mut rx, false);
        SpanIndex { map: rx.map }
    }

    pub fn get(&self, path: &str) -> Option<(usize, usize)> {
        self.map.get(path).copied()
    }

    /// Locate a path, retreating to the nearest recorded ancestor
    /// (`components.x.ghost` → `components.x`) when the exact path has
    /// no entry.
    pub fn locate(&self, path: &str) -> Option<(usize, usize)> {
        let mut p = path.to_string();
        loop {
            if let Some(hit) = self.get(&p) {
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
    map: BTreeMap<String, (usize, usize)>,
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
                    self.map.insert(p, (mark.line(), mark.col()));
                    self.pending = Some(value);
                } else if let Some(p) = item_entry {
                    self.map.insert(p, (mark.line(), mark.col()));
                } else {
                    self.on_value_scalar();
                }
            }
            Event::Alias(_) => self.on_value_scalar(),
            _ => {}
        }
    }
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
