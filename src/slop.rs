//! The `slop` gate: finds comment blocks longer than `MAX_BLOCK` lines.

use std::fs;
use std::path::{Path, PathBuf};

use crate::run::report::{Finding, GateReport, Run, Verdict};

/// The longest run of same-marker comment lines allowed. Longer reasoning belongs in a document.
pub const MAX_BLOCK: usize = 2;

const SKIP_DIRS: [&str; 11] = [
    "dist",
    "build",
    ".venv",
    "venv",
    "corpus",
    ".next",
    ".svelte-kit",
    ".turbo",
    "out",
    ".output",
    "coverage",
];

/// Whether the scan skips a directory, from either skip list.
pub(crate) fn skipped(name: &str) -> bool {
    SKIP_DIRS.contains(&name) || crate::project::SKIPPED.contains(&name)
}

/// A block over the limit: its first line and its length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub file: PathBuf,
    pub line: usize,
    pub length: usize,
}

/// Comment markers for a path, longest first so `///` is not read as `//`.
#[must_use]
pub fn markers_for(path: &str) -> Option<&'static [&'static str]> {
    const RUST: &[&str] = &["///", "//!", "//"];
    const SLASH: &[&str] = &["///", "//"];
    const SLASH2: &[&str] = &["//"];
    const HASH: &[&str] = &["#"];
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == "justfile" || name == "Justfile" || name.ends_with(".just") {
        return Some(HASH);
    }
    match name.rsplit_once('.')?.1 {
        "rs" => Some(RUST),
        "ts" | "tsx" => Some(SLASH),
        "js" | "jsx" => Some(SLASH2),
        "toml" | "py" | "yml" | "yaml" | "sh" => Some(HASH),
        _ => None,
    }
}

/// The comment near the top of a file that exempts it.
const MARKER: &str = "slop-ok-file:";

/// Whether a file claims the exemption: a line in its first 2000 bytes opens with `MARKER`. For
/// reference configs whose commented-out lines are their content.
#[must_use]
pub fn is_exempt(text: &str) -> bool {
    let head = text.get(..text.len().min(2000)).unwrap_or(text);
    head.lines().any(|line| {
        line.trim_start_matches(|c: char| !c.is_alphanumeric())
            .starts_with(MARKER)
    })
}

/// The marker a line opens with. A compiletest directive (`//@`) or a shebang is not a comment.
fn marker<'a>(line: &str, marks: &[&'a str]) -> Option<&'a str> {
    if line.starts_with("//@") || line.starts_with("#!") {
        return None;
    }
    marks.iter().find(|m| line.starts_with(**m)).copied()
}

/// Every block over the limit, as (first line, length). A block is a run of one marker, so a `///`
/// line after `//` lines starts a new one.
#[must_use]
pub fn over_length_blocks(text: &str, marks: &[&str]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut run, mut run_mark, mut start) = (0usize, None::<&str>, 0usize);
    let mut in_script_metadata = false;
    for (i, line) in text.split('\n').enumerate() {
        let n = i + 1;
        let stripped = line.trim_start();
        // PEP 723 metadata: the run of `#` lines must stay contiguous for uv to read it.
        if stripped.starts_with("# ///") {
            in_script_metadata = !in_script_metadata;
            run = 0;
            run_mark = None;
            continue;
        }
        if in_script_metadata {
            continue;
        }
        let mark = marker(stripped, marks);
        if mark.is_some() && mark == run_mark {
            run += 1;
            continue;
        }
        if run > MAX_BLOCK {
            out.push((start, run));
        }
        match mark {
            Some(m) => {
                run = 1;
                run_mark = Some(m);
                start = n;
            }
            None => {
                run = 0;
                run_mark = None;
            }
        }
    }
    if run > MAX_BLOCK {
        out.push((start, run));
    }
    out
}

/// Every over-length block under `root`, sorted. An unreadable file or directory is an error, since
/// skipping it would read as fewer blocks.
pub fn scan(root: &Path) -> Result<Vec<Block>, String> {
    let mut hits = Vec::new();
    for path in crate::project::walk(root, &|name| !skipped(name), &|_, _| true)? {
        hits.extend(blocks_in(&path)?);
    }
    hits.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    Ok(hits)
}

/// One file's over-length blocks; none for an unknown language, a non-text file or an exempt one.
fn blocks_in(path: &Path) -> Result<Vec<Block>, String> {
    let Some(marks) = markers_for(&path.to_string_lossy()) else {
        return Ok(Vec::new());
    };
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    if is_exempt(&text) {
        return Ok(Vec::new());
    }
    Ok(over_length_blocks(&text, marks)
        .into_iter()
        .map(|(line, length)| Block {
            file: path.to_path_buf(),
            line,
            length,
        })
        .collect())
}

#[must_use]
pub fn render(hits: &[Block], root: &Path) -> String {
    if hits.is_empty() {
        return String::new();
    }
    let mut out = format!("{} comment block(s) over {MAX_BLOCK} lines.\n", hits.len());
    for h in hits {
        out.push_str(&format!(
            "{}:{}: comment block of {} lines (limit {MAX_BLOCK})\n",
            crate::project::relative(root, &h.file),
            h.line,
            h.length
        ));
    }
    out
}

/// The findings as a `Run`, the JSON shape every gate reports in.
#[must_use]
pub fn render_json(hits: &[Block], root: &Path, chock_version: &str) -> String {
    let verdict = if hits.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Tripped
    };
    let mut report = GateReport::new("slop", verdict, "chock slop");
    report.measured = Some(hits.len() as u64);
    report.unit = Some("over-long comment block(s)".to_string());
    report.findings = hits
        .iter()
        .map(|hit| {
            let message = format!("comment block of {} lines (limit {MAX_BLOCK})", hit.length);
            let at = Finding::at(&crate::project::relative(root, &hit.file), &message);
            match u32::try_from(hit.line) {
                Ok(line) => at.line(line),
                Err(_) => at,
            }
        })
        .collect();
    Run::new(chock_version, vec![report]).render_json()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    const RUST: &[&str] = &["///", "//!", "//"];
    const SLASH: &[&str] = &["///", "//"];
    const SLASH2: &[&str] = &["//"];
    const HASH: &[&str] = &["#"];

    #[test]
    fn a_directory_in_either_skip_list_is_passed_over() {
        assert!(skipped("vendor"));
        assert!(skipped(".outpost"));
        assert!(skipped("target"));
        assert!(!skipped("src"));
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_will_not_open_stops_the_walk_rather_than_being_skipped() {
        let dir = tree("slop-file-unreadable", &[("a.rs", OVER_LIMIT)]);
        let locked = dir.join("a.rs");
        set_mode(&locked, 0o000);
        let refused = fs::read_to_string(&locked).is_err();
        let found = scan(&dir);
        set_mode(&locked, 0o644);
        assert!(refused, "this proves nothing while the file still opens");
        assert!(
            found.is_err_and(|why| why.contains("a.rs")),
            "an unreadable file has to stop the walk"
        );
    }

    #[test]
    fn a_file_that_is_not_text_holds_no_block_rather_than_ending_the_walk() {
        let dir = tree(
            "slop-not-text",
            &[("bin.rs", b"\xff\xfe\x00// a\n// b\n// c\n")],
        );
        assert_eq!(scan(&dir).unwrap(), Vec::new());
    }

    /// The ratchet keys findings by path, and a path through the link names no file to edit.
    #[cfg(unix)]
    #[test]
    fn a_file_under_a_directory_link_is_reported_once_rather_than_once_per_pass() {
        let dir = crate::testdir::make("slop-symlink-loop");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/one.rs"), "/// a\n/// b\n/// c\nfn f() {}\n").unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("src/back")).unwrap();
        let found: Vec<(PathBuf, usize)> = scan(&dir)
            .unwrap()
            .into_iter()
            .map(|b| (b.file, b.line))
            .collect();
        assert_eq!(found, vec![(dir.join("src/one.rs"), 1)]);
    }

    #[test]
    fn a_block_at_the_limit_is_allowed_and_one_line_longer_is_not() {
        assert_eq!(over_length_blocks("// a\n// b\nfn f() {}", RUST), vec![]);
        assert_eq!(
            over_length_blocks("// a\n// b\n// c\nfn f() {}", RUST),
            vec![(1, 3)]
        );
    }

    #[test]
    fn every_over_length_block_is_reported_not_only_the_worst() {
        let src = "// a\n// b\n// c\nfn f() {}\n// d\n// e\n// f\n// g\n";
        assert_eq!(over_length_blocks(src, RUST), vec![(1, 3), (5, 4)]);
    }

    #[test]
    fn a_block_running_to_the_end_of_the_file_is_still_reported() {
        assert_eq!(over_length_blocks("// a\n// b\n// c", RUST), vec![(1, 3)]);
    }

    #[test]
    fn a_different_marker_starts_a_new_block_rather_than_extending_one() {
        assert_eq!(over_length_blocks("// a\n// b\n/// c\n", RUST), vec![]);
    }

    #[test]
    fn a_doc_comment_is_read_as_itself_and_not_as_a_line_comment() {
        assert_eq!(
            over_length_blocks("/// a\n/// b\n/// c\n", RUST),
            vec![(1, 3)]
        );
    }

    #[test]
    fn an_indented_comment_counts() {
        assert_eq!(
            over_length_blocks("fn f() {\n    // a\n    // b\n    // c\n}", RUST),
            vec![(2, 3)]
        );
    }

    #[test]
    fn a_blank_line_ends_a_block() {
        assert_eq!(
            over_length_blocks("// a\n// b\n\n// c\n// d\n", RUST),
            vec![]
        );
    }

    #[test]
    fn compiletest_directives_are_not_a_comment_block_and_end_one() {
        let directives = "//@ print-mutations\n//@ run: exit 3\n//@ stdout\n//@ stderr: empty\n";
        assert_eq!(over_length_blocks(directives, RUST), vec![]);
        let prose_then_directive = "// a\n// b\n//@ run\n// c\n// d\n";
        assert_eq!(over_length_blocks(prose_then_directive, RUST), vec![]);
        assert_eq!(
            over_length_blocks("//@ run\n// a\n// b\n// c\n", RUST),
            vec![(2, 3)]
        );
        let script = "#!/bin/bash\n# What this does.\n# Run only inside the sandbox.\ncode\n";
        assert_eq!(
            over_length_blocks(script, HASH),
            vec![],
            "a shebang is no comment"
        );
    }

    #[test]
    fn script_metadata_is_skipped_because_its_lines_cannot_be_split() {
        let src = "# /// script\n# a\n# b\n# c\n# ///\ncode\n";
        assert_eq!(over_length_blocks(src, HASH), vec![]);
    }

    #[test]
    fn a_comment_after_script_metadata_is_still_checked() {
        let src = "# /// script\n# a\n# ///\n# x\n# y\n# z\n";
        assert_eq!(over_length_blocks(src, HASH), vec![(4, 3)]);
    }

    #[test]
    fn markers_are_chosen_by_extension_and_justfile_by_name() {
        assert_eq!(markers_for("src/a.rs"), Some(RUST));
        assert_eq!(markers_for("x/justfile"), Some(HASH));
        assert_eq!(markers_for("Cargo.toml"), Some(HASH));
        assert_eq!(markers_for("a.md"), None);
        assert_eq!(markers_for("LICENSE"), None);
    }

    /// TypeScript has doc comments and JavaScript does not, so they do not share a marker list.
    #[test]
    fn typescript_and_javascript_each_get_their_own_markers() {
        assert_eq!(markers_for("a.ts"), Some(SLASH));
        assert_eq!(markers_for("a.tsx"), Some(SLASH));
        assert_eq!(markers_for("a.js"), Some(SLASH2));
        assert_eq!(markers_for("a.jsx"), Some(SLASH2));
    }

    /// The only test that fails if the metadata flag is never set.
    #[test]
    fn an_unclosed_metadata_block_skips_every_line_after_it() {
        let src = "# /// script\n# a\n# b\n# c\ncode\n";
        assert_eq!(over_length_blocks(src, HASH), vec![]);
    }

    #[test]
    fn a_file_marked_exempt_in_its_head_is_skipped() {
        assert!(is_exempt("# slop-ok-file: this is a reference config\n"));
        assert!(!is_exempt("# an ordinary file\n"));
    }

    #[test]
    fn an_exemption_past_the_head_does_not_count() {
        let text = format!("{}\n# slop-ok-file: too late\n", "x".repeat(2100));
        assert!(!is_exempt(&text));
    }

    #[test]
    fn a_clean_tree_renders_nothing_at_all() {
        assert_eq!(render(&[], Path::new("/w")), "");
    }

    #[test]
    fn the_report_names_each_file_relative_to_the_root_with_its_line() {
        let hits = vec![
            Block {
                file: PathBuf::from("/w/src/a.rs"),
                line: 4,
                length: 5,
            },
            Block {
                file: PathBuf::from("/w/justfile"),
                line: 1,
                length: 3,
            },
        ];
        assert_eq!(
            render(&hits, Path::new("/w")),
            "2 comment block(s) over 2 lines.\n\
             src/a.rs:4: comment block of 5 lines (limit 2)\n\
             justfile:1: comment block of 3 lines (limit 2)\n"
        );
    }

    #[test]
    fn a_block_at_the_limit_that_ends_the_file_is_allowed_too() {
        assert_eq!(over_length_blocks("// a\n// b", RUST), vec![]);
    }

    #[test]
    fn an_exemption_counts_only_while_its_last_byte_is_inside_the_head() {
        const MARK: &str = "// slop-ok-file:";
        let ending_at = |byte: usize| format!("{}{MARK}", "\n".repeat(byte - MARK.len()));
        assert!(is_exempt(&ending_at(2000)));
        assert!(!is_exempt(&ending_at(2001)));
    }

    /// This file holds the marker as a constant and must not exempt itself.
    #[test]
    fn a_file_that_only_mentions_the_marker_does_not_claim_the_exemption() {
        assert!(!is_exempt("const MARKER: &str = \"slop-ok-file:\";\n"));
        assert!(!is_exempt("let ok = head.contains(\"slop-ok-file:\");\n"));
        assert!(is_exempt("// slop-ok-file: a reference config\n"));
        assert!(is_exempt("# slop-ok-file: a template\n"));
    }

    const OVER_LIMIT: &[u8] = b"// a\n// b\n// c\n";

    /// Fixtures live under `target/`, which the walk skips, so chock never scans its own tests.
    fn tree(name: &str, files: &[(&str, &[u8])]) -> crate::testdir::Scratch {
        let dir = crate::testdir::make(name);
        for (path, bytes) in files {
            let at = dir.join(path);
            fs::create_dir_all(at.parent().unwrap_or(&dir)).unwrap();
            fs::write(&at, bytes).unwrap();
        }
        dir
    }

    /// Each entry before `e_real.rs` is skipped for a different reason.
    #[test]
    fn a_skipped_candidate_does_not_end_the_walk_over_the_files_after_it() {
        let dir = tree(
            "slop-scan-skips",
            &[
                ("a_dir/.keep", b""),
                ("b_notes.md", OVER_LIMIT),
                ("c_binary.rs", &[0xff, 0xfe]),
                (
                    "d_template.rs",
                    b"// slop-ok-file: a reference\n// a\n// b\n// c\n",
                ),
                ("e_real.rs", OVER_LIMIT),
            ],
        );
        assert_eq!(
            scan(&dir).unwrap(),
            vec![Block {
                file: dir.join("e_real.rs"),
                line: 1,
                length: 3,
            }]
        );
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_that_will_not_open_stops_the_walk_rather_than_being_skipped() {
        let dir = tree("slop-scan-unreadable", &[("a_good/over.rs", OVER_LIMIT)]);
        let locked = dir.join("z_locked");
        fs::create_dir_all(&locked).unwrap();
        set_mode(&locked, 0o000);
        let refused = fs::read_dir(&locked).is_err();
        let found = scan(&dir);
        set_mode(&locked, 0o755);
        assert!(
            refused,
            "this proves nothing while the directory still opens"
        );
        assert!(
            found.is_err_and(|why| why.starts_with("cannot read") && why.contains("z_locked")),
            "an unreadable listing has to stop the walk"
        );
    }
}
