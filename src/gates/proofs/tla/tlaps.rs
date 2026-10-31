//! The TLAPS half of the `tla` gate: tlapm proves every obligation of each module that states a
//! theorem, and no proof in it is left out.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{SCRATCH, holders_under, limit_of, names_in, relative, uncommented};
use crate::exec;
use crate::gates::proofs::Run;
use crate::run::Ctx;
use crate::run::report::Finding;

/// How tlapm starts the line that names the error it stopped on.
const ABNORMAL: &str = "tlapm ending abnormally with ";

/// The words that state what tlapm must prove.
const STATED: [&str; 4] = ["THEOREM", "LEMMA", "PROPOSITION", "COROLLARY"];

/// The directory under the gate's scratch that holds tlapm's caches. tlapm makes its cache
/// directory, but not the directory that holds it.
pub(super) const CACHES: &str = "tlaps";

pub(super) const UNPINNED: &str = "TLAPM is not pinned in tool-versions.env, so no theorem was \
                                   proved; pin TLAPM_VERSION with its URL and SHA256 to prove them";

/// One module tlapm reads twice: its summary names each proof left out, and its proof run each
/// obligation that no backend proved.
pub(super) struct Module {
    file: String,
    pub(super) model: String,
    limit: u64,
}

/// The TLAPS checks of a run: each module, tlapm, its two runs per module, and the notes.
#[derive(Default)]
pub(super) struct Proofs {
    pub(super) modules: Vec<Module>,
    pub(super) program: String,
    pub(super) runs: Vec<Run>,
    pub(super) notes: Vec<Finding>,
}

/// The tlapm runs of each module under `dirs` that states a theorem. A project that pins no tlapm
/// gets a note, as one that pins no Apalache does.
pub(super) fn theorems(ctx: &Ctx, dirs: &[String], workers: u32) -> Result<Proofs, String> {
    let mut holders = Vec::new();
    for dir in dirs {
        holders_under(&ctx.root.join(dir), ".tla", 4, &mut holders);
    }
    let mut stating: Vec<(PathBuf, String)> = Vec::new();
    for dir in holders {
        for name in names_in(&dir) {
            let text = std::fs::read_to_string(dir.join(&name)).unwrap_or_default();
            let asked = |stem: &&str| ctx.scope.asks_model(stem) && states(&text);
            if let Some(stem) = name.strip_suffix(".tla").filter(asked) {
                stating.push((dir.clone(), stem.to_string()));
            }
        }
    }
    if stating.is_empty() {
        return Ok(Proofs::default());
    }
    let home = ctx.tools.home.as_deref();
    let program = match crate::setup::downloads::located(&ctx.root, home, "TLAPM", "tlapm") {
        Ok(program) => program.to_string_lossy().into_owned(),
        Err(why) if why.ends_with(crate::setup::downloads::NOT_PINNED) => {
            return Ok(unrun(UNPINNED));
        }
        Err(why) if why.ends_with(crate::setup::downloads::NO_FILE) => {
            return Ok(unrun(&format!("{why}; no theorem was proved")));
        }
        Err(why) => return Err(why),
    };
    let scratch = ctx.root.join(SCRATCH).join(CACHES);
    let threads = workers.to_string();
    let mut proofs = Proofs {
        program,
        ..Proofs::default()
    };
    for (n, (dir, stem)) in stating.into_iter().enumerate() {
        let limit = limit_of(&ctx.tla, &stem, &stem);
        let file = format!("{stem}.tla");
        let cache = |run: &str| {
            scratch
                .join(format!("{n}-{run}"))
                .to_string_lossy()
                .into_owned()
        };
        let summary = ["--summary", "--cache-dir", &cache("summary"), &file];
        let (proof_cache, threads) = (cache("proof"), threads.as_str());
        let proof = [
            "--toolbox",
            "0",
            "0",
            "--threads",
            threads,
            "--cache-dir",
            &proof_cache,
            &file,
        ];
        for argv in [&summary[..], &proof[..]] {
            let argv = argv.iter().map(|arg| (*arg).to_string()).collect();
            let dir = dir.clone();
            proofs.runs.push(Run { dir, argv, limit });
        }
        let file = relative(&ctx.root, &dir.join(&file));
        proofs.modules.push(Module {
            file,
            model: stem,
            limit,
        });
    }
    Ok(proofs)
}

/// No proof run, with the note on the pin file that says why.
fn unrun(why: &str) -> Proofs {
    Proofs {
        notes: vec![Finding::at(crate::project::PIN_FILE, why)],
        ..Proofs::default()
    }
}

/// Whether TLA+ text states a theorem outside its comments.
fn states(tla: &str) -> bool {
    let text = uncommented(tla);
    let mut words = text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_');
    words.any(|word| STATED.contains(&word))
}

type Out = Result<exec::Output, exec::ExecError>;

/// Where a run went wrong, by line where tlapm names one, and how.
type Wrong = (Option<u32>, String);

/// What the runs of each module say is wrong: each module has two runs in turn, its summary, then
/// its proof.
pub(super) fn judged_each(modules: &[Module], proved: Vec<Out>) -> Result<Vec<Finding>, String> {
    let mut proved = proved.into_iter();
    let pairs = std::iter::from_fn(|| proved.next().zip(proved.next()));
    let mut findings = Vec::new();
    for (module, (summary, proof)) in modules.iter().zip(pairs) {
        findings.extend(module.judged(summary, proof)?);
    }
    Ok(findings)
}

impl Module {
    /// What the summary and the proof run of this module say is wrong with its proofs.
    pub(super) fn judged(&self, summary: Out, proof: Out) -> Result<Vec<Finding>, String> {
        let left = self.ran(summary)?.map(|out| left_out(&out.stdout));
        let failed = self.ran(proof)?.map(|out| unproved(&out.stderr));
        let said = match (left, failed) {
            (Err((line, why)), _) | (_, Err((line, why))) => return Ok(vec![self.at(line, &why)]),
            (Ok(Some(left)), Ok(Some(failed))) => left.into_iter().chain(failed),
            _ => return Ok(vec![self.at(None, "tlapm gave no verdict on its proofs")]),
        };
        Ok(said.map(|(line, why)| self.at(Some(line), &why)).collect())
    }

    /// A run's output, or why it has none: tlapm's own error, or its time limit.
    fn ran(&self, out: Out) -> Result<Result<exec::Output, Wrong>, String> {
        let out = match out {
            Ok(out) => out,
            Err(e) if e.hung() => {
                let why = format!("tlapm ran past its {} s limit", self.limit);
                return Ok(Err((None, why)));
            }
            Err(e) => return Err(e.to_string()),
        };
        if out.code == Some(0) {
            return Ok(Ok(out));
        }
        // tlapm names an error's place on one line and the error on the next, or ends with the
        // error it stopped on; else its last line names it.
        let lines: Vec<&str> = out
            .stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        let file = |line: &str| line.starts_with("File \"");
        let placed = lines
            .windows(2)
            .find(|pair| file(pair[0]) && !file(pair[1]));
        let line = placed.and_then(|pair| number_after(pair[0], ", line "));
        let said = placed
            .map(|pair| pair[1])
            .or_else(|| {
                lines
                    .iter()
                    .copied()
                    .find(|line| line.starts_with(ABNORMAL))
            })
            .or_else(|| lines.last().copied())
            .unwrap_or("no output");
        let exit = out
            .code
            .map_or("a signal".to_string(), |code| format!("exit {code}"));
        let said: String = said.chars().take(240).collect();
        Ok(Err((
            line,
            format!("tlapm could not check it: {exit}: {said}"),
        )))
    }

    fn at(&self, line: Option<u32>, why: &str) -> Finding {
        let finding = Finding::at(&self.file, why).item(&self.model);
        match line {
            Some(line) => finding.line(line),
            None => finding,
        }
    }
}

/// How the summary names a proof left out, and what that means.
const LEFT_OUT: [(&str, &str); 2] = [
    (
        "omitted_proof_",
        "an OMITTED proof, which TLAPS does not check",
    ),
    (
        "missing_proof_",
        "a step with no proof, which TLAPS does not check",
    ),
];

/// Each proof the summary names as OMITTED or as missing, by line; `None` without a summary.
fn left_out(summary: &str) -> Option<Vec<(u32, String)>> {
    let found = summary.lines().filter_map(|line| {
        let (_, why) = LEFT_OUT
            .iter()
            .find(|(head, _)| line.trim().starts_with(head))?;
        Some((number_after(line, " at line ")?, why.to_string()))
    });
    let summarised = summary.contains("---- summary of module");
    summarised.then(|| found.collect())
}

/// The number that follows `marker` in `text`.
fn number_after(text: &str, marker: &str) -> Option<u32> {
    let after = text.split_once(marker)?.1;
    let digits = after.split(|c: char| !c.is_ascii_digit()).next()?;
    digits.parse().ok()
}

/// One obligation the proof run reported: where it starts, whether a backend proved it, what it
/// says, and why each backend that tried gave up.
#[derive(Default)]
struct Obligation {
    line: u32,
    proved: bool,
    text: String,
    reasons: Vec<String>,
}

/// Each obligation that no backend proved, by line; `None` where the run counted no obligations.
fn unproved(toolbox: &str) -> Option<Vec<(u32, String)>> {
    let mut counted = false;
    let mut obligations: BTreeMap<u32, Obligation> = BTreeMap::new();
    for message in toolbox.split("@!!BEGIN").skip(1) {
        let body = message.split("@!!END").next().unwrap_or_default();
        let (fields, text) = body.split_once("@!!obl:").unwrap_or((body, ""));
        let field = |name: &str| {
            let wanted = format!("@!!{name}:");
            let mut lines = fields.lines();
            lines
                .find_map(|line| line.strip_prefix(&wanted))
                .unwrap_or_default()
        };
        counted |= field("type") == "obligationsnumber";
        let Ok(id) = field("id").parse::<u32>() else {
            continue;
        };
        let it = obligations.entry(id).or_default();
        let line = field("loc")
            .split(':')
            .next()
            .and_then(|line| line.parse().ok());
        it.line = line.unwrap_or(it.line);
        match field("status") {
            "proved" | "trivial" => it.proved = true,
            "failed" => it
                .reasons
                .push(format!("{}: {}", field("prover"), field("reason"))),
            _ => {}
        }
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !text.is_empty() {
            it.text = text.chars().take(200).collect();
        }
    }
    let left = obligations.into_values().filter(|it| !it.proved);
    let said = left.map(|it| {
        let why = match it.text.as_str() {
            "" => "an obligation that no backend proved".to_string(),
            text => format!("no backend proved `{text}`"),
        };
        if it.reasons.is_empty() {
            (it.line, why)
        } else {
            (it.line, format!("{why} ({})", it.reasons.join(", ")))
        }
    });
    counted.then(|| said.collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tlapm's toolbox messages: one obligation proved, and one that two backends failed.
    const TOOLBOX: &str = "@!!BEGIN\n@!!type:obligation\n@!!id:1\n@!!loc:4:3:4:10\n\
@!!status:to be proved\n@!!END\n\n@!!BEGIN\n@!!type:obligation\n@!!id:2\n@!!loc:6:3:6:10\n\
@!!status:to be proved\n@!!END\n\n@!!BEGIN\n@!!type:obligationsnumber\n@!!count:2\n@!!END\n\n\
@!!BEGIN\n@!!type:obligation\n@!!id:1\n@!!loc:4:3:4:10\n@!!status:proved\n@!!prover:smt\n@!!END\n\n\
@!!BEGIN\n@!!type:obligation\n@!!id:2\n@!!loc:6:3:6:10\n@!!status:failed\n@!!prover:smt\n\
@!!reason:timeout\n@!!obl:\n1 + 1\n  = 3\n\n@!!END\n\n@!!BEGIN\n@!!type:obligation\n@!!id:2\n\
@!!loc:6:3:6:10\n@!!status:failed\n@!!prover:zenon\n@!!reason:false\n@!!obl:\n1 + 1 = 3\n\n@!!END\n";

    /// tlapm's summary of a module with one OMITTED proof and one step with no proof.
    const SUMMARY: &str = "---- summary of module \"Probe\" ----\n  obligations_count = 10\n  \
---- incomplete proof of theorem at line 7, character 1 ----\n    omitted_proofs_count = 1\n    \
omitted_proof_1 at line 8, characters 3-9\n  ====\n  ---- incomplete proof of theorem at line 10, \
character 1 ----\n    missing_proof_2 at line 11, characters 1-11\n  ====\n====\n";

    fn module() -> Module {
        let (file, model) = ("proofs/tla/Probe.tla".to_string(), "Probe".to_string());
        Module {
            file,
            model,
            limit: 300,
        }
    }

    fn ran(code: i32, stdout: &str, stderr: &str) -> exec::Output {
        exec::Output::of(Some(code), stdout, stderr)
    }

    #[test]
    fn each_unproved_obligation_and_each_proof_left_out_is_named_by_its_line() {
        let runs = vec![Ok(ran(0, SUMMARY, "")), Ok(ran(0, "", TOOLBOX))];
        let found = judged_each(&[module()], runs).unwrap();
        let said: Vec<String> = found.iter().map(Finding::render).collect();
        assert_eq!(
            said,
            [
                "proofs/tla/Probe.tla:8: Probe: an OMITTED proof, which TLAPS does not check",
                "proofs/tla/Probe.tla:11: Probe: a step with no proof, which TLAPS does not check",
                "proofs/tla/Probe.tla:6: Probe: no backend proved `1 + 1 = 3` (smt: timeout, zenon: false)",
            ]
        );
        let clean = "@!!BEGIN\n@!!type:obligationsnumber\n@!!count:0\n@!!END\n";
        let summary = "---- summary of module \"A\" ----\n====\n";
        assert_eq!(
            module().judged(Ok(ran(0, summary, "")), Ok(ran(0, "", clean))),
            Ok(Vec::new())
        );
    }

    #[test]
    fn a_run_that_gave_no_verdict_trips_with_tlapm_s_reason() {
        let broken = "File \"./Probe.tla\", line 2, character 13\nincomplete expression\n\
File \"<unknown>\":\nError: Could not parse \"./Probe.tla\" successfully.\n";
        let reason = |summary: Out, proof: Out| {
            let found = module().judged(summary, proof).unwrap();
            found.iter().map(Finding::render).collect::<Vec<_>>()
        };
        assert_eq!(
            reason(Ok(ran(3, "", broken)), Ok(ran(3, "", broken))),
            [
                "proofs/tla/Probe.tla:2: Probe: tlapm could not check it: exit 3: incomplete expression"
            ]
        );
        let stopped = "Could not create the cache directory\n tlapm ending abnormally with \
Unix.Unix_error(Unix.ENOENT, \"mkdir\", \"c/P.tlaps\")\nCalled from Tlapm_lib.init\n";
        let abnormal = "proofs/tla/Probe.tla: Probe: tlapm could not check it: exit 3: tlapm \
ending abnormally with Unix.Unix_error(Unix.ENOENT, \"mkdir\", \"c/P.tlaps\")";
        assert_eq!(
            reason(Ok(ran(3, "", stopped)), Ok(ran(0, "", ""))),
            [abnormal]
        );
        assert_eq!(
            reason(Ok(ran(0, SUMMARY, "")), Ok(ran(0, "", "no messages"))),
            ["proofs/tla/Probe.tla: Probe: tlapm gave no verdict on its proofs"]
        );
        let hung = Err(exec::ExecError::of("tlapm", exec::Stage::Hung, "gone"));
        assert_eq!(
            reason(Ok(ran(0, SUMMARY, "")), hung),
            ["proofs/tla/Probe.tla: Probe: tlapm ran past its 300 s limit"]
        );
        let gone = exec::ExecError::of("tlapm", exec::Stage::Spawn, "gone");
        let said = gone.to_string();
        assert_eq!(
            module().judged(Ok(ran(0, SUMMARY, "")), Err(gone)),
            Err(said)
        );
    }

    #[test]
    fn each_proof_the_summary_leaves_out_is_named_by_its_line() {
        let omitted = (8, LEFT_OUT[0].1.to_string());
        let missing = (11, LEFT_OUT[1].1.to_string());
        assert_eq!(left_out(SUMMARY), Some(vec![omitted, missing]));
        assert_eq!(left_out("omitted_proof_1 at line 8"), None);
    }

    #[test]
    fn each_obligation_no_backend_proved_is_named_by_its_line() {
        let why = "no backend proved `1 + 1 = 3` (smt: timeout, zenon: false)";
        assert_eq!(unproved(TOOLBOX), Some(vec![(6, why.to_string())]));
        let untold = "@!!BEGIN\n@!!type:obligationsnumber\n@!!count:2\n@!!END\n\n@!!BEGIN\n\
@!!type:obligation\n@!!id:1\n@!!loc:5:1:5:2\n@!!status:failed\n@!!prover:smt\n@!!reason:timeout\n\
@!!END\n@!!BEGIN\n@!!type:obligation\n@!!id:2\n@!!loc:7:1:7:2\n@!!status:interrupted\n@!!END\n";
        let why = "an obligation that no backend proved";
        let named = vec![(5, format!("{why} (smt: timeout)")), (7, why.to_string())];
        assert_eq!(unproved(untold), Some(named));
        assert_eq!(
            unproved("@!!BEGIN\n@!!type:obligation\n@!!id:1\n@!!END\n"),
            None
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_module_that_states_a_theorem_is_proved_where_tlapm_is_pinned() {
        let files = [
            ("proofs/tla/Probe.tla", "THEOREM T == TRUE\n"),
            ("proofs/tla/Plain.tla", "(* THEOREM *)\n"),
        ];
        let mut ctx = crate::testdir::Held::tree("tlaps-theorems", &files);
        ctx.tools = crate::testdir::faked(&ctx.root, "exit 0\n");
        let dirs = ["proofs/tla".to_string()];
        let notes = theorems(&ctx, &dirs, 3).unwrap().notes;
        let shown: Vec<(&str, &str)> = notes
            .iter()
            .map(|it| (it.file.as_str(), it.message.as_str()))
            .collect();
        assert_eq!(shown, [(crate::project::PIN_FILE, UNPINNED)]);
        let sha = "c".repeat(64);
        let other = if cfg!(windows) { "linux" } else { "windows" };
        let elsewhere = format!(
            "TLAPM_VERSION=1\nTLAPM_URL_{other}=https://x/tlapm\nTLAPM_SHA256_{other}={sha}\n"
        );
        std::fs::write(ctx.root.join(crate::project::PIN_FILE), elsewhere).unwrap();
        let unrun = theorems(&ctx, &dirs, 3).unwrap();
        let why = &unrun.notes[0].message;
        assert!(
            why.ends_with("so it cannot run on this machine; no theorem was proved"),
            "{why}"
        );
        let unfetched = format!("TLAPM_VERSION=1\nTLAPM_URL=https://x/tlapm\nTLAPM_SHA256={sha}\n");
        std::fs::write(ctx.root.join(crate::project::PIN_FILE), unfetched).unwrap();
        let not_fetched = crate::gates::fixes::not_fetched("tlapm");
        assert_eq!(theorems(&ctx, &dirs, 3).err(), Some(not_fetched));
        let pin = crate::testdir::fetched(&ctx.root, "TLAPM", "tlapm");
        std::fs::write(ctx.root.join(crate::project::PIN_FILE), pin).unwrap();
        let pinned = theorems(&ctx, &dirs, 3).unwrap();
        let cache = |run: &str| {
            ctx.root
                .join(SCRATCH)
                .join(CACHES)
                .join(run)
                .display()
                .to_string()
        };
        let said: Vec<String> = pinned.runs.iter().map(|run| run.argv.join(" ")).collect();
        assert_eq!(
            said,
            [
                format!("--summary --cache-dir {} Probe.tla", cache("0-summary")),
                format!(
                    "--toolbox 0 0 --threads 3 --cache-dir {} Probe.tla",
                    cache("0-proof")
                ),
            ]
        );
        let modules: Vec<&str> = pinned.modules.iter().map(|it| it.file.as_str()).collect();
        assert_eq!(modules, ["proofs/tla/Probe.tla"]);
    }

    #[test]
    fn a_module_states_a_theorem_only_outside_its_comments() {
        assert!(states("---- MODULE A ----\nLEMMA X == TRUE\n===="));
        let commented = "---- MODULE A ----\n\\* THEOREM X\n(* LEMMA *)\nTHEOREMS == 1\n====";
        assert!(!states(commented));
    }
}
