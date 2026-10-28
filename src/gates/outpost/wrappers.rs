//! The wrapper gate: a shipped Rust function whose body is one call and that one function calls,
//! read from Outpost's call graph. docs/gates.md holds the rule whole.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde::Deserialize;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Block, Expr, ExprCall, ExprMethodCall, Ident, ImplItemFn, ItemFn, ItemImpl, ItemMod,
    ItemTrait, Signature, Stmt, UnOp, Visibility,
};

use crate::gates::metrics::{dead, lean, prodlines};
use crate::run::baseline::{Keys, Series};
use crate::run::report::{Detail, Finding, Place};
use crate::run::{Ctx, Gate, Group, Kind, Measurement};

pub const GATE: Gate = Gate {
    name: "wrappers",
    about: "shipped Rust functions that are one call and have one caller, which could make that \
            call itself; needs `outpost`",
    group: Group::OptIn,
    builds: false,
    reads: Some(crate::run::verdicts::OUTPOST),
    kind: Kind::AnnotatedRatchet {
        measure,
        keys: Keys::Items,
        unit: "wrapper line(s)",
    },
};

const FIX: &str = "make each wrapper's call in its one caller, then remove the wrapper";

/// The parts of `outpost graph export --format json` this gate reads.
#[derive(Deserialize)]
struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

#[derive(Deserialize)]
struct Node {
    id: u64,
    file: String,
    name: String,
    kind: String,
    /// The first line of the entity, its doc comment included.
    line: u32,
    end_line: u32,
}

#[derive(Deserialize)]
struct Edge {
    from: u64,
    to: u64,
    relation: String,
}

/// Each file's settled wrappers, and the others as candidates with the reason they are not settled.
fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    wrappers_in(ctx, &exported(&ctx.root, super::spawn)?)
}

/// A wrapper is settled when the tree names it only at its definition and its one call, that call
/// is in the same file, and it is not `pub`. Test callers and uses that are not calls are left out.
fn wrappers_in(ctx: &Ctx, graph: &Graph) -> Result<Measurement, String> {
    let named = dead::names_in_tree(ctx)?;
    let mut files = Files::default();
    let mut read = Measurement::of(Series::new(), Vec::new());
    for (node, caller) in called_once(graph) {
        let Some(shape) = files.read(&ctx.root, &node.file)?.wrapper(node) else {
            continue;
        };
        let (public, callee) = (shape.public, shape.callee.clone());
        if prodlines::is_test_path(&caller.file) {
            continue;
        }
        let at = files.read(&ctx.root, &caller.file)?;
        if at.in_tests(caller.line) || !at.calls(&node.name, caller) {
            continue;
        }
        let why = if named.get(&node.name) != Some(&2) {
            Some("the tree names it beyond that call")
        } else if caller.file != node.file {
            Some("its caller is in another file, which may not see what it uses")
        } else if public {
            Some("it is `pub`, so code outside this tree may call it")
        } else {
            None
        };
        record(&mut read, (node, &caller.name, &callee), why);
    }
    Ok(read)
}

/// Where `outpost` writes the call graph in the project; the gate removes the file once read.
const EXPORTED: &str = "target/test-scratch/chock/graph.json";

/// The call graph, through a file, since a large tree's graph is more than chock captures. `run`
/// runs `outpost` in the root, so a test can stand in for it.
fn exported(
    root: &Path,
    run: fn(&Path, &[&str]) -> Result<crate::exec::Output, String>,
) -> Result<Graph, String> {
    let file = root.join(EXPORTED);
    // A directory that cannot be made leaves no graph, which the read below reports.
    drop(file.parent().map(std::fs::create_dir_all));
    let path = file.to_string_lossy();
    let argv = ["graph", "export", "--format", "json", "--output", &path];
    let read = run(root, &argv).and_then(|out| {
        if !out.success() {
            let why = out.why_it_failed();
            return Err(format!(
                "outpost could not export the call graph of this tree: {why}"
            ));
        }
        let text = std::fs::read(&file).map_err(|e| format!("outpost wrote no call graph: {e}"))?;
        serde_json::from_slice(&text)
            .map_err(|e| format!("outpost wrote a call graph chock cannot read: {e}"))
    });
    drop(std::fs::remove_file(&file));
    read
}

/// Each Rust function the graph shows one other function calling, with that caller, in file and
/// line order.
fn called_once(graph: &Graph) -> Vec<(&Node, &Node)> {
    let by_id: HashMap<u64, &Node> = graph.nodes.iter().map(|node| (node.id, node)).collect();
    let mut callers: HashMap<u64, BTreeSet<u64>> = HashMap::new();
    for edge in graph
        .edges
        .iter()
        .filter(|edge| edge.relation == "calls" && edge.from != edge.to)
    {
        callers.entry(edge.to).or_default().insert(edge.from);
    }
    let mut once: Vec<(&Node, &Node)> = graph
        .nodes
        .iter()
        .filter(|node| is_rust_function(node))
        .filter_map(|node| {
            let from = callers.get(&node.id).filter(|from| from.len() == 1)?;
            Some((node, *by_id.get(from.first()?)?))
        })
        .collect();
    once.sort_by_key(|(node, _)| (&node.file, node.line));
    once
}

fn is_rust_function(node: &Node) -> bool {
    matches!(node.kind.as_str(), "function" | "method") && node.file.ends_with(".rs")
}

/// A settled wrapper adds its lines to its file and a place to the file's detail; an unsettled one
/// is a candidate that says why.
fn record(read: &mut Measurement, (node, caller, callee): (&Node, &str, &str), why: Option<&str>) {
    let name = &node.name;
    if let Some(why) = why {
        let message =
            format!("`{name}` is one call to `{callee}` and only `{caller}` calls it, but {why}");
        let mut finding = Finding::at(&node.file, &message)
            .line(node.line)
            .candidate();
        finding.fix = Some(format!(
            "make the call to `{callee}` in `{caller}` if nothing else needs `{name}`"
        ));
        read.findings.push(finding);
        return;
    }
    let lines = u64::from(node.end_line.saturating_sub(node.line) + 1);
    read.series
        .set(&node.file, read.series.get(&node.file).unwrap_or(0) + lines);
    let fix = Some(FIX.to_string());
    let detail = read
        .details
        .entry(node.file.clone())
        .or_insert_with(|| Detail {
            line: Some(node.line),
            places: Vec::new(),
            fix,
        });
    detail.places.push(
        Place::at("wrapper", &node.file, node.line)
            .through(node.end_line)
            .item(name),
    );
}

/// Each file the gate reads, parsed once.
#[derive(Default)]
struct Files(HashMap<String, Parsed>);

impl Files {
    fn read(&mut self, root: &Path, file: &str) -> Result<&Parsed, String> {
        if !self.0.contains_key(file) {
            let src = std::fs::read_to_string(root.join(file))
                .map_err(|e| format!("outpost indexed {file}, which chock cannot read: {e}"))?;
            let parsed = Parsed::of(&src).map_err(|why| format!("{file}: {why}"))?;
            self.0.insert(file.to_string(), parsed);
        }
        Ok(&self.0[file])
    }
}

/// What the gate reads of one file: its wrappers, the name and line of each call it makes, and
/// the spans only tests compile.
struct Parsed {
    shapes: Vec<Shape>,
    calls: Vec<(String, u32)>,
    tests: Vec<(usize, usize)>,
}

impl Parsed {
    fn of(src: &str) -> Result<Self, String> {
        let file = prodlines::parse_rust(src)?;
        let mut shapes = Shapes::default();
        shapes.visit_file(&file);
        let mut sites = Sites::default();
        sites.visit_file(&file);
        Ok(Parsed {
            shapes: shapes.0,
            calls: sites.0,
            tests: prodlines::test_spans(src)?,
        })
    }

    /// The wrapper the graph's `node` spans, matched by name and the line of that name.
    fn wrapper(&self, node: &Node) -> Option<&Shape> {
        let spans = |shape: &&Shape| {
            node.name == shape.name && (node.line..=node.end_line).contains(&shape.line)
        };
        self.shapes.iter().find(spans)
    }

    /// Whether `caller` calls `name`, rather than naming it as a value or inside a macro.
    fn calls(&self, name: &str, caller: &Node) -> bool {
        let within = caller.line..=caller.end_line;
        self.calls
            .iter()
            .any(|(called, line)| called == name && within.contains(line))
    }

    fn in_tests(&self, line: u32) -> bool {
        let line = usize::try_from(line).unwrap_or(usize::MAX);
        self.tests
            .iter()
            .any(|&(from, to)| (from..=to).contains(&line))
    }
}

/// The name and line of each call a file makes: a call through a path, or a method call.
#[derive(Default)]
struct Sites(Vec<(String, u32)>);

impl<'ast> Visit<'ast> for Sites {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Expr::Path(path) = &*node.func
            && let Some(last) = path.path.segments.last()
        {
            self.0.push((last.ident.to_string(), line_of(&last.ident)));
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        self.0
            .push((node.method.to_string(), line_of(&node.method)));
        visit::visit_expr_method_call(self, node);
    }
}

fn line_of(ident: &Ident) -> u32 {
    u32::try_from(ident.span().start().line).unwrap_or(u32::MAX)
}

/// A function whose body is one call: its name, the line of its name, whether it is `pub`, and
/// its callee.
struct Shape {
    name: String,
    line: u32,
    public: bool,
    callee: String,
}

/// The wrappers outside test modules, trait impls and traits, whose attributes are doc only and
/// that pass their parameters on changed, since `lean` counts those that pass them unchanged.
#[derive(Default)]
struct Shapes(Vec<Shape>);

impl Shapes {
    fn function(&mut self, attrs: &[Attribute], vis: &Visibility, sig: &Signature, block: &Block) {
        let doc_only = attrs.iter().all(|attr| attr.path().is_ident("doc"));
        let plain = doc_only && lean::forwarded(sig, block).is_none();
        if let Some(callee) = one_call(block, &sig.ident).filter(|_| plain) {
            self.0.push(Shape {
                name: sig.ident.to_string(),
                line: line_of(&sig.ident),
                public: matches!(vis, Visibility::Public(_)),
                callee,
            });
        }
    }
}

impl<'ast> Visit<'ast> for Shapes {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        if !prodlines::is_test_gated(&node.attrs) {
            visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        if node.trait_.is_none() {
            visit::visit_item_impl(self, node);
        }
    }

    fn visit_item_trait(&mut self, _: &'ast ItemTrait) {}

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.function(&node.attrs, &node.vis, &node.sig, &node.block);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.function(&node.attrs, &node.vis, &node.sig, &node.block);
    }
}

/// The callee when `block` is one expression that makes exactly one call, does no work of its
/// own, and does not call or name `own`.
fn one_call(block: &Block, own: &Ident) -> Option<String> {
    let [Stmt::Expr(tail, _)] = block.stmts.as_slice() else {
        return None;
    };
    let mut calls = Calls {
        own,
        callees: Vec::new(),
        other: false,
    };
    calls.visit_expr(tail);
    match calls.callees.as_slice() {
        [callee] if !calls.other => Some(callee.clone()),
        _ => None,
    }
}

/// The calls an expression makes, and whether it holds anything a wrapper does not.
struct Calls<'a> {
    own: &'a Ident,
    callees: Vec<String>,
    other: bool,
}

impl Calls<'_> {
    /// A call through a path calls a function, unless the path ends in a tuple struct or a variant,
    /// which builds a value; a call through any other expression is no wrapper's.
    fn call(&mut self, func: &Expr) {
        let Expr::Path(path) = func else {
            self.other = true;
            return;
        };
        let names: Vec<String> = path
            .path
            .segments
            .iter()
            .map(|it| it.ident.to_string())
            .collect();
        let last = names.last().map_or("", String::as_str);
        self.other |= *self.own == last;
        if !last.starts_with(char::is_uppercase) {
            self.callees.push(names.join("::"));
        }
    }
}

/// Whether an expression does work or names a fact, which a wrapper's call does not, such as a
/// branch, arithmetic, a closure, a built value, a literal, or a path to a constant or a variant.
fn is_work(node: &Expr) -> bool {
    match node {
        Expr::Path(path) => path
            .path
            .segments
            .last()
            .is_some_and(|last| last.ident.to_string().starts_with(char::is_uppercase)),
        Expr::Unary(unary) => !matches!(unary.op, UnOp::Deref(_)),
        _ => matches!(
            node,
            Expr::Macro(_)
                | Expr::Closure(_)
                | Expr::Block(_)
                | Expr::Unsafe(_)
                | Expr::Async(_)
                | Expr::If(_)
                | Expr::Match(_)
                | Expr::Loop(_)
                | Expr::While(_)
                | Expr::ForLoop(_)
                | Expr::Assign(_)
                | Expr::Let(_)
                | Expr::Return(_)
                | Expr::Break(_)
                | Expr::Continue(_)
                | Expr::Binary(_)
                | Expr::Lit(_)
                | Expr::Range(_)
                | Expr::Struct(_)
                | Expr::Array(_)
                | Expr::Repeat(_)
                | Expr::Tuple(_)
        ),
    }
}

impl<'ast> Visit<'ast> for Calls<'_> {
    /// A call's path is its callee, not an argument, so only its arguments are judged as work.
    fn visit_expr(&mut self, node: &'ast Expr) {
        match node {
            Expr::Call(call) => {
                self.call(&call.func);
                call.args.iter().for_each(|arg| self.visit_expr(arg));
            }
            Expr::MethodCall(call) => {
                self.callees.push(call.method.to_string());
                self.other |= call.method == *self.own;
                self.visit_expr(&call.receiver);
                call.args.iter().for_each(|arg| self.visit_expr(arg));
            }
            _ => {
                self.other |= is_work(node);
                visit::visit_expr(self, node);
            }
        }
    }

    fn visit_ident(&mut self, node: &'ast Ident) {
        self.other |= node == self.own;
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    /// The callee of the one function in `src`, when it is a wrapper's one call.
    fn callee(src: &str) -> Option<String> {
        let item: ItemFn = syn::parse_str(src).unwrap();
        one_call(&item.block, &item.sig.ident)
    }

    #[test]
    fn a_body_that_only_passes_names_to_one_call_is_a_wrapper_of_that_callee() {
        let cases = [
            ("fn f(x: u32) -> u32 { g(&x) }", Some("g")),
            ("fn f(&self) -> usize { self.items.len() }", Some("len")),
            // A tuple struct or a variant builds a value, so it is no second call.
            ("fn f(x: &u8) -> T { Some(m::g(*x)) }", Some("m::g")),
            ("fn f(&self) { g(&self.n); }", Some("g")),
            ("fn f() -> u32 { g(h()) }", None),
            ("fn f() -> u32 { g(); h() }", None),
            ("fn f(n: &u32) -> u32 { f(n) }", None),
            ("fn f() -> String { format!(\"{}\", g()) }", None),
            ("fn f(x: bool) -> u32 { if x { g() } else { 0 } }", None),
            ("fn f(g: fn() -> u32) -> u32 { (g)() }", None),
            ("fn f() -> u32 { g(|x| x) }", None),
            ("fn f() -> u32 { let x = g(); }", None),
            ("fn f() -> u32 { 1 }", None),
            // Arithmetic, a test, a literal, a range or a constant is work or a fact with a name.
            ("fn f(x: u32) -> u32 { g(x + 1) }", None),
            ("fn f(x: bool) -> bool { !g(x) }", None),
            ("fn f(c: char) -> bool { ('a'..='f').contains(&c) }", None),
            ("fn f(x: u32) -> u32 { g(x, LIMIT) }", None),
            ("fn f(x: u32) -> u32 { g(S { x }) }", None),
            ("fn f(x: u32) -> u32 { g([x]) }", None),
            ("fn f(&self) -> u32 { self.f() }", None),
        ];
        for (src, wrapped) in cases {
            assert_eq!(callee(src).as_deref(), wrapped, "{src}");
        }
    }

    #[test]
    fn a_trait_item_a_test_an_attribute_or_an_unchanged_pass_is_no_wrapper_here() {
        let src = "impl T for S { fn a(&self) -> u32 { g(&self.n) } }\n\
                   impl S { fn b(&self) -> u32 { g(&self.n) } }\n\
                   #[inline] fn c(x: u32) -> u32 { g(&x) }\n\
                   fn d(x: u32) -> u32 { g(x) }\n\
                   #[cfg(test)] mod tests { fn e(x: u32) -> u32 { g(&x) } }\n\
                   trait U { fn f(&self) -> u32 { g(&self.n) } }\n";
        let mut shapes = Shapes::default();
        shapes.visit_file(&prodlines::parse_rust(src).unwrap());
        let names: Vec<_> = shapes
            .0
            .iter()
            .map(|shape| (shape.name.as_str(), shape.line))
            .collect();
        assert_eq!(names, [("b", 2)]);
    }

    #[test]
    fn a_function_counts_once_called_when_one_other_function_calls_it() {
        let graph: Graph = serde_json::from_str(
            r#"{"nodes": [
              {"id": 1, "file": "src/a.rs", "name": "a", "kind": "function", "line": 1, "end_line": 3},
              {"id": 2, "file": "src/a.rs", "name": "b", "kind": "function", "line": 4, "end_line": 9},
              {"id": 3, "file": "src/a.rs", "name": "c", "kind": "method", "line": 10, "end_line": 12},
              {"id": 4, "file": "py/d.py", "name": "d", "kind": "function", "line": 1, "end_line": 2},
              {"id": 5, "file": "src/a.rs", "name": "e", "kind": "method", "line": 13, "end_line": 15},
              {"id": 6, "file": "src/a.rs", "name": "f", "kind": "function", "line": 16, "end_line": 18},
              {"id": 7, "file": "src/a.rs", "name": "S", "kind": "struct", "line": 19, "end_line": 19}],
             "edges": [
              {"from": 2, "to": 1, "relation": "calls"}, {"from": 2, "to": 1, "relation": "calls"},
              {"from": 1, "to": 3, "relation": "calls"}, {"from": 2, "to": 3, "relation": "calls"},
              {"from": 2, "to": 4, "relation": "calls"},
              {"from": 5, "to": 5, "relation": "calls"}, {"from": 2, "to": 5, "relation": "calls"},
              {"from": 2, "to": 6, "relation": "names"}, {"from": 2, "to": 7, "relation": "calls"}],
             "communities": []}"#,
        )
        .unwrap();
        let pairs: Vec<_> = called_once(&graph)
            .iter()
            .map(|(node, caller)| (node.file.as_str(), node.name.as_str(), caller.name.as_str()))
            .collect();
        assert_eq!(pairs, [("src/a.rs", "a", "b"), ("src/a.rs", "e", "b")]);
    }

    /// Lines 1-4 a wrapper; 11-13 one also named as a value; 20-22 one a test calls; 30-32 a `pub`
    /// one; 36-38 one only named as a value; 42-44 one whose caller is in another file.
    const TREE: &str = "/// Reads it.\nfn wrap(x: u32) -> u32 {\n    inner(&x)\n}\n\
        fn inner(x: &u32) -> u32 {\n    *x\n}\n\
        pub fn caller() -> u32 {\n    wrap(1)\n}\n\
        fn named(x: u32) -> u32 {\n    inner(&x)\n}\n\
        pub fn other() -> u32 {\n    named(2)\n}\n\
        pub fn pointer() -> fn(u32) -> u32 {\n    named\n}\n\
        fn tested(x: u32) -> u32 {\n    inner(&x)\n}\n\
        #[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        super::tested(1);\n    }\n}\n\
        pub fn open(x: u32) -> u32 {\n    inner(&x)\n}\n\
        pub fn opener() -> u32 {\n    open(3)\n}\n\
        fn handler(x: u32) -> u32 {\n    inner(&x)\n}\n\
        pub fn route() -> fn(u32) -> u32 {\n    handler\n}\n\
        fn near(x: u32) -> u32 {\n    inner(&x)\n}\n\
        mod b;\n";

    /// Called only from a test file: `plain`, which is no wrapper, and `helper`, which is one.
    const AHEAD: &str =
        "fn plain() -> u32 {\n    1\n}\nfn helper(x: u32) -> u32 {\n    inner(&x)\n}\n";

    /// The one caller of the wrapper at line 42 of `TREE`.
    const FAR: &str = "pub fn far() -> u32 {\n    super::near(4)\n}\n";

    const GRAPH: &str = r#"{"nodes": [
      {"id": 1, "file": "src/lib.rs", "name": "wrap", "kind": "function", "line": 1, "end_line": 4},
      {"id": 2, "file": "src/lib.rs", "name": "inner", "kind": "function", "line": 5, "end_line": 7},
      {"id": 3, "file": "src/lib.rs", "name": "caller", "kind": "function", "line": 8, "end_line": 10},
      {"id": 4, "file": "src/lib.rs", "name": "named", "kind": "function", "line": 11, "end_line": 13},
      {"id": 5, "file": "src/lib.rs", "name": "other", "kind": "function", "line": 14, "end_line": 16},
      {"id": 6, "file": "src/lib.rs", "name": "pointer", "kind": "function", "line": 17, "end_line": 19},
      {"id": 7, "file": "src/lib.rs", "name": "tested", "kind": "function", "line": 20, "end_line": 22},
      {"id": 8, "file": "src/lib.rs", "name": "t", "kind": "function", "line": 26, "end_line": 28},
      {"id": 9, "file": "src/lib.rs", "name": "open", "kind": "function", "line": 30, "end_line": 32},
      {"id": 10, "file": "src/lib.rs", "name": "opener", "kind": "function", "line": 33, "end_line": 35},
      {"id": 11, "file": "src/lib.rs", "name": "handler", "kind": "function", "line": 36, "end_line": 38},
      {"id": 12, "file": "src/lib.rs", "name": "route", "kind": "function", "line": 39, "end_line": 41},
      {"id": 13, "file": "src/lib.rs", "name": "near", "kind": "function", "line": 42, "end_line": 44},
      {"id": 14, "file": "src/b.rs", "name": "far", "kind": "function", "line": 1, "end_line": 3},
      {"id": 15, "file": "src/a.rs", "name": "plain", "kind": "function", "line": 1, "end_line": 3},
      {"id": 16, "file": "src/a.rs", "name": "helper", "kind": "function", "line": 4, "end_line": 6},
      {"id": 17, "file": "tests/it.rs", "name": "it", "kind": "function", "line": 1, "end_line": 3}],
     "edges": [
      {"from": 3, "to": 1, "relation": "calls"}, {"from": 1, "to": 2, "relation": "calls"},
      {"from": 4, "to": 2, "relation": "calls"}, {"from": 7, "to": 2, "relation": "calls"},
      {"from": 5, "to": 4, "relation": "calls"}, {"from": 8, "to": 7, "relation": "calls"},
      {"from": 9, "to": 2, "relation": "calls"}, {"from": 10, "to": 9, "relation": "calls"},
      {"from": 11, "to": 2, "relation": "calls"}, {"from": 12, "to": 11, "relation": "calls"},
      {"from": 13, "to": 2, "relation": "calls"}, {"from": 14, "to": 13, "relation": "calls"},
      {"from": 17, "to": 15, "relation": "calls"}, {"from": 17, "to": 16, "relation": "calls"}]}"#;

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_private_wrapper_called_once_in_its_file_counts_and_others_are_candidates() {
        let ctx = crate::testdir::Held::tree(
            "gate-wrappers",
            &[
                ("Cargo.toml", "[package]\nname = \"p\"\n"),
                ("src/lib.rs", TREE),
                ("src/a.rs", AHEAD),
                ("src/b.rs", FAR),
            ],
        );
        let read = wrappers_in(&ctx, &serde_json::from_str(GRAPH).unwrap()).unwrap();
        assert_eq!(read.series, Series([("src/lib.rs".to_string(), 4)].into()));
        let places: Vec<_> = read.details["src/lib.rs"]
            .places
            .iter()
            .map(|place| (place.line, place.last, place.item.as_deref()))
            .collect();
        assert_eq!(places, [(1, Some(4), Some("wrap"))]);
        assert_eq!(
            Finding::rendered(&read.findings),
            [
                "src/lib.rs:11: `named` is one call to `inner` and only `other` calls it, but the \
                 tree names it beyond that call",
                "src/lib.rs:30: `open` is one call to `inner` and only `opener` calls it, but it is \
                 `pub`, so code outside this tree may call it",
                "src/lib.rs:42: `near` is one call to `inner` and only `far` calls it, but its caller \
                 is in another file, which may not see what it uses",
            ]
        );
    }

    #[test]
    fn a_call_on_the_last_line_of_a_caller_is_a_call_of_that_caller() {
        let parsed = Parsed::of("fn f() {\n    g() }\n").unwrap();
        let spans = |end: u32| -> Node {
            let node = format!(
                r#"{{"id": 1, "file": "src/a.rs", "name": "f", "kind": "function", "line": 1, "end_line": {end}}}"#
            );
            serde_json::from_str(&node).unwrap()
        };
        assert!(parsed.calls("g", &spans(2)));
        assert!(!parsed.calls("g", &spans(1)));
    }

    #[test]
    fn a_one_line_wrapper_spans_its_name_and_a_call_is_read_through_a_path_or_a_method() {
        let src = "fn f(x: u32) -> u32 { g(&x) }\nfn k(s: &S) -> usize { s.items.len() }\n";
        let parsed = Parsed::of(src).unwrap();
        let node: Node = serde_json::from_str(
            r#"{"id": 1, "file": "src/a.rs", "name": "f", "kind": "function", "line": 1, "end_line": 1}"#,
        )
        .unwrap();
        let callee = parsed.wrapper(&node).map(|shape| shape.callee.as_str());
        assert_eq!(callee, Some("g"));
        assert_eq!(parsed.calls, [("g".to_string(), 1), ("len".to_string(), 2)]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_graph_is_read_from_the_file_outpost_writes_and_the_file_is_removed() {
        use crate::exec::Output;
        let scratch = crate::testdir::make("wrappers-graph");
        let root: &Path = &scratch;
        let file = root.join(EXPORTED);
        let graph = exported(root, |_, argv| {
            std::fs::write(argv[5], GRAPH).unwrap();
            Ok(Output::of(Some(0), "", ""))
        });
        assert_eq!(graph.map(|graph| graph.nodes.len()), Ok(17));
        assert!(!file.exists());
        let errors = [
            exported(root, |_, _| Err("outpost is gone".to_string())),
            exported(root, |_, _| Ok(Output::of(Some(1), "", "no index"))),
            exported(root, |_, _| Ok(Output::of(Some(0), "", ""))),
            exported(root, |_, argv| {
                std::fs::write(argv[5], "{").unwrap();
                Ok(Output::of(Some(0), "", ""))
            }),
        ]
        .map(|read| read.err().unwrap_or_default());
        let said = [
            "outpost is gone",
            "outpost could not export the call graph of this tree",
            "outpost wrote no call graph",
            "outpost wrote a call graph chock cannot read",
        ];
        for (error, start) in errors.iter().zip(said) {
            assert!(error.starts_with(start), "{error}");
        }
        assert!(!file.exists());
    }
}
