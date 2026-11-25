//! Which files a local `mutest` run mutates. A change to Rust source alone mutates only those
//! files and keeps the record for the rest; CI and a recorded baseline mutate the whole crate.

use crate::gates::tools::miri::Part;
use crate::project::config::Unmutated;
use crate::run::baseline::Series;
use crate::run::report::Finding;
use crate::run::{Ctx, Measurement};

/// The `.rs` files `--mutate` names, each a Rust source mutest mutates, or on a local run with a
/// record those a change to Rust source alone touched; `None` mutates the whole crate.
pub(super) fn of(ctx: &Ctx) -> Result<Option<Vec<String>>, String> {
    let named = by_name(ctx)?;
    if let Some(part) = ctx.scope.part {
        return kept(ctx).map(|files| Some(parted(ctx, files, part)));
    }
    Ok(chosen(ctx, named).map(|files| {
        let kept = files.into_iter();
        kept.filter(|file| !left_out(&ctx.not_mutated, file))
            .collect()
    }))
}

/// The files `--mutate` names; else `None`, the whole crate, for CI, `--whole` or a run with no
/// record; else those a change to Rust source alone touched.
fn chosen(ctx: &Ctx, named: &[String]) -> Option<Vec<String>> {
    if !named.is_empty() {
        Some(named.to_vec())
    } else if ctx.ci || ctx.whole || !ctx.baseline.has(crate::gates::tools::MUTEST.name) {
        None
    } else {
        ctx.changed().ok().and_then(narrowed)
    }
}

/// The `.rs` files `--mutate` names, or an error for the first that is no Rust source mutest
/// mutates here.
fn by_name(ctx: &Ctx) -> Result<&[String], String> {
    let source = |file: &&String| mutable(file) && ctx.root.join(file.as_str()).is_file();
    match ctx.scope.mutate.iter().find(|file| !source(file)) {
        Some(file) => Err(format!(
            "`--mutate` names {file}, which is no Rust source mutest mutates here"
        )),
        None => Ok(&ctx.scope.mutate),
    }
}

/// The flag for a run of the whole crate: none, or where the project leaves files unmutated, each
/// other Rust source by name, since mutest's filters only name what to mutate.
pub(super) fn whole(ctx: &Ctx) -> Result<Option<String>, String> {
    if ctx.not_mutated.is_empty() {
        return Ok(None);
    }
    let files = kept(ctx)?;
    Ok((!files.is_empty()).then(|| filter(&files)))
}

/// Every Rust source mutest mutates, less the files the project leaves unmutated, each a path
/// mutest's filter can spell.
fn kept(ctx: &Ctx) -> Result<Vec<String>, String> {
    let sources = sources(ctx)?;
    for named in &ctx.not_mutated {
        let path = &named.path;
        if named.why.trim().is_empty() {
            return Err(format!("`not_mutated` leaves out {path} with no `why`"));
        }
        if !sources.contains(path) {
            return Err(format!(
                "`not_mutated` leaves out {path}, which is no Rust source mutest mutates here"
            ));
        }
    }
    let files: Vec<String> = sources
        .into_iter()
        .filter(|file| !left_out(&ctx.not_mutated, file))
        .collect();
    if let Some(file) = files.iter().find(|file| file.contains([':', ','])) {
        return Err(format!(
            "mutest's filter cannot name {file}, so chock cannot leave out the files in \
             `not_mutated` or split the crate into parts"
        ));
    }
    Ok(files)
}

/// The files of `part`: each file goes, largest first, to the part with the fewest lines so far,
/// so the parts cost about the same and every file is in exactly one of them.
fn parted(ctx: &Ctx, files: Vec<String>, part: Part) -> Vec<String> {
    let lines = |file: &String| {
        std::fs::read_to_string(ctx.root.join(file)).map_or(0, |text| text.lines().count())
    };
    let weighed = files.into_iter().map(|file| (lines(&file), file)).collect();
    split(weighed, part)
}

/// The files of `part`, from each file with its weight.
fn split(mut weighed: Vec<(usize, String)>, part: Part) -> Vec<String> {
    weighed.sort_by(|(a, x), (b, y)| b.cmp(a).then_with(|| x.cmp(y)));
    let mut totals = vec![0_usize; usize::try_from(part.of).unwrap_or(1).max(1)];
    let wanted = usize::try_from(part.index).unwrap_or(1).saturating_sub(1);
    let mut files = Vec::new();
    for (weight, file) in weighed {
        let lightest = (0..totals.len())
            .min_by_key(|&at| (totals[at], at))
            .unwrap_or(0);
        totals[lightest] = totals[lightest].saturating_add(weight.max(1));
        if lightest == wanted {
            files.push(file);
        }
    }
    files.sort();
    files
}

fn left_out(named: &[Unmutated], file: &str) -> bool {
    named.iter().any(|it| it.path == file)
}

/// Every Rust source mutest can mutate, from the root.
fn sources(ctx: &Ctx) -> Result<Vec<String>, String> {
    let entered = |dir: &str| !crate::project::SKIPPED.contains(&dir);
    let named = |name: &str, _: &std::path::Path| {
        std::path::Path::new(name)
            .extension()
            .is_some_and(|ext| ext == "rs")
    };
    let files = crate::project::walked(&ctx.listing, &ctx.root, &entered, &named)?;
    let shown = files
        .iter()
        .map(|path| crate::project::relative(&ctx.root, path));
    Ok(shown.filter(|shown| mutable(shown)).collect())
}

/// A `.rs` file under a `src` directory and in no `tests` one.
fn mutable(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    let rust = std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext == "rs");
    rust && parts.contains(&"src") && !parts.contains(&"tests")
}

/// The changed `.rs` files under `src`, or `None` for a change that can move a survivor anywhere: a
/// test, a manifest, the toolchain, data the source reads, or a path the filter cannot spell.
fn narrowed(changed: &[String]) -> Option<Vec<String>> {
    let mut files = Vec::new();
    for path in changed {
        let parts: Vec<&str> = path.split('/').collect();
        if mutable(path) {
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
    std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext == "md")
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
    let mut series = Series::new();
    series.0.extend(
        was.0
            .iter()
            .filter(|(key, _)| {
                // The file in a `file#operator` key.
                let named = key.rsplit_once('#').map_or(key.as_str(), |(file, _)| file);
                !files.iter().any(|file| file == named)
            })
            .map(|(key, count)| (key.clone(), *count)),
    );
    series.0.extend(measured.series.0);
    let mut findings = measured.findings;
    findings.push(note(files.len()));
    Measurement::of(series, findings)
}

/// The record as it stands, for a change that touched no Rust source.
pub(super) fn unchanged(was: Series) -> Measurement {
    Measurement::of(was, vec![note(0)])
}

/// What the run of `files` alone measured, set in the record. Where that run left the verdict to
/// the whole crate, what `whole` measures.
pub(super) fn widened(
    only: Option<Measurement>,
    was: &Series,
    files: &[String],
    whole: impl FnOnce() -> Result<Measurement, String>,
) -> Result<Measurement, String> {
    match only {
        Some(measured) => Ok(within(measured, was, files)),
        None => whole().map(|mut measured| {
            measured.findings.push(widening(files.len()));
            measured
        }),
    }
}

fn note(files: usize) -> Finding {
    local(&format!(
        "mutated only {files} Rust file(s), from this change, `--mutate` or \
         `--mutest-partition`; every other file keeps its record"
    ))
}

fn widening(files: usize) -> Finding {
    local(&format!(
        "mutated the whole crate, as CI does: the {files} Rust file(s) alone \
         gave more timeouts than the limit allows"
    ))
}

/// A note on what a local run mutated.
fn local(text: &str) -> Finding {
    Finding::at("", text).item("local run")
}

#[cfg(test)]
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

    fn weighed(all: &[(usize, &str)]) -> Vec<(usize, String)> {
        all.iter()
            .map(|(n, file)| (*n, (*file).to_string()))
            .collect()
    }

    /// Each file is in exactly one part, and the parts of a crate cost about the same.
    #[test]
    fn the_parts_of_a_partition_hold_every_file_once_and_weigh_about_the_same() {
        let files = weighed(&[
            (900, "src/a.rs"),
            (100, "src/b.rs"),
            (500, "src/c.rs"),
            (400, "src/d.rs"),
            (0, "src/e.rs"),
            (300, "src/f.rs"),
            (200, "src/g.rs"),
        ]);
        let parts: Vec<Vec<String>> = (1..=3)
            .map(|index| split(files.clone(), Part { index, of: 3 }))
            .collect();
        let mut all: Vec<String> = parts.iter().flatten().cloned().collect();
        all.sort();
        let mut expected: Vec<String> = files.iter().map(|(_, file)| file.clone()).collect();
        expected.sort();
        assert_eq!(all, expected, "every file once, in no two parts");
        assert_eq!(parts[0], paths(&["src/a.rs"]));
        // 900, 800 and 701 lines: a file of none still weighs one.
        assert_eq!(parts[1], paths(&["src/b.rs", "src/c.rs", "src/g.rs"]));
        assert_eq!(parts[2], paths(&["src/d.rs", "src/e.rs", "src/f.rs"]));
        // The order the files arrive in does not move them between parts.
        let mut turned = files;
        turned.reverse();
        assert_eq!(split(turned, Part { index: 2, of: 3 }), parts[1]);
        // More parts than files leaves the last parts empty, never a file out.
        let one = weighed(&[(10, "src/a.rs")]);
        assert_eq!(
            split(one.clone(), Part { index: 1, of: 2 }),
            paths(&["src/a.rs"])
        );
        assert_eq!(split(one, Part { index: 2, of: 2 }), Vec::<String>::new());
    }

    /// A partition mutates its part of every source, less the files the project leaves out.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_partition_mutates_its_part_of_every_source_the_project_mutates() {
        let lines = |n: usize| "fn f() {}\n".repeat(n);
        let (a, b, c, t) = (lines(30), lines(20), lines(10), lines(50));
        let root = crate::testdir::tree(
            "mutest-partition",
            &[
                ("src/a.rs", &a),
                ("src/b.rs", &b),
                ("src/c.rs", &c),
                ("tests/t.rs", &t),
            ],
        );
        let mut ctx = Ctx::at(&root);
        ctx.not_mutated = vec![unmutated("src/b.rs", "generated")];
        let part = |index| {
            let mut ctx = ctx.clone();
            ctx.scope.part = Some(Part { index, of: 2 });
            of(&ctx)
        };
        assert_eq!(part(1), Ok(Some(paths(&["src/a.rs"]))));
        assert_eq!(part(2), Ok(Some(paths(&["src/c.rs"]))));
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
        assert_eq!(of(&ctx(false, false, true)), Ok(Some(paths(&["src/a.rs"]))));
        for (ci, whole, recorded) in [
            (true, false, true),
            (false, true, true),
            (false, false, false),
        ] {
            assert_eq!(
                of(&ctx(ci, whole, recorded)),
                Ok(None),
                "{ci} {whole} {recorded}"
            );
        }
    }

    fn unmutated(path: &str, why: &str) -> Unmutated {
        Unmutated {
            path: path.into(),
            why: why.into(),
        }
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_file_left_unmutated_is_named_out_of_the_whole_crate_and_out_of_a_change() {
        let root = crate::testdir::tree(
            "mutest-not-mutated",
            &[
                ("src/lib.rs", ""),
                ("src/generated.rs", ""),
                ("src/a,b.rs", ""),
                ("crates/b/src/c.rs", ""),
                ("tests/cli.rs", ""),
                ("README.md", ""),
            ],
        );
        let mut baseline = crate::run::baseline::Baseline::empty("0.1.0");
        baseline.set(crate::gates::tools::MUTEST.name, series(&[]));
        let changed = paths(&["src/generated.rs", "src/lib.rs"]);
        let mut ctx = Ctx {
            changed: std::sync::Arc::new(std::sync::OnceLock::from(Ok(changed))),
            ..Ctx::for_root(root.to_path_buf(), baseline)
        };
        assert_eq!(whole(&ctx), Ok(None));
        assert_eq!(
            of(&ctx),
            Ok(Some(paths(&["src/generated.rs", "src/lib.rs"])))
        );
        ctx.not_mutated = vec![unmutated("src/generated.rs", "written by build.rs")];
        let comma = "mutest's filter cannot name src/a,b.rs, so chock cannot leave out the files \
                     in `not_mutated` or split the crate into parts";
        assert_eq!(whole(&ctx), Err(comma.to_string()));
        assert_eq!(of(&ctx), Ok(Some(paths(&["src/lib.rs"]))));
        ctx.scope.mutate = paths(&["crates/b/src/c.rs", "src/generated.rs"]);
        assert_eq!(of(&ctx), Ok(Some(paths(&["crates/b/src/c.rs"]))));
        ctx.scope.mutate = paths(&["src/gone.rs"]);
        let gone = "`--mutate` names src/gone.rs, which is no Rust source mutest mutates here";
        assert_eq!(of(&ctx), Err(gone.to_string()));
        ctx.scope.mutate = paths(&["tests/cli.rs"]);
        assert!(of(&ctx).is_err());
        ctx.scope.mutate = Vec::new();
        ctx.not_mutated
            .push(unmutated("src/a,b.rs", "a name the filter cannot spell"));
        let named = "--filter-mutations=file:crates/b/src/c.rs,file:src/lib.rs";
        assert_eq!(whole(&ctx), Ok(Some(named.to_string())));
        ctx.not_mutated = vec![unmutated("src/generated.rs", " ")];
        let bare = "`not_mutated` leaves out src/generated.rs with no `why`";
        assert_eq!(whole(&ctx), Err(bare.to_string()));
        ctx.not_mutated = vec![unmutated("tests/cli.rs", "a test")];
        let absent = "`not_mutated` leaves out tests/cli.rs, which is no Rust source mutest \
                      mutates here";
        assert_eq!(whole(&ctx), Err(absent.to_string()));
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
        let measured = Measurement::of(series(&[("src/b.rs#eq_op_invert", 1)]), Vec::new());
        let merged = within(measured, &was, &paths(&["src/b.rs"]));
        assert_eq!(
            merged.series,
            series(&[("src/a.rs#eq_op_invert", 2), ("src/b.rs#eq_op_invert", 1)])
        );
        assert_eq!(merged.findings, [note(1)]);
        assert_eq!(unchanged(was.clone()).series, was);
    }

    #[test]
    fn files_that_cannot_be_judged_alone_give_way_to_the_whole_crate() {
        let was = series(&[("src/a.rs#eq_op_invert", 2)]);
        let files = paths(&["src/b.rs", "src/c.rs"]);
        let measuring = |key: &str| Measurement::of(series(&[(key, 1)]), Vec::new());
        let unasked = || Err("the whole crate was not asked for".to_string());
        let only = Some(measuring("src/b.rs#eq_op_invert"));
        let alone = widened(only, &was, &files, unasked).unwrap();
        assert_eq!(
            alone.series,
            series(&[("src/a.rs#eq_op_invert", 2), ("src/b.rs#eq_op_invert", 1)])
        );
        assert_eq!(alone.findings, [note(2)]);
        let all = || Ok(measuring("src/z.rs#eq_op_invert"));
        let whole = widened(None, &was, &files, all).unwrap();
        assert_eq!(whole.series, series(&[("src/z.rs#eq_op_invert", 1)]));
        let said: Vec<String> = Finding::rendered(&whole.findings);
        assert_eq!(
            said,
            [
                "local run: mutated the whole crate, as CI does: the 2 Rust file(s) alone gave \
                 more timeouts than the limit allows"
            ]
        );
        let refused = widened(None, &was, &files, || Err("mutest gave up".to_string()));
        assert_eq!(
            refused.map(|read| read.series),
            Err("mutest gave up".to_string())
        );
    }
}
