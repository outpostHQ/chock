//! Miri as `chock init --global` installs it and `chock doctor` judges it. Miri is no crate: it is
//! a part of the `nightly` toolchain, and only rustup installs it.

use crate::exec::{self, Start};
use crate::run::report::Verdict;
use std::path::Path;

/// The name of the row in `chock doctor`, and of the check that needs it.
pub const NAME: &str = "miri";

const LISTED: [&str; 5] = ["component", "list", "--toolchain", "nightly", "--installed"];

/// Miri, and the library source it builds its sysroot from at first use.
const PARTS: [&str; 2] = ["miri", "rust-src"];

const TOOLCHAIN: [&str; 6] = [
    "toolchain",
    "install",
    "nightly",
    "--profile",
    "minimal",
    "--no-self-update",
];

const ADD: [&str; 6] = [
    "component",
    "add",
    "--toolchain",
    "nightly",
    "miri",
    "rust-src",
];

/// What this machine's `nightly` has of the parts.
#[derive(Debug, PartialEq, Eq)]
enum Have {
    Both,
    Part,
    /// rustup did not list the parts, and why: most often there is no `nightly`.
    Unlisted(String),
    NoRustup(String),
}

fn have(start: Start, cwd: &Path) -> Have {
    match start("rustup", &LISTED, cwd, &[]) {
        Err(why) if why.stage == exec::Stage::Spawn => Have::NoRustup(why.to_string()),
        Err(why) => Have::Unlisted(why.to_string()),
        Ok(out) if !out.success() => Have::Unlisted(out.why_it_failed().to_string()),
        Ok(out) if complete(&out.stdout) => Have::Both,
        Ok(_) => Have::Part,
    }
}

fn complete(listing: &str) -> bool {
    PARTS.iter().all(|part| {
        // `miri-x86_64-unknown-linux-gnu` names `miri`; `rust-src` has no host in its name.
        let mut hosts = listing
            .lines()
            .filter_map(|line| line.trim().strip_prefix(*part));
        hosts.any(|host| host.is_empty() || host.starts_with('-'))
    })
}

/// Adds what is missing; an installed `nightly` keeps its date. `Ok` and `Err` both hold the line
/// to print; `Err` is an install that failed.
pub fn install(start: Start, cwd: &Path) -> Result<String, String> {
    let steps: &[&[&str]] = match have(start, cwd) {
        Have::Both => return Ok(format!("  current   {NAME} — on the `nightly` toolchain")),
        Have::NoRustup(why) => {
            return Ok(format!(
                "  skipped   {NAME} — only rustup installs it, and chock {why}"
            ));
        }
        Have::Part => &[&ADD],
        Have::Unlisted(_) => &[&TOOLCHAIN, &ADD],
    };
    steps
        .iter()
        .try_for_each(|args| exec::printed(start, "rustup", args, cwd, &[]).map(drop))
        .map(|()| format!("  installed {NAME} — on the `nightly` toolchain"))
        .map_err(|why| format!("  FAILED    {NAME} — {why}"))
}

/// The verdict and the words of Miri's row in `chock doctor`.
pub fn standing(start: Start, cwd: &Path) -> (Verdict, String) {
    const REPAIR: &str = "run `chock init --global`";
    match have(start, cwd) {
        Have::Both => (
            Verdict::Pass,
            "on the `nightly` toolchain, with `rust-src`".to_string(),
        ),
        Have::Part => (
            Verdict::Tripped,
            format!("the `nightly` toolchain lacks `miri` or `rust-src`: {REPAIR}"),
        ),
        Have::Unlisted(why) => (Verdict::Tripped, format!("{why}: {REPAIR}")),
        Have::NoRustup(why) => (Verdict::CannotRun, format!("chock {why}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const BOTH: &str = "cargo-x86_64-unknown-linux-gnu\nmiri-x86_64-unknown-linux-gnu\nrust-src\nrustc-x86_64-unknown-linux-gnu\n";
    const NO_NIGHTLY: &str = "error: toolchain 'nightly-x86_64-unknown-linux-gnu' is not installed\nhelp: run `rustup toolchain install nightly-x86_64-unknown-linux-gnu` to install it\n";
    const LIST: &str = "rustup component list --toolchain nightly --installed";
    const INSTALL: &str = "rustup toolchain install nightly --profile minimal --no-self-update";
    const ADDING: &str = "rustup component add --toolchain nightly miri rust-src";

    fn output(code: i32, stdout: &str, stderr: &str) -> exec::Output {
        exec::Output::of(Some(code), stdout, stderr)
    }

    fn unstarted(stage: exec::Stage) -> exec::ExecError {
        exec::ExecError {
            program: "rustup".to_string(),
            stage,
            reason: "no answer".to_string(),
        }
    }

    /// What one rustup call gives back.
    type Answer = Result<exec::Output, exec::ExecError>;

    /// A rustup whose listing is `listed`, and whose other commands pass unless they hold `failing`.
    fn rustup<'a>(
        log: &'a RefCell<Vec<String>>,
        listed: &'a Answer,
        failing: &'a str,
    ) -> impl Fn(&str, &[&str], &Path, &[(&str, &str)]) -> Answer + 'a {
        move |program: &str, args: &[&str], _cwd: &Path, _env: &[(&str, &str)]| {
            let line = format!("{program} {}", args.join(" "));
            log.borrow_mut().push(line.clone());
            match (line == LIST, line.contains(failing)) {
                (true, _) => listed.clone(),
                (false, true) => Ok(output(1, "", "error: no download\n")),
                (false, false) => Ok(output(0, "", "")),
            }
        }
    }

    /// The line `install` prints and the commands it ran for one listing.
    fn installed(listed: &Answer, failing: &str) -> (Result<String, String>, Vec<String>) {
        let log = RefCell::new(Vec::new());
        let done = install(&rustup(&log, listed, failing), Path::new("."));
        (done, log.into_inner())
    }

    fn judged(listed: &Answer) -> (Verdict, String) {
        let log = RefCell::new(Vec::new());
        standing(&rustup(&log, listed, "nothing fails"), Path::new("."))
    }

    #[test]
    fn a_part_is_named_by_its_own_line_with_or_without_a_host() {
        assert!(complete(BOTH));
        assert!(complete("  miri\n  rust-src\n"));
        assert!(!complete(&BOTH.replace("rust-src", "rust-std")));
        assert!(!complete(&BOTH.replace("miri-", "miriam-")));
        assert!(!complete("rust-src\n"));
        assert!(!complete(""));
    }

    #[test]
    fn a_nightly_with_both_parts_is_left_as_it_is() {
        let (done, ran) = installed(&Ok(output(0, BOTH, "")), "nothing fails");
        assert_eq!(
            done,
            Ok("  current   miri — on the `nightly` toolchain".to_string())
        );
        assert_eq!(ran, [LIST]);
    }

    #[test]
    fn a_nightly_that_lacks_a_part_gets_the_parts_and_keeps_its_date() {
        let partial = BOTH.replace("rust-src\n", "");
        let (done, ran) = installed(&Ok(output(0, &partial, "")), "nothing fails");
        assert_eq!(
            done,
            Ok("  installed miri — on the `nightly` toolchain".to_string())
        );
        assert_eq!(ran, [LIST, ADDING]);
    }

    #[test]
    fn a_machine_with_no_nightly_gets_the_toolchain_and_then_the_parts() {
        for listed in [
            Ok(output(1, "", NO_NIGHTLY)),
            Err(unstarted(exec::Stage::Hung)),
        ] {
            let (done, ran) = installed(&listed, "nothing fails");
            assert_eq!(
                done,
                Ok("  installed miri — on the `nightly` toolchain".to_string())
            );
            assert_eq!(ran, [LIST, INSTALL, ADDING]);
        }
    }

    #[test]
    fn a_step_rustup_refuses_fails_the_install_and_ends_it() {
        let stderr = "stderr (bounded output):\nerror: no download\n";
        let (done, ran) = installed(&Ok(output(1, "", NO_NIGHTLY)), "toolchain install");
        assert_eq!(
            done,
            Err(format!(
                "  FAILED    miri — `{INSTALL}` failed: error: no download\n{stderr}"
            ))
        );
        assert_eq!(ran, [LIST, INSTALL]);
        let (done, ran) = installed(&Ok(output(0, "", "")), "component add");
        assert_eq!(
            done,
            Err(format!(
                "  FAILED    miri — `{ADDING}` failed: error: no download\n{stderr}"
            ))
        );
        assert_eq!(ran, [LIST, ADDING]);
    }

    #[test]
    fn a_machine_with_no_rustup_is_told_so_and_that_is_no_failed_install() {
        let (done, ran) = installed(&Err(unstarted(exec::Stage::Spawn)), "nothing fails");
        assert_eq!(
            done,
            Ok("  skipped   miri — only rustup installs it, and chock could not start rustup: no answer".to_string())
        );
        assert_eq!(ran, [LIST]);
    }

    #[test]
    fn the_row_in_doctor_names_what_is_missing_and_the_install_that_adds_it() {
        assert_eq!(
            judged(&Ok(output(0, BOTH, ""))),
            (
                Verdict::Pass,
                "on the `nightly` toolchain, with `rust-src`".to_string()
            )
        );
        assert_eq!(
            judged(&Ok(output(0, "rust-src\n", ""))),
            (
                Verdict::Tripped,
                "the `nightly` toolchain lacks `miri` or `rust-src`: run `chock init --global`"
                    .to_string()
            )
        );
        assert_eq!(
            judged(&Ok(output(1, "", NO_NIGHTLY))),
            (
                Verdict::Tripped,
                "error: toolchain 'nightly-x86_64-unknown-linux-gnu' is not installed: run `chock init --global`".to_string()
            )
        );
        assert_eq!(
            judged(&Err(unstarted(exec::Stage::Hung))),
            (
                Verdict::Tripped,
                "gave up waiting for rustup: no answer: run `chock init --global`".to_string()
            )
        );
        assert_eq!(
            judged(&Err(unstarted(exec::Stage::Spawn))),
            (
                Verdict::CannotRun,
                "chock could not start rustup: no answer".to_string()
            )
        );
    }
}
