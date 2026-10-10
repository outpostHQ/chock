//! Finds the tests in a Rust source file, without compiling it.

use std::ops::{Range, RangeInclusive};

use crate::gates::metrics::prodlines;
use crate::tokens::{Item, Lexed, Tok};

/// One `#[test]`-shaped function, with what a rule needs to judge it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestItem {
    /// Line of the first attribute, which is what a reader looks for.
    pub line: usize,
    pub name: String,
    /// Token range of the attribute block.
    pub attrs: Range<usize>,
    /// Token range of the function body, braces included.
    pub body: Range<usize>,
    /// The `test-lint: allow(<rule>)` comments from the first attribute to the closing brace.
    pub waivers: Vec<Waiver>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiver {
    pub rule: String,
    pub reason: String,
    pub line: usize,
}

/// A function of the test code, a test or not: a test may assert through it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Helper {
    pub name: String,
    /// Token range of the function body, braces included.
    pub body: Range<usize>,
}

/// One file's text, tokens, tests and the functions its tests may assert through.
#[derive(Clone, Debug, Default)]
pub struct FileScan {
    pub file: String,
    pub raw: String,
    pub lexed: Lexed,
    pub tests: Vec<TestItem>,
    pub helpers: Vec<Helper>,
}

/// A file that does not lex compiles nowhere, so it holds no test.
pub fn scan_source(file: &str, raw: &str) -> FileScan {
    let lexed = Lexed::new(raw).unwrap_or_default();
    let mut tests = Vec::new();
    let items = lexed.items();
    for item in &items {
        let body = lexed.pair[item.end]..item.end + 1;
        let fn_kw = (item.head..body.start).find(|&i| lexed.ident(i) == "fn");
        let name = fn_kw.map_or("", |kw| lexed.ident(kw + 1));
        let is_test = lexed.opens(body.start, '{') && marks_a_test(&lexed, item.first..item.head);
        if name.is_empty() || !is_test {
            continue;
        }
        tests.push(TestItem {
            line: lexed.line[item.first],
            name: name.to_string(),
            attrs: item.first..item.head,
            body,
            waivers: waivers_in(raw, lexed.line[item.first]..=lexed.line[item.end]),
        });
    }
    FileScan {
        file: file.to_string(),
        raw: raw.to_string(),
        helpers: helpers_in(file, &lexed, &items),
        lexed,
        tests,
    }
}

/// Whether the whole file is test code, judged by its path: a `tests` or `benches` directory, or a
/// whole-file test module.
fn is_test_code(file: &str) -> bool {
    let (dirs, name) = file.rsplit_once('/').unwrap_or(("", file));
    let holds_tests = |dir: &str| matches!(dir, "tests" | "benches");
    prodlines::is_test_file(name) || dirs.split('/').any(holds_tests)
}

/// Whether an attribute block compiles its item for tests only: a `cfg` that names `test` and
/// does not negate it.
fn gated_for_tests(lexed: &Lexed, attrs: Range<usize>) -> bool {
    let word = |text: &str| Tok::Ident(text.to_string());
    let negated = [word("not"), Tok::Open('('), word("test")];
    lexed.attrs(attrs).any(|open| {
        let inside = lexed.inside(open);
        let names_test = attr_name(inside) == "cfg" && inside.contains(&word("test"));
        names_test && !inside.windows(3).any(|three| three == negated)
    })
}

/// The functions of the test code: each one of a test file, and in any other file each one under
/// a `#[cfg(test)]`.
fn helpers_in(file: &str, lexed: &Lexed, items: &[Item]) -> Vec<Helper> {
    let whole = is_test_code(file);
    let gated = |item: &&Item| gated_for_tests(lexed, item.first..item.head);
    let span = |item: &Item| item.first..=item.end;
    let spans: Vec<_> = items.iter().filter(gated).map(span).collect();
    let in_tests = |f: &(usize, Range<usize>)| whole || spans.iter().any(|s| s.contains(&f.0));
    let functions = lexed.functions().into_iter().filter(in_tests);
    let named = functions.map(|(name, body)| (lexed.ident(name).to_string(), body));
    named.map(|(name, body)| Helper { name, body }).collect()
}

/// The last word of the path an attribute starts with: `test` for `#[tokio::test(flavor = "x")]`.
fn attr_name(inside: &[Tok]) -> &str {
    let words = inside.iter().map_while(|t| match t {
        Tok::Ident(s) => Some(s.as_str()),
        Tok::Punct(':', _) => Some(""),
        _ => None,
    });
    words.filter(|w| !w.is_empty()).last().unwrap_or_default()
}

/// Whether an attribute block marks a test: `#[test]`, a runtime's own `#[x::test]`, or a
/// parameterised one.
fn marks_a_test(lexed: &Lexed, attrs: Range<usize>) -> bool {
    let mut names = lexed.attrs(attrs).map(|open| attr_name(lexed.inside(open)));
    names.any(|name| matches!(name, "test" | "rstest" | "test_case"))
}

/// The waivers written on the given lines.
fn waivers_in(raw: &str, lines: RangeInclusive<usize>) -> Vec<Waiver> {
    let numbered = raw.lines().enumerate().map(|(n, text)| (n + 1, text));
    let waiver = |(line, text): (usize, &str)| {
        let (rule, rest) = text.split_once("test-lint: allow(")?.1.split_once(')')?;
        let reason = rest.trim_start_matches([' ', '\t', '—', '–', '-', ':']);
        Some(Waiver {
            rule: rule.trim().to_string(),
            reason: reason.trim().to_string(),
            line,
        })
    };
    let on_the_lines = numbered.filter(|(line, _)| lines.contains(line));
    on_the_lines.filter_map(waiver).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(src: &str) -> Vec<String> {
        let tests = scan_source("src/thing.rs", src).tests;
        tests.into_iter().map(|test| test.name).collect()
    }

    /// Whether the body of the first test holds the identifier `word` as code.
    fn body_has(scan: &FileScan, word: &str) -> bool {
        let body = scan.tests[0].body.clone();
        scan.lexed.toks[body].contains(&Tok::Ident(word.to_string()))
    }

    #[test]
    fn a_scan_keeps_the_file_its_text_and_each_test_s_place() {
        let src =
            "fn helper() {}\n#[test]\n#[ignore = \"needs a bucket\"]\nfn it_holds() { run(); }\n";
        let scan = scan_source("src/thing.rs", src);
        assert_eq!(
            (scan.file.as_str(), scan.raw.as_str()),
            ("src/thing.rs", src)
        );
        let expected = TestItem {
            line: 2,
            name: "it_holds".to_string(),
            attrs: 6..16,
            body: 20..26,
            waivers: Vec::new(),
        };
        assert_eq!(scan.tests, [expected]);
        let reason = Tok::Literal("\"needs a bucket\"".to_string());
        assert!(scan.lexed.toks[6..16].contains(&reason));
        assert!(body_has(&scan, "run"));
    }

    #[test]
    fn the_helpers_are_the_functions_of_the_test_code() {
        let helpers = |file: &str, src: &str| -> Vec<String> {
            let found = scan_source(file, src).helpers;
            found.into_iter().map(|helper| helper.name).collect()
        };
        let src = "fn prod() {}\n#[cfg(test)]\nmod tests {\n    fn says() {}\n    #[test]\n    fn t() {}\n}\n#[cfg(not(test))]\nmod live { fn real() {} }\n#[cfg(all(test, not(windows)))]\nfn gated() {}\n#[cfg_attr(test, allow(dead_code))]\nfn attributed() {}\n#[doc = \"test\"]\nfn described() {}\ntrait T { fn declared(); }\n";
        for file in ["src/thing.rs", "src/tests_of.rs", "src/testsuite/a.rs"] {
            assert_eq!(helpers(file, src), ["says", "t", "gated"], "{file}");
        }
        let all = [
            "prod",
            "says",
            "t",
            "real",
            "gated",
            "attributed",
            "described",
        ];
        for file in [
            "tests/it.rs",
            "crates/a/tests/common/mod.rs",
            "benches/b.rs",
            "src/tests.rs",
            "src/merge/property_tests.rs",
            "tests.rs",
        ] {
            assert_eq!(helpers(file, src), all, "{file}");
        }
        let says = Helper {
            name: "says".to_string(),
            body: 4..7,
        };
        assert_eq!(
            scan_source("tests/a.rs", "fn says() { x }\n").helpers,
            [says]
        );
    }

    #[test]
    fn a_test_named_in_a_comment_or_a_string_is_not_a_test() {
        assert_eq!(
            names("// #[test]\n// fn not_a_test() {}\nfn real() {}\n"),
            [""; 0]
        );
        let scan = scan_source(
            "a.rs",
            "#[test]\nfn t() { let s = \"assert!(x)\"; drop(s); }\n",
        );
        assert!(!body_has(&scan, "assert"), "{:?}", scan.lexed.toks);
        let raw = scan_source(
            "a.rs",
            "#[test]\nfn t() { let s = r#\"assert!(\"x\")\"#; }\n",
        );
        assert!(!body_has(&raw, "assert"), "{:?}", raw.lexed.toks);
    }

    #[test]
    fn a_brace_in_a_string_a_raw_identifier_and_a_lifetime_hide_no_test() {
        let braces = "mod outer {\n  fn f() { let s = \"}}}}\"; }\n  #[test]\n  fn inner() {}\n}\n";
        assert_eq!(names(braces), ["inner"]);
        assert_eq!(
            names("struct S { r#type: u8 }\n#[test]\nfn t() {}\n"),
            ["t"]
        );
        let lifetime = "fn f<'a>(x: &'a str) -> &'a str { x }\n#[test]\nfn t() {}\n";
        assert_eq!(names(lifetime), ["t"]);
    }

    #[test]
    fn a_return_type_is_not_mistaken_for_the_body() {
        let src = "#[tokio::test]\nasync fn t() -> Result<(), E> { assert!(true); Ok(()) }\n";
        let scan = scan_source("a.rs", src);
        assert_eq!(scan.tests[0].name, "t");
        assert!(scan.lexed.opens(scan.tests[0].body.start, '{'));
        assert!(body_has(&scan, "assert"));
    }

    #[test]
    fn a_function_between_two_tests_takes_no_attribute_from_either() {
        let src = "#[test]\nfn one() {}\nfn helper() {}\n#[test]\nfn two() {}\n";
        assert_eq!(names(src), ["one", "two"]);
    }

    #[test]
    fn every_spelling_of_a_test_attribute_is_a_test_and_nothing_else_is() {
        let marked = |attr: &&str| names(&format!("{attr}\nfn t() {{}}\n")) == ["t"];
        let tests = "#[test]|#[tokio::test(flavor = \"current_thread\")]|#[actix_web::test]|\
            #[rstest]|#[test_case(1 ; \"one\")]|#[cfg(unix)]\n#[test]";
        let others = "#[cfg(test)]|#[testing]|#[bench]|#[test::helper]|#[derive(test)]";
        let unmarked: Vec<&str> = tests.split('|').filter(|attr| !marked(attr)).collect();
        assert_eq!(unmarked, [""; 0]);
        let wrongly: Vec<&str> = others.split('|').filter(marked).collect();
        assert_eq!(wrongly, [""; 0]);
    }

    #[test]
    fn an_attributed_item_that_is_no_function_with_a_body_is_not_a_test() {
        assert_eq!(names("#[test]\nstruct S { a: u8 }\n"), [""; 0]);
        assert_eq!(names("#[test]\nfn declared();\n"), [""; 0]);
        assert_eq!(
            names("trait T {\n    #[test]\n    fn declared();\n}\n"),
            [""; 0]
        );
    }

    #[test]
    fn a_waiver_from_the_first_attribute_to_the_closing_brace_is_collected() {
        let src = "// test-lint: allow(early) — before\n#[test]\n// test-lint: allow(no-assertion) — compile-time check only\nfn t() {\n    f(); // test-lint: allow(bare)\n}\n// test-lint: allow(late) — after\n";
        let waiver = |rule: &str, reason: &str, line| Waiver {
            rule: rule.to_string(),
            reason: reason.to_string(),
            line,
        };
        let expected = [
            waiver("no-assertion", "compile-time check only", 3),
            waiver("bare", "", 5),
        ];
        assert_eq!(scan_source("a.rs", src).tests[0].waivers, expected);
    }

    #[test]
    fn a_waiver_s_reason_starts_after_any_dash_colon_or_space() {
        let reason = |text: &str| waivers_in(text, 1..=1)[0].reason.clone();
        assert_eq!(reason("// test-lint: allow( x ): \t– - why so  "), "why so");
        assert_eq!(
            waivers_in("// test-lint: allow( x ) why", 1..=1)[0].rule,
            "x"
        );
        assert_eq!(waivers_in("// test-lint: allow(x", 1..=1), []);
        assert_eq!(waivers_in("// test-lint: allowed", 1..=1), []);
    }

    #[test]
    fn a_file_that_does_not_lex_holds_no_test() {
        assert_eq!(names("#[test]\nfn t( {\n"), [""; 0]);
    }
}
