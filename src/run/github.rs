//! What a run tells GitHub Actions: an annotation at each finding, which the pull request shows
//! beside the line, and a table of every gate on the job's summary page. Elsewhere it says nothing.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

use super::report::{Finding, GateReport, Grade, Run, Verdict};

/// GitHub keeps ten error annotations from one step; past that, one notice counts the rest.
const SHOWN: usize = 10;

/// Annotations on stderr, since `--json` owns stdout and the runner reads both, and the table
/// appended to the job summary.
#[expect(clippy::print_stderr, reason = "the annotations go to stderr")]
pub fn tell(run: &Run, root: &Path) {
    eprint!("{}", told(run, root, &|name| std::env::var_os(name)));
}

/// What `tell` writes on stderr, with the environment it reads given.
fn told(run: &Run, root: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> String {
    if env("GITHUB_ACTIONS").is_none_or(|it| it != "true") {
        return String::new();
    }
    let workspace = env("GITHUB_WORKSPACE");
    let prefix = workspace.map(|it| within(root, Path::new(&it)));
    let lines = annotations(run, &prefix.unwrap_or_default());
    let said: String = lines.into_iter().map(|line| line + "\n").collect();
    let Some(path) = env("GITHUB_STEP_SUMMARY") else {
        return said;
    };
    let opened = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path);
    let written = opened.and_then(|mut file| file.write_all(summary(run).as_bytes()));
    let failed = written
        .err()
        .map(|e| format!("chock: could not write the job summary: {e}\n"));
    said + &failed.unwrap_or_default()
}

/// Where `root` sits under the checkout, with `/` after each part, since an annotation names a
/// file from the checkout; empty when the root is the checkout or lies outside it.
fn within(root: &Path, workspace: &Path) -> String {
    let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let (root, workspace) = (real(root), real(workspace));
    let parts = |inside: &Path| {
        let names = inside
            .components()
            .map(|part| part.as_os_str().to_string_lossy());
        names.fold(String::new(), |mut joined, name| {
            let _ = write!(joined, "{name}/");
            joined
        })
    };
    root.strip_prefix(&workspace).map(parts).unwrap_or_default()
}

/// One workflow command for each finding of a tripped gate and for each gate that could not run.
fn annotations(run: &Run, prefix: &str) -> Vec<String> {
    let all: Vec<String> = run.gates.iter().flat_map(|it| about(it, prefix)).collect();
    let hidden = all.len().saturating_sub(SHOWN);
    let mut lines: Vec<String> = all.into_iter().take(SHOWN).collect();
    if hidden > 0 {
        lines.push(format!(
            "::notice title=chock::{hidden} more finding(s) are in the log, and the job summary \
             names each gate's command"
        ));
    }
    lines
}

fn about(report: &GateReport, prefix: &str) -> Vec<String> {
    let again = format!(" (run again: {})", report.rerun);
    let error = |at: &str, what: &str, said: &str| {
        let title = property(&format!("chock {}{what}", report.gate));
        format!("::error {at}title={title}::{}", data(said))
    };
    let at = |it: &Finding| match (it.file.as_str(), it.line) {
        ("", _) => String::new(),
        (file, line) => {
            let line = line.map(|line| format!("line={line},"));
            let file = property(&format!("{prefix}{file}"));
            format!("file={file},{}", line.unwrap_or_default())
        }
    };
    let said = |it: &Finding| match &it.item {
        Some(item) => format!("{item}: {}{again}", it.message),
        None => format!("{}{again}", it.message),
    };
    let findings = report.findings.iter().filter(|f| f.grade == Grade::Finding);
    let found: Vec<String> = findings.map(|it| error(&at(it), "", &said(it))).collect();
    match report.verdict {
        Verdict::Pass => Vec::new(),
        Verdict::CannotRun => {
            let reason = report.cannot_run_reason.as_deref();
            vec![error(
                "",
                " could not run",
                reason.unwrap_or("no reason given"),
            )]
        }
        Verdict::Tripped if found.is_empty() => vec![error("", "", &format!("tripped{again}"))],
        Verdict::Tripped => found,
    }
}

/// A command's message: the runner reads `%`, a carriage return and a line feed as escapes.
fn data(text: &str) -> String {
    let percent = text.replace('%', "%25");
    percent.replace('\r', "%0D").replace('\n', "%0A")
}

/// A property's value, where `:` and `,` also end it.
fn property(text: &str) -> String {
    data(text).replace(':', "%3A").replace(',', "%2C")
}

/// A markdown table of every gate, its result and its command, and the totals under it.
fn summary(run: &Run) -> String {
    let count = |verdict: Verdict| run.gates.iter().filter(|it| it.verdict == verdict).count();
    let row = |report: &GateReport| {
        let found = report.findings.iter().filter(|f| f.grade == Grade::Finding);
        let rerun = report.rerun.replace('|', "\\|");
        let (gate, verdict) = (&report.gate, report.verdict);
        format!("| `{gate}` | {verdict} | {} | `{rerun}` |\n", found.count())
    };
    let rows: String = run.gates.iter().map(row).collect();
    let (passed, tripped) = (count(Verdict::Pass), count(Verdict::Tripped));
    let stopped = count(Verdict::CannotRun);
    let head =
        "### chock\n\n| gate | result | findings | run again |\n| --- | --- | ---: | --- |\n";
    format!("{head}{rows}\n{passed} passed, {tripped} tripped, {stopped} could not run.\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tripped(findings: Vec<Finding>) -> GateReport {
        let mut report = GateReport::new("lint", Verdict::Tripped, "chock run lint");
        report.findings = findings;
        report
    }

    fn passed(gate: &str) -> GateReport {
        GateReport::new(gate, Verdict::Pass, &format!("chock run {gate}"))
    }

    #[test]
    fn a_finding_is_an_error_at_its_file_and_line_under_the_checkout() {
        let found = Finding::at("src/a.rs", "50% done,\nthen: more")
            .item("clippy::todo")
            .line(7);
        let mut stopped = GateReport::new("miri", Verdict::CannotRun, "chock run miri");
        stopped.cannot_run_reason = Some("miri is not installed".into());
        let gates = vec![
            passed("fmt"),
            tripped(vec![found, Finding::at("", "the build failed")]),
            stopped,
        ];
        let expected = [
            "::error file=crates/x/src/a.rs,line=7,title=chock lint::clippy::todo: 50%25 done,%0A\
             then: more (run again: chock run lint)",
            "::error title=chock lint::the build failed (run again: chock run lint)",
            "::error title=chock miri could not run::miri is not installed",
        ];
        assert_eq!(
            annotations(&Run::new("0.6.0", gates), "crates/x/"),
            expected
        );
    }

    #[test]
    fn a_tripped_gate_with_no_finding_still_says_so_and_too_many_end_in_a_count() {
        let run = Run::new("0.6.0", vec![tripped(Vec::new())]);
        let expected = ["::error title=chock lint::tripped (run again: chock run lint)"];
        assert_eq!(annotations(&run, ""), expected);
        let many = (1..=13)
            .map(|line| Finding::at("src/a.rs", "unused").line(line))
            .collect();
        let lines = annotations(&Run::new("0.6.0", vec![tripped(many)]), "");
        let last =
            "::error file=src/a.rs,line=10,title=chock lint::unused (run again: chock run lint)";
        assert_eq!(lines[SHOWN - 1], last);
        let notice = "::notice title=chock::3 more finding(s) are in the log, and the job summary \
                      names each gate's command";
        assert_eq!(lines[SHOWN..], [notice]);
    }

    #[test]
    fn the_summary_lists_each_gate_and_the_totals() {
        let gates = vec![
            passed("fmt"),
            tripped(vec![Finding::at("src/a.rs", "unused")]),
        ];
        let expected = "### chock\n\n| gate | result | findings | run again |\n| --- | --- | ---: | \
                        --- |\n| `fmt` | ok | 0 | `chock run fmt` |\n| `lint` | TRIPPED | 1 | `chock \
                        run lint` |\n\n1 passed, 1 tripped, 0 could not run.\n";
        assert_eq!(summary(&Run::new("0.6.0", gates)), expected);
    }

    #[test]
    fn a_root_is_named_from_the_checkout() {
        let workspace = Path::new("/work/repo");
        assert_eq!(
            within(Path::new("/work/repo/crates/x"), workspace),
            "crates/x/"
        );
        assert_eq!(within(workspace, workspace), "");
        assert_eq!(within(Path::new("/elsewhere"), workspace), "");
    }

    /// Inside Actions a run annotates and appends its table; elsewhere it writes nothing.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn only_a_run_inside_actions_writes_the_summary() {
        let dir = crate::testdir::make("github-summary");
        let table = dir.join("summary.md");
        let run = Run::new(
            "0.6.0",
            vec![tripped(vec![Finding::at("src/a.rs", "unused")])],
        );
        let told_in = |actions: &str, summary: &Path| {
            let env = |name: &str| match name {
                "GITHUB_ACTIONS" => Some(OsString::from(actions)),
                "GITHUB_WORKSPACE" => Some(dir.as_os_str().to_os_string()),
                "GITHUB_STEP_SUMMARY" => Some(summary.as_os_str().to_os_string()),
                _ => None,
            };
            told(&run, &dir, &env)
        };
        assert_eq!(told_in("false", &table), "");
        assert!(!table.exists(), "a run outside Actions writes no summary");
        let said = told_in("true", &table);
        let line = "::error file=src/a.rs,title=chock lint::unused (run again: chock run lint)\n";
        assert_eq!(said, line);
        assert_eq!(std::fs::read_to_string(&table).unwrap(), summary(&run));
        let no_summary = |name: &str| (name == "GITHUB_ACTIONS").then(|| OsString::from("true"));
        assert_eq!(
            told(&run, &dir, &no_summary),
            line,
            "no summary file is named"
        );
        let refused = told_in("true", &dir);
        assert!(refused.starts_with(line), "{refused}");
        assert!(
            refused.contains("could not write the job summary"),
            "{refused}"
        );
    }
}
