//! The rules judged one test at a time: what its body, its attributes and its name say.

use super::scan::{FileScan, TestItem};
use super::{finding, invoked, waived};
use crate::run::report::Finding;
use crate::tokens::Tok;

/// What one rule finds in one test: the words of the finding, or nothing.
type Rule = fn(&FileScan, &TestItem) -> Option<String>;

/// The rules, each under the name a record and a waiver use.
const RULES: [(&str, Rule); 6] = [
    ("tautological-assertion", tautology),
    ("ignore-without-reason", bare_ignore),
    ("should-panic-without-expected", bare_should_panic),
    ("placeholder-test-name", placeholder_name),
    ("one-word-test-name", one_word_name),
    ("escapes-the-run-dir", escape),
];
const WAIVER: &str = "waiver-without-reason";

/// What the tests of one file break; a rule a test waives with a reason is left out.
pub(super) fn in_file(s: &FileScan) -> Vec<Finding> {
    let mut found = Vec::new();
    for t in &s.tests {
        let unreasoned = t.waivers.iter().filter(|w| w.reason.is_empty());
        found.extend(unreasoned.map(|w| {
            let detail = format!("`test-lint: allow({})` states no reason", w.rule);
            finding(&s.file, w.line, WAIVER, &detail)
        }));
        let judged = RULES.iter().filter(|(rule, _)| !waived(t, rule));
        let broken = judged.filter_map(|(rule, check)| Some((rule, check(s, t)?)));
        found.extend(broken.map(|(rule, detail)| finding(&s.file, t.line, rule, &detail)));
    }
    found
}

/// Whether these arguments fix what the assert macro `name` decides: `true`, or one value twice.
fn cannot_fail(name: &str, args: &str) -> bool {
    let mut parts = args.split(',').map(str::trim);
    let (left, right) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    let same = left == right && !left.is_empty() && !left.contains(['(', '!']);
    (name == "assert" && matches!(args, "true" | "!false"))
        || (matches!(name, "assert_eq" | "assert_ne") && same)
}

/// An assertion whose truth the code under test cannot change: `assert!(true)`, `assert_eq!(x, x)`.
fn tautology(s: &FileScan, t: &TestItem) -> Option<String> {
    let grouped =
        |i: &usize| invoked(&s.lexed, *i) && matches!(s.lexed.toks.get(i + 2), Some(Tok::Open(_)));
    let shape = t.body.clone().filter(grouped).find_map(|i| {
        let (name, open) = (s.lexed.ident(i), i + 2);
        let inside = s.lexed.at[open].end..s.lexed.at[s.lexed.pair[open]].start;
        let args = s.raw.get(inside)?.trim();
        cannot_fail(name, args).then(|| format!("`{name}!({args})`"))
    });
    shape.map(|shape| format!("`{}` contains {shape}", t.name))
}

/// The test's first attribute named `word` that holds no string with something in it.
fn bare<'a>(s: &'a FileScan, t: &TestItem, word: &str) -> Option<&'a [Tok]> {
    let lexed = &s.lexed;
    let named = lexed
        .attrs(t.attrs.clone())
        .filter(|&open| lexed.ident(open + 1) == word);
    let stated = |inside: &[Tok]| {
        let mut strings = inside.iter().filter_map(|tok| match tok {
            Tok::Literal(text) => text.split('"').nth(1),
            _ => None,
        });
        strings.next().is_some_and(|text| !text.trim().is_empty())
    };
    named
        .map(|open| lexed.inside(open))
        .find(|inside| !stated(inside))
}

fn bare_ignore(s: &FileScan, t: &TestItem) -> Option<String> {
    let inside = bare(s, t, "ignore")?;
    let reason = if inside.len() == 1 { "no" } else { "an empty" };
    Some(format!("`{}` is #[ignore] with {reason} reason", t.name))
}

fn bare_should_panic(s: &FileScan, t: &TestItem) -> Option<String> {
    let name = &t.name;
    bare(s, t, "should_panic")
        .map(|_| format!("`{name}` accepts any panic, including its own setup's"))
}

const PLACEHOLDERS: [&str; 22] = [
    "test", "tests", "test1", "test2", "test_1", "test_2", "it_works", "works", "basic", "simple",
    "sanity", "smoke", "tmp", "temp", "foo", "bar", "baz", "qux", "todo", "wip", "check",
    "example",
];

/// A name without the `test_` and the `_test` that say only that it is a test.
fn claim(name: &str) -> &str {
    let name = name.strip_prefix("test_").unwrap_or(name);
    name.strip_suffix("_test").unwrap_or(name)
}

fn placeholder_name(_: &FileScan, t: &TestItem) -> Option<String> {
    let spellings = [t.name.as_str(), claim(&t.name)];
    let placeholder = spellings.iter().any(|name| PLACEHOLDERS.contains(name));
    placeholder.then(|| format!("`{}` names no behaviour", t.name))
}

fn one_word_name(s: &FileScan, t: &TestItem) -> Option<String> {
    let words = claim(&t.name).split('_').filter(|word| !word.is_empty());
    let one = placeholder_name(s, t).is_none() && words.count() < 2;
    one.then(|| format!("`{}` is one word — state what it proves", t.name))
}

/// The calls that reach a place no test run owns: the shared temp directory, the working
/// directory of the whole process, or the developer's home.
const ESCAPES: [&str; 5] = [
    "env::temp_dir",
    "set_current_dir",
    "dirs::home_dir",
    "dirs::data_dir",
    "dirs::config_dir",
];

/// Whether the tokens end in `owner::`.
fn ends_in_path(toks: &[Tok], owner: &str) -> bool {
    matches!(toks, [.., Tok::Ident(o), Tok::Punct(':', true), Tok::Punct(':', false)] if o == owner)
}

/// The first of those calls in the test. A method or another owner's function of the same name
/// is no escape.
fn escape(s: &FileScan, t: &TestItem) -> Option<String> {
    let lexed = &s.lexed;
    let called = |path: &&str| {
        let (owner, call) = path.rsplit_once("::").unwrap_or(("", *path));
        let owned = |i: usize| owner.is_empty() || ends_in_path(&lexed.toks[..i], owner);
        let calls = |i: usize| lexed.ident(i) == call && lexed.opens(i + 1, '(') && owned(i);
        t.body.clone().any(calls)
    };
    let path = ESCAPES.into_iter().find(called)?;
    Some(format!("`{}` calls `{path}()`", t.name))
}
