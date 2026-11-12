//! Every line of every text file the repository tracks, blank ones included, as `wc -l` counts
//! them. A leaner tree holds this total down; chock's own records under `.chock` do not count.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use crate::project::vcs::Kind as Vcs;
use crate::run::baseline::{Keys, Series};
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Measurement};

/// The `lines` gate: the line count of every tracked text file.
pub const GATE: Gate = Gate {
    name: "lines",
    about: "the lines of every tracked text file, blank ones included, as `wc -l` counts them; the \
            total only goes down",
    // Every change that adds a line trips it, so a project asks for it.
    group: Group::OptIn,
    builds: false,
    reads: None,
    kind: Kind::AnnotatedRatchet {
        measure,
        keys: Keys::Items,
        unit: "line(s)",
    },
};

/// The one key: the total over the tree, so a line that moves to another file is no change.
const TOTAL: &str = "total";

/// Most packages a note names; the rest grew too, but a short list reads better than a long one.
const NAMED: usize = 3;

fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let series = totals(ctx)?;
    let findings = grown_packages(ctx)?;
    Ok(Measurement::of(series, findings))
}

/// A record that another gate changes would move this total, so chock's own files do not count.
fn totals(ctx: &Ctx) -> Result<Series, String> {
    let mut total = 0;
    for shown in crate::project::vcs::tracked(&ctx.root, ctx.vcs)? {
        if !shown.starts_with(".chock/") {
            total += newlines(&ctx.root.join(&shown)).map_err(|e| format!("{shown}: {e}"))?;
        }
    }
    let mut series = Series::new();
    series.set(TOTAL, total);
    Ok(series)
}

/// Lines a file gained and lost since `HEAD`, summed by package.
#[derive(Default)]
struct Delta {
    grown: u64,
    shrunk: u64,
}

/// The packages whose tracked lines grew most since `HEAD`, most grown first, as notes that name
/// no file. Git only: an Outpost-tracked project has no match for `git show` at a revision.
fn grown_packages(ctx: &Ctx) -> Result<Vec<Finding>, String> {
    if ctx.vcs != Some(Vcs::Git) {
        return Ok(Vec::new());
    }
    let dirty = crate::project::vcs::dirty(&ctx.root, ctx.vcs)?;
    let dirs = package_dirs(ctx)?;
    if dirty.is_empty() || dirs.is_empty() {
        return Ok(Vec::new());
    }
    let mut by_package: BTreeMap<String, Delta> = BTreeMap::new();
    for shown in &dirty {
        held_delta(ctx, &dirs, shown, &mut by_package)?;
    }
    Ok(named(by_package))
}

/// `shown`'s line delta since `HEAD`, added into its package's running total; a path with no
/// package of its own, such as `.chock/`, adds nothing.
fn held_delta(
    ctx: &Ctx,
    dirs: &[(PathBuf, String)],
    shown: &str,
    by_package: &mut BTreeMap<String, Delta>,
) -> Result<(), String> {
    if shown.starts_with(".chock/") {
        return Ok(());
    }
    let Some(name) = nearest(dirs, &ctx.root.join(shown)) else {
        return Ok(());
    };
    let before = match crate::project::vcs::text_at(&ctx.root, "HEAD", shown)? {
        Some(text) => counted(text.as_bytes()).map_err(|e| format!("{shown}: {e}"))?,
        None => 0,
    };
    let after = newlines(&ctx.root.join(shown)).map_err(|e| format!("{shown}: {e}"))?;
    let held = by_package.entry(name.to_string()).or_default();
    if after >= before {
        held.grown += after - before;
    } else {
        held.shrunk += before - after;
    }
    Ok(())
}

/// The packages that grew net, most grown first, as a note each with no file.
fn named(by_package: BTreeMap<String, Delta>) -> Vec<Finding> {
    let mut grown: Vec<(String, u64)> = by_package
        .into_iter()
        .filter_map(|(name, d)| (d.grown > d.shrunk).then(|| (name, d.grown - d.shrunk)))
        .collect();
    grown.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    grown
        .into_iter()
        .take(NAMED)
        .map(|(name, delta)| Finding::at("", &format!("{name} grew by {delta} line(s)")))
        .collect()
}

/// Each workspace member's directory and name, from `cargo metadata`.
fn package_dirs(ctx: &Ctx) -> Result<Vec<(PathBuf, String)>, String> {
    let metadata_json = crate::project::metadata(&ctx.root)?;
    let metadata: crate::project::workspace::Metadata =
        serde_json::from_str(&metadata_json).map_err(|e| format!("cargo metadata: {e}"))?;
    let members: std::collections::HashSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    Ok(metadata
        .packages
        .into_iter()
        .filter(|package| members.contains(package.id.as_str()))
        .filter_map(|package| {
            let dir = Path::new(&package.manifest_path).parent()?.to_path_buf();
            Some((dir, package.name))
        })
        .collect())
}

/// The package whose directory is the deepest ancestor of `file`.
fn nearest<'a>(dirs: &'a [(PathBuf, String)], file: &Path) -> Option<&'a str> {
    dirs.iter()
        .filter(|(dir, _)| file.starts_with(dir))
        .max_by_key(|(dir, _)| dir.components().count())
        .map(|(_, name)| name.as_str())
}

/// The newline bytes in the file at `path`. A path that holds no file, such as a link, a submodule
/// or a file deleted since the last commit, holds none.
fn newlines(path: &Path) -> std::io::Result<u64> {
    match std::fs::symlink_metadata(path) {
        Ok(it) if !it.is_file() => return Ok(0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    counted(std::io::BufReader::new(std::fs::File::open(path)?))
}

/// The newline bytes `reader` holds. A binary file holds no lines: git reads a file with a NUL
/// byte in its first 8000 bytes as binary.
fn counted(mut reader: impl BufRead) -> std::io::Result<u64> {
    if reader.fill_buf()?.iter().take(8000).any(|byte| *byte == 0) {
        return Ok(0);
    }
    let mut count = 0;
    loop {
        let read = reader.fill_buf()?;
        if read.is_empty() {
            return Ok(count);
        }
        count += read.iter().fold(0, |n, byte| n + u64::from(*byte == b'\n'));
        let len = read.len();
        reader.consume(len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn each_newline_counts_and_a_path_with_no_file_counts_none() {
        let root = crate::testdir::tree(
            "lines-newlines",
            &[
                ("a.txt", "one\n\nthree\nno newline"),
                ("dir/b.txt", ""),
                ("c.png", "\u{89}PNG\r\n\u{1a}\n\0\0\n"),
            ],
        );
        assert_eq!(newlines(&root.join("a.txt")).unwrap(), 3);
        assert_eq!(newlines(&root.join("dir/b.txt")).unwrap(), 0);
        assert_eq!(newlines(&root.join("dir")).unwrap(), 0);
        assert_eq!(newlines(&root.join("gone.txt")).unwrap(), 0);
        assert_eq!(newlines(&root.join("c.png")).unwrap(), 0);
        // A NUL byte is no path on any system, so the error is no `NotFound`.
        assert!(newlines(Path::new("bad\0path")).is_err());
    }

    #[test]
    fn a_nul_byte_in_the_first_8000_bytes_makes_a_file_binary() {
        let mut late = vec![b'\n'; 8000];
        late.push(0);
        assert_eq!(counted(&late[..]).unwrap(), 8000);
        late[7999] = 0;
        assert_eq!(counted(&late[..]).unwrap(), 0);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn the_total_counts_tracked_files_only_and_not_chock_s_own() {
        let dir = crate::testdir::repo(
            "lines-total",
            &[
                ("src/lib.rs", "fn a() {}\n\nfn b() {}\n"),
                ("README.md", "# x\n"),
                (".chock/baseline.json", "{\n}\n"),
                ("gone.rs", "1\n2\n"),
                ("dir", "\n"),
            ],
        );
        std::fs::write(dir.join("untracked.rs"), "1\n2\n3\n").unwrap();
        std::fs::remove_file(dir.join("gone.rs")).unwrap();
        // A tracked path that holds a directory now, as a submodule does.
        std::fs::remove_file(dir.join("dir")).unwrap();
        std::fs::create_dir(dir.join("dir")).unwrap();
        let baseline = crate::run::baseline::Baseline::empty("0.1.0");
        let mut ctx = Ctx::for_root(dir.to_path_buf(), baseline);
        ctx.vcs = Some(crate::project::vcs::Kind::Git);
        let series = measure(&ctx).unwrap().series;
        let keys = crate::testdir::rows(&series);
        assert_eq!(keys, [("total", 4)]);
        ctx.vcs = None;
        assert!(measure(&ctx).unwrap_err().contains("no repository"));
    }

    /// Two workspace members, one committed, then `a` grows and `b` shrinks on disk with nothing
    /// committed: the note names `a` alone, by how much it grew net of what it lost.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_package_that_grew_on_disk_since_head_is_named_with_its_net_delta() {
        let dir = crate::testdir::repo(
            "lines-grown",
            &[
                ("Cargo.toml", "[workspace]\nmembers = [\"a\", \"b\"]\n"),
                (
                    "a/Cargo.toml",
                    "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
                ),
                ("a/src/lib.rs", "fn a() {}\nfn a2() {}\n"),
                (
                    "b/Cargo.toml",
                    "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
                ),
                ("b/src/lib.rs", "fn b() {}\nfn b2() {}\nfn b3() {}\n"),
            ],
        );
        std::fs::write(
            dir.join("a/src/lib.rs"),
            "fn a() {}\nfn a2() {}\nfn a3() {}\nfn a4() {}\n",
        )
        .unwrap();
        std::fs::write(dir.join("b/src/lib.rs"), "fn b() {}\n").unwrap();
        let baseline = crate::run::baseline::Baseline::empty("0.1.0");
        let mut ctx = Ctx::for_root(dir.to_path_buf(), baseline);
        ctx.vcs = Some(crate::project::vcs::Kind::Git);
        let findings = grown_packages(&ctx).unwrap();
        assert_eq!(Finding::rendered(&findings), ["?: a grew by 2 line(s)"]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_tree_with_nothing_dirty_or_outside_git_names_no_package() {
        let dir = crate::testdir::repo("lines-no-growth", &[("src/lib.rs", "fn a() {}\n")]);
        let baseline = crate::run::baseline::Baseline::empty("0.1.0");
        let mut ctx = Ctx::for_root(dir.to_path_buf(), baseline);
        ctx.vcs = Some(crate::project::vcs::Kind::Git);
        assert_eq!(grown_packages(&ctx).unwrap(), Vec::new());
        ctx.vcs = Some(crate::project::vcs::Kind::Outpost);
        std::fs::write(dir.join("src/lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        assert_eq!(grown_packages(&ctx).unwrap(), Vec::new());
    }
}
