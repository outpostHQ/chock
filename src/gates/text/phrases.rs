//! Phrases a project lists under `forbidden`, each with a reason, found in its source in any case.

use crate::project::config::Forbidden;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Inspection, Kind};

pub const GATE: Gate = Gate {
    name: "phrases",
    about: "a phrase the project lists under `forbidden` appears in its source",
    group: Group::OptIn,
    builds: false,
    reads: None,
    kind: Kind::Debt {
        inspect,
        unit: "forbidden phrase(s)",
    },
};

fn inspect(ctx: &Ctx) -> Result<Inspection, String> {
    if ctx.forbidden.is_empty() {
        return Err(format!(
            "no phrase is listed under `forbidden` in {}",
            crate::project::config::FILE
        ));
    }
    if ctx
        .forbidden
        .iter()
        .any(|phrase| phrase.text.trim().is_empty())
    {
        return Err("a `forbidden` entry has no text, which would match every line".to_string());
    }
    let files = crate::project::walked(
        &ctx.listing,
        &ctx.root,
        &|name| !crate::project::SKIPPED.contains(&name),
        &|_, path| crate::slop::markers_for(&path.to_string_lossy()).is_some(),
    )?;
    let mut debt = Vec::new();
    for path in files {
        let shown = crate::project::relative(&ctx.root, &path);
        match std::fs::read_to_string(&path) {
            Ok(src) => debt.extend(found(&shown, &src, &applying(&ctx.forbidden, &shown))),
            // A file that is not UTF-8 holds no phrase.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {}
            Err(e) => return Err(format!("{shown}: {e}")),
        }
    }
    Ok(Inspection {
        debt,
        blockers: Vec::new(),
    })
}

/// Every listed phrase on every line; an entry with no text matches nothing. The editor hook uses
/// it too, so an edit and a commit refuse the same lines.
#[must_use]
pub fn found(path: &str, src: &str, forbidden: &[Forbidden]) -> Vec<Finding> {
    let mut hits = Vec::new();
    for (index, line) in src.lines().enumerate() {
        let lower = line.to_lowercase();
        for phrase in forbidden
            .iter()
            .filter(|phrase| !phrase.text.trim().is_empty())
        {
            if mentions(&lower, &phrase.text.to_lowercase(), phrase.word) {
                let at = u32::try_from(index + 1).unwrap_or(u32::MAX);
                hits.push(Finding::at(path, &phrase.why).item(&phrase.text).line(at));
            }
        }
    }
    hits
}

/// As a word, a phrase counts only where no letter, digit, `_` or `-` touches it, and no `.` comes
/// before it: `.dvc` is a file name, while `dvc.` ends a sentence.
fn mentions(line: &str, phrase: &str, word: bool) -> bool {
    let joins = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    line.match_indices(phrase).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + phrase.len()..].chars().next();
        !word || !(before.is_some_and(|c| joins(c) || c == '.') || after.is_some_and(joins))
    })
}

/// The entries that apply at a path from the project root, without those whose `except` names it.
#[must_use]
pub fn applying(forbidden: &[Forbidden], path: &str) -> Vec<Forbidden> {
    forbidden
        .iter()
        .filter(|phrase| !phrase.except.iter().any(|allowed| globbed(allowed, path)))
        .cloned()
        .collect()
}

fn globbed(pattern: &str, path: &str) -> bool {
    let parts = |text: &str| -> Vec<String> {
        text.split('/')
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect()
    };
    segments(&parts(pattern), &parts(path))
}

/// `**` stands for any number of whole segments, including none.
fn segments(pattern: &[String], path: &[String]) -> bool {
    let Some((first, rest)) = pattern.split_first() else {
        return path.is_empty();
    };
    if first == "**" {
        return (0..=path.len()).any(|skip| segments(rest, &path[skip..]));
    }
    path.split_first()
        .is_some_and(|(name, left)| segment(first, name) && segments(rest, left))
}

/// Within one segment, `*` stands for any run of characters.
fn segment(pattern: &str, name: &str) -> bool {
    let Some((head, tail)) = pattern.split_once('*') else {
        return pattern == name;
    };
    name.strip_prefix(head).is_some_and(|rest| {
        rest.char_indices()
            .map(|(at, _)| at)
            .chain([rest.len()])
            .any(|at| segment(tail, &rest[at..]))
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn refused(text: &str, why: &str) -> Forbidden {
        Forbidden {
            text: text.to_string(),
            why: why.to_string(),
            ..Forbidden::default()
        }
    }

    const SHIM_FOUND: &str = "src/lib.rs:1: legacy shim: delete it or name what it does";

    /// A new context on each call, because a context caches its file listing.
    fn shimmed(dir: &std::path::Path) -> Ctx {
        let shim = Forbidden {
            except: vec!["plans/**".to_string()],
            ..refused("legacy shim", "delete it or name what it does")
        };
        Ctx {
            forbidden: vec![shim],
            ..Ctx::for_root(
                dir.to_path_buf(),
                crate::run::baseline::Baseline::empty("0.1.0"),
            )
        }
    }

    #[test]
    fn a_word_counts_only_where_nothing_joins_it() {
        let rival = [Forbidden {
            word: true,
            ..refused("dvc", "describe the behaviour")
        }];
        let src = "dvc here\nuses DVC.\n(dvc)\nadvc\ndvcs\ndvc-core\nx-dvc\ndvc_x\nx_dvc\n\
                   file.dvc\ndvc2\n advc then dvc\n";
        assert_eq!(
            found("src/lib.rs", src, &rival)
                .iter()
                .map(|finding| finding.line)
                .collect::<Vec<_>>(),
            [Some(1), Some(2), Some(3), Some(12)]
        );
        assert_eq!(
            found("src/lib.rs", "// shims\n", &[refused("shim", "no")])
                .iter()
                .map(Finding::render)
                .collect::<Vec<_>>(),
            ["src/lib.rs:1: shim: no"]
        );
    }

    #[test]
    fn an_exception_is_a_path_from_the_root_with_one_and_many_segment_wildcards() {
        for (pattern, path, holds) in [
            ("src/lib.rs", "src/lib.rs", true),
            ("src/lib.rs", "src/lib.rsx", false),
            ("plans/**", "plans/083.md", true),
            ("plans/**", "plans", true),
            ("plans/**", "docs/plans/083.md", false),
            ("**/vcs/**", "crates/core/src/vcs/hg.rs", true),
            ("**/vcs/**", "vcs/hg.rs", true),
            ("**/vcs/**", "crates/core/src/vcsx/hg.rs", false),
            ("src/*", "src/x", true),
            ("src/*.toml", "src/a.toml", true),
            ("src/*.toml", "src/a/b.toml", false),
            ("src/*.rs", "src/é.rs", true),
            ("src/a*b.rs", "src/ab.rs", true),
            ("src/a*b.rs", "src/xab.rs", false),
        ] {
            assert_eq!(globbed(pattern, path), holds, "{pattern} against {path}");
        }
        let listed = [
            Forbidden {
                except: vec!["**/vcs/**".to_string()],
                ..refused("mercurial", "describe the behaviour")
            },
            refused("§", "cite the file"),
        ];
        assert_eq!(
            applying(&listed, "src/vcs/hg.rs"),
            [refused("§", "cite the file")]
        );
        assert_eq!(applying(&listed, "src/lib.rs"), listed);
    }

    fn debt_in(dir: &std::path::Path) -> Vec<String> {
        let found = inspect(&shimmed(dir)).unwrap();
        found.debt.iter().map(Finding::render).collect()
    }

    #[test]
    fn a_listed_phrase_is_found_on_its_line_whatever_its_case() {
        let listed = [
            refused("for compatibility", "say what it does instead"),
            refused("§", "cite the file, not a plan section"),
        ];
        let src = "fn f() {}\n// Kept For Compatibility with v1\n// see §3.2\n";
        assert_eq!(
            found("src/lib.rs", src, &listed)
                .iter()
                .map(Finding::render)
                .collect::<Vec<_>>(),
            [
                "src/lib.rs:2: for compatibility: say what it does instead",
                "src/lib.rs:3: §: cite the file, not a plan section",
            ]
        );
        assert_eq!(found("src/lib.rs", src, &[refused("  ", "no")]), Vec::new());
    }

    #[test]
    fn the_gate_walks_every_source_file_and_refuses_with_nothing_listed() {
        let dir = crate::testdir::make("phrases-tree");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "// a legacy shim\n").unwrap();
        std::fs::write(dir.join("notes.toml"), "# nothing to see\n").unwrap();
        std::fs::create_dir_all(dir.join("plans")).unwrap();
        std::fs::write(dir.join("plans/shim.toml"), "# the legacy shim, planned\n").unwrap();
        assert_eq!(debt_in(&dir), [SHIM_FOUND]);
        let absent = dir.join("absent");
        assert_eq!(
            inspect(&shimmed(&absent)).unwrap_err(),
            format!(
                "cannot read {}: {}",
                absent.display(),
                std::fs::read_dir(&absent).unwrap_err()
            )
        );
        let none = Ctx::for_root(
            dir.to_path_buf(),
            crate::run::baseline::Baseline::empty("0.1.0"),
        );
        assert_eq!(
            inspect(&none).unwrap_err(),
            "no phrase is listed under `forbidden` in .chock/config.json"
        );
        let blank = Ctx {
            forbidden: vec![refused("", "nothing")],
            ..Ctx::for_root(
                dir.to_path_buf(),
                crate::run::baseline::Baseline::empty("0.1.0"),
            )
        };
        assert_eq!(
            inspect(&blank).unwrap_err(),
            "a `forbidden` entry has no text, which would match every line"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bytes_that_are_not_text_are_passed_over_and_a_file_that_cannot_be_read_refuses() {
        let dir = crate::testdir::make("phrases-unreadable");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "// a legacy shim\n").unwrap();
        std::fs::write(dir.join("src/bytes.rs"), [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(debt_in(&dir), [SHIM_FOUND]);
        std::os::unix::fs::symlink(dir.join("nowhere"), dir.join("src/gone.rs")).unwrap();
        assert_eq!(
            inspect(&shimmed(&dir)).unwrap_err(),
            "src/gone.rs: No such file or directory (os error 2)"
        );
    }
}
