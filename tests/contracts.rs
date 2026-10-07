//! Checks each tool reader against what the installed tool emits today. Opt-in, since each runs a
//! real tool: `cargo test --test contracts -- --ignored --test-threads 1`.

#![allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]

use std::path::{Path, PathBuf};

use chock::run::baseline::{Baseline, Series};
use chock::run::report::{GateReport, Verdict};
use chock::run::{Ctx, Gate, run_one};

/// A scratch crate under `target/test-scratch/`, with its own `[workspace]` so cargo does not
/// treat it as part of chock.
fn scratch_crate(name: &str, manifest_tail: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-scratch")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let manifest = format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n{manifest_tail}"
    );
    let mut written = vec![("Cargo.toml", manifest.as_str())];
    written.extend_from_slice(files);
    for (path, text) in written {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

/// Judged against an empty record, so every item the tool reports comes back as a finding.
fn judged(gate: &Gate, root: &Path) -> GateReport {
    let mut baseline = Baseline::empty("0.1.0");
    if let Some(unit) = gate.counts_in() {
        baseline.record(gate.name, unit, Series::new());
    }
    run_one(gate, &Ctx::for_root(root.to_path_buf(), baseline))
}

/// Each finding as the file it names and the item in it, which is how a `file#item` key reports.
fn found(report: &GateReport) -> Vec<(String, String)> {
    report
        .findings
        .iter()
        .map(|finding| {
            (
                finding.file.clone(),
                finding.item.clone().unwrap_or_default(),
            )
        })
        .collect()
}

/// A passing test leaves nothing behind; a failing one keeps its crate as the evidence.
fn done(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "runs the installed cargo-sort"]
fn cargo_sort_still_names_the_package_whose_dependencies_are_out_of_order() {
    let dir = scratch_crate(
        "contract-sort",
        "\n[dependencies]\nzeta = \"1\"\nalpha = \"1\"\n",
        &[("src/lib.rs", "")],
    );
    let report = judged(&chock::gates::tools::SORT, &dir);
    // cargo-sort names the directory it read, not the package.
    let named = dir.file_name().unwrap().to_string_lossy().to_string();
    assert_eq!(
        (report.verdict, found(&report)),
        (Verdict::Tripped, vec![(String::new(), named)]),
        "{report:?}"
    );
    done(&dir);
}

#[test]
#[ignore = "runs the installed typos"]
fn typos_still_names_the_file_a_misspelling_is_in() {
    // Assembled so the misspelling is never in this file's own text.
    let word = format!("{}h", "te");
    let text = format!("// {word} answer\n");
    let dir = scratch_crate("contract-typos", "", &[("src/lib.rs", text.as_str())]);
    let report = judged(&chock::gates::tools::TYPOS, &dir);
    assert_eq!(
        (report.verdict, found(&report)),
        (Verdict::Tripped, vec![("src/lib.rs".to_string(), word)]),
        "{report:?}"
    );
    done(&dir);
}

#[test]
#[ignore = "runs the installed cargo-machete"]
fn cargo_machete_still_names_a_dependency_nothing_uses() {
    let dir = scratch_crate(
        "contract-machete",
        "\n[dependencies]\nserde = \"1\"\n",
        &[("src/lib.rs", "pub fn f() {}\n")],
    );
    let report = judged(&chock::gates::tools::UNUSED, &dir);
    assert_eq!(
        (report.verdict, found(&report)),
        (
            Verdict::Tripped,
            vec![("Cargo.toml".to_string(), "serde".to_string())]
        ),
        "{report:?}"
    );
    done(&dir);
}

#[test]
#[ignore = "runs the installed kani, which compiles and solves"]
fn kani_still_lists_and_proves_a_harness_the_way_the_proof_gate_reads_it() {
    let lib = "#[cfg(kani)]\n#[kani::proof]\nfn doubling_a_small_byte_fits() {\n    \
               let x: u8 = kani::any();\n    kani::assume(x < 100);\n    \
               assert!(u16::from(x) * 2 < 256);\n}\n";
    let dir = scratch_crate(
        "contract-kani",
        "\n[lints.rust]\nunexpected_cfgs = { level = \"allow\", check-cfg = [\"cfg(kani)\"] }\n",
        &[("src/lib.rs", lib)],
    );
    let report = judged(&chock::gates::tools::proof::GATE, &dir);
    assert_eq!(
        (report.verdict, report.cannot_run_reason.clone()),
        (Verdict::Pass, None),
        "{report:?}"
    );
    done(&dir);
}

/// `x * 2` and `x + 2` agree at 2, so the one test cannot kill that mutation.
#[test]
#[ignore = "runs the installed cargo-mutest on its nightly"]
fn mutest_still_reports_a_survivor_where_the_survivor_gate_reads_it() {
    let lib = "pub fn double(x: u32) -> u32 {\n    x * 2\n}\n\n#[cfg(test)]\nmod tests {\n    \
               #[test]\n    fn two_doubles_to_four() {\n        assert_eq!(super::double(2), 4);\n    }\n}\n";
    let dir = scratch_crate("contract-mutest", "", &[("src/lib.rs", lib)]);
    let report = judged(&chock::gates::tools::MUTEST, &dir);
    assert!(
        found(&report).contains(&("src/lib.rs".to_string(), "math_op_add_mul_swap".to_string())),
        "{report:?}"
    );
    done(&dir);
}

/// A record every real score is above, so cargo-crap must report regressions for the gate to read.
/// The build script is in that record too, and the gate leaves it out: no test can reach it.
#[test]
#[ignore = "runs cargo-llvm-cov and the installed cargo-crap"]
fn cargo_crap_still_reports_a_regression_the_crap_gate_can_name() {
    let lib = "pub fn branchy(x: u32) -> u32 {\n    if x > 1 { 1 } else { 2 }\n}\n\n\
               pub fn seven() -> u32 {\n    7\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    \
               fn seven_is_seven() {\n        assert_eq!(super::seven(), 7);\n    }\n}\n";
    let build = "fn main() {\n    if std::env::var(\"X\").is_ok() {\n        println!(\"x\");\n    \
                 }\n}\n";
    let files = [("src/lib.rs", lib), ("build.rs", build)];
    let dir = scratch_crate("contract-crap", "", &files);
    let ctx = Ctx::for_root(dir.clone(), Baseline::empty("0.1.0"));
    chock::gates::coverage::ensure(&ctx).unwrap();
    let args = [
        "crap",
        "--lcov",
        chock::gates::coverage::crap::COVERAGE,
        "--workspace",
        "--format",
        "json",
    ];
    let out = chock::exec::run("cargo", &args, &dir).unwrap();
    let mut record: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let entries = record["entries"].as_array_mut().unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry["file"].as_str().unwrap().ends_with("build.rs")),
        "{}",
        out.stdout
    );
    for entry in entries {
        entry["crap"] = serde_json::json!(0.5);
    }
    let held = chock::run::baseline::relativize(&record.to_string(), &dir);
    let path = dir.join(chock::gates::coverage::crap::baseline());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, held).unwrap();
    let report = run_one(&chock::gates::coverage::crap::GATE, &ctx);
    assert_eq!(report.verdict, Verdict::Tripped, "{report:?}");
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.render().contains("branchy")),
        "{report:?}"
    );
    assert!(
        !report
            .findings
            .iter()
            .any(|finding| finding.render().contains("build.rs")),
        "{report:?}"
    );
    done(&dir);
}
