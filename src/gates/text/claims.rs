//! Markdown that cites a line its file no longer has, or names as absent a thing the tree holds.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use crate::gates::metrics::prodlines;
use crate::gates::metrics::testlint::finding;
use crate::gates::source::targets::TARGET_DIRS;
use crate::project;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Inspection, Kind};
use crate::tokens::{Item, Lexed, Tok};

pub const GATE: Gate = Gate {
    name: "claims",
    about: "a document citing a line past the end of its file, or naming as absent a thing the tree holds, per file and rule",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Debt {
        inspect,
        unit: "stale claim(s)",
    },
};

const ANCHOR: &str = "stale-doc-anchor";
const REFUTED: &str = "absent-claim-refuted";
const UNVERIFIABLE: &str = "unverifiable-absence-claim";
const WAIVER: &str = "absence-waiver-without-reason";

/// The extensions a cited path must end in; any other code span is prose or code.
const EXTENSIONS: [&str; 11] = [
    "rs", "toml", "py", "md", "sh", "yml", "yaml", "json", "ts", "tsx", "txt",
];

/// The words that say what kind of thing a name is, in code and in an `absent:` item.
const KINDS: [&str; 10] = [
    "fn", "struct", "enum", "trait", "type", "const", "static", "mod", "union", "macro",
];

/// Words that follow a defining keyword and are not the name it defines.
const NOT_A_NAME: [&str; 12] = [
    "fn", "mut", "dyn", "impl", "unsafe", "async", "extern", "move", "ref", "self", "Self", "where",
];

/// `(kind, file, line)` of each definition, under its name.
type Defined = BTreeMap<String, Vec<(&'static str, String, usize)>>;

/// One `file:line` citation in a document.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Anchor {
    line: usize,
    path: String,
    /// The highest line cited: `a.rs:8,30` and `a.rs:8-30` both reach 30.
    last: Option<usize>,
    /// Written under `<!-- doc-check: foreign -->`, so about another tree.
    foreign: bool,
}

/// One item of an `<!-- absent: … -->` list.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Absent {
    line: usize,
    kind: Option<String>,
    name: String,
}

/// What one document claims, before anything is looked up in the tree.
#[derive(Debug, Default, PartialEq, Eq)]
struct Claims {
    anchors: Vec<Anchor>,
    absent: Vec<Absent>,
    /// `(line, rule, detail)` of each claim that is wrong whatever the tree holds.
    wrong: Vec<(usize, &'static str, String)>,
}

impl Claims {
    /// The items of one `absent:` list: a name or a path to look up, or one nothing can check.
    fn listed(&mut self, items: &str, line: usize) {
        let items = items.split(',').map(|item| item.replace('`', ""));
        for item in items.filter(|item| !item.trim().is_empty()) {
            let (kind, name) = match item.trim().split_once(char::is_whitespace) {
                Some((head, rest)) if KINDS.contains(&head) => (Some(head), rest.trim()),
                _ => (None, item.trim()),
            };
            if is_a_path(name) || is_an_identifier(name) {
                self.absent.push(Absent {
                    line,
                    kind: kind.map(str::to_string),
                    name: name.to_string(),
                });
            } else {
                let detail =
                    format!("`{name}` is neither a symbol nor a path, so nothing can check it");
                self.wrong.push((line, UNVERIFIABLE, detail));
            }
        }
    }
}

/// What a tree holds, as far as a claim can ask.
#[derive(Default)]
struct Tree {
    /// Every file's path from the root, under its file name.
    by_name: BTreeMap<String, Vec<String>>,
    /// Each name production code defines.
    defined: Defined,
}

impl Tree {
    fn of<'a>(files: impl IntoIterator<Item = &'a str>) -> Self {
        let mut tree = Self::default();
        for file in files {
            let name = file.rsplit_once('/').map_or(file, |(_, name)| name);
            let same_name = tree.by_name.entry(name.to_string()).or_default();
            same_name.push(file.to_string());
        }
        tree
    }

    /// The files `cited` names in `doc`. A path that starts at a directory of cargo's layout is read
    /// from this root or from a directory above the document; another names each file ending in it.
    fn named(&self, doc: &str, cited: &str) -> Vec<&str> {
        let name = cited.rsplit_once('/').map_or(cited, |(_, name)| name);
        let starts = |dir: &&str| project::under(cited, dir).is_some();
        let rooted = TARGET_DIRS.iter().any(starts);
        let hit = |file: &&String| {
            let whole = |dir: &&str| dir.is_empty() || dir.ends_with('/');
            let from = file.strip_suffix(cited).filter(whole);
            from.is_some_and(|dir| !rooted || doc.starts_with(dir))
        };
        let same_name = self.by_name.get(name).map_or(&[][..], Vec::as_slice);
        same_name.iter().filter(hit).map(String::as_str).collect()
    }
}

fn inspect(ctx: &Ctx) -> Result<Inspection, String> {
    let seen = |name: &str| !project::SKIPPED.contains(&name);
    let paths = project::walked(&ctx.listing, &ctx.root, &seen, &|_, _| true)?;
    let shown = paths.iter().map(|path| project::relative(&ctx.root, path));
    let files: Vec<String> = shown.collect();
    let mut tree = Tree::of(files.iter().map(String::as_str));
    let mut docs = Vec::new();
    for file in files.iter().filter(|file| file.ends_with(".md")) {
        let text = std::fs::read_to_string(ctx.root.join(file));
        docs.push((file, read(&text.map_err(|e| format!("{file}: {e}"))?)));
    }
    let names_a_symbol = |absent: &[Absent]| absent.iter().any(|item| !is_a_path(&item.name));
    if docs
        .iter()
        .any(|(_, claims)| names_a_symbol(&claims.absent))
    {
        tree.defined = defined(ctx)?;
    }
    let mut counted = BTreeMap::new();
    let mut lines = |file: &str| {
        let count = || {
            Some(
                std::fs::read_to_string(ctx.root.join(file))
                    .ok()?
                    .lines()
                    .count(),
            )
        };
        *counted.entry(file.to_string()).or_insert_with(count)
    };
    let mut found = Vec::new();
    for (doc, claims) in &docs {
        let wrong = claims.wrong.iter();
        found.extend(wrong.map(|(line, rule, detail)| finding(doc, *line, rule, detail)));
        found.extend(stale(doc, &claims.anchors, &tree, &mut lines));
        found.extend(refuted(doc, &claims.absent, &tree));
    }
    found.sort_by(|a, b| (&a.file, a.line, &a.item).cmp(&(&b.file, b.line, &b.item)));
    Ok(Inspection::debt(found))
}

/// What a document claims. A fenced block holds sample output and shell transcripts, so it
/// claims nothing.
fn read(text: &str) -> Claims {
    let mut claims = Claims::default();
    let (mut foreign, mut fenced) = (false, false);
    for (at, text) in text.lines().enumerate() {
        let fence = text.trim_start().starts_with("```");
        fenced ^= fence;
        if fence || fenced {
            continue;
        }
        let cited = |(path, last)| Anchor {
            line: at + 1,
            path,
            last,
            foreign,
        };
        match directive(text) {
            Some(("doc-check", side)) => foreign = side == "foreign",
            Some(("absent", items)) => claims.listed(items, at + 1),
            Some((_, what)) if reason(what).is_none() => {
                let detail = format!("`absent-by-design: {what}` states no reason");
                claims.wrong.push((at + 1, WAIVER, detail));
            }
            Some(_) => {}
            None => claims
                .anchors
                .extend(spans(text).filter_map(anchor).map(cited)),
        }
    }
    claims
}

/// The directive in a line's `<!-- … -->`: its word and the text after the colon. One inside a
/// code span is a document showing the syntax.
fn directive(line: &str) -> Option<(&str, &str)> {
    let masked = masked(line);
    let at = masked.find("<!--")? + 4;
    let end = at + masked.get(at..)?.find("-->")?;
    let (word, rest) = line.get(at..end)?.split_once(':')?;
    let (word, rest) = (word.trim(), rest.trim());
    let sides = word == "doc-check" && matches!(rest, "foreign" | "ours");
    let known = sides || matches!(word, "absent" | "absent-by-design");
    known.then_some((word, rest))
}

/// The line with the text of every code span blanked byte for byte, so an offset holds in both.
fn masked(line: &str) -> String {
    let mut inside = false;
    let blank = |c: char| {
        inside ^= c == '`';
        let hidden = inside && c != '`';
        if hidden {
            " ".repeat(c.len_utf8())
        } else {
            c.to_string()
        }
    };
    line.chars().map(blank).collect()
}

/// The text of each code span on a line.
fn spans(line: &str) -> impl Iterator<Item = &str> {
    line.split('`').skip(1).step_by(2)
}

/// The reason half of `absent-by-design: <what> — <why>`.
fn reason(what: &str) -> Option<&str> {
    let halves = what.split_once('—');
    let halves = halves.or_else(|| what.split_once("--"));
    let halves = halves.or_else(|| what.split_once(':'));
    halves
        .map(|(_, why)| why.trim())
        .filter(|why| !why.is_empty())
}

/// A path to a file, then perhaps `:N`, `:N-M` or `:N,M`, as the path and the highest line.
fn anchor(span: &str) -> Option<(String, Option<usize>)> {
    let span = span.trim();
    let (path, tail) = span.split_once(':').unwrap_or((span, ""));
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-');
    let extension = path.rsplit_once('.')?.1;
    if !path.chars().all(plain) || !EXTENSIONS.contains(&extension) {
        return None;
    }
    let mut last = None;
    let parts = tail.split(['-', ',', ':', ' ']);
    for part in parts.filter(|part| !part.is_empty()) {
        last = last.max(Some(part.parse::<usize>().ok()?));
    }
    Some((path.to_string(), last))
}

fn is_a_path(item: &str) -> bool {
    let extension = item.rsplit_once('.').map(|(_, extension)| extension);
    item.contains('/') || extension.is_some_and(|e| EXTENSIONS.contains(&e))
}

fn is_an_identifier(item: &str) -> bool {
    let starts = |c: char| c.is_ascii_alphabetic() || c == '_';
    let goes_on = |c: char| c.is_ascii_alphanumeric() || c == '_';
    item.starts_with(starts) && item.chars().all(goes_on)
}

/// The anchors past the end of their file. A line that cites a file this tree lacks describes
/// another tree, so on it only a path written from the root is checked.
fn stale(
    doc: &str,
    anchors: &[Anchor],
    tree: &Tree,
    lines: &mut dyn FnMut(&str) -> Option<usize>,
) -> Vec<Finding> {
    let mut found = Vec::new();
    for on_a_line in anchors.chunk_by(|a, b| a.line == b.line) {
        let lacks = |a: &Anchor| a.foreign || tree.named(doc, &a.path).is_empty();
        let foreign = on_a_line.iter().any(lacks);
        for anchor in on_a_line {
            let named = tree.named(doc, &anchor.path);
            let (Some(cited), [file]) = (anchor.last, named.as_slice()) else {
                continue;
            };
            let ours = !foreign || *file == anchor.path;
            let checked = anchor.path.contains('/') && ours;
            let have = checked.then(|| lines(file)).flatten();
            let Some(have) = have.filter(|&have| cited > have) else {
                continue;
            };
            let path = &anchor.path;
            let detail =
                format!("`{path}:{cited}` — {file} has {have} lines; the code it named moved");
            found.push(finding(doc, anchor.line, ANCHOR, &detail));
        }
    }
    found
}

/// The claims of absence the tree refutes.
fn refuted(doc: &str, absent: &[Absent], tree: &Tree) -> Vec<Finding> {
    let held = |claim: &Absent| {
        let name = &claim.name;
        let is = if is_a_path(name) {
            tree.named(doc, name)
                .first()
                .map(|file| format!("is {file}"))
        } else {
            definition(claim, tree)
        };
        let detail = format!("claimed absent, but `{name}` {}", is?);
        Some(finding(doc, claim.line, REFUTED, &detail))
    };
    absent.iter().filter_map(held).collect()
}

/// Where the name is defined, of the kind claimed when the claim states one.
fn definition(claim: &Absent, tree: &Tree) -> Option<String> {
    let claimed = |kind: &str| claim.kind.as_deref().is_none_or(|k| k == kind);
    let all = tree.defined.get(&claim.name)?;
    let mut hits = all.iter().filter(|hit| claimed(hit.0));
    let (kind, file, line) = hits.next()?;
    let more = match hits.count() {
        0 => String::new(),
        n => format!(" (and {n} more)"),
    };
    Some(format!("is the {kind} at {file}:{line}{more}"))
}

/// Every production definition. Test code is left out: a name only a test defines is absent
/// from the product.
fn defined(ctx: &Ctx) -> Result<Defined, String> {
    let mut by_name = Defined::new();
    for (file, found) in prodlines::for_each_source(ctx, &|src| Ok(definitions(src)))? {
        for (name, kind, line) in found {
            let same_name = by_name.entry(name).or_default();
            same_name.push((kind, file.clone(), line));
        }
    }
    Ok(by_name)
}

/// The token ranges of the items under `#[cfg(test)]`.
fn test_only(lexed: &Lexed) -> Vec<RangeInclusive<usize>> {
    let word = |word: &str| Tok::Ident(word.to_string());
    let gate = [word("cfg"), Tok::Open('('), word("test"), Tok::Close(')')];
    let gated = |item: &&Item| {
        let mut attrs = lexed.attrs(item.first..item.head);
        attrs.any(|open| lexed.inside(open) == gate)
    };
    let items = lexed.items();
    let spans = items.iter().filter(gated).map(|item| item.first..=item.end);
    spans.collect()
}

/// `(name, kind, line)` of each definition: a defining keyword with the name as the very next
/// token, so the `fn` of a `fn(u8)` type defines nothing. A file that does not lex defines nothing.
fn definitions(src: &str) -> Vec<(String, &'static str, usize)> {
    let lexed = Lexed::new(src).unwrap_or_default();
    let tested = test_only(&lexed);
    let defines = |i: usize| {
        let word = lexed.ident(i);
        let is_macro = word == "macro_rules" && lexed.is_punct(i + 1, '!');
        let kind = KINDS.iter().find(|kind| **kind == word);
        let (kind, at) = match kind {
            _ if is_macro => ("macro", i + 2),
            Some(kind) => (*kind, i + 1),
            None => return None,
        };
        let name = lexed.ident(at);
        let named = !name.is_empty() && !NOT_A_NAME.contains(&name);
        let shipped = !tested.iter().any(|span| span.contains(&i));
        (named && shipped).then(|| (name.to_string(), kind, lexed.line[at]))
    };
    let places = lexed.toks.iter().enumerate();
    places.filter_map(|(i, _)| defines(i)).collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::testdir::Held;

    fn cited(line: usize, path: &str, last: Option<usize>, foreign: bool) -> Anchor {
        Anchor {
            line,
            path: path.to_string(),
            last,
            foreign,
        }
    }

    fn absent(line: usize, kind: Option<&str>, name: &str) -> Absent {
        Absent {
            line,
            kind: kind.map(str::to_string),
            name: name.to_string(),
        }
    }

    fn defined_in(src: &str) -> Vec<String> {
        let found = definitions(src).into_iter();
        found
            .map(|(name, kind, line)| format!("{kind} {name} {line}"))
            .collect()
    }

    #[test]
    fn a_span_is_an_anchor_when_it_is_a_path_with_a_known_extension() {
        let parsed = |span: &str| anchor(span).map(|(path, last)| (path, last.unwrap_or(0)));
        let at = |path: &str, last| Some((path.to_string(), last));
        assert_eq!(parsed("src/a.rs"), at("src/a.rs", 0));
        assert_eq!(anchor("b.toml"), Some(("b.toml".to_string(), None)));
        assert_eq!(parsed(" src/a.rs:12 "), at("src/a.rs", 12));
        assert_eq!(parsed("src/a.rs:8-30"), at("src/a.rs", 30));
        assert_eq!(parsed("src/a.rs:30,8"), at("src/a.rs", 30));
        assert_eq!(parsed("a-b_c/d.yml:1:2"), at("a-b_c/d.yml", 2));
        assert_eq!(parsed("src/a.rs: 7"), at("src/a.rs", 7));
        for prose in [
            "",
            "src/a",
            "src/a.exe",
            "src/a.rs:run",
            "a b.rs",
            "f(x).rs",
        ] {
            assert_eq!(anchor(prose), None, "{prose}");
        }
    }

    #[test]
    fn a_document_s_anchors_absences_and_wrong_claims_are_read_with_their_lines() {
        let text = "See `src/a.rs:9`, `cargo test` and `b.toml`.\n<!-- absent: Evidence, fn `decide`, src/gone.rs, a b, 9lives, , -->\n<!-- absent-by-design: undo — the log replays instead -->\n<!--absent-by-design:redo-->\n<!--absent:macro held-->\n";
        let claims = read(text);
        assert_eq!(
            claims.anchors,
            [
                cited(1, "src/a.rs", Some(9), false),
                cited(1, "b.toml", None, false)
            ]
        );
        assert_eq!(
            claims.absent,
            [
                absent(2, None, "Evidence"),
                absent(2, Some("fn"), "decide"),
                absent(2, None, "src/gone.rs"),
                absent(5, Some("macro"), "held"),
            ]
        );
        let unverifiable = |name: &str| {
            let detail =
                format!("`{name}` is neither a symbol nor a path, so nothing can check it");
            (2, UNVERIFIABLE, detail)
        };
        let waiver = "`absent-by-design: redo` states no reason".to_string();
        assert_eq!(
            claims.wrong,
            [
                unverifiable("a b"),
                unverifiable("9lives"),
                (4, WAIVER, waiver)
            ]
        );
    }

    #[test]
    fn a_fenced_block_and_a_directive_in_a_code_span_claim_nothing() {
        let text = "```sh\ncat `src/a.rs:9`\n<!-- absent: Evidence -->\n  ```\nUse `<!-- absent: Décision -->` to claim it; see `src/b.rs:3`.\n";
        let claims = read(text);
        assert_eq!(claims.anchors, [cited(5, "src/b.rs", Some(3), false)]);
        assert_eq!((claims.absent, claims.wrong), (Vec::new(), Vec::new()));
        assert_eq!(masked("é `é` <!--"), "é `  ` <!--");
    }

    #[test]
    fn a_foreign_directive_marks_each_anchor_until_it_is_turned_back_off() {
        let text = "<!-- doc-check: foreign -->\nTheir `engine/push.rs:994`.\n<!-- doc-check: ours -->\nOur `engine/push.rs:994`.\n<!-- doc-check: theirs --> `src/a.rs:1`\n<!-- a note --> `src/b.rs:2`\n<!-- see: this --> `src/c.rs:3`\n";
        assert_eq!(
            read(text).anchors,
            [
                cited(2, "engine/push.rs", Some(994), true),
                cited(4, "engine/push.rs", Some(994), false),
                cited(5, "src/a.rs", Some(1), false),
                cited(6, "src/b.rs", Some(2), false),
                cited(7, "src/c.rs", Some(3), false),
            ]
        );
    }

    #[test]
    fn a_waiver_s_reason_follows_a_dash_two_hyphens_or_a_colon() {
        assert_eq!(reason("undo — the log replays"), Some("the log replays"));
        assert_eq!(reason("undo -- the log replays"), Some("the log replays"));
        assert_eq!(reason("undo: the log replays"), Some("the log replays"));
        assert_eq!(reason("undo — "), None);
        assert_eq!(reason("undo"), None);
    }

    #[test]
    fn a_path_from_inside_a_crate_names_each_file_that_is_it_or_ends_in_it() {
        let tree = Tree::of(["src/gates/a.rs", "tests/gates/a.rs", "src/data.rs", "a.rs"]);
        let named = |cited: &str| tree.named("docs/plan.md", cited);
        assert_eq!(named("gates/a.rs"), ["src/gates/a.rs", "tests/gates/a.rs"]);
        assert_eq!(
            named("a.rs"),
            ["src/gates/a.rs", "tests/gates/a.rs", "a.rs"]
        );
        assert_eq!(named("ata.rs"), [""; 0]);
        assert_eq!(named("s/a.rs"), [""; 0]);
    }

    #[test]
    fn a_path_from_a_root_names_a_file_of_this_root_or_of_a_directory_above_the_document() {
        let tree = Tree::of([
            "src/lib.rs",
            "crates/core/src/push.rs",
            "crates/core/tests/it.rs",
        ]);
        for (doc, cited, file) in [
            ("docs/plan.md", "src/lib.rs", Some("src/lib.rs")),
            ("docs/plan.md", "src/gone.rs", None),
            ("docs/plan.md", "src/push.rs", None),
            ("docs/plan.md", "tests/it.rs", None),
            (
                "docs/plan.md",
                "core/src/push.rs",
                Some("crates/core/src/push.rs"),
            ),
            (
                "docs/plan.md",
                "crates/core/src/push.rs",
                Some("crates/core/src/push.rs"),
            ),
            (
                "crates/core/README.md",
                "src/push.rs",
                Some("crates/core/src/push.rs"),
            ),
            (
                "crates/core/docs/deep/plan.md",
                "tests/it.rs",
                Some("crates/core/tests/it.rs"),
            ),
            ("crates/corex/README.md", "src/push.rs", None),
        ] {
            let expected: Vec<&str> = file.into_iter().collect();
            assert_eq!(tree.named(doc, cited), expected, "{doc} cites {cited}");
        }
    }

    /// The stale anchors of one document in a tree whose every readable file has 120 lines.
    fn stale_among(anchors: &[Anchor]) -> Vec<String> {
        let tree = Tree::of([
            "crates/core/src/push.rs",
            "a/gates/run.rs",
            "b/gates/run.rs",
            "data/blob.json",
        ]);
        let mut lines = |file: &str| (file != "data/blob.json").then_some(120);
        Finding::rendered(&stale("docs/plan.md", anchors, &tree, &mut lines))
    }

    #[test]
    fn an_anchor_past_the_end_of_its_one_file_is_stale() {
        let stale = |cited: usize| {
            let what = "crates/core/src/push.rs has 120 lines; the code it named moved";
            format!("docs/plan.md:7: stale-doc-anchor: `core/src/push.rs:{cited}` — {what}")
        };
        let found = stale_among(&[
            cited(7, "core/src/push.rs", Some(121), false),
            cited(7, "core/src/push.rs", Some(120), false),
            cited(7, "core/src/push.rs", None, false),
            cited(7, "push.rs", Some(500), false),
            cited(7, "gates/run.rs", Some(500), false),
            cited(7, "data/blob.json", Some(500), false),
            cited(7, "core/src/push.rs", Some(500), false),
        ]);
        assert_eq!(found, [stale(121), stale(500)]);
    }

    #[test]
    fn a_line_about_another_tree_is_checked_only_where_it_cites_a_path_from_the_root() {
        let found = stale_among(&[
            cited(3, "engine/merge.rs", Some(9), false),
            cited(3, "core/src/push.rs", Some(500), false),
            cited(3, "crates/core/src/push.rs", Some(501), false),
            cited(4, "core/src/push.rs", Some(502), true),
            cited(4, "crates/core/src/push.rs", Some(503), true),
            cited(5, "core/src/push.rs", Some(504), false),
            cited(6, "src/push.rs", Some(505), false),
            cited(6, "crates/core/src/push.rs", Some(120), false),
        ]);
        let stale = |line: u8, cited: &str| {
            let what = "crates/core/src/push.rs has 120 lines; the code it named moved";
            format!("docs/plan.md:{line}: stale-doc-anchor: `{cited}` — {what}")
        };
        let expected = [
            stale(3, "crates/core/src/push.rs:501"),
            stale(4, "crates/core/src/push.rs:503"),
            stale(5, "core/src/push.rs:504"),
        ];
        assert_eq!(found, expected);
    }

    #[test]
    fn a_claim_of_absence_is_refuted_by_a_file_or_by_a_definition_of_the_kind_claimed() {
        let mut tree = Tree::of(["src/gone.rs", "docs/a/notes.md", "docs/b/notes.md"]);
        let at = |kind, file: &str, line| (kind, file.to_string(), line);
        let defs = vec![
            at("struct", "src/a.rs", 4),
            at("fn", "src/b.rs", 9),
            at("struct", "src/c.rs", 2),
        ];
        tree.defined.insert("Evidence".to_string(), defs);
        let claims = [
            absent(3, None, "src/gone.rs"),
            absent(4, None, "notes.md"),
            absent(5, None, "src/kept.rs"),
            absent(6, None, "Evidence"),
            absent(7, Some("fn"), "Evidence"),
            absent(8, Some("enum"), "Evidence"),
            absent(9, None, "Decision"),
        ];
        let expected = [
            "docs/plan.md:3: absent-claim-refuted: claimed absent, but `src/gone.rs` is src/gone.rs",
            "docs/plan.md:4: absent-claim-refuted: claimed absent, but `notes.md` is docs/a/notes.md",
            "docs/plan.md:6: absent-claim-refuted: claimed absent, but `Evidence` is the struct at src/a.rs:4 (and 2 more)",
            "docs/plan.md:7: absent-claim-refuted: claimed absent, but `Evidence` is the fn at src/b.rs:9",
        ];
        let found = refuted("docs/plan.md", &claims, &tree);
        assert_eq!(Finding::rendered(&found), expected);
    }

    #[test]
    fn each_defining_keyword_with_a_name_after_it_is_a_definition() {
        let src = "pub fn run() {}\nstruct Plan;\nenum Mode { A }\ntrait Gate {}\ntype Rows = Vec<u8>;\nconst LIMIT: u8 = 1;\nstatic SEEN: u8 = 0;\nmod inner {}\nunion Bits { a: u8 }\nmacro_rules! held { () => {} }\npub const fn fixed() {}\nfn takes(f: fn(u8) -> u8, g: impl Fn()) {}\nfn late() { macro_rules(); }\n";
        let expected = "fn run 1|struct Plan 2|enum Mode 3|trait Gate 4|type Rows 5|\
            const LIMIT 6|static SEEN 7|mod inner 8|union Bits 9|macro held 10|fn fixed 11|\
            fn takes 12|fn late 13";
        assert_eq!(defined_in(src).join("|"), expected);
        assert_eq!(defined_in("fn broken( {\n"), [""; 0]);
    }

    #[test]
    fn a_name_only_test_code_defines_is_no_definition() {
        let src = "fn kept() {}\n#[cfg(test)]\nmod tests {\n    fn helper() {}\n}\n#[cfg(test)]\nuse x::y;\nfn after() {}\n#[cfg(not(test))]\nfn shipped() {}\n#[inline]\n#[cfg(test)]\nfn probe() {}\n";
        assert_eq!(
            defined_in(src),
            ["fn kept 1", "fn after 8", "fn shipped 10"]
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_s_documents_are_checked_against_its_files_and_its_production_code() {
        let ctx = Held::tree(
            "claims-inspect",
            &[
                ("Cargo.toml", ""),
                (
                    "src/lib.rs",
                    "pub struct Evidence;\n#[cfg(test)]\nmod tests {\n    fn probe() {}\n}\n",
                ),
                (
                    "docs/plan.md",
                    "Read `src/lib.rs:9`, then `src/lib.rs:5` and `src/lib.rs:9`.\n<!-- absent: Evidence, probe -->\n<!-- absent-by-design: undo -->\n",
                ),
                ("README.md", "The gate is `src/lib.rs:40`.\n"),
                ("target/doc/old.md", "`src/lib.rs:99`\n"),
                ("notes.txt", "`src/lib.rs:99`\n"),
            ],
        );
        let inspection = inspect(&ctx).unwrap();
        assert_eq!(Finding::rendered(&inspection.blockers), [""; 0]);
        let moved = "src/lib.rs has 5 lines; the code it named moved";
        let expected = [
            format!("README.md:1: stale-doc-anchor: `src/lib.rs:40` — {moved}"),
            format!("docs/plan.md:1: stale-doc-anchor: `src/lib.rs:9` — {moved}"),
            format!("docs/plan.md:1: stale-doc-anchor: `src/lib.rs:9` — {moved}"),
            "docs/plan.md:2: absent-claim-refuted: claimed absent, but `Evidence` is the struct at src/lib.rs:1".to_string(),
            "docs/plan.md:3: absence-waiver-without-reason: `absent-by-design: undo` states no reason".to_string(),
        ];
        assert_eq!(Finding::rendered(&inspection.debt), expected);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_document_or_a_source_that_cannot_be_read_stops_the_gate_and_is_named() {
        let files = [
            ("Cargo.toml", "[package]\nname = \"x\"\n"),
            ("src/lib.rs", ""),
            ("docs/plan.md", "<!-- absent: Evidence -->\n"),
        ];
        let ctx = Held::tree("claims-unread", &files);
        std::fs::write(ctx.root.join("src/lib.rs"), [0xff, 0xfe]).unwrap();
        let err = inspect(&ctx).map(|found| found.debt).unwrap_err();
        assert!(err.starts_with("src/lib.rs: "), "{err}");
        std::fs::write(ctx.root.join("docs/plan.md"), [0xff, 0xfe]).unwrap();
        let err = inspect(&ctx).map(|found| found.debt).unwrap_err();
        assert!(err.starts_with("docs/plan.md: "), "{err}");
    }
}
