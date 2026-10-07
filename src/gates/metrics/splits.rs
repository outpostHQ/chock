//! Parts of a file that only one private item uses. Such a part can move to a child module where
//! only its owner and what the file uses of it need `pub(super)`: the file holds two concerns.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Attribute, Ident, ImplItem, Item, ItemImpl, Meta, TraitItem, Type, Visibility};

use crate::gates::metrics::{bigfiles, prodlines};
use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::report::{Detail, Place};
use crate::run::{Ctx, Gate, Group, Kind, Measurement};

/// Lines a part must hold, and lines the file must keep without it, before a move is worth a file.
pub const PART: usize = 100;

pub const GATE: Gate = Gate {
    name: "splits",
    about: "a file gains a part of 100 lines or more that only one private item uses, so it could \
            be a module of its own",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::AnnotatedRatchet {
        measure,
        // Keyed by file and counted in parts, so an edit inside a part never trips the gate.
        keys: Keys::Items,
        unit: "movable parts",
    },
};

/// Files under the `bigfiles` budget with a part that could leave them; `bigfiles` names the
/// parts of a file over it.
fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let sized = |lines: usize| (PART.saturating_mul(2)..=bigfiles::BUDGET).contains(&lines);
    measured(ctx, &sized, &|file, movable| {
        movable.then_some(file.parts.len())
    })
}

/// The files `sized` keeps, each counted by `count` and told whether a part could leave it; a file
/// counted as `None` is left out.
pub fn measured(
    ctx: &Ctx,
    sized: &dyn Fn(usize) -> bool,
    count: &dyn Fn(&Parted, bool) -> Option<usize>,
) -> Result<Measurement, String> {
    let mut read = Measurement::of(Series::new(), Vec::new());
    for file in files(ctx, sized)? {
        let detail = detail(&file);
        if let Some(counted) = count(&file, detail.is_some()) {
            read.series
                .set(&file.shown, u64::try_from(counted).unwrap_or(u64::MAX));
            read.details
                .extend(detail.map(|detail| (file.shown, detail)));
        }
    }
    Ok(read)
}

/// A part of a file and the item that alone leads into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub owner: String,
    /// The owner's own lines, where a reader starts.
    pub line: u32,
    pub last: u32,
    /// Items the part holds, its owner among them, and the lines they span.
    pub items: usize,
    pub lines: usize,
}

/// A production file, its line count, and the parts that could leave it, largest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parted {
    pub shown: String,
    pub lines: usize,
    pub parts: Vec<Part>,
}

/// Every production file whose line count `keep` accepts, with its parts.
pub fn files(ctx: &Ctx, keep: &dyn Fn(usize) -> bool) -> Result<Vec<Parted>, String> {
    let kept: Vec<(PathBuf, usize)> = prodlines::measure(ctx)?
        .into_iter()
        .filter(|(_, lines)| keep(*lines))
        .collect();
    let words = if kept.is_empty() {
        Vec::new()
    } else {
        words_by_file(ctx)?
    };
    let mut out = Vec::new();
    for (path, lines) in kept {
        let shown = project::relative(&ctx.root, &path);
        let src = std::fs::read_to_string(&path).map_err(|e| format!("{shown}: {e}"))?;
        let parts = parts(&src, lines, &used_below(&words, &path));
        out.push(Parted {
            shown,
            lines,
            parts,
        });
    }
    Ok(out)
}

/// The words of each Rust file, by file.
pub(crate) type Words = Vec<(PathBuf, HashSet<String>)>;

/// Whether a file below `path`'s module names a word: a private item of `path` is visible there.
/// A `mod.rs` or crate root sits in its own module's directory, and is not below itself.
pub(crate) fn used_below<'a>(words: &'a Words, path: &'a Path) -> impl Fn(&str) -> bool + 'a {
    let below = prodlines::module_dir(path);
    move |name| {
        words.iter().any(|(other, held)| {
            let under = below.as_ref().is_some_and(|dir| other.starts_with(dir));
            under && other != path && held.contains(name)
        })
    }
}

/// The words of every Rust file in the tree, test files too.
pub(crate) fn words_by_file(ctx: &Ctx) -> Result<Words, String> {
    let paths = project::walked(
        &ctx.listing,
        &ctx.root,
        &|name| !project::SKIPPED.contains(&name) && !name.starts_with('.'),
        &|name, _| name.ends_with(".rs"),
    )?;
    Ok(paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let words = words_of(&text).collect();
            (path, words)
        })
        .collect())
}

pub(crate) fn words_of(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty())
        .map(str::to_string)
}

/// The finding for a file with parts: each part as a place, and the move of the largest as the fix.
#[must_use]
pub fn detail(file: &Parted) -> Option<Detail> {
    let largest = file.parts.first()?;
    let places = file
        .parts
        .iter()
        .map(|part| {
            Place::at("can move out", &file.shown, part.line)
                .through(part.last)
                .item(&part.owner)
        })
        .collect();
    let fix = format!(
        "move `{}` with the items only it uses ({} items, {} lines) to `{}`",
        largest.owner,
        largest.items,
        largest.lines,
        target(&file.shown, &largest.owner)
    );
    Some(Detail {
        line: Some(largest.line),
        places,
        fix: Some(fix),
    })
}

/// The file a part moves to: a child of the module it leaves.
fn target(shown: &str, owner: &str) -> String {
    let name = snake(owner);
    let stem = shown.strip_suffix(".rs").unwrap_or(shown);
    match stem.rsplit_once('/') {
        Some((parent, "mod" | "lib" | "main")) => format!("{parent}/{name}.rs"),
        None if matches!(stem, "mod" | "lib" | "main") => format!("{name}.rs"),
        _ => format!("{stem}/{name}.rs"),
    }
}

/// `HttpClient` as a file name: `http_client`.
fn snake(name: &str) -> String {
    let mut out = String::new();
    let mut after_lower = false;
    for c in name.chars() {
        if c.is_uppercase() && after_lower {
            out.push('_');
        }
        after_lower = c.is_lowercase() || c.is_ascii_digit();
        out.extend(c.to_lowercase());
    }
    out
}

/// The parts of one source that could leave it, largest first. `lines` is its production count,
/// and `outside` says whether a file below it names a word.
pub fn parts(src: &str, lines: usize, outside: &dyn Fn(&str) -> bool) -> Vec<Part> {
    let Ok(file) = prodlines::parse_rust(src) else {
        return Vec::new();
    };
    let nodes = nodes(&file, outside);
    let root = nodes.len();
    let mut edges = edges(&nodes);
    reach_all(&mut edges, root);
    let idom = dominators(&edges, root);
    let mut parts = owned(&nodes, &idom, lines);
    parts.sort_by(|a, b| b.lines.cmp(&a.lines).then(a.line.cmp(&b.line)));
    parts
}

/// What moves as a whole: an item, or a type with every `impl` of it in the file.
#[derive(Debug, Default)]
struct Node {
    /// The names other items reach it by: its own, then its methods.
    names: Vec<String>,
    /// Every word it names in code, macro input and attribute input.
    uses: HashSet<String>,
    /// The first and last line of each of its items.
    spans: Vec<(usize, usize)>,
    /// Reached from outside the file: it may own a part but never sit inside one.
    entered: bool,
    /// Owns no part: `main`, a `pub` item (a move needs a re-export), a `mod`, `use`, macro or
    /// foreign `impl`.
    pinned: bool,
}

impl Node {
    fn lines(&self) -> usize {
        self.spans
            .iter()
            .map(|(first, last)| last.saturating_sub(*first).saturating_add(1))
            .sum()
    }
}

/// The file's production items as nodes, each `impl` of a local type joined to the type.
fn nodes(file: &syn::File, outside: &dyn Fn(&str) -> bool) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut types = HashMap::new();
    let mut tests = Names::default();
    let mut impls = Vec::new();
    for item in &file.items {
        let seen = seen(item);
        if prodlines::is_test_gated(seen.attrs) {
            tests.visit_item(item);
            continue;
        }
        if let Item::Impl(block) = item {
            impls.push(block);
            continue;
        }
        let node = node(item, &seen);
        if is_type(item)
            && let Some(name) = node.names.first()
        {
            types.insert(name.clone(), nodes.len());
        }
        nodes.push(node);
    }
    for block in impls {
        attach(&mut nodes, &types, block);
    }
    for node in &mut nodes {
        node.entered |= node
            .names
            .iter()
            .any(|name| outside(name) || tests.0.contains(name));
    }
    nodes
}

fn is_type(item: &Item) -> bool {
    matches!(
        item,
        Item::Struct(_) | Item::Enum(_) | Item::Union(_) | Item::Type(_)
    )
}

/// What the graph needs of one item before its `impl`s are joined to it.
struct Seen<'a> {
    attrs: &'a [Attribute],
    name: Option<&'a Ident>,
    entered: bool,
    pinned: bool,
}

fn seen(item: &Item) -> Seen<'_> {
    match item {
        Item::Const(it) => named(&it.attrs, &it.ident, &it.vis),
        Item::Enum(it) => named(&it.attrs, &it.ident, &it.vis),
        Item::Fn(it) => named(&it.attrs, &it.sig.ident, &it.vis),
        Item::Static(it) => named(&it.attrs, &it.ident, &it.vis),
        Item::Struct(it) => named(&it.attrs, &it.ident, &it.vis),
        Item::Trait(it) => named(&it.attrs, &it.ident, &it.vis),
        Item::TraitAlias(it) => named(&it.attrs, &it.ident, &it.vis),
        Item::Type(it) => named(&it.attrs, &it.ident, &it.vis),
        Item::Union(it) => named(&it.attrs, &it.ident, &it.vis),
        Item::Mod(it) => pinned(&it.attrs, Some(&it.ident)),
        Item::Macro(it) => pinned(&it.attrs, it.ident.as_ref()),
        Item::Impl(it) => pinned(&it.attrs, None),
        // A `use` or a foreign block keeps what it names in the file, test-gated or not.
        _ => pinned(&[], None),
    }
}

fn named<'a>(attrs: &'a [Attribute], name: &'a Ident, vis: &Visibility) -> Seen<'a> {
    let held = name == "main" || !matches!(vis, Visibility::Inherited);
    Seen {
        attrs,
        name: Some(name),
        entered: held,
        pinned: held,
    }
}

fn pinned<'a>(attrs: &'a [Attribute], name: Option<&'a Ident>) -> Seen<'a> {
    Seen {
        attrs,
        name,
        entered: true,
        pinned: true,
    }
}

fn node(item: &Item, seen: &Seen<'_>) -> Node {
    let mut uses = Names::default();
    uses.visit_item(item);
    let mut names: Vec<String> = seen.name.iter().map(ToString::to_string).collect();
    if let Item::Trait(it) = item {
        names.extend(it.items.iter().filter_map(trait_method));
    }
    Node {
        names,
        uses: uses.0,
        spans: vec![span_of(item)],
        entered: seen.entered,
        pinned: seen.pinned,
    }
}

fn trait_method(member: &TraitItem) -> Option<String> {
    match member {
        TraitItem::Fn(it) => Some(it.sig.ident.to_string()),
        _ => None,
    }
}

fn impl_method(member: &ImplItem) -> Option<String> {
    match member {
        ImplItem::Fn(it) => Some(it.sig.ident.to_string()),
        _ => None,
    }
}

/// Joins an `impl` to the local type it is for; an `impl` for any other type stays where it is.
fn attach(nodes: &mut Vec<Node>, types: &HashMap<String, usize>, block: &ItemImpl) {
    let index = match self_name(&block.self_ty).and_then(|name| types.get(&name)) {
        Some(&index) => index,
        None => {
            nodes.push(Node {
                entered: true,
                pinned: true,
                ..Node::default()
            });
            nodes.len().saturating_sub(1)
        }
    };
    let mut uses = Names::default();
    uses.visit_item_impl(block);
    let node = &mut nodes[index];
    node.uses.extend(uses.0);
    node.spans.push(span_of(block));
    if block.trait_.is_none() {
        node.names
            .extend(block.items.iter().filter_map(impl_method));
    }
}

fn self_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(path) if path.qself.is_none() => {
            path.path.segments.last().map(|it| it.ident.to_string())
        }
        Type::Reference(reference) => self_name(&reference.elem),
        _ => None,
    }
}

fn span_of(node: &impl Spanned) -> (usize, usize) {
    let span = node.span();
    (span.start().line, span.end().line)
}

/// Every word a node names. Macro and attribute input count by token, and a string there by its
/// words, since a format string or a `serde` attribute may name an item.
#[derive(Default)]
struct Names(HashSet<String>);

impl<'ast> Visit<'ast> for Names {
    fn visit_ident(&mut self, ident: &'ast Ident) {
        self.0.insert(ident.to_string());
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        token_words(mac.tokens.clone(), &mut |word| {
            self.0.insert(word);
        });
    }

    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        if let Meta::List(list) = &attr.meta {
            token_words(list.tokens.clone(), &mut |word| {
                self.0.insert(word);
            });
        }
    }
}

/// Each word of macro or attribute input: every name, and every word of a string.
pub(crate) fn token_words(tokens: TokenStream, out: &mut dyn FnMut(String)) {
    for token in tokens {
        match token {
            TokenTree::Group(group) => token_words(group.stream(), out),
            TokenTree::Ident(ident) => out(ident.to_string()),
            TokenTree::Literal(literal) => words_of(&literal.to_string()).for_each(&mut *out),
            TokenTree::Punct(_) => {}
        }
    }
}

/// What each node names, by index. The last list is the outside's: the nodes it enters.
fn edges(nodes: &[Node]) -> Vec<Vec<usize>> {
    let mut named: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        for name in &node.names {
            named.entry(name.as_str()).or_default().push(index);
        }
    }
    let mut edges: Vec<Vec<usize>> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| targets(node, index, &named))
        .collect();
    edges.push(
        (0..nodes.len())
            .filter(|&index| nodes[index].entered)
            .collect(),
    );
    edges
}

fn targets(node: &Node, index: usize, named: &HashMap<&str, Vec<usize>>) -> Vec<usize> {
    let mut to: Vec<usize> = node
        .uses
        .iter()
        .filter_map(|word| named.get(word.as_str()))
        .flatten()
        .copied()
        .filter(|&other| other != index)
        .collect();
    to.sort_unstable();
    to.dedup();
    to
}

/// Gives the outside an edge to each node it cannot reach, so code nothing calls still keeps what
/// it calls in the file.
fn reach_all(edges: &mut [Vec<usize>], root: usize) {
    let mut reached = vec![false; edges.len()];
    for node in postorder(edges, root) {
        reached[node] = true;
    }
    let missed: Vec<usize> = (0..reached.len()).filter(|&node| !reached[node]).collect();
    edges[root].extend(missed);
}

fn postorder(edges: &[Vec<usize>], root: usize) -> Vec<usize> {
    let mut seen = vec![false; edges.len()];
    let mut order = Vec::new();
    let mut stack = vec![(root, 0)];
    seen[root] = true;
    while let Some((node, next)) = stack.pop() {
        let Some(&child) = edges[node].get(next) else {
            order.push(node);
            continue;
        };
        stack.push((node, next.saturating_add(1)));
        if !seen[child] {
            seen[child] = true;
            stack.push((child, 0));
        }
    }
    order
}

/// Each node's immediate dominator, by the iterative method of Cooper, Harvey and Kennedy. Every
/// node must be reachable from `root`, which dominates itself.
fn dominators(edges: &[Vec<usize>], root: usize) -> Vec<usize> {
    let order = postorder(edges, root);
    let mut rank = vec![0; edges.len()];
    for (at, &node) in order.iter().enumerate() {
        rank[node] = at;
    }
    let preds = predecessors(edges);
    let mut idom = vec![usize::MAX; edges.len()];
    idom[root] = root;
    let mut changed = true;
    while changed {
        changed = false;
        for &node in order.iter().rev().skip(1) {
            let found = preds[node]
                .iter()
                .copied()
                .filter(|&pred| idom[pred] != usize::MAX)
                .reduce(|left, right| meet(&idom, &rank, left, right));
            if let Some(found) = found
                && idom[node] != found
            {
                idom[node] = found;
                changed = true;
            }
        }
    }
    idom
}

fn predecessors(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut preds = vec![Vec::new(); edges.len()];
    for (from, to) in edges.iter().enumerate() {
        for &node in to {
            preds[node].push(from);
        }
    }
    preds
}

/// The nearest node that dominates both, found by walking up from the one ranked lower.
fn meet(idom: &[usize], rank: &[usize], mut left: usize, mut right: usize) -> usize {
    loop {
        match rank[left].cmp(&rank[right]) {
            Ordering::Less => left = idom[left],
            Ordering::Greater => right = idom[right],
            Ordering::Equal => return left,
        }
    }
}

/// `node` and each node above it in the dominator tree, short of the outside.
fn chain(idom: &[usize], node: usize) -> impl Iterator<Item = usize> + '_ {
    let root = idom.len().saturating_sub(1);
    std::iter::successors(Some(node), move |&at| Some(idom[at])).take_while(move |&at| at != root)
}

/// The parts whose owner may move and holds enough, where no larger such part holds them.
fn owned(nodes: &[Node], idom: &[usize], lines: usize) -> Vec<Part> {
    let mut held = vec![(0_usize, 0_usize); nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        for owner in chain(idom, index) {
            held[owner].0 += node.spans.len();
            held[owner].1 += node.lines();
        }
    }
    let movable = |index: usize| {
        let (items, size) = held[index];
        !nodes[index].pinned && items >= 2 && size >= PART && lines.saturating_sub(size) >= PART
    };
    (0..nodes.len())
        .filter(|&index| movable(index) && !chain(idom, idom[index]).any(movable))
        .map(|index| part(&nodes[index], held[index]))
        .collect()
}

fn part(owner: &Node, (items, lines): (usize, usize)) -> Part {
    let (first, last) = owner.spans.first().copied().unwrap_or_default();
    Part {
        owner: owner.names.first().cloned().unwrap_or_default(),
        line: line_of(first),
        last: line_of(last),
        items,
        lines,
    }
}

fn line_of(line: usize) -> u32 {
    u32::try_from(line).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::testdir::Held;

    /// A private function of `lines` lines that calls each of `calls`. Blank lines fill it, since a
    /// span counts them and they parse at no cost: a statement on each line took minutes under Miri.
    fn function(name: &str, calls: &[&str], lines: usize) -> String {
        let mut out = format!("fn {name}() {{\n");
        for call in calls {
            out.push_str(&format!("    {call}();\n"));
        }
        out.push_str(&"\n".repeat(lines.saturating_sub(calls.len() + 2)));
        out.push_str("}\n");
        out
    }

    /// `entry` calls `a` and `b`; `a` alone calls `a1` and `a2`, and `b` alone calls `b1`.
    fn two_parts(extra: &str) -> String {
        [
            "pub fn entry() {\n    a();\n    b();\n}\n".to_string(),
            function("a", &["a1", "a2"], 20),
            function("a1", &[], 50),
            function("a2", &[], 50),
            function("b", &["b1"], 60),
            function("b1", &[], 60),
            extra.to_string(),
        ]
        .concat()
    }

    /// `entry` calls `a` and `other`; `a` alone calls `a1`.
    fn beside(other: usize, a: usize, a1: usize) -> String {
        [
            "pub fn entry() {\n    a();\n    other();\n}\n".to_string(),
            function("other", &[], other),
            function("a", &["a1"], a),
            function("a1", &[], a1),
        ]
        .concat()
    }

    fn parted(src: &str) -> Vec<Part> {
        parts(src, src.lines().count(), &|_| false)
    }

    fn owners(src: &str) -> Vec<String> {
        parted(src).into_iter().map(|part| part.owner).collect()
    }

    fn part(owner: &str, line: u32, last: u32, items: usize, lines: usize) -> Part {
        Part {
            owner: owner.to_string(),
            line,
            last,
            items,
            lines,
        }
    }

    #[test]
    fn each_item_that_alone_uses_a_hundred_lines_owns_a_part() {
        let found = parted(&two_parts(""));
        assert_eq!(
            found,
            [part("a", 5, 24, 3, 120), part("b", 125, 184, 2, 120)]
        );
    }

    #[test]
    fn a_helper_two_owners_share_stays_with_neither() {
        let src = two_parts("").replace("    b1();\n", "    b1();\n    a1();\n");
        assert_eq!(owners(&src), ["b"]);
    }

    #[test]
    fn an_item_the_tests_name_stays_in_the_file() {
        let tests = "#[cfg(test)]\nmod tests {\n    fn t() {\n        super::a2();\n    }\n}\n";
        assert_eq!(owners(&two_parts(tests)), ["b"]);
        assert_eq!(owners(&format!("{tests}{}", two_parts(""))), ["b"]);
    }

    #[test]
    fn an_item_a_module_below_names_stays_in_the_file() {
        let found = parts(&two_parts(""), 244, &|word| word == "b1");
        assert_eq!(found, [part("a", 5, 24, 3, 120)]);
    }

    #[test]
    fn code_nothing_calls_still_keeps_what_it_calls_in_the_file() {
        assert_eq!(owners(&two_parts(&function("unused", &["a2"], 3))), ["b"]);
    }

    #[test]
    fn a_name_in_a_format_string_or_an_attribute_input_is_a_use() {
        let shown = "pub fn show() -> String {\n    format!(\"{a2}\")\n}\n";
        assert_eq!(owners(&two_parts(shown)), ["b"]);
        let field = "pub struct S {\n    #[serde(default = \"b1\")]\n    x: u32,\n}\n";
        assert_eq!(owners(&two_parts(field)), ["a"]);
    }

    #[test]
    fn a_part_and_a_rest_of_a_hundred_lines_each_are_enough() {
        assert_eq!(owners(&beside(96, 50, 50)), ["a"]);
        assert_eq!(owners(&beside(95, 50, 50)), Vec::<String>::new());
        assert_eq!(owners(&beside(96, 50, 49)), Vec::<String>::new());
    }

    #[test]
    fn one_long_function_with_no_helper_of_its_own_is_no_part() {
        let src = [
            "pub fn entry() {\n    a();\n    other();\n}\n".to_string(),
            function("other", &[], 120),
            function("a", &[], 150),
        ]
        .concat();
        assert_eq!(owners(&src), Vec::<String>::new());
    }

    #[test]
    fn a_part_inside_a_larger_part_moves_with_it() {
        let src = [
            beside(120, 10, 10).replace("fn a1() {\n", "fn a1() {\n    a11();\n"),
            function("a11", &[], 95),
        ]
        .concat();
        assert_eq!(owners(&src), ["a"]);
    }

    #[test]
    fn a_type_moves_with_its_impls_and_what_only_they_use() {
        let src = [
            "pub fn entry() {\n    Toml::load();\n    other();\n}\n".to_string(),
            function("other", &[], 120),
            "struct Toml;\n".to_string(),
            "impl Toml {\n    fn load() {\n        parse();\n    }\n}\n".to_string(),
            function("parse", &[], 100),
        ]
        .concat();
        assert_eq!(parted(&src), [part("Toml", 125, 125, 3, 106)]);
        let probed = format!("{src}#[cfg(test)]\nimpl Toml {{\n    fn probe() {{}}\n}}\n");
        assert_eq!(parted(&probed), [part("Toml", 125, 125, 3, 106)]);
    }

    fn node(names: &[&str], uses: &[&str], spans: &[(usize, usize)]) -> Node {
        Node {
            names: names.iter().map(ToString::to_string).collect(),
            uses: uses.iter().map(ToString::to_string).collect(),
            spans: spans.to_vec(),
            ..Node::default()
        }
    }

    #[test]
    fn a_node_reaches_each_other_node_it_names_and_the_outside_enters_the_entered() {
        let entered = Node {
            entered: true,
            ..node(&["c"], &["T"], &[])
        };
        let nodes = [
            node(&["a"], &["a", "b", "m"], &[]),
            node(&["T", "m"], &["x"], &[]),
            entered,
        ];
        assert_eq!(edges(&nodes), [vec![1], vec![], vec![1], vec![2]]);
    }

    #[test]
    fn a_node_two_paths_reach_is_dominated_where_the_paths_part() {
        let edges = [vec![1, 2], vec![3], vec![3], vec![4], vec![]];
        assert_eq!(dominators(&edges, 0), [0, 0, 0, 0, 3]);
    }

    #[test]
    fn a_loop_two_entries_reach_is_settled_by_a_second_pass() {
        let edges = [
            vec![1, 2],
            vec![3],
            vec![4, 5],
            vec![4],
            vec![3, 5],
            vec![4],
        ];
        assert_eq!(dominators(&edges, 0), [0; 6]);
    }

    #[test]
    fn a_postorder_visits_each_node_once_and_after_what_it_reaches() {
        assert_eq!(postorder(&[vec![1], vec![0]], 0), [1, 0]);
        let diamond = [vec![1, 2], vec![3], vec![3], vec![]];
        assert_eq!(postorder(&diamond, 0), [3, 1, 2, 0]);
    }

    #[test]
    fn an_impl_is_joined_to_the_last_name_of_its_type() {
        let named = |src: &str| self_name(&syn::parse_str::<Type>(src).unwrap());
        assert_eq!(named("&mut a::T").as_deref(), Some("T"));
        assert_eq!(named("<T as R>::Y"), None);
        assert_eq!(named("[u8]"), None);
    }

    #[test]
    fn an_attribute_with_no_list_names_nothing() {
        let item: Item = syn::parse_str("#[inline]\n#[doc = \"word\"]\nfn f() {}").unwrap();
        let mut names = Names::default();
        names.visit_item(&item);
        assert_eq!(names.0, HashSet::from(["f".to_string()]));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_that_cannot_be_listed_has_no_words() {
        let mut root = Held::tree("splits-unlisted", &[("src/lib.rs", "")]);
        root.listing = std::sync::Arc::new(std::sync::OnceLock::from(Err("no list".to_string())));
        assert_eq!(words_by_file(&root).err().as_deref(), Some("no list"));
    }

    #[test]
    fn a_part_holds_what_its_owner_dominates_and_leaves_enough_behind() {
        let nodes = [
            node(&["a"], &[], &[(1, 10)]),
            node(&["b"], &[], &[(11, 110)]),
        ];
        assert_eq!(owned(&nodes, &[2, 0, 2], 230), [part("a", 1, 10, 2, 110)]);
        assert_eq!(owned(&nodes, &[2, 0, 2], 200), []);
    }

    #[test]
    fn a_source_the_parser_rejects_has_no_parts() {
        assert_eq!(parted("fn ("), []);
    }

    #[test]
    fn each_kind_of_item_is_named_or_held_where_it_is_written() {
        let src = "const C: u8 = 0;\nenum E {}\nstatic S: u8 = 0;\nstruct T;\n\
                   trait R { fn r(); const K: u8; }\ntrait A = R;\ntype Y = T;\nunion U { a: u8 }\npub(crate) struct P;\n\
                   fn main() {}\nmod m {}\nmacro_rules! mac { () => {} }\nmac!();\nuse std::fmt;\n\
                   extern crate alloc;\nextern \"C\" {}\nmacro v() {}\n\
                   impl T { const Z: u8 = 0; fn new() {} }\nimpl R for &T {}\nimpl R for u8 {}\n";
        let file = prodlines::parse_rust(src).unwrap();
        let found: Vec<String> = nodes(&file, &|_| false)
            .into_iter()
            .map(|node| format!("{} {} {}", node.names.join(","), node.pinned, node.entered))
            .collect();
        let expected = [
            "C false false",
            "E false false",
            "S false false",
            "T,new false false",
            "R,r false false",
            "A false false",
            "Y false false",
            "U false false",
            "P true true",
            "main true true",
            "m true true",
            "mac true true",
            " true true",
            " true true",
            " true true",
            " true true",
            " true true",
            " true true",
        ];
        assert_eq!(found, expected);
    }

    #[test]
    fn the_largest_part_is_the_move_a_finding_asks_for() {
        let file = Parted {
            shown: "src/config.rs".to_string(),
            lines: 300,
            parts: vec![part("Toml", 125, 130, 3, 106), part("b", 200, 260, 2, 101)],
        };
        let told = detail(&file).unwrap();
        assert_eq!(told.line, Some(125));
        let second = Place::at("can move out", "src/config.rs", 200)
            .through(260)
            .item("b");
        assert_eq!(told.places[1], second);
        assert_eq!(
            told.fix.as_deref(),
            Some(
                "move `Toml` with the items only it uses (3 items, 106 lines) to `src/config/toml.rs`"
            )
        );
        let none = Parted {
            parts: Vec::new(),
            ..file
        };
        assert_eq!(detail(&none), None);
    }

    #[test]
    fn a_part_moves_to_a_child_of_the_module_it_leaves() {
        assert_eq!(target("src/a/b.rs", "HttpClient"), "src/a/b/http_client.rs");
        assert_eq!(target("src/a/mod.rs", "run_all"), "src/a/run_all.rs");
        assert_eq!(target("src/lib.rs", "BUDGET"), "src/budget.rs");
        assert_eq!(target("main.rs", "Toml"), "toml.rs");
        assert_eq!(target("build.rs", "owner"), "build/owner.rs");
        assert_eq!(snake("Version2Name"), "version2_name");
    }

    #[test]
    fn a_mod_rs_is_not_below_itself() {
        let held =
            |file: &str, word: &str| (PathBuf::from(file), HashSet::from([word.to_string()]));
        let words = vec![held("src/a/mod.rs", "own"), held("src/a/b.rs", "child")];
        let below = used_below(&words, Path::new("src/a/mod.rs"));
        assert!(!below("own") && below("child"));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_name_a_child_module_uses_keeps_its_item_in_the_parent() {
        let parent = format!("mod child;\n{}", two_parts(""));
        let flat = function("flat", &[], 250);
        let root = Held::tree(
            "splits-child",
            &[
                ("src/lib.rs", "pub mod big;\npub mod flat;\n"),
                ("src/big.rs", parent.as_str()),
                ("src/big/child.rs", "fn c() {\n    super::b1();\n}\n"),
                ("src/flat.rs", flat.as_str()),
            ],
        );
        let mut read = measure(&root).unwrap();
        assert_eq!(read.series.get("src/big.rs"), Some(1));
        assert_eq!(read.series.get("src/flat.rs"), None);
        let told = read.details.remove("src/big.rs").unwrap();
        assert_eq!(told.places[0].item.as_deref(), Some("a"));
        assert!(told.fix.unwrap().ends_with("to `src/big/a.rs`"));
    }

    #[test]
    fn the_gate_is_a_ratchet_over_items_counted_in_movable_parts() {
        assert_eq!(GATE.name, "splits");
        assert_eq!(GATE.group, Group::Quality);
        assert!(matches!(
            GATE.kind,
            Kind::AnnotatedRatchet {
                keys: Keys::Items,
                unit: "movable parts",
                ..
            }
        ));
    }
}
