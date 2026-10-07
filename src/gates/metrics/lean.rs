//! Private functions that only pass their parameters on to one call, which a caller can make itself.
//! Shapes the source alone cannot settle are candidates, and never trip the gate.

mod walk;

use std::collections::{BTreeSet, HashMap};

use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ExprCall, ExprMethodCall, Ident, ItemEnum, ItemImpl, ItemStruct, Macro, Meta,
    Signature,
};

use crate::gates::metrics::{prodlines, splits};
use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::report::{Detail, Finding, Place};
use crate::run::{Ctx, Gate, Group, Kind, Measurement};

pub const GATE: Gate = Gate {
    name: "lean",
    about: "a file gains a private function that only passes its parameters on to another call",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::AnnotatedRatchet {
        measure,
        keys: Keys::Items,
        unit: "forwarders",
    },
};

/// A function that only passes its parameters on, and the call it makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forwarder {
    pub name: String,
    pub callee: String,
    pub line: u32,
    pub last: u32,
}

/// One file's forwarders, which count, and its candidates, which do not.
#[derive(Debug, Default)]
pub struct Read {
    pub forwarders: Vec<Forwarder>,
    pub candidates: Vec<Finding>,
}

/// Every production file's forwarders as a count by file, each a place of the file's finding; its
/// candidates ride along as notes.
fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let words = splits::words_by_file(ctx)?;
    let mut read = Measurement::of(Series::new(), Vec::new());
    for (path, _) in prodlines::measure(ctx)? {
        let shown = project::relative(&ctx.root, &path);
        let src = std::fs::read_to_string(&path).map_err(|e| format!("{shown}: {e}"))?;
        let found = scan(&src, &shown, &splits::used_below(&words, &path));
        read.findings.extend(found.candidates);
        let Some(Forwarder {
            name, callee, line, ..
        }) = found.forwarders.first()
        else {
            continue;
        };
        let places = found
            .forwarders
            .iter()
            .map(|it| {
                Place::at("forwards", &shown, it.line)
                    .through(it.last)
                    .item(&it.name)
            })
            .collect();
        let fix = format!("call `{callee}` where `{name}` is called, then remove `{name}`");
        let count = u64::try_from(found.forwarders.len()).unwrap_or(u64::MAX);
        read.series.set(&shown, count);
        let detail = Detail {
            line: Some(*line),
            places,
            fix: Some(fix),
        };
        read.details.insert(shown, detail);
    }
    Ok(read)
}

/// The forwarders and candidates among one file's production items. `below` says whether a file
/// below the module names a word; a file that does not parse, or only builds for tests, has neither.
pub fn scan(src: &str, shown: &str, below: &dyn Fn(&str) -> bool) -> Read {
    let Ok(file) = prodlines::parse_rust(src) else {
        return Read::default();
    };
    if prodlines::is_test_gated(&file.attrs) {
        return Read::default();
    }
    let mut uses = Uses::default();
    uses.visit_file(&file);
    let mut lean = walk::Lean {
        shown,
        uses: &uses,
        below,
        read: Read::default(),
    };
    lean.visit_file(&file);
    lean.read
}

/// How the file names each word: at all, as the callee of a call, and as a function it defines;
/// the types that implement each trait, and the structs and enums it defines.
#[derive(Debug, Default)]
struct Uses {
    words: HashMap<String, usize>,
    calls: HashMap<String, usize>,
    defs: HashMap<String, usize>,
    impls: HashMap<String, Vec<String>>,
    types: BTreeSet<String>,
}

fn bump(map: &mut HashMap<String, usize>, word: String) {
    *map.entry(word).or_default() += 1;
}

impl<'ast> Visit<'ast> for Uses {
    fn visit_ident(&mut self, node: &'ast Ident) {
        bump(&mut self.words, node.to_string());
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Expr::Path(path) = &*node.func
            && let Some(last) = path.path.segments.last()
        {
            bump(&mut self.calls, last.ident.to_string());
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        bump(&mut self.calls, node.method.to_string());
        visit::visit_expr_method_call(self, node);
    }

    fn visit_signature(&mut self, node: &'ast Signature) {
        bump(&mut self.defs, node.ident.to_string());
        visit::visit_signature(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        if let Some((path, _)) = &node.trait_
            && let Some(last) = path.segments.last()
        {
            let ty = node.self_ty.span().source_text().unwrap_or_default();
            self.impls
                .entry(last.ident.to_string())
                .or_default()
                .push(ty);
        }
        visit::visit_item_impl(self, node);
    }

    fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
        self.types.insert(node.ident.to_string());
        visit::visit_item_struct(self, node);
    }

    fn visit_item_enum(&mut self, node: &'ast ItemEnum) {
        self.types.insert(node.ident.to_string());
        visit::visit_item_enum(self, node);
    }

    fn visit_macro(&mut self, node: &'ast Macro) {
        let words = &mut self.words;
        splits::token_words(node.tokens.clone(), &mut |word| bump(words, word));
        visit::visit_macro(self, node);
    }

    fn visit_attribute(&mut self, node: &'ast Attribute) {
        if let Meta::List(list) = &node.meta {
            let words = &mut self.words;
            splits::token_words(list.tokens.clone(), &mut |word| bump(words, word));
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::run::report::Grade;
    use crate::testdir::Held;

    fn read(src: &str) -> Read {
        scan(src, "src/a.rs", &|_| false)
    }

    fn forwarders(src: &str) -> Vec<String> {
        read(src)
            .forwarders
            .iter()
            .map(|it| format!("{} {} {}-{}", it.name, it.callee, it.line, it.last))
            .collect()
    }

    fn candidates(src: &str) -> Vec<String> {
        read(src)
            .candidates
            .iter()
            .map(|it| format!("{}: {}", it.line.unwrap(), it.message))
            .collect()
    }

    /// A caller for each name, so a forwarder's only use is a call.
    fn called_once(names: &[&str]) -> String {
        let calls: String = names.iter().map(|name| format!("{name}(1); ")).collect();
        format!("pub fn entry() {{ {calls} }}\n")
    }

    #[test]
    fn a_function_that_passes_its_parameters_on_unchanged_is_a_forwarder() {
        let src = format!(
            "fn a(x: u8, y: u8) -> u8 {{\n    b(x, y)\n}}\nfn c(mut x: u8) {{ m::d(x) }}\n{}",
            "pub fn entry() { a(1, 2); c(3); }\n"
        );
        assert_eq!(forwarders(&src), ["a b 1-3", "c m::d 4-4"]);
    }

    #[test]
    fn a_method_that_passes_self_and_its_parameters_on_is_a_forwarder() {
        let src = "struct T;\nimpl T {\n    fn a(&self, x: u8) { self.b(x) }\n    \
                   fn c(&self) { Self::d(self) }\n    pub fn e(&self) { self.a(1); self.c(); }\n}\n";
        assert_eq!(forwarders(src), ["a self.b 3-3", "c Self::d 4-4"]);
    }

    #[test]
    fn a_call_that_changes_reorders_adds_or_drops_a_parameter_forwards_nothing() {
        let bodies = [
            "fn a(x: u8, y: u8) { b(x + 1, y) }",
            "fn a(x: u8, y: u8) { b(y, x) }",
            "fn a(x: u8, y: u8) { b(x, y, 1) }",
            "fn a(x: u8, y: u8) { b(x) }",
            "fn a(x: u8, y: u8) { b::<u8>(x, y) }",
            "fn a(x: u8, y: u8) { <T>::b(x, y) }",
            "fn a(x: u8, y: u8) { a(x, y) }",
            "fn a(x: u8, y: u8) { (b)(x, y) }",
            "fn a(x: u8, y: u8) { b(x, y); }",
            "fn a(x: u8, y: u8) { let z = 1; b(x, y) }",
            "fn a() { b() }",
            "fn a((x, y): (u8, u8)) { b(x, y) }",
            "fn a(ref x: u8) { b(x) }",
            "fn a(x @ 1: u8) { b(x) }",
            "async fn a(x: u8) { b(x) }",
        ];
        for body in bodies {
            let src = format!("{body}\n{}", called_once(&["a"]));
            assert_eq!(forwarders(&src), Vec::<String>::new(), "{body}");
        }
    }

    #[test]
    fn a_method_on_anything_but_self_or_calling_itself_forwards_nothing() {
        let bodies = [
            "fn a(&self, x: u8) { other.b(x) }",
            "fn a(&self, x: u8) { self.a(x) }",
            "fn a(&self, x: u8) { Self::a(self, x) }",
            "fn a(&self, x: u8) { self.b::<u8>(x) }",
            "fn a(&self, x: u8) { x.b(self) }",
        ];
        for body in bodies {
            let src = format!("struct T;\nimpl T {{ {body} pub fn e(&self) {{ self.a(1); }} }}\n");
            assert_eq!(forwarders(&src), Vec::<String>::new(), "{body}");
        }
    }

    #[test]
    fn a_function_another_module_names_forwards_from_a_different_self() {
        let src = "struct T;\nimpl T { fn a(&self, x: u8) { m::T::a(self, x) } pub fn e(&self) { \
                   self.a(1); } }\n";
        assert_eq!(forwarders(src), ["a m::T::a 2-2"]);
    }

    #[test]
    fn trait_methods_and_test_code_are_never_forwarders() {
        let src = "trait R { fn a(&self, x: u8) { self.b(x) } fn b(&self, x: u8); }\n\
                   impl R for u8 { fn b(&self, x: u8) { self.c(x) } }\n\
                   impl R for u16 { fn b(&self, x: u8) { self.c(x) } }\n\
                   #[cfg(test)]\nfn t(x: u8) { u(x) }\n\
                   #[cfg(test)]\nmod tests { fn v(x: u8) { w(x) } }\n\
                   #[cfg(test)]\nimpl Q { fn s(&self, x: u8) { self.c(x) } }\n\
                   struct Q;\nimpl Q { #[cfg(test)]\n fn r(&self, x: u8) { self.c(x) } }\n";
        assert_eq!(forwarders(src), Vec::<String>::new());
        assert_eq!(candidates(src), Vec::<String>::new());
    }

    #[test]
    fn a_match_in_an_associated_constant_is_read_too() {
        let src = "struct Q;\nimpl Q {\n    const C: u8 = match 1 {\n        1 => 2,\n        2 => 2,\n        \
                   _ => 0,\n    };\n}\n";
        assert_eq!(
            candidates(src),
            ["5: this arm's body is the same as the arm's at line 4"]
        );
    }

    #[test]
    fn a_forwarder_the_source_cannot_settle_is_a_candidate_that_says_why() {
        let src = "pub fn a(x: u8) { b(x) }\n#[inline]\nfn c(x: u8) { b(x) }\n\
                   fn d<T>(x: T) { b(x) }\nfn e(x: u8) { b(x) }\nconst F: fn(u8) = e;\n\
                   fn g(x: u8) { b(x) }\nfn h(x: u8) { b(x) }\n\
                   #[serde(default = \"h\")]\nstruct S;\n\
                   /// Said once.\nfn i(x: u8) { b(x) }\n\
                   pub fn entry() { c(1); d(1); println!(\"{}\", g(1)); i(1); }\n";
        assert_eq!(
            candidates(src),
            [
                "1: `a` only passes its parameters to `b`, but it is visible outside its module",
                "3: `c` only passes its parameters to `b`, but it carries an attribute",
                "4: `d` only passes its parameters to `b`, but it is generic",
                "5: `e` only passes its parameters to `b`, but its name is used other than in a call",
                "7: `g` only passes its parameters to `b`, but its name is used other than in a call",
                "8: `h` only passes its parameters to `b`, but its name is used other than in a call",
            ]
        );
        assert_eq!(forwarders(src), ["i b 12-12"]);
        let told = read(src).candidates.remove(0);
        assert_eq!(told.grade, Grade::Candidate);
        assert_eq!(
            told.fix.as_deref(),
            Some("call `b` where `a` is called if the types agree")
        );
    }

    #[test]
    fn a_forwarder_a_file_below_its_module_names_is_a_candidate() {
        let src = format!("fn a(x: u8) {{ b(x) }}\n{}", called_once(&["a"]));
        let read = scan(&src, "src/a.rs", &|name| name == "a");
        assert!(read.forwarders.is_empty());
        assert!(
            read.candidates[0]
                .message
                .ends_with("used other than in a call")
        );
    }

    #[test]
    fn a_short_function_of_one_statement_and_one_caller_is_a_candidate() {
        // `edge` spans five lines, the most a one-caller function may; `long` spans six.
        let src = format!(
            "fn a(x: u8) -> u8 {{\n    x + 1\n}}\n\
             fn edge(x: u8) -> u8 {{\n    x\n        + 1\n        + 2\n}}\n\
             fn long(x: u8) -> u8 {{\n    x\n        + 1\n        + 2\n        + 3\n}}\n\
             fn two(x: u8) -> u8 {{ let y = x; y }}\nfn twice(x: u8) -> u8 {{ x }}\n\
             pub fn p(x: u8) -> u8 {{ x }}\n#[inline]\nfn q(x: u8) -> u8 {{ x }}\n\
             fn named(x: u8) -> u8 {{ x }}\nconst N: fn(u8) -> u8 = named;\n{}\
             pub fn more() {{ twice(1); }}\n",
            called_once(&["a", "edge", "long", "two", "twice", "p", "q", "named"])
        );
        assert_eq!(
            candidates(&src),
            [
                "1: `a` has one statement and one caller",
                "4: `edge` has one statement and one caller"
            ]
        );
        let told = read(&src).candidates.remove(0);
        assert_eq!(
            told.fix.as_deref(),
            Some("move the statement of `a` into its caller, then remove it")
        );
    }

    #[test]
    fn two_functions_of_one_name_are_no_one_caller_candidate() {
        let src = "struct A;\nimpl A { fn new() -> u8 { 1 } }\nstruct B;\nimpl B { fn new() -> u8 \
                   { 2 } pub fn e() { A::new(); } }\n";
        assert_eq!(candidates(src), Vec::<String>::new());
    }

    #[test]
    fn an_arm_that_repeats_the_body_just_above_is_a_candidate() {
        let src = "pub fn f(x: u8, y: Option<u8>) -> u8 {\n    let a = match x {\n        \
                   1 => g( 2 ),\n        2 => g(2),\n        4 => g(3),\n        5 => g(2),\n        \
                   3 if x > 0 => g(2),\n        6 => g(2),\n        _ => g(2),\n    };\n    \
                   let b = match y {\n        Some(c) => 0,\n        None => 0,\n    };\n    a + b\n}\n";
        let read = read(src);
        let said: Vec<String> = read
            .candidates
            .iter()
            .map(|it| format!("{}: {}", it.line.unwrap(), it.message))
            .collect();
        assert_eq!(
            said,
            ["4: this arm's body is the same as the arm's at line 3"]
        );
        let told = &read.candidates[0];
        assert_eq!(told.places, [Place::at("same body", "src/a.rs", 3)]);
        assert_eq!(
            told.fix.as_deref(),
            Some("join the two patterns with `|` into one arm")
        );
    }

    #[test]
    fn a_private_trait_with_one_impl_in_the_file_is_a_candidate() {
        let src = "struct V;\nenum E { A }\ntrait R { fn r(&self); }\nimpl R for V { fn r(&self) {} }\n\
                   trait Two {}\nimpl Two for V {}\nimpl Two for E {}\ntrait Foreign {}\n\
                   impl Foreign for Vec<u8> {}\npub trait Open {}\nimpl Open for V {}\n\
                   trait Below {}\nimpl Below for E {}\ntrait Unused {}\n\
                   #[cfg(test)]\ntrait T {}\nimpl T for V {}\n";
        let read = scan(src, "src/a.rs", &|name| name == "Below");
        let said: Vec<String> = read
            .candidates
            .iter()
            .map(|it| format!("{}: {}", it.line.unwrap(), it.message))
            .collect();
        assert_eq!(said, ["3: only `V` implements `R`"]);
        assert_eq!(
            read.candidates[0].fix.as_deref(),
            Some("move the methods of `R` into an inherent `impl V`")
        );
    }

    #[test]
    fn a_source_the_parser_rejects_or_only_tests_build_has_nothing_to_report() {
        let gated = format!(
            "#![cfg(test)]\nfn a(x: u8) {{ b(x) }}\n{}",
            called_once(&["a"])
        );
        for src in ["fn (", gated.as_str()] {
            let read = read(src);
            assert!(
                read.forwarders.is_empty() && read.candidates.is_empty(),
                "{src}"
            );
        }
        assert_eq!(
            forwarders(&gated.replace("#![cfg(test)]\n", "")),
            ["a b 1-1"]
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_gate_counts_forwarders_by_file_and_carries_candidates_as_notes() {
        let src = "fn a(x: u8) { b(x) }\nfn c(x: u8) { b(x) }\npub fn b(_x: u8) {}\n\
                   pub fn entry() { a(1); c(2); }\nfn d(x: u8) { b(x) }\n";
        let root = Held::tree(
            "lean-gate",
            &[
                ("src/lib.rs", src),
                ("src/lib/below.rs", "fn e() { d(1) }\n"),
            ],
        );
        let mut read = measure(&root).unwrap();
        assert_eq!(read.series.get("src/lib.rs"), Some(2));
        let told = read.details.remove("src/lib.rs").unwrap();
        assert_eq!(told.line, Some(1));
        assert_eq!(
            told.places,
            [
                Place::at("forwards", "src/lib.rs", 1).through(1).item("a"),
                Place::at("forwards", "src/lib.rs", 2).through(2).item("c"),
            ]
        );
        assert_eq!(
            told.fix.as_deref(),
            Some("call `b` where `a` is called, then remove `a`")
        );
        let notes: Vec<&str> = read.findings.iter().map(|it| it.message.as_str()).collect();
        assert_eq!(
            notes,
            ["`d` only passes its parameters to `b`, but its name is used other than in a call"]
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_with_no_forwarder_stays_out_of_the_series() {
        let root = Held::tree("lean-none", &[("src/lib.rs", "pub fn a() {}\n")]);
        let read = measure(&root).unwrap();
        assert_eq!(read.series, Series::new());
        assert!(read.details.is_empty());
    }

    #[test]
    fn the_gate_is_a_ratchet_over_items_counted_in_forwarders() {
        assert!(matches!(
            GATE.kind,
            Kind::AnnotatedRatchet {
                keys: Keys::Items,
                unit: "forwarders",
                ..
            }
        ));
        assert_eq!(GATE.name, "lean");
        assert_eq!(GATE.group, Group::Quality);
    }
}
