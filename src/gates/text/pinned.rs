//! Tests that a document pins by name, so a rewrite that deletes one trips a gate rather than
//! leaving the document to promise a check nobody runs.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Range, RangeInclusive};
use std::path::Path;

use super::claims;
use crate::gates::metrics::testlint::scan;
use crate::project;
use crate::run::Measurement;
use crate::run::baseline::{Keys, Series};
use crate::run::report::{Detail, Place};
use crate::run::{Ctx, Gate, Group, Kind};
use crate::tokens::{Item, Lexed, Tok};

pub const GATE: Gate = Gate {
    name: "pinned",
    about: "a `test:<file>#<name>` pin in a document whose test or table row is gone, per pin; a \
            pin that leaves the documents trips until the record lets it go",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::AnnotatedRatchet {
        measure,
        // One row per pin, zero while its test is there, so a pin taken out is a row that stops.
        keys: Keys::Census,
        unit: "lost test(s)",
    },
};

/// The word that opens a pin in a code span.
const OPENS: &str = "test:";

/// Each pin as its file and test name, with the document and line of each place that writes it.
type Pinned = BTreeMap<(String, String), Vec<(String, usize)>>;

/// What a pin into one file may name, `Ok(Err(why))` where no pin into it can hold, or `Err` for a
/// file that is there and cannot be read.
type Names = Result<Result<BTreeSet<String>, String>, String>;

fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let seen = |name: &str| !project::SKIPPED.contains(&name);
    let paths = project::walked(&ctx.listing, &ctx.root, &seen, &|_, _| true)?;
    let shown = paths.iter().map(|path| project::relative(&ctx.root, path));
    let mut pinned = Pinned::new();
    for doc in shown.filter(|file| file.ends_with(".md")) {
        let text = std::fs::read_to_string(ctx.root.join(&doc));
        for (key, line) in pins(&text.map_err(|e| format!("{doc}: {e}"))?) {
            pinned.entry(key).or_default().push((doc.clone(), line));
        }
    }
    let place = |(doc, line): &(String, usize)| {
        Place::at("pin", doc, u32::try_from(*line).unwrap_or(u32::MAX))
    };
    let mut measured = Measurement::of(Series::new(), Vec::new());
    let mut files: BTreeMap<&str, Names> = BTreeMap::new();
    for ((file, name), places) in &pinned {
        let key = format!("{file}#{name}");
        let read = files
            .entry(file.as_str())
            .or_insert_with(|| names_in(&ctx.root, file));
        let lost = match read {
            Err(unread) => {
                measured.unmeasured.push(unread.clone());
                continue;
            }
            Ok(Err(why)) => Some(why.clone()),
            Ok(Ok(names)) => (!names.contains(name))
                .then(|| format!("`{file}` has no test and no table row named `{name}`")),
        };
        measured.series.set(&key, u64::from(lost.is_some()));
        if let Some(why) = lost {
            let fix =
                format!("{why}: restore the test, or point the pin at the test's new file or name");
            let places = places.iter().map(place).collect();
            let detail = Detail {
                line: None,
                places,
                fix: Some(fix),
            };
            measured.details.insert(key, detail);
        }
    }
    measured.whole()
}

/// Each pin of a document with its line. A fenced block shows syntax, and a pin under
/// `<!-- doc-check: foreign -->` names another tree's test.
fn pins(text: &str) -> Vec<((String, String), usize)> {
    let mut out = Vec::new();
    let (mut foreign, mut fenced) = (false, false);
    for (at, line) in text.lines().enumerate() {
        let fence = line.trim_start().starts_with("```");
        fenced ^= fence;
        if fence || fenced {
            continue;
        }
        if let Some(("doc-check", side)) = claims::directive(line) {
            foreign = side == "foreign";
        }
        if !foreign {
            out.extend(claims::spans(line).filter_map(pin).map(|key| (key, at + 1)));
        }
    }
    out
}

/// The file and the name of a code span that pins a test: `test:`, then a Rust file written from
/// the root, `#` and the name.
fn pin(span: &str) -> Option<(String, String)> {
    let (file, name) = span.strip_prefix(OPENS)?.split_once('#')?;
    let name = name.trim();
    (file.ends_with(".rs") && !name.is_empty()).then(|| (file.to_string(), name.to_string()))
}

fn names_in(root: &Path, file: &str) -> Names {
    let source = match std::fs::read_to_string(root.join(file)) {
        Ok(source) => source,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Err(format!("`{file}` is gone")));
        }
        Err(e) => return Err(format!("{file}: {e}")),
    };
    let lexed = Lexed::new(&source).map_err(|e| format!("`{file}` {e}"));
    Ok(lexed.map(|lexed| named(file, &lexed)))
}

/// The tests of a file, `proptest!` ones too, and the row labels of its test code.
fn named(file: &str, lexed: &Lexed) -> BTreeSet<String> {
    let (items, functions) = (lexed.items(), lexed.functions());
    let test = |item: &Item| {
        let mut attrs = lexed.attrs(item.first..item.head);
        if !attrs.any(|open| marks_a_test(scan::attr_name(lexed.inside(open)))) {
            return None;
        }
        let own = |f: &&(usize, Range<usize>)| f.1.start == lexed.pair[item.end];
        let (name, _) = functions.iter().find(own)?;
        Some(lexed.ident(*name).to_string())
    };
    // A row label is the string that opens a row of a table in test code: `("label", …)`.
    let whole = scan::is_test_code(file);
    let gated = |item: &&Item| scan::gated_for_tests(lexed, item.first..item.head);
    let spans: Vec<RangeInclusive<usize>> = items
        .iter()
        .filter(gated)
        .map(|item| item.first..=item.end)
        .collect();
    let in_tests = |at: &usize| whole || spans.iter().any(|span| span.contains(at));
    let opens_a_row = |at: &usize| {
        let after_paren = at
            .checked_sub(1)
            .is_some_and(|before| lexed.opens(before, '('));
        after_paren && lexed.is_punct(at + 1, ',')
    };
    let label = |(_, tok): (usize, &Tok)| match tok {
        Tok::Literal(literal) => text_of(literal).map(str::to_string),
        _ => None,
    };
    let rows = lexed
        .toks
        .iter()
        .enumerate()
        .filter(|(at, _)| opens_a_row(at))
        .filter(|(at, _)| in_tests(at));
    let mut names: BTreeSet<String> = items.iter().filter_map(test).collect();
    names.extend(rows.filter_map(label));
    names
}

/// Whether an attribute's last word marks a test: `#[test]`, a runtime's `#[tokio::test]`, a
/// parameterised one, or a project's own wrapper such as `#[repo_test]`.
fn marks_a_test(name: &str) -> bool {
    matches!(name, "test" | "rstest" | "test_case") || name.ends_with("_test")
}

/// The text of a string literal as written, `r"…"` and `r#"…"#` too; `None` for any other literal.
fn text_of(literal: &str) -> Option<&str> {
    let unraw = literal.strip_prefix('r').map(|rest| rest.trim_matches('#'));
    unraw
        .unwrap_or(literal)
        .strip_prefix('"')?
        .strip_suffix('"')
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::testdir::Held;

    #[test]
    fn a_pin_is_a_code_span_outside_a_fence_and_outside_a_foreign_part() {
        let text = "- Q157: `test:crates/a/src/b.rs#holds`\n\
                    ```\n`test:src/shown.rs#syntax`\n```\n\
                    Prose test:src/bare.rs#no and `test:Cargo.toml#no` and `test:src/a.rs#`.\n\
                    <!-- doc-check: foreign -->\n`test:src/theirs.rs#t`\n\
                    <!-- doc-check: ours -->\n`test:src/a.rs#a label` `test:tests/it.rs#t`\n";
        let pin = |file: &str, name: &str, line| ((file.to_string(), name.to_string()), line);
        assert_eq!(
            pins(text),
            [
                pin("crates/a/src/b.rs", "holds", 1),
                pin("src/a.rs", "a label", 9),
                pin("tests/it.rs", "t", 9),
            ]
        );
    }

    #[test]
    fn a_test_fn_under_any_test_attribute_and_one_in_proptest_are_named() {
        let source = "#[test]\nfn plain() {}\n\
                      #[tokio::test(flavor = \"multi_thread\", worker_threads = 2)]\n\
                      async fn awaited() {}\n\
                      #[repo_test(\"x\")]\nfn wrapped() {}\n\
                      proptest! {\n    #![proptest_config(Config::with_cases(8))]\n\
                      /// A doc line.\n    #[test]\n    fn generated(n in 0u32..3) {}\n}\n\
                      fn helper() {}\n#[inline]\nfn inlined() {}\n";
        let named = named("src/lib.rs", &Lexed::new(source).unwrap());
        assert_eq!(
            named.into_iter().collect::<Vec<_>>(),
            ["awaited", "generated", "plain", "wrapped"]
        );
    }

    #[test]
    fn a_row_label_counts_only_where_it_opens_a_row_of_test_code() {
        let table = "const SCANS: &[(&str, &[&str])] = &[\n    (\n        \"one_label\",\n\
                     &[\"a\"],\n    ),\n    (r#\"raw_label\"#, &[]),\n    (LABEL, &[]),\n];\n\
                     fn f() { assert!(x.contains(\"called\")); g(\"last\") }\n";
        let in_tests = named("src/taint/tests/cross_file.rs", &Lexed::new(table).unwrap());
        assert_eq!(
            in_tests.into_iter().collect::<Vec<_>>(),
            ["one_label", "raw_label"]
        );
        assert!(named("src/lib.rs", &Lexed::new(table).unwrap()).is_empty());
        let gated = format!("#[cfg(test)]\nmod tests {{\n{table}}}\n");
        let in_a_mod = named("src/lib.rs", &Lexed::new(&gated).unwrap());
        assert_eq!(
            in_a_mod.into_iter().collect::<Vec<_>>(),
            ["one_label", "raw_label"]
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn each_pin_measures_zero_while_its_test_is_there_and_one_once_it_is_lost() {
        let plan = "# Open findings\n\n## Pinned\n\n\
                    - Q150: `test:tests/scan.rs#a_row`\n\
                    - C5: `test:src/docs.rs#reads_a_note`\n\
                    - C6: `test:src/docs.rs#was_deleted`\n\
                    - C7: `test:src/gone.rs#anything`\n\
                    - C8: `test:src/torn.rs#t`\n";
        let docs = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn reads_a_note() {}\n}\n";
        let ctx = Held::tree(
            "gate-pinned",
            &[
                ("plans/open.md", plan),
                ("README.md", "Also `test:src/docs.rs#reads_a_note`.\n"),
                ("src/docs.rs", docs),
                ("src/torn.rs", "fn f() {\n"),
                (
                    "tests/scan.rs",
                    "const ROWS: &[(&str, u8)] = &[(\"a_row\", 1)];\n",
                ),
            ],
        );
        let measured = measure(&ctx).unwrap();
        let rows = crate::testdir::rows(&measured.series);
        assert_eq!(
            rows,
            [
                ("src/docs.rs#reads_a_note", 0),
                ("src/docs.rs#was_deleted", 1),
                ("src/gone.rs#anything", 1),
                ("src/torn.rs#t", 1),
                ("tests/scan.rs#a_row", 0),
            ]
        );
        let lost = &measured.details["src/docs.rs#was_deleted"];
        assert_eq!(lost.places, [Place::at("pin", "plans/open.md", 7)]);
        let fix = lost.fix.as_deref().unwrap_or_default();
        assert!(
            fix.starts_with("`src/docs.rs` has no test and no table row named `was_deleted`: "),
            "{fix}"
        );
        let gone = &measured.details["src/gone.rs#anything"];
        assert!(
            gone.fix
                .as_deref()
                .unwrap_or_default()
                .starts_with("`src/gone.rs` is gone: ")
        );
        let torn = measured.details["src/torn.rs#t"].fix.as_deref();
        assert!(
            torn.unwrap_or_default()
                .starts_with("`src/torn.rs` does not lex: "),
            "{torn:?}"
        );
        assert!(!measured.details.contains_key("src/docs.rs#reads_a_note"));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn each_pinned_file_that_cannot_be_read_is_named_and_nothing_is_measured() {
        let plan = "`test:src/a.rs#t` `test:src/b.rs#lost` `test:src/c.rs#t`\n";
        let files = [
            ("plan.md", plan),
            ("src/a.rs", ""),
            ("src/b.rs", "fn f() {}\n"),
            ("src/c.rs", ""),
        ];
        let ctx = Held::tree("gate-pinned-unread", &files);
        for file in ["src/a.rs", "src/c.rs"] {
            std::fs::write(ctx.root.join(file), [0xff, 0xfe]).unwrap();
        }
        let err = measure(&ctx).unwrap_err();
        assert!(err.starts_with("2 file(s) could not be read"), "{err}");
        assert!(
            err.contains("src/a.rs: ") && err.contains("src/c.rs: "),
            "{err}"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_without_pins_measures_nothing() {
        let ctx = Held::tree("gate-pinned-none", &[("README.md", "`test:` alone\n")]);
        assert_eq!(measure(&ctx).unwrap().series, Series::new());
    }
}
