//! The oldest Rust a crate promises, as `chock init --global` installs it for the `msrv` gate. It
//! is no pin: the crate's own `rust-version` names it, and only rustup installs it.

use crate::exec::{self, Start};
use std::path::Path;

/// The name of the gate that needs it.
pub const NAME: &str = "msrv";

/// Installs the release that the crate's `rust-version` promises, or `None` with no promise. `Ok`
/// and `Err` both hold the line to print; `Err` is an install that failed.
pub fn install(start: Start, root: Option<&Path>) -> Option<Result<String, String>> {
    let root = root?;
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let version = crate::gates::tools::rust_version(&manifest)?;
    Some(promised(start, &version, root))
}

fn promised(start: Start, version: &str, cwd: &Path) -> Result<String, String> {
    let args = [
        "toolchain",
        "install",
        version,
        "--profile",
        "minimal",
        "--no-self-update",
    ];
    let about = format!("{NAME} — Rust {version}, the oldest release that Cargo.toml promises");
    match start("rustup", &args, cwd, &[]) {
        Err(why) if why.stage == exec::Stage::Spawn => Ok(format!(
            "  skipped   {about}: only rustup installs it, and chock {why}"
        )),
        Err(why) => Err(format!("  FAILED    {about}: {why}")),
        Ok(out) if !out.success() => Err(format!("  FAILED    {about}: {}", out.why_it_failed())),
        Ok(_) => Ok(format!("  installed {about}")),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// What one rustup call gives back.
    type Answer = Result<exec::Output, exec::ExecError>;

    /// What `install` printed in `root`, and each command it ran, for a rustup that answers so.
    fn installed(
        root: Option<&Path>,
        answer: &Answer,
    ) -> (Option<Result<String, String>>, Vec<String>) {
        let log = RefCell::new(Vec::new());
        let rustup = |program: &str, args: &[&str], _: &Path, _: &[(&str, &str)]| {
            log.borrow_mut()
                .push(format!("{program} {}", args.join(" ")));
            answer.clone()
        };
        (install(&rustup, root), log.into_inner())
    }

    fn unstarted(stage: exec::Stage) -> Answer {
        Err(exec::ExecError {
            program: "rustup".to_string(),
            stage,
            reason: "no answer".to_string(),
            printed: None,
        })
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_promised_release_is_installed_and_a_crate_with_no_promise_installs_none() {
        let promises = crate::testdir::tree(
            "msrv-promises",
            &[(
                "Cargo.toml",
                "[package]\nname = \"a\"\nrust-version = \"1.68\"\n",
            )],
        );
        let (done, ran) = installed(Some(&*promises), &Ok(exec::Output::of(Some(0), "", "")));
        let about = "msrv — Rust 1.68, the oldest release that Cargo.toml promises";
        assert_eq!(done, Some(Ok(format!("  installed {about}"))));
        assert_eq!(
            ran,
            ["rustup toolchain install 1.68 --profile minimal --no-self-update"]
        );
        let silent = crate::testdir::tree(
            "msrv-silent",
            &[("Cargo.toml", "[package]\nname = \"a\"\n")],
        );
        for root in [None, Some(&*silent), Some(Path::new("/no/such/crate"))] {
            let (done, ran) = installed(root, &unstarted(exec::Stage::Wait));
            assert_eq!((done, ran), (None, Vec::<String>::new()), "{root:?}");
        }
    }

    #[test]
    fn a_failed_install_is_an_err_and_no_rustup_skips_it() {
        let cwd = Path::new(".");
        let rustup = |answer: Answer| {
            move |_: &str, _: &[&str], _: &Path, _: &[(&str, &str)]| answer.clone()
        };
        let refused = Ok(exec::Output::of(Some(1), "", "error: no download\n"));
        let failed = promised(&rustup(refused), "1.68", cwd).unwrap_err();
        assert!(
            failed.starts_with("  FAILED    msrv — Rust 1.68"),
            "{failed}"
        );
        assert!(failed.contains("no download"), "{failed}");
        let stopped = promised(&rustup(unstarted(exec::Stage::Wait)), "1.68", cwd).unwrap_err();
        assert!(stopped.ends_with("no answer"), "{stopped}");
        let skipped = promised(&rustup(unstarted(exec::Stage::Spawn)), "1.68", cwd).unwrap();
        assert!(skipped.starts_with("  skipped   msrv"), "{skipped}");
        assert!(skipped.contains("only rustup installs it"), "{skipped}");
    }
}
