//! Every line of every text file the repository tracks, blank ones included, as `wc -l` counts
//! them. A leaner tree holds this total down; chock's own records under `.chock` do not count.

use std::io::BufRead;
use std::path::Path;

use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

pub const GATE: Gate = Gate {
    name: "lines",
    about: "the lines of every tracked text file, blank ones included, as `wc -l` counts them; the \
            total only goes down",
    // Every change that adds a line trips it, so a project asks for it.
    group: Group::OptIn,
    builds: false,
    reads: None,
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "line(s)",
    },
};

/// The one key: the total over the tree, so a line that moves to another file is no change.
const TOTAL: &str = "total";

/// A record that another gate changes would move this total, so chock's own files do not count.
fn measure(ctx: &Ctx) -> Result<Series, String> {
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
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
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
        let series = measure(&ctx).unwrap();
        let keys = crate::testdir::rows(&series);
        assert_eq!(keys, [("total", 4)]);
        ctx.vcs = None;
        assert!(measure(&ctx).unwrap_err().contains("no repository"));
    }
}
