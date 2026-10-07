//! Reading Outpost's JSON output: whether the command ran, and whether chock knows the shape.

pub mod boundaries;
pub mod duplicates;
pub mod intel;
pub mod measures;
pub mod padding;
pub mod scan;

use serde::de::DeserializeOwned;

use crate::exec;

/// Parse Outpost's JSON. A failed command or an unknown shape is an error, never an empty answer.
pub fn read<T: DeserializeOwned>(out: &exec::Output, what: &str) -> Result<T, String> {
    // Outpost exits non-zero when it has findings, so readable output is an answer at any code.
    match serde_json::from_str(&out.stdout) {
        Ok(read) => Ok(read),
        Err(_) if !out.success() => Err(format!(
            "outpost could not {what} this tree: {}",
            out.why_it_failed()
        )),
        Err(e) => Err(format!("outpost printed a {what} chock cannot read: {e}")),
    }
}

/// Run `outpost` in the root. Every gate goes through here, so a missing binary reads the same.
pub fn spawn(root: &std::path::Path, args: &[&str]) -> Result<exec::Output, String> {
    exec::run("outpost", args, root).map_err(|e| format!("{e} — this gate needs `outpost` on PATH"))
}

/// The output contract this chock reads; a newer one is refused, since a field may change meaning.
pub const CONTRACT: u32 = 1;

/// One `outpost check --json` shared by every gate in a run, so the tree is read once.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Check {
    pub contract: u32,
    #[serde(default)]
    pub measures: Vec<Measure>,
    #[serde(default)]
    pub read_around: Vec<String>,
    /// The bar a `relative` measure was judged against; `None` from an outpost too old to say.
    #[serde(default)]
    pub bar: Option<Bar>,
}

/// The density padding findings are measured against: a pinned value, or the tree's own median.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Bar {
    pub tree_median: f64,
    pub population: u64,
    pub pinned: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Measure {
    pub measure: String,
    pub verdict: String,
    #[serde(default)]
    pub cannot_run_reason: Option<String>,
    /// Outpost's own total, which a census reads because a measure at zero has no findings.
    #[serde(default)]
    pub measured: Option<u64>,
    #[serde(default)]
    pub findings: Vec<Finding>,
    /// Whether this was judged against a tree-wide statistic, so a verdict can move with unrelated
    /// edits unless that statistic is pinned.
    #[serde(default)]
    pub relative: bool,
    /// Set on a grouping measure reported although the tree's modularity is under Outpost's floor.
    #[serde(default)]
    pub below_modularity_floor: bool,
    /// The tree's modularity, carried beside such a measure.
    #[serde(default)]
    pub modularity: Option<f64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Finding {
    pub file: String,
    #[serde(default)]
    pub item: Option<String>,
    /// The size this finding counts, such as excess lines; `None` for a measure of occurrences.
    #[serde(default)]
    pub measured: Option<u64>,
    /// A hazard's `note`, `warn` or `deny`. When absent, the finding counts as debt.
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub message: Option<String>,
}

impl Finding {
    /// A `note` is advice, such as a loop in a test fixture; only `warn` and `deny` are debt.
    #[must_use]
    pub fn advisory(&self) -> bool {
        self.severity.as_deref() == Some("note")
    }

    /// This finding as chock reports one under `measure`, with Outpost's file, line and message.
    #[must_use]
    pub fn site(&self, measure: &str) -> crate::run::report::Finding {
        let said = self.message.as_deref().unwrap_or("outpost gave no detail");
        let at = crate::run::report::Finding::at(&self.file, said).item(measure);
        self.line
            .into_iter()
            .fold(at, crate::run::report::Finding::line)
    }
}

impl Check {
    /// Every measure whose name `wanted` accepts; the caller decides whether none is an error.
    pub fn measures(&self, wanted: &dyn Fn(&str) -> bool) -> Vec<&Measure> {
        self.measures
            .iter()
            .filter(|held| wanted(&held.measure))
            .collect()
    }

    /// The named measure, or an error if Outpost no longer reports it or it could not run.
    pub fn measure(&self, name: &str) -> Result<&Measure, String> {
        self.measures
            .iter()
            .find(|held| held.measure == name)
            .ok_or_else(|| format!("outpost no longer reports `{name}`, so nothing measured it"))?
            .ran()
    }

    /// One measure's findings as a ratchet series, keyed by file and item so a key survives edits
    /// above it. Each value is the largest `measured` at that key, or one.
    pub fn series(&self, name: &str) -> Result<crate::run::baseline::Series, String> {
        let mut series = crate::run::baseline::Series::new();
        for found in &self.measure(name)?.findings {
            let key = match &found.item {
                Some(item) => format!("{}#{item}", found.file),
                None => found.file.clone(),
            };
            let worst = series
                .get(&key)
                .unwrap_or(0)
                .max(found.measured.unwrap_or(1));
            series.set(&key, worst);
        }
        Ok(series)
    }

    /// The non-advisory sites of each measure whose count rose past its record, so a rise names
    /// where to look.
    #[must_use]
    pub fn sites_over(
        &self,
        now: &crate::run::baseline::Series,
        was: &crate::run::baseline::Series,
    ) -> Vec<crate::run::report::Finding> {
        now.0
            .iter()
            .filter(|(name, count)| **count > was.get(name).unwrap_or(0))
            .filter_map(|(name, _)| self.measure(name).ok())
            .flat_map(|held| {
                held.findings
                    .iter()
                    .filter(|found| !found.advisory())
                    .map(|found| found.site(&held.measure))
            })
            .collect()
    }
}

impl Measure {
    /// This measure, or an error carrying Outpost's reason if it could not run.
    pub fn ran(&self) -> Result<&Self, String> {
        match self.verdict.as_str() {
            "cannot_run" => Err(format!(
                "outpost could not measure `{}`: {}",
                self.measure,
                self.cannot_run_reason
                    .as_deref()
                    .unwrap_or("no reason given")
            )),
            _ => Ok(self),
        }
    }
}

/// The run's one `outpost check --json`, started by whichever gate asks first and held on `Ctx`.
pub fn once(ctx: &crate::run::Ctx) -> Result<&Check, String> {
    ctx.checked
        .get_or_init(|| {
            // Below modularity 0.3 Outpost withholds the grouping rows `boundaries` reads unless
            // `--grouping=report` asks for them.
            checked(&spawn(
                &ctx.root,
                &["check", "--json", "--grouping=report"],
            )?)
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Read one `outpost check --json`, refusing a contract this chock does not know.
pub fn checked(out: &exec::Output) -> Result<Check, String> {
    let read: Check = read(out, "check")?;
    if read.contract > CONTRACT {
        return Err(format!(
            "outpost speaks contract {} and this chock reads {CONTRACT}; upgrade chock",
            read.contract
        ));
    }
    Ok(read)
}

/// The error a gate gives when Outpost read no files, which is not the same as a clean tree.
pub fn nothing_read(what: &str) -> String {
    format!("outpost read no files, so no {what} ran")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize, PartialEq, Debug)]
    struct Counted {
        files: u64,
    }

    fn said(stdout: &str, code: i32) -> exec::Output {
        exec::Output {
            code: Some(code),
            stdout: stdout.to_string(),
            stderr: "not a repository".to_string(),
            truncated: false,
        }
    }

    #[test]
    fn a_finding_with_no_item_and_no_number_is_keyed_by_its_file_and_counted_as_one() {
        let check = checked(&said(
            r#"{"contract": 1, "measures": [
              {"measure": "scattered_imports_production", "verdict": "passed", "findings": [
                {"file": "src/gates/crap.rs", "line": 15, "item": null, "measured": null},
                {"file": "src/init.rs", "line": 564}]}]}"#,
            0,
        ))
        .unwrap();
        let series = check.series("scattered_imports_production").unwrap();
        assert_eq!(series.get("src/gates/crap.rs"), Some(1));
        assert_eq!(series.get("src/init.rs"), Some(1));
    }

    /// A `note` finding is advisory and names no site either.
    #[test]
    fn a_measure_past_its_record_names_its_sites_and_one_at_it_names_none() {
        let check = checked(&said(
            r#"{"contract": 1, "measures": [
              {"measure": "hazard_loop", "verdict": "passed", "findings": [
                {"file": "src/a.rs", "line": 7, "message": "sync_all in a loop", "severity": "warn"},
                {"file": "src/t.rs", "line": 9, "severity": "note"}]},
              {"measure": "mixed", "verdict": "passed", "findings": [{"file": "crates/p/src"}]},
              {"measure": "held", "verdict": "passed", "findings": [{"file": "src/b.rs"}]}]}"#,
            0,
        ))
        .unwrap();
        let series = |rows: &[(&str, u64)]| {
            let mut series = crate::run::baseline::Series::new();
            rows.iter().for_each(|(key, count)| series.set(key, *count));
            series
        };
        let now = series(&[("hazard_loop", 1), ("mixed", 9), ("held", 1)]);
        let was = series(&[("mixed", 7), ("held", 1)]);
        let told: Vec<String> = check
            .sites_over(&now, &was)
            .iter()
            .map(crate::run::report::Finding::render)
            .collect();
        assert_eq!(
            told,
            [
                "src/a.rs:7: hazard_loop: sync_all in a loop",
                "crates/p/src: mixed: outpost gave no detail",
            ]
        );
    }

    #[test]
    fn a_shape_chock_knows_is_read() {
        let got: Counted = read(&said(r#"{"files":51}"#, 0), "measure").unwrap();
        assert_eq!(got, Counted { files: 51 });
    }

    #[test]
    fn output_that_reads_is_an_answer_whatever_the_command_exited_with() {
        let got: Counted = read(&said(r#"{"files":51}"#, 1), "measure").unwrap();
        assert_eq!(got, Counted { files: 51 });
    }

    #[test]
    fn nothing_readable_and_a_non_zero_code_is_still_the_command_failing() {
        let err = read::<Counted>(&said("", 2), "measure").unwrap_err();
        assert_eq!(err, "outpost could not measure this tree: not a repository");
    }

    #[test]
    fn a_command_that_failed_carries_the_reason_it_gave() {
        let err = read::<Counted>(&said("", 1), "scan").unwrap_err();
        assert_eq!(err, "outpost could not scan this tree: not a repository");
    }

    #[test]
    fn a_shape_chock_does_not_know_names_the_command_that_printed_it() {
        let err = read::<Counted>(&said("not json", 0), "measure").unwrap_err();
        assert!(
            err.starts_with("outpost printed a measure chock cannot read"),
            "{err}"
        );
    }

    /// A missing root fails the spawn the same way a missing binary does.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn an_outpost_that_could_not_start_says_the_gate_needs_it_on_path() {
        let nowhere = std::path::Path::new("/nonexistent-chock-root");
        let err = spawn(nowhere, &["check"]).unwrap_err();
        assert!(err.starts_with("could not start outpost: "), "{err}");
        assert!(
            err.ends_with(" — this gate needs `outpost` on PATH"),
            "{err}"
        );
    }

    #[test]
    fn nothing_read_names_what_did_not_run() {
        assert_eq!(
            nothing_read("lens"),
            "outpost read no files, so no lens ran"
        );
    }
}
