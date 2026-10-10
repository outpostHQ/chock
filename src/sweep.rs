//! `chock sweep` and `chock moved`: proof by token that a change of comments, or a move of code
//! between files, changed no code.

use std::path::Path;

use crate::project::vcs;
use crate::tokens::{self, Tok, TokenCounts, Verdict, describe};

/// How many kinds of token one side of a delta lists.
const SHOWN: usize = 40;

/// The revision to compare with and the paths after it. `moved` needs its paths: a move is
/// between files the caller names.
pub fn asked<'a>(name: &str, rest: &'a [&'a str]) -> Result<(&'a str, &'a [&'a str]), String> {
    if let Some(flag) = rest.iter().find(|arg| arg.starts_with('-')) {
        return Err(format!("unknown option `{flag}`"));
    }
    match rest {
        [] => Err(format!("{name} needs the revision to compare with")),
        [_] if name == "moved" => Err("moved needs each file the code left or reached".to_string()),
        [rev, paths @ ..] => Ok((rev, paths)),
    }
}

/// What a sweep found for one file.
#[derive(Debug, PartialEq, Eq)]
enum Row {
    Judged(Verdict),
    Skipped(String),
}

impl Row {
    /// One file judged: its text at the revision, where the revision holds it, and its text now.
    fn of(rev: &str, before: Option<String>, after: std::io::Result<String>) -> Self {
        let Some(before) = before else {
            return Self::Skipped(format!("new since {rev} — use `moved`"));
        };
        let after = match after {
            Ok(after) => after,
            Err(e) => return Self::Skipped(format!("unreadable: {e}")),
        };
        match (tokens::lex(&before), tokens::lex(&after)) {
            (Ok(before), Ok(after)) => Self::Judged(tokens::classify(&before, &after)),
            _ => Self::Skipped("does not lex".to_string()),
        }
    }

    /// The report's line for the file, where it has a verdict.
    fn line(&self, path: &str) -> Option<String> {
        match self {
            Self::Judged(Verdict::Clean) => Some(format!("  clean      {path}\n")),
            Self::Judged(Verdict::DocsOnly { removed, added }) => Some(format!(
                "  docs only  {path}  (-{removed} +{added} doc attrs)\n"
            )),
            Self::Judged(Verdict::CodeChanged { at, before, after }) => {
                let (before, after) = (describe(before.as_ref()), describe(after.as_ref()));
                let first = format!("first divergence at token {at}: {before} -> {after}");
                Some(format!("  CODE       {path}\n             {first}\n"))
            }
            Self::Skipped(_) => None,
        }
    }
}

/// `chock sweep`: each file against its text at `rev`. The report, and whether any file's code
/// changed. With no path, every Rust file that differs from `rev`.
pub fn sweep(root: &Path, rev: &str, paths: &[&str]) -> Result<(String, bool), String> {
    let files: Vec<String> = match paths {
        [] => vcs::rust_changed_since(root, rev)?,
        named => named.iter().map(ToString::to_string).collect(),
    };
    let mut rows = Vec::new();
    for path in files {
        let before = vcs::text_at(root, rev, &path)?;
        let after = std::fs::read_to_string(root.join(&path));
        rows.push((path, Row::of(rev, before, after)));
    }
    Ok(swept(rev, &rows))
}

fn swept(rev: &str, rows: &[(String, Row)]) -> (String, bool) {
    if rows.is_empty() {
        let nothing = format!("no .rs files differ from {rev} — nothing to verify\n");
        return (nothing, false);
    }
    let count = |is: fn(&Row) -> bool| rows.iter().filter(|(_, row)| is(row)).count();
    let clean = count(|row| matches!(row, Row::Judged(Verdict::Clean)));
    let docs = count(|row| matches!(row, Row::Judged(Verdict::DocsOnly { .. })));
    let code = count(|row| matches!(row, Row::Judged(Verdict::CodeChanged { .. })));
    let mut text: String = rows
        .iter()
        .filter_map(|(path, row)| row.line(path))
        .collect();
    text += &format!("\n{clean} clean, {docs} docs-only, {code} with code changes\n");
    for (path, row) in rows {
        if let Row::Skipped(why) = row {
            text += &format!("  skipped: {path} ({why})\n");
        }
    }
    text += &match code {
        0 => "\nNo code moved. A comment sweep over these files is proven, not asserted.\n"
            .to_string(),
        files => format!("\nCode changed in {files} file(s) — a comment sweep must not.\n"),
    };
    (text, code > 0)
}

/// `chock moved`: the tokens of the paths now against their tokens at `rev`, pooled. The report,
/// and whether a token was lost.
pub fn moved(root: &Path, rev: &str, paths: &[&str]) -> Result<(String, bool), String> {
    let (mut before, mut after) = (Vec::new(), Vec::new());
    for path in paths {
        // A split makes files the revision lacks, and deletes files it emptied.
        if let Some(text) = vcs::text_at(root, rev, path)? {
            before.extend(tokens::lex(&text).map_err(|e| format!("{path} at {rev} {e}"))?);
        }
        if let Ok(text) = std::fs::read_to_string(root.join(path)) {
            after.extend(tokens::lex(&text).map_err(|e| format!("{path} {e}"))?);
        }
    }
    Ok(pooled(paths.len(), &before, &after))
}

fn pooled(paths: usize, before: &[Tok], after: &[Tok]) -> (String, bool) {
    let (removed, added) = tokens::multiset_delta(before, after);
    let (was, is) = (before.len(), after.len());
    let mut text = format!("pooled over {paths} path(s): {was} tokens before, {is} after\n");
    if removed.is_empty() && added.is_empty() {
        text +=
            "\nIdentical token multiset. The move is pure — not one token was lost or gained.\n";
        return (text, false);
    }
    text += &listed("REMOVED — a pure move loses nothing:", '-', &removed);
    let wiring = "ADDED — expected to be module wiring only (`mod`, `use`, `pub`, paths):";
    text += &listed(wiring, '+', &added);
    let lost = !removed.is_empty();
    text += match lost {
        true => "\nTokens were lost — this is not a pure move.\n",
        false => "\nNothing was lost. Read the ADDED list above and confirm it is only wiring.\n",
    };
    (text, lost)
}

/// One side of a delta under its title, or nothing when that side is empty.
fn listed(title: &str, sign: char, counts: &TokenCounts) -> String {
    if counts.is_empty() {
        return String::new();
    }
    let shown = counts.iter().take(SHOWN);
    let rows = shown.map(|(tok, n)| format!("  {sign}{n:<4} {}\n", describe(Some(tok))));
    let more = match counts.len().saturating_sub(SHOWN) {
        0 => String::new(),
        more => format!("  … and {more} more kinds\n"),
    };
    format!("\n{title}\n{}{more}", rows.collect::<String>())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    /// A repository with one commit that holds `files`.
    fn repo(name: &str, files: &[(&str, &str)]) -> crate::testdir::Scratch {
        let root = crate::testdir::tree(name, files);
        let identity = ["-c", "user.email=t@example.com", "-c", "user.name=t"];
        let commit = [&identity[..], &["commit", "--quiet", "-m", "first"]].concat();
        for args in [&["init", "--quiet"][..], &["add", "."], &commit] {
            let out = crate::exec::run("git", args, &root).unwrap();
            assert!(out.success(), "git {args:?}: {}", out.stderr);
        }
        root
    }

    #[test]
    fn a_command_line_is_the_revision_then_the_paths() {
        assert_eq!(asked("sweep", &["HEAD~1"]), Ok(("HEAD~1", &[][..])));
        assert_eq!(
            asked("sweep", &["main", "a.rs"]),
            Ok(("main", &["a.rs"][..]))
        );
        let both = ["main", "a.rs", "b.rs"];
        assert_eq!(asked("moved", &both), Ok(("main", &["a.rs", "b.rs"][..])));
        let no_rev = |name: &str| Err(format!("{name} needs the revision to compare with"));
        assert_eq!(asked("sweep", &[]), no_rev("sweep"));
        assert_eq!(asked("moved", &[]), no_rev("moved"));
        let no_path = "moved needs each file the code left or reached".to_string();
        assert_eq!(asked("moved", &["main"]), Err(no_path));
        let flag = Err("unknown option `--all`".to_string());
        assert_eq!(asked("sweep", &["main", "--all"]), flag);
    }

    #[test]
    fn a_file_is_judged_by_its_tokens_or_skipped_with_the_reason() {
        let text = |s: &str| Some(s.to_string());
        let now = |s: &str| Ok(s.to_string());
        let code = "fn f() -> u8 { 1 }\n";
        let clean = Row::of("main", text(code), now("// why\nfn f() -> u8 { 1 }\n"));
        assert_eq!(clean, Row::Judged(Verdict::Clean));
        let docs = Row::of("main", text(code), now("/// One.\nfn f() -> u8 { 1 }\n"));
        let docs_only = Verdict::DocsOnly {
            removed: 0,
            added: 1,
        };
        assert_eq!(docs, Row::Judged(docs_only));
        let changed = Verdict::CodeChanged {
            at: 8,
            before: Some(Tok::Literal("1".to_string())),
            after: Some(Tok::Literal("2".to_string())),
        };
        let other = Row::of("main", text(code), now("fn f() -> u8 { 2 }\n"));
        assert_eq!(other, Row::Judged(changed));
        let new = Row::of("main", None, now(code));
        assert_eq!(
            new,
            Row::Skipped("new since main — use `moved`".to_string())
        );
        let gone = std::io::Error::other("gone");
        let unread = Row::of("main", text(code), Err(gone));
        assert_eq!(unread, Row::Skipped("unreadable: gone".to_string()));
        let broken = Row::Skipped("does not lex".to_string());
        assert_eq!(Row::of("main", text("fn f( {"), now(code)), broken);
        assert_eq!(Row::of("main", text(code), now("fn f( {")), broken);
    }

    #[test]
    fn a_sweep_s_report_names_each_file_counts_each_verdict_and_fails_on_changed_code() {
        let changed = Verdict::CodeChanged {
            at: 7,
            before: Some(Tok::Literal("1".to_string())),
            after: None,
        };
        let docs = Verdict::DocsOnly {
            removed: 2,
            added: 1,
        };
        let row = |path: &str, row| (path.to_string(), row);
        let rows = [
            row("src/a.rs", Row::Judged(Verdict::Clean)),
            row("src/b.rs", Row::Judged(docs)),
            row("src/new.rs", Row::Skipped("does not lex".to_string())),
            row("src/c.rs", Row::Judged(changed)),
            row("src/d.rs", Row::Judged(Verdict::Clean)),
        ];
        let expected = "  clean      src/a.rs
  docs only  src/b.rs  (-2 +1 doc attrs)
  CODE       src/c.rs
             first divergence at token 7: literal 1 -> <end of file>
  clean      src/d.rs

2 clean, 1 docs-only, 1 with code changes
  skipped: src/new.rs (does not lex)

Code changed in 1 file(s) — a comment sweep must not.
";
        assert_eq!(swept("main", &rows), (expected.to_string(), true));
        let proven = "  clean      src/a.rs

1 clean, 0 docs-only, 0 with code changes

No code moved. A comment sweep over these files is proven, not asserted.
";
        assert_eq!(swept("main", &rows[..1]), (proven.to_string(), false));
        let nothing = "no .rs files differ from main — nothing to verify\n";
        assert_eq!(swept("main", &[]), (nothing.to_string(), false));
    }

    #[test]
    fn a_move_is_pure_when_the_pooled_tokens_are_the_same_and_fails_when_one_is_lost() {
        let lex = |src: &str| tokens::lex(src).unwrap();
        let (one, two) = (lex("fn f() {} fn g() {}"), lex("fn g() {} fn f() {}"));
        let pure = "pooled over 2 path(s): 12 tokens before, 12 after

Identical token multiset. The move is pure — not one token was lost or gained.
";
        assert_eq!(pooled(2, &one, &two), (pure.to_string(), false));
        let wired = "pooled over 3 path(s): 12 tokens before, 15 after

ADDED — expected to be module wiring only (`mod`, `use`, `pub`, paths):
  +1    ident `g`
  +1    ident `mod`
  +1    punct `;`

Nothing was lost. Read the ADDED list above and confirm it is only wiring.
";
        let with_mod = lex("mod g; fn g() {} fn f() {}");
        assert_eq!(pooled(3, &one, &with_mod), (wired.to_string(), false));
        let lost = "pooled over 1 path(s): 15 tokens before, 14 after

REMOVED — a pure move loses nothing:
  -2    ident `g`
  -1    ident `mod`

ADDED — expected to be module wiring only (`mod`, `use`, `pub`, paths):
  +2    ident `h`

Tokens were lost — this is not a pure move.
";
        let renamed = lex("h; fn h() {} fn f() {}");
        assert_eq!(pooled(1, &with_mod, &renamed), (lost.to_string(), true));
    }

    #[test]
    fn a_long_side_of_a_delta_lists_its_first_forty_kinds_and_counts_the_rest() {
        let kinds = |n: usize| -> TokenCounts {
            let name = |i: usize| Tok::Ident(format!("n{i:02}"));
            (0..n).map(|i| (name(i), i + 1)).collect()
        };
        assert_eq!(listed("Lost:", '-', &kinds(0)), "");
        assert_eq!(
            listed("Lost:", '-', &kinds(1)),
            "\nLost:\n  -1    ident `n00`\n"
        );
        let forty = listed("Lost:", '-', &kinds(40));
        assert!(forty.ends_with("  -40   ident `n39`\n"), "{forty}");
        let more = listed("Lost:", '-', &kinds(42));
        let tail = "  -40   ident `n39`\n  … and 2 more kinds\n";
        assert!(more.ends_with(tail), "{more}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_sweep_reads_each_changed_file_at_the_revision_and_now() {
        let files = [
            ("src/a.rs", "fn a() -> u8 { 1 }\n"),
            ("src/b.rs", "fn b() -> u8 { 1 }\n"),
            ("src/c.rs", "fn c() -> u8 { 1 }\n"),
        ];
        let root = repo("sweep-reads", &files);
        std::fs::write(root.join("src/a.rs"), "// why\nfn a() -> u8 { 1 }\n").unwrap();
        std::fs::write(root.join("src/b.rs"), "fn b() -> u8 { 2 }\n").unwrap();
        std::fs::write(root.join("src/new.rs"), "fn n() {}\n").unwrap();
        let (report, tripped) = sweep(&root, "HEAD", &[]).unwrap();
        let expected = "  clean      src/a.rs
  CODE       src/b.rs
             first divergence at token 8: literal 1 -> literal 2

1 clean, 0 docs-only, 1 with code changes

Code changed in 1 file(s) — a comment sweep must not.
";
        assert_eq!((report.as_str(), tripped), (expected, true));
        let named = sweep(&root, "HEAD", &["src/a.rs", "src/new.rs", "src/c.rs"]).unwrap();
        let expected = "  clean      src/a.rs
  clean      src/c.rs

2 clean, 0 docs-only, 0 with code changes
  skipped: src/new.rs (new since HEAD — use `moved`)

No code moved. A comment sweep over these files is proven, not asserted.
";
        assert_eq!(named, (expected.to_string(), false));
        let err = sweep(&root, "no-such-rev", &[]).unwrap_err();
        assert!(
            err.starts_with("git could not compare with `no-such-rev`"),
            "{err}"
        );
        let err = sweep(&root, "no-such-rev", &["src/a.rs"]).unwrap_err();
        assert!(
            err.starts_with("git could not read `src/a.rs` at `no-such-rev`"),
            "{err}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_move_pools_the_files_a_split_made_and_the_files_it_deleted() {
        let files = [
            ("src/a.rs", "fn a() {}\nfn b() {}\n"),
            ("src/old.rs", "fn c() {}\n"),
        ];
        let root = repo("moved-reads", &files);
        std::fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(root.join("src/b.rs"), "fn c() {}\nfn b() {}\n").unwrap();
        std::fs::remove_file(root.join("src/old.rs")).unwrap();
        let paths = ["src/a.rs", "src/b.rs", "src/old.rs"];
        let pure = "pooled over 3 path(s): 18 tokens before, 18 after

Identical token multiset. The move is pure — not one token was lost or gained.
";
        assert_eq!(moved(&root, "HEAD", &paths), Ok((pure.to_string(), false)));
        let (report, lost) = moved(&root, "HEAD", &paths[..1]).unwrap();
        assert!(lost, "{report}");
        assert!(report.contains("\n  -1    ident `b`\n"), "{report}");
        std::fs::write(root.join("src/b.rs"), "fn c( {").unwrap();
        let err = moved(&root, "HEAD", &paths).unwrap_err();
        assert!(err.starts_with("src/b.rs does not lex: "), "{err}");
        let err = moved(&root, "no-such-rev", &paths).unwrap_err();
        assert!(
            err.starts_with("git could not read `src/a.rs` at `no-such-rev`"),
            "{err}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_file_that_did_not_lex_at_the_revision_stops_a_move_and_names_the_revision() {
        let root = repo("moved-broken", &[("src/a.rs", "fn a( {\n")]);
        let err = moved(&root, "HEAD", &["src/a.rs"]).unwrap_err();
        assert!(err.starts_with("src/a.rs at HEAD does not lex: "), "{err}");
    }
}
