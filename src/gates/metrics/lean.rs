//! The lines a file could lose: functions that only pass their parameters on, and code repeated in
//! one shape, tests too. Shapes the source cannot settle are candidates; files a tool wrote are out.

mod repeats;
mod walk;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

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
    about: "a file gains lines that a call past a forwarder, or one function, closure or table \
            for code repeated in one shape, in production code or in tests, would remove",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::AnnotatedRatchet {
        measure,
        keys: Keys::Items,
        unit: "removable line(s)",
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

impl Forwarder {
    /// The forwarder as a place in `shown`, and the fix that removes it.
    fn shows(&self, shown: &str) -> Detail {
        let (name, callee) = (&self.name, &self.callee);
        Detail {
            line: None,
            places: vec![
                Place::at("forwards", shown, self.line)
                    .through(self.last)
                    .item(name),
            ],
            fix: Some(format!(
                "call `{callee}` where `{name}` is called, then remove `{name}`"
            )),
        }
    }
}

/// One file's forwarders, which count, and its candidates, which do not.
#[derive(Debug, Default)]
pub struct Read {
    pub forwarders: Vec<Forwarder>,
    pub candidates: Vec<Finding>,
}

/// Every file's removable lines: each production forwarder's lines, and its part of what merging
/// each repeated group would remove. Each place shows a forwarder or a copy; candidates are notes.
fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let words = splits::words_by_file(ctx)?;
    let mut read = Measurement::of(Series::new(), Vec::new());
    let mut corpus = repeats::Corpus::default();
    let mut files = BTreeMap::new();
    let mut tests: BTreeSet<PathBuf> = prodlines::sources(ctx)?.into_iter().collect();
    let (counted, unread) = prodlines::parsed(ctx)?;
    read.unmeasured = unread;
    for (path, _) in counted {
        tests.remove(&path);
        let Some((shown, src)) = source(ctx, &path)? else {
            continue;
        };
        let found = scan(&src, &shown, &splits::used_below(&words, &path));
        read.findings.extend(found.candidates);
        corpus.add(&shown, &src, false);
        for it in &found.forwarders {
            charge(
                &mut files,
                &shown,
                u64::from(it.last + 1 - it.line),
                it.shows(&shown),
            );
        }
    }
    for path in tests {
        if let Some((shown, src)) = source(ctx, &path)? {
            corpus.add(&shown, &src, true);
        }
    }
    for repeat in corpus.repeats() {
        for (shown, lines) in repeat.by_file() {
            let shows = Detail {
                line: None,
                places: repeat.places(),
                fix: Some(repeat.fix()),
            };
            charge(&mut files, shown, lines, shows);
        }
    }
    for (shown, (lines, detail)) in files {
        read.series.set(&shown, lines);
        read.details.insert(shown, detail);
    }
    Ok(read)
}

/// A file's path as shown and its text, or `None` for a file a tool wrote.
fn source(ctx: &Ctx, path: &Path) -> Result<Option<(String, String)>, String> {
    let shown = project::relative(&ctx.root, path);
    let src = std::fs::read_to_string(path).map_err(|e| format!("{shown}: {e}"))?;
    Ok((!generated(&shown, &src)).then_some((shown, src)))
}

/// Adds `lines` to a file's removable lines, and the places and fix that show them. The file's
/// line is its first place in that file, and a fix not given yet joins the fixes.
fn charge(files: &mut BTreeMap<String, (u64, Detail)>, shown: &str, lines: u64, shows: Detail) {
    let (total, detail) = files.entry(shown.to_string()).or_default();
    *total += lines;
    let here = shows.places.iter().find(|place| place.file == shown);
    detail.line = detail.line.or(here.map(|place| place.line));
    detail.places.extend(shows.places);
    detail.fix = match (detail.fix.take(), shows.fix) {
        (Some(given), Some(fix)) if !given.contains(&fix) => Some(format!("{given}; {fix}")),
        (given, fix) => given.or(fix),
    };
}

/// Whether a tool wrote the file: a part of its path is `generated`, or one of its first lines
/// says so.
fn generated(shown: &str, src: &str) -> bool {
    const SAID: [&str; 4] = [
        "@generated",
        "do not edit",
        "automatically generated",
        "auto-generated",
    ];
    let named = shown
        .split(['/', '\\', '.', '_', '-'])
        .any(|part| part == "generated");
    named
        || (src.lines().take(5)).any(|line| SAID.iter().any(|it| line.to_lowercase().contains(it)))
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
            Some(
                "call `b` where `a` is called, then remove `a`; call `b` where `c` is called, then \
                 remove `c`"
            )
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
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_the_parser_rejects_is_named_while_the_gate_still_shows_every_other_file() {
        let src = "fn a(x: u8) { b(x) }\npub fn b(_x: u8) {}\npub fn entry() { a(1); }\n";
        let files = [
            ("Cargo.toml", ""),
            ("src/lib.rs", src),
            ("src/broken.rs", "fn broken( {\n"),
        ];
        let root = Held::tree("lean-partial", &files);
        let read = measure(&root).unwrap();
        assert_eq!(read.series.get("src/lib.rs"), Some(1));
        let named = matches!(read.unmeasured.as_slice(),
            [one] if one.starts_with("src/broken.rs: line 1:"));
        assert!(named, "{:?}", read.unmeasured);
        let report = crate::run::run_one(&GATE, &root);
        assert_eq!(report.exit_code, 2);
        let reason = report.cannot_run_reason.unwrap_or_default();
        assert!(reason.contains(": src/broken.rs: line 1:"), "{reason}");
        let shown = report.findings.iter().find(|it| it.measured == Some(1));
        assert_eq!(shown.map(|it| it.file.as_str()), Some("src/lib.rs"));
    }

    /// A function holding a six-line run that differs from its copies only in `value`.
    fn copy(name: &str, value: &str) -> String {
        format!(
            "pub fn {name}() {{\n    let total = items\n        .iter()\n        .map(|item| item.size * 2 + \
             offset)\n        .sum::<u64>();\n    let mean = total / count;\n    log({value}, total, \
             mean);\n    {name}!();\n}}\n"
        )
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn tests_are_charged_for_their_own_copies_and_never_merge_with_production_code() {
        let tests = copy("a", "1") + &copy("b", "2") + &copy("c", "3");
        let root = Held::tree(
            "lean-tests",
            &[
                ("src/lib.rs", copy("d", "4").as_str()),
                (
                    "src/tests.rs",
                    &(copy("e", "5") + &copy("f", "6") + &copy("g", "7")),
                ),
                ("tests/cli.rs", tests.as_str()),
            ],
        );
        let read = measure(&root).unwrap();
        let charged = ["src/lib.rs", "src/tests.rs", "tests/cli.rs"].map(|it| read.series.get(it));
        assert_eq!(
            charged,
            [None, Some(6), Some(6)],
            "unit tests and `tests` stay apart"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn each_file_is_charged_its_copies_share_of_the_lines_a_merge_removes() {
        let lib = copy("a", "1") + &copy("b", "2");
        let root = Held::tree(
            "lean-repeats",
            &[
                ("src/lib.rs", lib.as_str()),
                ("src/b.rs", &copy("c", "3")),
                ("src/generated/d.rs", &copy("d", "4")),
            ],
        );
        let read = measure(&root).unwrap();
        let (lib, b) = (read.series.get("src/lib.rs"), read.series.get("src/b.rs"));
        assert_eq!((lib, b), (Some(4), Some(2)));
        let told = &read.details["src/lib.rs"];
        assert_eq!(told.line, Some(2));
        assert_eq!(
            told.places,
            [
                Place::at("copy 1 of 3", "src/b.rs", 2).through(7),
                Place::at("copy 2 of 3", "src/lib.rs", 2).through(7),
                Place::at("copy 3 of 3", "src/lib.rs", 11).through(16),
            ]
        );
    }

    #[test]
    fn a_file_charged_again_keeps_its_first_line_and_names_each_fix_once() {
        let mut files = BTreeMap::new();
        for (line, fix) in [(9, "merge x"), (4, "merge x"), (2, "merge y")] {
            let shows = Detail {
                line: None,
                places: vec![
                    Place::at("copy", "src/b.rs", 1),
                    Place::at("copy", "src/a.rs", line),
                ],
                fix: Some(fix.to_string()),
            };
            charge(&mut files, "src/a.rs", 3, shows);
        }
        let (total, detail) = &files["src/a.rs"];
        assert_eq!((*total, detail.line), (9, Some(9)));
        assert_eq!(detail.fix.as_deref(), Some("merge x; merge y"));
        let lines: Vec<u32> = detail.places.iter().map(|place| place.line).collect();
        assert_eq!(lines, [1, 9, 1, 4, 1, 2]);
    }

    #[test]
    fn a_file_a_tool_wrote_is_known_by_its_path_or_its_first_lines() {
        for (shown, src, wrote) in [
            ("src/generated/pb.rs", "", true),
            ("src/pb_generated.rs", "", true),
            ("src/api.generated.rs", "", true),
            ("src/degenerated.rs", "", false),
            ("src/pb.rs", "// @generated by prost-build\n", true),
            ("src/pb.rs", "\n\n\n// Code generated. DO NOT EDIT.\n", true),
            ("src/pb.rs", "\n\n\n\n\n// @generated\n", false),
            (
                "src/pb.rs",
                "/// Parses what a generated file holds.\n",
                false,
            ),
        ] {
            assert_eq!(generated(shown, src), wrote, "{shown} {src:?}");
        }
    }

    #[test]
    fn the_gate_is_a_ratchet_over_removable_lines_by_item() {
        let annotated = matches!(GATE.kind, Kind::AnnotatedRatchet { .. });
        let by_item = crate::gates::holds_each_file(GATE.name);
        assert!(
            annotated && by_item,
            "lean keeps a number per file, with notes beside it"
        );
        assert_eq!(GATE.counts_in(), Some("removable line(s)"));
        // Lean reads the source, so it runs without a compiler.
        let (name, group, builds) = (GATE.name, GATE.group, GATE.builds);
        assert_eq!((name, group, builds), ("lean", Group::Quality, false));
    }
}
