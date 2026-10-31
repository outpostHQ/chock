//! The mutant patches of the `verus` and `proof` gates: a unified diff that puts a fault in one
//! file, with line 1 `--- <the file>` and line 2 `+++ <what must fail>`, applied in memory.

use std::path::{Component, Path, PathBuf};

use crate::project;

/// The `.patch` files of the mutants directory, in name order; none where it does not exist.
pub(crate) fn patches(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = read
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|it| it == "patch") && path.is_file())
        .collect();
    found.sort();
    found
}

/// A mutant read and applied in memory, before chock copies anything.
#[derive(Debug)]
pub(crate) struct Mutant {
    /// What must fail, from line 2: a function for Verus, a harness for Kani.
    pub(crate) name: String,
    /// The file line 1 names.
    pub(crate) target: PathBuf,
    /// The text of `target` with the patch applied.
    pub(crate) patched: String,
}

/// The mutant at `patch`, whose line 1 is a path from `base`; `names` refuses one that names too
/// little. `Err` says why it cannot run, and is the patch's fault.
pub(crate) fn mutant(
    root: &Path,
    base: &Path,
    patch: &Path,
    names: &str,
) -> Result<Mutant, String> {
    let diff = std::fs::read_to_string(patch).map_err(|e| format!("chock cannot read it: {e}"))?;
    let (file, name) = named(&diff, names)?;
    let target = normalized(&base.join(file));
    if !target.starts_with(root) {
        return Err(format!("patches {file}, which is outside the project"));
    }
    let shown = project::relative(root, &target);
    let text = std::fs::read_to_string(&target)
        .map_err(|e| format!("patches {shown}, which chock cannot read: {e}"))?;
    let patched =
        patched(&text, &diff).map_err(|why| format!("{why} in {shown}; make the mutant again"))?;
    Ok(Mutant {
        name: name.to_string(),
        target,
        patched,
    })
}

/// The file a mutant patches, from line 1 `--- path`, and what must fail, from `+++ name`.
fn named<'a>(diff: &'a str, names: &str) -> Result<(&'a str, &'a str), String> {
    let mut lines = diff.lines();
    let file = lines.next().and_then(|line| line.strip_prefix("--- "));
    let file = file.and_then(|rest| rest.split('\t').next()).map(str::trim);
    let name = lines
        .next()
        .and_then(|line| line.strip_prefix("+++ "))
        .map(str::trim);
    match (file, name) {
        (Some(file), Some(name))
            if !file.is_empty() && !name.is_empty() && !name.contains(['/', ' ']) =>
        {
            Ok((file, name))
        }
        _ => Err(names.into()),
    }
}

/// `path` with each `.` and `..` resolved by name, as line 1 of a patch means it.
pub(crate) fn normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// A copy of the directory `from` at `to`, without what a build or a version control keeps.
pub(crate) fn copied(from: &Path, to: &Path) -> std::io::Result<()> {
    super::made(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let skipped = project::SKIPPED.iter().any(|skipped| name == *skipped);
        // `copy` follows a link, so a linked module is copied as the file it points to. The copy
        // never descends into a linked directory, which could loop.
        match skipped {
            true => {}
            false if project::descends_into(&entry.path()) => {
                copied(&entry.path(), &to.join(&name))?;
            }
            false => {
                std::fs::copy(entry.path(), to.join(&name))?;
            }
        }
    }
    Ok(())
}

/// One hunk of a unified diff: the line its old text starts at, counted from 0, and both texts.
struct Hunk<'a> {
    start: usize,
    old: Vec<&'a str>,
    new: Vec<&'a str>,
}

fn hunks(diff: &str) -> Result<Vec<Hunk<'_>>, String> {
    let mut hunks: Vec<Hunk<'_>> = Vec::new();
    for line in diff.lines().skip(2) {
        if let Some(header) = line.strip_prefix("@@ -") {
            let start = header
                .split([',', ' '])
                .next()
                .and_then(|n| n.parse::<usize>().ok());
            let start = start.ok_or_else(|| format!("`{line}` is no hunk header"))?;
            let (old, new) = (Vec::new(), Vec::new());
            hunks.push(Hunk {
                start: start.saturating_sub(1),
                old,
                new,
            });
            continue;
        }
        let Some(hunk) = hunks.last_mut() else {
            continue;
        };
        let rest = line.get(1..).unwrap_or_default();
        match line.chars().next() {
            // Some editors strip the one space of an empty context line.
            None | Some(' ') => {
                hunk.old.push(rest);
                hunk.new.push(rest);
            }
            Some('-') => hunk.old.push(rest),
            Some('+') => hunk.new.push(rest),
            Some('\\') => {}
            Some(_) => return Err(format!("`{line}` is no line of a unified diff")),
        }
    }
    if hunks.is_empty() {
        Err("the patch holds no hunk".into())
    } else {
        Ok(hunks)
    }
}

/// `text` with each hunk applied after the one before: at its line, moved as far as the hunk before
/// moved, or else at the one place its old lines match; `Err` names the first hunk that cannot.
pub(crate) fn patched(text: &str, diff: &str) -> Result<String, String> {
    let mut lines: Vec<&str> = text.lines().collect();
    let mut from = 0;
    let mut shift = 0_isize;
    for hunk in hunks(diff)? {
        let wanted = hunk.start.saturating_add_signed(shift);
        let at = found(&lines, &hunk.old, wanted, from)
            .map_err(|why| format!("the hunk at line {} {why}", hunk.start + 1))?;
        let (old, new) = (hunk.old.len(), hunk.new.len());
        lines.splice(at..at + old, hunk.new);
        shift = at.cast_signed() - hunk.start.cast_signed() + new.cast_signed() - old.cast_signed();
        from = at + new;
    }
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Where `old` starts in `lines`, at or after `from`: at `wanted`, or else at the one place it
/// matches, since a hunk that matches at two places may put the fault in the wrong one.
fn found(lines: &[&str], old: &[&str], wanted: usize, from: usize) -> Result<usize, String> {
    let fits = |at: &usize| *at >= from && lines.get(*at..*at + old.len()) == Some(old);
    if fits(&wanted) {
        return Ok(wanted);
    }
    let mut places = (from..=lines.len()).filter(fits);
    match (places.next(), places.next()) {
        (Some(at), None) => Ok(at),
        (None, _) => Err("no longer matches".into()),
        (Some(one), Some(two)) => Err(format!(
            "moved to more than one place, such as lines {} and {}",
            one + 1,
            two + 1
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str =
        "--- ../../crates/k/src/cas.rs\n+++ cas::put\n@@ -2,3 +2,3 @@\n a\n-b\n+B\n c\n";
    const NAMES: &str = "name the file and what must fail";

    #[test]
    fn a_hunk_applies_at_its_line_or_where_it_moved_and_one_that_no_longer_matches_is_refused() {
        assert_eq!(patched("0\na\nb\nc\n", DIFF).unwrap(), "0\na\nB\nc\n");
        assert_eq!(
            patched("x\ny\n0\na\nb\nc\nd", DIFF).unwrap(),
            "x\ny\n0\na\nB\nc\nd"
        );
        let gone = patched("0\na\nB\nc\n", DIFF).unwrap_err();
        assert_eq!(gone, "the hunk at line 2 no longer matches");
        let two = "--- f\n+++ g\n@@ -1,2 +1,1 @@\n a\n-b\n@@ -4,1 +3,2 @@\n d\n+e\n";
        assert_eq!(patched("a\nb\nc\nd\n", two).unwrap(), "a\nc\nd\ne\n");
        // The second hunk's line is moved by the lines the first added, past an earlier copy.
        let grown = "--- f\n+++ g\n@@ -1 +1,3 @@\n-a\n+a\n+b\n+c\n@@ -3 +5 @@\n-m\n+z\n";
        assert_eq!(
            patched("a\nm\nm\nm\n", grown).unwrap(),
            "a\nb\nc\nm\nz\nm\n"
        );
        // A hunk never applies before or inside the lines that the hunk before it wrote.
        let back = "--- f\n+++ g\n@@ -2 +2 @@\n-q\n+r\n@@ -1 +1 @@\n-r\n+x\n";
        assert_eq!(patched("r\nq\nr\n", back).unwrap(), "r\nr\nx\n");
        // A hunk past the end of the text applies at the one place it matches.
        let far = "--- f\n+++ g\n@@ -6 +6 @@\n-a\n+b\n";
        assert_eq!(patched("a\n", far).unwrap(), "b\n");
        // A hunk that moved where its lines are twice is refused, but one at its line is not.
        let moved = patched("x\nm\nx\nm\n", "--- f\n+++ g\n@@ -3 +3 @@\n-m\n+z\n").unwrap_err();
        assert_eq!(
            moved,
            "the hunk at line 3 moved to more than one place, such as lines 2 and 4"
        );
        let stayed = "--- f\n+++ g\n@@ -2 +2 @@\n-m\n+z\n";
        assert_eq!(patched("m\nm\n", stayed).unwrap(), "m\nz\n");
        // The second hunk moves as far as the first, so it skips a copy of its lines nearer its line.
        let pair = "--- f\n+++ g\n@@ -1 +1 @@\n-a\n+A\n@@ -3 +3 @@\n-m\n+z\n";
        assert_eq!(patched("x\nx\na\nm\nm\n", pair).unwrap(), "x\nx\nA\nm\nz\n");
        // A hunk that only adds lines fits at the end of the text, after the hunk before it.
        assert_eq!(found(&["a"], &[], 5, 1), Ok(1));
        assert_eq!(
            patched("a\n", "--- f\n+++ g\n").unwrap_err(),
            "the patch holds no hunk"
        );
        let noted =
            "--- f\n+++ g\nmade by hand\n@@ -1 +1 @@\n-a\n+b\n\\ No newline at end of file\n";
        assert_eq!(patched("a", noted).unwrap(), "b");
        let odd = hunks("--- f\n+++ g\n@@ -1 +1 @@\n*a\n").err().unwrap();
        assert_eq!(odd, "`*a` is no line of a unified diff");
        assert_eq!(normalized(Path::new("./a/../b")), PathBuf::from("b"));
    }

    #[test]
    fn lines_1_and_2_name_the_file_and_what_must_fail_and_a_git_header_is_refused() {
        assert_eq!(
            named(DIFF, NAMES).unwrap(),
            ("../../crates/k/src/cas.rs", "cas::put")
        );
        assert_eq!(
            named("--- law3.rs\t2026-10-01\n+++ merge\n", NAMES).unwrap(),
            ("law3.rs", "merge")
        );
        let git = named("--- a/law3.rs\n+++ b/law3.rs\n", NAMES).unwrap_err();
        assert_eq!(git, NAMES);
        assert_eq!(named("+++ merge\n", NAMES).unwrap_err(), NAMES);
        for unnamed in ["--- \n+++ put\n", "--- f\n+++ \n", "--- f\n+++ a b\n"] {
            assert_eq!(named(unnamed, NAMES).unwrap_err(), NAMES, "{unnamed}");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot copy a file")]
    fn a_copy_leaves_out_what_a_build_or_a_version_control_keeps() {
        let files = [
            ("from/.git/HEAD", "ref\n"),
            ("from/a/b.rs", "b\n"),
            ("from/target/x", "x\n"),
            ("from/z.rs", "z\n"),
        ];
        let dir = crate::testdir::tree("patch-copied", &files);
        let to = dir.join("to");
        copied(&dir.join("from"), &to).unwrap();
        assert!(to.join("a").join("b.rs").is_file() && to.join("z.rs").is_file());
        assert!(!to.join(".git").exists() && !to.join("target").exists());
    }
}
