//! A YAML document as a tree whose every scalar keeps the line it starts on, from the events of
//! `saphyr-parser`. An alias stands for a copy of its anchor, given the alias's own line.

use std::collections::BTreeMap;

use saphyr_parser::{Event, Parser, Span};

/// The most nodes one file may make, aliases copied out; past it the file is refused, so a few
/// nested aliases cannot make a tree too large for memory.
const MAX_NODES: usize = 100_000;

/// One YAML node. A mapping keeps its keys in order; a key that is not a scalar is dropped.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Node {
    Scalar { text: String, line: u32 },
    List(Vec<Node>),
    Map(Vec<(String, Node)>),
}

impl Node {
    /// The value at `key`, when this is a mapping that has it.
    pub(super) fn get(&self, key: &str) -> Option<&Self> {
        self.entries()
            .iter()
            .find_map(|(name, value)| (name == key).then_some(value))
    }

    /// The key and value pairs of a mapping; nothing for any other node.
    pub(super) fn entries(&self) -> &[(String, Self)] {
        match self {
            Self::Map(entries) => entries,
            _ => &[],
        }
    }

    /// The items of a sequence; nothing for any other node.
    pub(super) fn items(&self) -> &[Self] {
        match self {
            Self::List(items) => items,
            _ => &[],
        }
    }

    /// A scalar's text and line.
    pub(super) fn scalar(&self) -> Option<(&str, u32)> {
        match self {
            Self::Scalar { text, line } => Some((text, *line)),
            _ => None,
        }
    }

    fn size(&self) -> usize {
        1 + self.items().iter().map(Self::size).sum::<usize>()
            + self
                .entries()
                .iter()
                .map(|(_, value)| value.size())
                .sum::<usize>()
    }

    fn relined(self, line: u32) -> Self {
        match self {
            Self::Scalar { text, .. } => Self::Scalar { text, line },
            other => other,
        }
    }
}

/// A collection still open: its anchor, and the items or the entries read so far, with the key
/// that waits for its value.
enum Open {
    List(usize, Vec<Node>),
    Map(usize, Vec<(String, Node)>, Option<Node>),
}

impl Open {
    /// The anchor and the node of a collection that its end event closes.
    fn closed(self) -> (usize, Node) {
        match self {
            Self::List(anchor, items) => (anchor, Node::List(items)),
            Self::Map(anchor, entries, _) => (anchor, Node::Map(entries)),
        }
    }
}

/// The documents in `text`, or the parser's error with its line.
pub(super) fn documents(text: &str) -> Result<Vec<Node>, String> {
    within(text, MAX_NODES)
}

/// The documents in `text` while they make at most `limit` nodes, aliases copied out.
fn within(text: &str, limit: usize) -> Result<Vec<Node>, String> {
    let mut fold = Fold {
        limit,
        ..Fold::default()
    };
    for event in Parser::new_from_str(text) {
        let (event, span) =
            event.map_err(|e| format!("line {}: {}", e.marker().line(), e.info()))?;
        fold.take(event, span)?;
    }
    Ok(fold.documents)
}

#[derive(Default)]
struct Fold {
    open: Vec<Open>,
    anchors: BTreeMap<usize, Node>,
    documents: Vec<Node>,
    made: usize,
    limit: usize,
}

impl Fold {
    fn take(&mut self, event: Event<'_>, span: Span) -> Result<(), String> {
        let line = u32::try_from(span.start.line()).unwrap_or(u32::MAX);
        match event {
            Event::SequenceStart(anchor, _) => self.open.push(Open::List(anchor, Vec::new())),
            Event::MappingStart(anchor, _) => self.open.push(Open::Map(anchor, Vec::new(), None)),
            Event::SequenceEnd | Event::MappingEnd => {
                let closed = self.open.pop().map(Open::closed);
                closed.map_or(Ok(()), |(anchor, node)| self.place(anchor, node, 1))?;
            }
            Event::Scalar(text, _, anchor, _) => {
                let text = text.into_owned();
                self.place(anchor, Node::Scalar { text, line }, 1)?;
            }
            Event::Alias(anchor) => {
                let copy = self.anchors.get(&anchor).cloned();
                let node = copy.ok_or_else(|| format!("line {line}: an alias names no anchor"))?;
                let copied = node.size();
                self.place(0, node.relined(line), copied)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Puts a finished node in the collection open around it, or among the documents. `new` counts
    /// the nodes it adds: a collection's children are counted, and an alias's copy is all new.
    fn place(&mut self, anchor: usize, node: Node, new: usize) -> Result<(), String> {
        self.made += new;
        if self.made > self.limit {
            return Err(format!(
                "more than {} nodes once its aliases are copied",
                self.limit
            ));
        }
        if anchor != 0 {
            self.anchors.insert(anchor, node.clone());
        }
        match self.open.last_mut() {
            Some(Open::List(_, items)) => items.push(node),
            Some(Open::Map(_, entries, waiting)) => match waiting.take() {
                Some(Node::Scalar { text, .. }) => entries.push((text, node)),
                Some(_) => {}
                None => *waiting = Some(node),
            },
            None => self.documents.push(node),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(text: &str, line: u32) -> Node {
        Node::Scalar {
            text: text.into(),
            line,
        }
    }

    /// The one document in `text`, or an empty map when `text` holds no single readable document.
    fn only(text: &str) -> Node {
        match documents(text).as_deref() {
            Ok([document]) => document.clone(),
            _ => Node::Map(Vec::new()),
        }
    }

    #[test]
    fn each_scalar_keeps_the_line_it_starts_on() {
        let document = only("a: 1\nb:\n  - x\n  - {c: y}\n");
        assert_eq!(document.get("a"), Some(&scalar("1", 1)));
        let items = document.get("b").map(Node::items).unwrap_or_default();
        assert_eq!(items[0], scalar("x", 3));
        assert_eq!(items[1].get("c"), Some(&scalar("y", 4)));
    }

    #[test]
    fn an_alias_copies_its_anchor_and_a_scalar_alias_takes_its_own_line() {
        let text = "a: &v one\nb: *v\nc: &m {k: two}\nd: *m\n";
        let document = only(text);
        assert_eq!(document.get("b"), Some(&scalar("one", 2)));
        assert_eq!(
            document.get("d").and_then(|d| d.get("k")),
            Some(&scalar("two", 3))
        );
    }

    #[test]
    fn a_key_that_is_not_a_scalar_drops_its_entry() {
        let document = only("? [a]\n: x\nb: y\n");
        assert_eq!(document.entries(), [("b".to_string(), scalar("y", 3))]);
    }

    /// A list, then an anchored list of `scalars` scalars and `copies` aliases of it:
    /// 1 + (copies + 1) × (scalars + 1) nodes.
    fn copied(scalars: usize, copies: usize) -> String {
        format!(
            "[&a [{}x]{}]\n",
            "x, ".repeat(scalars - 1),
            ", *a".repeat(copies)
        )
    }

    /// `read` takes `stream` whole, and refuses it with one more document of one node.
    fn holds(read: fn(&str) -> Result<Vec<Node>, String>, stream: &str, limit: usize) {
        assert!(read(stream).is_ok());
        let refused = read(&format!("{stream}--- y\n")).err().unwrap_or_default();
        assert!(
            refused.contains(&format!("more than {limit} nodes")),
            "{refused}"
        );
    }

    #[test]
    fn nested_aliases_past_the_node_limit_are_refused() {
        let mut text = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
        for level in 1..8 {
            let below = level - 1;
            text.push_str(&format!("a{level}: &a{level} [*a{below}, *a{below}, *a{below}, *a{below}, *a{below}, *a{below}, *a{below}, *a{below}, *a{below}, *a{below}]\n"));
        }
        let refused = within(&text, 1_000).err().unwrap_or_default();
        assert!(refused.contains("more than 1000 nodes"), "{refused}");
    }

    #[test]
    fn a_stream_of_exactly_the_node_limit_is_read_and_one_node_more_is_refused() {
        holds(|text| within(text, 1_000), &copied(36, 26), 1_000);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "Miri takes minutes to copy 100,000 nodes; the test above holds the rule at 1,000"
    )]
    fn the_gate_reads_a_hundred_thousand_nodes_and_refuses_one_more() {
        holds(documents, &copied(368, 270), MAX_NODES);
    }

    #[test]
    fn a_syntax_error_names_its_line() {
        let refused = documents("a: [x\nb: y\n").err().unwrap_or_default();
        assert!(refused.starts_with("line "), "{refused}");
    }
}
