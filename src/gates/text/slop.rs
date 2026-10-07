//! The comment-length gate: a ratchet on comment blocks longer than two lines, counted per file.

use crate::project;
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};
use crate::slop;

pub const GATE: Gate = Gate {
    name: "slop",
    about: "comment blocks longer than two lines, per file",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "over-long comment block(s)",
    },
};

/// Keyed by file, not block, so an edit above a block does not read as new debt.
fn measure(ctx: &Ctx) -> Result<Series, String> {
    let mut series = Series::new();
    for block in slop::scan(&ctx.root)? {
        let key = project::relative(&ctx.root, &block.file);
        series.set(&key, series.get(&key).unwrap_or(0) + 1);
    }
    Ok(series)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn ctx_of(files: &[(&str, &str)]) -> crate::testdir::Held {
        crate::testdir::Held::tree("gate-slop", files)
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_with_no_long_comment_blocks_measures_nothing() {
        let ctx = ctx_of(&[("src/a.rs", "// one\n// two\nfn f() {}\n")]);
        assert_eq!(measure(&ctx).unwrap(), Series::new());
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_is_keyed_by_its_path_and_counted_once_per_block() {
        let ctx = ctx_of(&[(
            "src/a.rs",
            "// a\n// b\n// c\nfn f() {}\n// d\n// e\n// f\n",
        )]);
        let held = Series([("src/a.rs".to_string(), 2)].into());
        assert_eq!(measure(&ctx).unwrap(), held);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn every_file_that_has_one_appears_under_its_own_key() {
        let ctx = ctx_of(&[
            ("src/a.rs", "// a\n// b\n// c\n"),
            ("src/b.rs", "// a\n// b\n// c\n"),
            ("src/clean.rs", "fn f() {}\n"),
        ]);
        let measured = measure(&ctx).unwrap();
        assert_eq!(measured.get("src/a.rs"), Some(1));
        assert_eq!(measured.get("src/b.rs"), Some(1));
        assert_eq!(measured.get("src/clean.rs"), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_key_is_relative_to_the_root_so_it_travels_between_machines() {
        let ctx = ctx_of(&[("src/deep/a.rs", "// a\n// b\n// c\n")]);
        let measured = measure(&ctx).unwrap();
        assert_eq!(measured.0.keys().collect::<Vec<_>>(), vec!["src/deep/a.rs"]);
    }
}
