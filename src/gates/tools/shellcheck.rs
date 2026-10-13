//! What shellcheck says about each shell script the repository tracks, per file and check.

use std::io::Read;
use std::path::Path;

use crate::exec;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Inspection, Kind};

pub const GATE: Gate = Gate {
    name: "shellcheck",
    about: "what shellcheck says about each shell script the repository tracks, per file and check",
    // shellcheck is no crate, so `chock init --global` cannot install it for CI.
    group: Group::OptIn,
    builds: false,
    reads: None,
    kind: Kind::Debt {
        inspect,
        unit: "shellcheck finding(s)",
    },
};

/// The shells shellcheck reads; a script for any other is not its to judge.
const SHELLS: [&str; 5] = ["sh", "bash", "dash", "ksh", "busybox"];

/// Most scripts given to one shellcheck process, so no command line grows past the system's limit.
const BATCH: usize = 200;

fn inspect(ctx: &Ctx) -> Result<Inspection, String> {
    let tracked = crate::project::vcs::tracked(&ctx.root, ctx.vcs)?;
    let shellcheck = |args: &[&str]| exec::tool(&ctx.root, "shellcheck", args);
    checked(&ctx.root, tracked, &shellcheck).map(Inspection::debt)
}

/// What `shellcheck` says about the scripts among `tracked`, a batch at a time.
fn checked(
    root: &Path,
    tracked: Vec<String>,
    shellcheck: &dyn Fn(&[&str]) -> Result<exec::Output, String>,
) -> Result<Vec<Finding>, String> {
    let scripts: Vec<String> = tracked
        .into_iter()
        .filter(|shown| is_script(shown, &root.join(shown)))
        .collect();
    if scripts.is_empty() {
        return Err(
            "the repository tracks no shell script, so shellcheck has nothing to read".into(),
        );
    }
    let mut found = Vec::new();
    for batch in scripts.chunks(BATCH) {
        let mut args = vec!["--format=json1"];
        args.extend(batch.iter().map(String::as_str));
        found.extend(read(&shellcheck(&args)?)?);
    }
    Ok(found)
}

/// A `.sh` or `.bash` file, or a file with no extension whose first line starts one of `SHELLS`.
fn is_script(shown: &str, path: &Path) -> bool {
    let extension = Path::new(shown).extension().and_then(|it| it.to_str());
    match extension {
        Some(it) => matches!(it, "sh" | "bash"),
        None => first_line(path).is_some_and(|line| starts_a_shell(&line)),
    }
}

/// The first line of a file, read no further than a shebang can reach.
fn first_line(path: &Path) -> Option<String> {
    let mut head = Vec::new();
    let file = std::fs::File::open(path).ok()?;
    file.take(256).read_to_end(&mut head).ok()?;
    let text = String::from_utf8_lossy(&head);
    Some(text.lines().next()?.to_string())
}

/// `#!/bin/sh`, `#!/usr/bin/env bash` and the like.
fn starts_a_shell(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("#!") else {
        return false;
    };
    let mut words = rest.split_whitespace();
    let program = words.next().unwrap_or_default();
    let mut name = program.rsplit('/').next().unwrap_or_default();
    if name == "env" {
        name = words
            .find(|word| !word.starts_with('-'))
            .unwrap_or_default();
    }
    SHELLS.contains(&name)
}

#[derive(serde::Deserialize)]
struct Said {
    comments: Vec<Comment>,
}

#[derive(serde::Deserialize)]
struct Comment {
    file: String,
    line: u32,
    level: String,
    code: u32,
    message: String,
}

/// shellcheck exits 1 when it has something to say; any other failure means it read nothing.
fn read(out: &exec::Output) -> Result<Vec<Finding>, String> {
    if out.truncated {
        return Err("shellcheck said more than chock reads, so any count would be partial".into());
    }
    if !matches!(out.code, Some(0 | 1)) {
        return Err(format!("shellcheck failed: {}", out.why_it_failed()));
    }
    let said: Said = serde_json::from_str(out.stdout.trim())
        .map_err(|e| format!("shellcheck printed no report chock can read: {e}"))?;
    Ok(said
        .comments
        .into_iter()
        .map(|it| {
            let message = format!("{}: {}", it.level, it.message);
            Finding::at(&it.file, &message)
                .item(&format!("SC{}", it.code))
                .line(it.line)
        })
        .collect())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn a_shebang_names_a_shell_directly_or_through_env() {
        let shells = "#!/bin/sh\n#!/bin/bash -eu\n#!/usr/bin/env bash\n#!/usr/bin/env -S bash -e\n\
                      #! /bin/dash";
        for line in shells.lines() {
            assert!(starts_a_shell(line), "{line}");
        }
        for line in "#!/usr/bin/env python3\n#!/bin/zsh\n# bash\nbash".lines() {
            assert!(!starts_a_shell(line), "{line}");
        }
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_script_is_named_for_its_shell_or_starts_one() {
        let root = crate::testdir::tree(
            "shellcheck-scripts",
            &[
                ("bin/run", "#!/usr/bin/env bash\necho hi\n"),
                ("bin/tool.py", "#!/bin/sh\n"),
                ("README", "A script starts with #!/bin/sh\n"),
                ("ci.sh", "echo no shebang\n"),
            ],
        );
        let picked: Vec<&str> = ["bin/run", "bin/tool.py", "README", "ci.sh"]
            .into_iter()
            .filter(|shown| is_script(shown, &root.join(shown)))
            .collect();
        assert_eq!(picked, ["bin/run", "ci.sh"]);
    }

    #[test]
    fn each_comment_is_a_finding_at_its_file_line_and_code() {
        let json = r#"{"comments":[{"file":"bin/run","line":3,"endLine":3,"column":6,"endColumn":8,"level":"warning","code":2086,"message":"Double quote to prevent globbing.","fix":null}]}"#;
        let found = read(&exec::Output::of(Some(1), json, "")).unwrap();
        let expected = Finding::at("bin/run", "warning: Double quote to prevent globbing.")
            .item("SC2086")
            .line(3);
        assert_eq!(found, [expected]);
        let clean = read(&exec::Output::of(Some(0), r#"{"comments":[]}"#, "")).unwrap();
        assert_eq!(
            clean,
            Vec::<Finding>::new(),
            "nothing said is nothing found"
        );
    }

    /// Only the scripts go to shellcheck, and what it says about each comes back.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_tracked_scripts_go_to_shellcheck_and_none_is_no_count() {
        let root = crate::testdir::tree(
            "shellcheck-checked",
            &[("bin/run.sh", "echo $1\n"), ("src/lib.rs", "")],
        );
        let tracked = || vec!["bin/run.sh".to_string(), "src/lib.rs".to_string()];
        let json = r#"{"comments":[{"file":"bin/run.sh","line":1,"level":"info","code":2086,"message":"Double quote."}]}"#;
        let asked = std::cell::RefCell::new(Vec::new());
        let fake = |args: &[&str]| {
            asked.borrow_mut().push(args.join(" "));
            Ok(exec::Output::of(Some(1), json, ""))
        };
        let found = checked(&root, tracked(), &fake).unwrap();
        let expected = Finding::at("bin/run.sh", "info: Double quote.")
            .item("SC2086")
            .line(1);
        assert_eq!(found, [expected]);
        assert_eq!(*asked.borrow(), ["--format=json1 bin/run.sh"]);
        let refused = |_: &[&str]| Err("shellcheck is not installed".to_string());
        assert_eq!(
            checked(&root, tracked(), &refused).unwrap_err(),
            "shellcheck is not installed"
        );
        let none = checked(&root, vec!["src/lib.rs".into()], &fake).unwrap_err();
        assert!(none.contains("tracks no shell script"), "{none}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_repository_with_no_script_is_no_count() {
        let dir = crate::testdir::repo("shellcheck-none", &[("src/lib.rs", "")]);
        let baseline = crate::run::baseline::Baseline::empty("0.1.0");
        let mut ctx = Ctx::for_root(dir.to_path_buf(), baseline);
        ctx.vcs = Some(crate::project::vcs::Kind::Git);
        let said = inspect(&ctx).err().unwrap_or_default();
        assert!(said.contains("tracks no shell script"), "{said}");
    }

    #[test]
    fn a_shellcheck_that_could_not_read_is_no_count() {
        let refused = read(&exec::Output::of(Some(3), "", "bad option"));
        assert!(refused.unwrap_err().starts_with("shellcheck failed"));
        let garbled = read(&exec::Output::of(Some(1), "In bin/run line 3:", ""));
        assert!(garbled.unwrap_err().contains("no report"));
        let mut cut = exec::Output::of(Some(1), r#"{"comments":["#, "");
        cut.truncated = true;
        assert!(read(&cut).unwrap_err().contains("partial"));
    }
}
