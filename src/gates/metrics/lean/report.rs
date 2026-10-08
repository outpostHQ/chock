//! `chock lean`: every line a tree could lose, file by file. It reads no record and writes none.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

use super::repeats::{Repeat, Site};
use super::shapes::Seen;
use super::{Tree, read_tree};
use crate::run::Ctx;

/// What the caller asked for: test code too, and the fewest removable lines a listed file has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Asked {
    pub tests: bool,
    pub min: u64,
}

/// The flags of `chock lean`, and the one path it may name.
pub fn asked<'a>(args: &[&'a str]) -> Result<(Asked, Option<&'a str>), String> {
    let mut asked = Asked::default();
    let mut path = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match *arg {
            "--tests" => asked.tests = true,
            "--min" => {
                let given = rest.next().and_then(|n| n.parse().ok());
                asked.min = given.ok_or("lean: `--min` takes a number of lines")?;
            }
            flag if flag.starts_with('-') => return Err(format!("lean does not take `{flag}`")),
            dir if path.is_none() => path = Some(dir),
            _ => return Err("lean takes at most one path".to_string()),
        }
    }
    Ok((asked, path))
}

/// One place that could go. `evidence` is `exact` for a fact the source settles and `estimate`
/// for a shape a person must judge; `twin` is the other copy of a repeat or a mirror.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cut {
    pub line: u32,
    pub end_line: u32,
    pub kind: &'static str,
    pub removable_lines: u64,
    pub fix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub twin: Option<String>,
    pub evidence: &'static str,
}

impl Cut {
    pub fn estimate(kind: &'static str, lines: (u32, u32), removable: u64, fix: &str) -> Self {
        Self {
            line: lines.0,
            end_line: lines.1,
            kind,
            removable_lines: removable,
            fix: fix.to_string(),
            twin: None,
            evidence: "estimate",
        }
    }
}

/// One file: its lines, the lines it could lose, and each place in line order.
#[derive(Debug, Serialize)]
pub struct Row {
    pub path: String,
    pub lines: u64,
    pub removable_lines: u64,
    pub places: Vec<Cut>,
}

/// The whole tree. `files` and `lines` count production files; `rows` puts the file that could
/// lose the most lines first.
#[derive(Debug, Serialize)]
pub struct Report {
    pub chock: String,
    pub files: usize,
    pub lines: u64,
    pub removable_lines: u64,
    pub by_kind: BTreeMap<&'static str, u64>,
    pub lines_counted: &'static str,
    pub tests: bool,
    pub min: u64,
    pub unread: Vec<String>,
    pub rows: Vec<Row>,
}

/// How `lines` is counted, said in the report so a reader can count the same way.
const COUNTED: &str = "each line of a .rs file outside its #[cfg(test)] modules, blank and \
                       comment lines too; test files are not in `files` or `lines`";

/// Each forwarder as an exact cut of its own lines.
fn forwarders(tree: &Tree) -> Vec<(String, Cut)> {
    let cut = |(shown, it): &(String, super::Forwarder)| {
        let fix = it.shows(shown).fix.unwrap_or_default();
        let lines = u64::from(it.last + 1 - it.line);
        let mut cut = Cut::estimate("forwarder", (it.line, it.last), lines, &fix);
        cut.evidence = "exact";
        (shown.clone(), cut)
    };
    tree.forwarders.iter().map(cut).collect()
}

/// Each copy of one repeated group, with its part of the lines the merge removes, so the group
/// counts once. A copy's twin is the first other copy.
fn copies(repeat: &Repeat) -> Vec<(String, Cut)> {
    let fix = repeat.fix();
    let mut cuts = Vec::new();
    for (at, (site, share)) in repeat.sites.iter().zip(repeat.shares()).enumerate() {
        let other = (repeat.sites.iter().enumerate()).find(|(it, _)| *it != at);
        let mut cut = Cut::estimate("repeat", (site.line, site.last), share, &fix);
        cut.twin = other.map(|(_, twin)| format!("{}:{}", twin.file, twin.line));
        cuts.push((site.file.clone(), cut));
    }
    cuts
}

/// The copies of each repeated group. A group of test code is in only where asked.
fn repeats(tree: &Tree, tests: bool) -> Vec<(String, Cut)> {
    let in_tests = tree.corpus.test_starts();
    let test = |site: &Site| in_tests.contains(&(site.file.as_str(), site.line));
    let asked = |repeat: &&Repeat| tests || !repeat.sites.first().is_some_and(test);
    let groups = tree.corpus.repeats();
    groups.iter().filter(asked).flat_map(copies).collect()
}

/// The tree under `ctx` as a report.
pub fn survey(ctx: &Ctx, asked: Asked, version: &str) -> Result<Report, String> {
    let tree = read_tree(ctx)?;
    let mut cuts = forwarders(&tree);
    cuts.extend(repeats(&tree, asked.tests));
    let mut seen = Seen::default();
    for shown in tree.lines.keys() {
        if let Ok(src) = std::fs::read_to_string(ctx.root.join(shown)) {
            seen.read(shown, &src);
        }
    }
    cuts.extend(seen.cuts(&tree.lines));
    let whole = |shown: &str| {
        let read = std::fs::read_to_string(ctx.root.join(shown));
        read.map_or(0, |src| src.split('\n').count() as u64)
    };
    let mut report = tally(&tree.lines, cuts, asked, &whole);
    report.chock = version.to_string();
    report.unread = tree.unread;
    Ok(report)
}

/// The cuts as rows and totals. `whole` gives the length of a file `lines` does not hold, which
/// is a test file. The totals cover every file; `asked.min` only drops rows.
fn tally(
    lines: &BTreeMap<String, u64>,
    cuts: Vec<(String, Cut)>,
    asked: Asked,
    whole: &dyn Fn(&str) -> u64,
) -> Report {
    let mut by_kind = BTreeMap::new();
    let mut by_file: BTreeMap<String, Vec<Cut>> = BTreeMap::new();
    for (shown, cut) in cuts {
        *by_kind.entry(cut.kind).or_default() += cut.removable_lines;
        by_file.entry(shown).or_default().push(cut);
    }
    let row = |(path, mut places): (String, Vec<Cut>)| {
        places.sort_by_key(|it| (it.line, it.end_line));
        Row {
            lines: lines.get(&path).copied().unwrap_or_else(|| whole(&path)),
            removable_lines: places.iter().map(|it| it.removable_lines).sum(),
            path,
            places,
        }
    };
    let mut rows: Vec<Row> = by_file.into_iter().map(row).collect();
    rows.retain(|it| it.removable_lines >= asked.min);
    rows.sort_by(|a, b| (b.removable_lines, &a.path).cmp(&(a.removable_lines, &b.path)));
    Report {
        chock: String::new(),
        files: lines.len(),
        lines: lines.values().sum(),
        removable_lines: by_kind.values().sum(),
        by_kind,
        lines_counted: COUNTED,
        tests: asked.tests,
        min: asked.min,
        unread: Vec::new(),
        rows,
    }
}

/// How many rows the text shows; `--json` holds every row and every place.
const SHOWN: usize = 30;

impl Report {
    /// Zero for a report that read each file, else the files it could not read.
    pub fn whole(&self) -> Result<u8, String> {
        match self.unread.as_slice() {
            [] => Ok(0),
            unread => Err(format!("could not read {}", unread.join(", "))),
        }
    }

    #[must_use]
    pub fn render_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// The totals, the kinds, and the files that could lose the most lines.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
            "chock lean: {} removable line(s) in {} production file(s) of {} line(s)\n",
            self.removable_lines, self.files, self.lines
        );
        for (kind, lines) in &self.by_kind {
            let _ = writeln!(out, "  {lines:>8}  {kind}");
        }
        if !self.rows.is_empty() {
            out.push_str("\n removable    lines  file\n");
        }
        for row in self.rows.iter().take(SHOWN) {
            let _ = writeln!(
                out,
                "  {:>8} {:>8}  {}",
                row.removable_lines, row.lines, row.path
            );
        }
        if let Some(more) = self.rows.len().checked_sub(SHOWN).filter(|more| *more > 0) {
            let _ = writeln!(out, "  and {more} more file(s); `--json` lists every place");
        }
        for file in &self.unread {
            let _ = writeln!(out, "  not read: {file}");
        }
        out
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::testdir::Held;

    /// A function holding a six-line run that differs from its copies only in `value`.
    fn copy(name: &str, value: &str) -> String {
        format!(
            "pub fn {name}() {{\n    let total = items\n        .iter()\n        .map(|item| \
             item.size * 2 + offset)\n        .sum::<u64>();\n    let mean = total / count;\n    \
             log({value}, total, mean);\n    {name}!();\n}}\n"
        )
    }

    #[test]
    fn the_flags_and_the_path_are_read_in_any_order() {
        assert_eq!(asked(&[]), Ok((Asked::default(), None)));
        let all = Asked {
            tests: true,
            min: 12,
        };
        assert_eq!(
            asked(&["--min", "12", "crates/a", "--tests"]),
            Ok((all, Some("crates/a")))
        );
    }

    #[test]
    fn a_flag_it_does_not_know_a_second_path_or_a_min_that_is_no_number_is_refused() {
        assert_eq!(
            asked(&["--fast"]),
            Err("lean does not take `--fast`".to_string())
        );
        assert_eq!(
            asked(&["a", "b"]),
            Err("lean takes at most one path".to_string())
        );
        let no_number = Err("lean: `--min` takes a number of lines".to_string());
        assert_eq!(asked(&["--min", "many"]), no_number);
        assert_eq!(asked(&["--min"]), no_number);
    }

    fn cut(kind: &'static str, line: u32, removable: u64) -> Cut {
        Cut::estimate(kind, (line, line + 1), removable, "fix it")
    }

    fn rows(report: &Report) -> Vec<(&str, u64, u64)> {
        (report.rows.iter())
            .map(|it| (it.path.as_str(), it.lines, it.removable_lines))
            .collect()
    }

    #[test]
    fn rows_put_the_most_removable_lines_first_then_the_path_and_places_go_by_line() {
        let lines = BTreeMap::from([("b.rs".to_string(), 40), ("a.rs".to_string(), 30)]);
        let cuts = vec![
            ("b.rs".to_string(), cut("repeat", 9, 3)),
            ("a.rs".to_string(), cut("repeat", 2, 5)),
            ("b.rs".to_string(), cut("forwarder", 4, 2)),
            ("t.rs".to_string(), cut("repeat", 1, 9)),
            ("c.rs".to_string(), cut("repeat", 1, 5)),
        ];
        let report = tally(&lines, cuts, Asked::default(), &|_| 7);
        assert_eq!(
            rows(&report),
            [
                ("t.rs", 7, 9),
                ("a.rs", 30, 5),
                ("b.rs", 40, 5),
                ("c.rs", 7, 5)
            ]
        );
        let placed: Vec<u32> = report.rows[2].places.iter().map(|it| it.line).collect();
        assert_eq!(placed, [4, 9]);
        assert_eq!(
            (report.files, report.lines, report.removable_lines),
            (2, 70, 24)
        );
        assert_eq!(
            report.by_kind,
            BTreeMap::from([("forwarder", 2), ("repeat", 22)])
        );
    }

    #[test]
    fn min_drops_the_rows_below_it_and_leaves_the_totals_whole() {
        let cuts = vec![
            ("a.rs".to_string(), cut("repeat", 2, 5)),
            ("b.rs".to_string(), cut("repeat", 2, 4)),
        ];
        let asked = Asked {
            tests: false,
            min: 5,
        };
        let report = tally(&BTreeMap::new(), cuts, asked, &|_| 0);
        assert_eq!(rows(&report), [("a.rs", 0, 5)]);
        assert_eq!(report.removable_lines, 9);
        assert_eq!(report.min, 5);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_forwarder_is_exact_and_each_copy_of_a_repeat_names_its_twin() {
        let lib = copy("a", "1") + &copy("b", "2") + "fn f(x: u8) { g(x) }\npub fn h() { f(1) }\n";
        let root = Held::tree(
            "lean-report",
            &[("src/lib.rs", &lib), ("src/b.rs", &copy("c", "3"))],
        );
        let report = survey(&root, Asked::default(), "9.9.9").unwrap();
        assert_eq!(report.chock, "9.9.9");
        assert_eq!(rows(&report), [("src/lib.rs", 21, 5), ("src/b.rs", 10, 2)]);
        assert_eq!(
            report.by_kind,
            BTreeMap::from([("forwarder", 1), ("repeat", 6)])
        );
        let places: Vec<_> = (report.rows[0].places.iter())
            .map(|it| {
                (
                    it.kind,
                    it.line,
                    it.end_line,
                    it.removable_lines,
                    it.twin.as_deref(),
                    it.evidence,
                )
            })
            .collect();
        assert_eq!(
            places,
            [
                ("repeat", 2, 7, 2, Some("src/b.rs:2"), "estimate"),
                ("repeat", 11, 16, 2, Some("src/b.rs:2"), "estimate"),
                ("forwarder", 19, 19, 1, None, "exact"),
            ]
        );
        assert_eq!(
            report.rows[1].places[0].twin.as_deref(),
            Some("src/lib.rs:2")
        );
        assert_eq!(
            report.rows[0].places[2].fix,
            "call `g` where `f` is called, then remove `f`"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn test_code_is_out_until_it_is_asked_for_and_a_test_file_shows_its_whole_length() {
        let tests = copy("a", "1") + &copy("b", "2") + &copy("c", "3");
        let files = [
            ("src/lib.rs", "pub fn a() {}\n"),
            ("tests/cli.rs", tests.as_str()),
        ];
        let root = Held::tree("lean-report-tests", &files);
        let without = survey(&root, Asked::default(), "1").unwrap();
        assert!(without.rows.is_empty(), "{:?}", without.rows);
        assert!(!without.tests);
        let with = survey(
            &root,
            Asked {
                tests: true,
                min: 0,
            },
            "1",
        )
        .unwrap();
        assert_eq!(rows(&with), [("tests/cli.rs", 28, 6)]);
        assert!(with.tests);
        assert_eq!((with.files, with.lines), (1, 2));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_shape_that_spans_files_is_in_the_report_and_a_file_that_does_not_parse_is_named() {
        let files = [
            ("Cargo.toml", ""),
            (
                "src/lib.rs",
                "pub trait Shape {\n    fn area(&self) -> u32;\n}\n",
            ),
            (
                "src/b.rs",
                "struct Square;\nimpl crate::Shape for Square {\n}\n",
            ),
            ("src/broken.rs", "fn broken( {\n"),
        ];
        let root = Held::tree("lean-report-shapes", &files);
        let report = survey(&root, Asked::default(), "1").unwrap();
        assert_eq!(rows(&report), [("src/lib.rs", 4, 3)]);
        assert_eq!(report.rows[0].places[0].kind, "single_impl_trait");
        assert!(matches!(report.unread.as_slice(), [one] if one.starts_with("src/broken.rs: ")));
    }

    #[test]
    fn the_text_shows_the_totals_each_kind_and_the_first_thirty_files() {
        let cuts = (0..32).map(|n| (format!("f{n:02}.rs"), cut("repeat", 1, 40 - n)));
        let mut report = tally(&BTreeMap::new(), cuts.collect(), Asked::default(), &|_| 50);
        report.unread = vec!["src/x.rs: line 1: bad".to_string()];
        let text = report.render();
        assert!(
            text.starts_with(
                "chock lean: 784 removable line(s) in 0 production file(s) of 0 line(s)\n       \
                 784  repeat\n\n removable    lines  file\n        40       50  f00.rs\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("  f29.rs\n  and 2 more file(s); `--json` lists every place\n"),
            "{text}"
        );
        assert!(!text.contains("f30.rs"), "{text}");
        assert!(
            text.ends_with("  not read: src/x.rs: line 1: bad\n"),
            "{text}"
        );
    }

    #[test]
    fn the_text_of_a_tree_with_nothing_to_remove_has_no_table_and_thirty_rows_have_no_more_line() {
        let empty = tally(&BTreeMap::new(), Vec::new(), Asked::default(), &|_| 0);
        assert_eq!(
            empty.render(),
            "chock lean: 0 removable line(s) in 0 production file(s) of 0 line(s)\n"
        );
        let cuts = (0..30).map(|n| (format!("f{n:02}.rs"), cut("repeat", 1, 1)));
        let thirty = tally(&BTreeMap::new(), cuts.collect(), Asked::default(), &|_| 1);
        assert!(!thirty.render().contains("more file(s)"));
    }

    #[test]
    fn the_json_names_each_field_and_leaves_out_a_twin_a_place_does_not_have() {
        let mut twin = cut("repeat", 3, 2);
        twin.twin = Some("b.rs:9".to_string());
        let cuts = vec![
            ("a.rs".to_string(), twin),
            ("a.rs".to_string(), cut("forwarder", 8, 1)),
        ];
        let report = tally(&BTreeMap::new(), cuts, Asked::default(), &|_| 20);
        let json: serde_json::Value = serde_json::from_str(&report.render_json()).unwrap();
        assert_eq!(json["removable_lines"], 3);
        assert_eq!(json["by_kind"]["repeat"], 2);
        assert_eq!(json["lines_counted"], COUNTED);
        let places = &json["rows"][0]["places"];
        assert_eq!(
            places[0],
            serde_json::json!({
                "line": 3, "end_line": 4, "kind": "repeat", "removable_lines": 2,
                "fix": "fix it", "twin": "b.rs:9", "evidence": "estimate"
            })
        );
        assert!(places[1].get("twin").is_none(), "{places}");
    }

    #[test]
    fn a_report_is_whole_only_when_each_file_was_read() {
        let mut report = tally(&BTreeMap::new(), Vec::new(), Asked::default(), &|_| 0);
        assert_eq!(report.whole(), Ok(0));
        report.unread = vec!["a.rs: line 1: bad".to_string(), "b.rs: gone".to_string()];
        let why = "could not read a.rs: line 1: bad, b.rs: gone".to_string();
        assert_eq!(report.whole(), Err(why));
    }
}
