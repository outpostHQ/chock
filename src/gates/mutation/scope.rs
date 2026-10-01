//! Which files a local `mutest` run mutates. A change to Rust source alone mutates only those
//! files and keeps the record for the rest; CI and a recorded baseline mutate the whole crate.

use crate::run::baseline::Series;
use crate::run::report::Finding;
use crate::run::{Ctx, Measurement};

/// The `.rs` files this local run mutates, or `None` for the whole crate. Without a record there
/// is no count to keep for the files left out.
pub(super) fn of(ctx: &Ctx) -> Option<Vec<String>> {
    if ctx.ci || ctx.whole || !ctx.baseline.has(crate::gates::tools::MUTEST.name) {
        return None;
    }
    narrowed(ctx.changed().ok()?)
}

/// The changed `.rs` files under `src`, or `None` for a change that can move a survivor anywhere: a
/// test, a manifest, the toolchain, data the source reads, or a path the filter cannot spell.
fn narrowed(changed: &[String]) -> Option<Vec<String>> {
    let mut files = Vec::new();
    for path in changed {
        let parts: Vec<&str> = path.split('/').collect();
        if path.ends_with(".rs") && parts.contains(&"src") && !parts.contains(&"tests") {
            files.push(path.clone());
        } else if !reaches_no_mutation(path, &parts) {
            return None;
        }
    }
    files
        .iter()
        .all(|file| !file.contains([':', ',']))
        .then_some(files)
}

/// Prose and the repository's own records: no build reads them.
fn reaches_no_mutation(path: &str, parts: &[&str]) -> bool {
    let in_dir = |dir: &str| parts.first() == Some(&dir);
    path.ends_with(".md")
        || in_dir(".github")
        || (in_dir(".chock") && path != crate::project::config::FILE)
}

/// mutest's spelling of a mutation filter for each file.
pub(super) fn filter(files: &[String]) -> String {
    let each: Vec<String> = files.iter().map(|file| format!("file:{file}")).collect();
    format!("--filter-mutations={}", each.join(","))
}

/// The record for each file outside `files`, and what this run measured inside them.
pub(super) fn within(measured: Measurement, was: &Series, files: &[String]) -> Measurement {
    let mut series = Series(
        was.0
            .iter()
            .filter(|(key, _)| !files.iter().any(|file| file == file_of(key)))
            .map(|(key, count)| (key.clone(), *count))
            .collect(),
    );
    series.0.extend(measured.series.0);
    let mut findings = measured.findings;
    findings.push(note(files.len()));
    Measurement { series, findings }
}

/// The record as it stands, for a change that touched no Rust source.
pub(super) fn unchanged(was: Series) -> Measurement {
    Measurement {
        series: was,
        findings: vec![note(0)],
    }
}

fn note(files: usize) -> Finding {
    Finding::at(
        "",
        &format!(
            "mutated only the {files} Rust file(s) this change touched; CI mutates the whole crate"
        ),
    )
    .item("local run")
}

/// The file in a `file#operator` key.
fn file_of(key: &str) -> &str {
    key.rsplit_once('#').map_or(key, |(file, _)| file)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn paths(all: &[&str]) -> Vec<String> {
        all.iter().map(ToString::to_string).collect()
    }

    fn series(all: &[(&str, u64)]) -> Series {
        Series(
            all.iter()
                .map(|(key, n)| ((*key).to_string(), *n))
                .collect(),
        )
    }

    #[test]
    fn a_change_to_source_alone_mutates_only_the_files_it_touched() {
        let changed = paths(&[
            "src/a.rs",
            "crates/b/src/c.rs",
            "README.md",
            ".chock/baseline.json",
        ]);
        assert_eq!(
            narrowed(&changed),
            Some(paths(&["src/a.rs", "crates/b/src/c.rs"]))
        );
        assert_eq!(narrowed(&paths(&["docs/notes.md"])), Some(Vec::new()));
    }

    #[test]
    fn a_change_that_can_move_any_survivor_mutates_the_whole_crate() {
        for touched in [
            "tests/cli.rs",
            "src/tests/a.rs",
            "build.rs",
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            ".chock/config.json",
            "data/quality-bar.json",
            "src/a:b.rs",
            "src/a,b.rs",
        ] {
            assert_eq!(narrowed(&paths(&["src/a.rs", touched])), None, "{touched}");
        }
    }

    /// CI, a whole run and a run without a record each mutate the whole crate on their own.
    #[test]
    fn ci_a_whole_run_or_a_missing_record_each_mutate_the_whole_crate() {
        let ctx = |ci: bool, whole: bool, recorded: bool| {
            let mut baseline = crate::run::baseline::Baseline::empty("0.1.0");
            if recorded {
                let was = series(&[("src/a.rs#eq_op_invert", 1)]);
                baseline.set(crate::gates::tools::MUTEST.name, was);
            }
            Ctx {
                changed: std::sync::Arc::new(std::sync::OnceLock::from(Ok(paths(&["src/a.rs"])))),
                ci,
                whole,
                ..Ctx::for_root(std::path::PathBuf::from("."), baseline)
            }
        };
        assert_eq!(of(&ctx(false, false, true)), Some(paths(&["src/a.rs"])));
        for (ci, whole, recorded) in [
            (true, false, true),
            (false, true, true),
            (false, false, false),
        ] {
            assert_eq!(
                of(&ctx(ci, whole, recorded)),
                None,
                "{ci} {whole} {recorded}"
            );
        }
    }

    #[test]
    fn each_file_is_one_filter_in_one_flag() {
        assert_eq!(
            filter(&paths(&["src/a.rs", "src/b.rs"])),
            "--filter-mutations=file:src/a.rs,file:src/b.rs"
        );
    }

    #[test]
    fn the_files_mutated_take_the_new_counts_and_the_rest_keep_the_record() {
        let was = series(&[
            ("src/a.rs#eq_op_invert", 2),
            ("src/b.rs#bool_expr_negate", 1),
        ]);
        let measured = Measurement {
            series: series(&[("src/b.rs#eq_op_invert", 1)]),
            findings: Vec::new(),
        };
        let merged = within(measured, &was, &paths(&["src/b.rs"]));
        assert_eq!(
            merged.series,
            series(&[("src/a.rs#eq_op_invert", 2), ("src/b.rs#eq_op_invert", 1)])
        );
        assert_eq!(merged.findings, [note(1)]);
        assert_eq!(unchanged(was.clone()).series, was);
    }
}
