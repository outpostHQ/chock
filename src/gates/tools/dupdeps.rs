//! Crates the graph compiles more than once: a second version, or one version built with a second
//! feature set. Each copy costs compile time on every build, and the copies' types do not mix.

use crate::exec;
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

pub const GATE: Gate = Gate {
    name: "dupdeps",
    about: "crates the dependency graph compiles more than once, against the count recorded",
    group: Group::Quality,
    builds: false,
    reads: None,
    // A ratchet, since most graphs already carry some duplicates.
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "duplicate crate build(s)",
    },
};

/// Lists duplicates for every target, so the count is the same on every platform. `--locked` is
/// passed only when a lockfile exists, since cargo refuses it otherwise.
fn measure(ctx: &Ctx) -> Result<Series, String> {
    let mut args = vec![
        "tree",
        "--workspace",
        "--duplicates",
        "--target",
        "all",
        "--depth",
        "0",
        "--prefix",
        "none",
    ];
    args.extend(ctx.root.join("Cargo.lock").is_file().then_some("--locked"));
    args.extend(ctx.features.iter().map(String::as_str));
    read(&exec::tool(&ctx.root, "cargo", &args)?)
}

/// The number of builds of each crate, by name. An unreadable line refuses the whole listing.
fn read(out: &exec::Output) -> Result<Series, String> {
    if !out.success() {
        return Err(format!(
            "cargo tree did not list duplicates: {}",
            out.why_it_failed()
        ));
    }
    if out.truncated {
        return Err(
            "cargo tree's listing was cut short, so any count would be partial".to_string(),
        );
    }
    let mut series = Series::new();
    for line in out.stdout.lines().filter(|line| !line.trim().is_empty()) {
        let mut words = line.split_whitespace();
        let (Some(name), Some(version)) = (words.next(), words.next()) else {
            return Err(format!("unreadable cargo tree line: {line}"));
        };
        if !version.starts_with('v') {
            return Err(format!("unreadable cargo tree line: {line}"));
        }
        let held = series.get(name).unwrap_or(0);
        series.set(name, held.saturating_add(1));
    }
    Ok(series)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn said(stdout: &str) -> exec::Output {
        exec::Output::of(Some(0), stdout, "")
    }

    fn counts(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs
            .iter()
            .map(|(name, count)| ((*name).to_string(), *count))
            .collect()
    }

    #[test]
    fn one_version_built_twice_counts_once_per_build() {
        let listed = "proc-macro2 v1.0.107\n\nproc-macro2 v1.0.107\n\nsyn v3.0.6\n\nsyn v3.0.6\n";
        assert_eq!(
            read(&said(listed)).unwrap().0,
            counts(&[("proc-macro2", 2), ("syn", 2)])
        );
    }

    #[test]
    fn two_versions_and_a_kind_suffix_are_keyed_by_the_crate_name() {
        let listed = "windows-sys v0.52.0\nwindows-sys v0.59.0\n\n\
                      serde_derive v1.0.200 (proc-macro)\nserde_derive v1.0.210 (proc-macro)\n";
        assert_eq!(
            read(&said(listed)).unwrap().0,
            counts(&[("serde_derive", 2), ("windows-sys", 2)])
        );
    }

    #[test]
    fn a_graph_with_no_duplicates_measures_nothing() {
        assert_eq!(read(&said("\n")).unwrap().0, counts(&[]));
    }

    #[test]
    fn a_refused_cut_short_or_unreadable_listing_is_not_a_clean_graph() {
        let locked = "error: the lock file needs to be updated but --locked was passed";
        let error = read(&exec::Output::of(Some(101), "", locked)).unwrap_err();
        assert!(
            error.starts_with("cargo tree did not list duplicates: "),
            "{error}"
        );
        assert!(error.contains("--locked was passed"), "{error}");
        let mut cut = said("syn v3.0.6\n");
        cut.truncated = true;
        let partial = "cargo tree's listing was cut short, so any count would be partial";
        assert_eq!(read(&cut).unwrap_err(), partial);
        for line in ["warning: something else", "lonely"] {
            let error = read(&said(&format!("{line}\n"))).unwrap_err();
            assert_eq!(error, format!("unreadable cargo tree line: {line}"));
        }
    }
}
