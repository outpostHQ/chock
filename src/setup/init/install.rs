//! The install of the tools that init sets up: each download, its attempts, and the report of each install.

use std::path::Path;
use std::process::Command;

use super::Error;
use crate::exec::Start;
use crate::project;
use crate::project::config::Config;
use crate::setup::fetching::{
    Fetch, fetch_args, kept, not_a_crate, not_fetched, replacing, starting,
};
use crate::setup::pins::Pin;

/// A setup line is a command and its arguments split on whitespace. Quoting is unsupported: no
/// pinned tool needs it, and splitting a quoted path wrongly would be worse than refusing.
pub(super) fn setup_command(line: &str) -> Option<(&str, Vec<&str>)> {
    let mut parts = line.split_whitespace();
    let program = parts.next()?;
    Some((program, parts.collect()))
}

/// How often a setup line runs before its install counts as failed, and the wait that grows before
/// each new attempt: the line downloads, and GitHub once answered Kani's bundle with a 500.
const SETUP_ATTEMPTS: u32 = 3;

pub(super) const SETUP_PAUSE: std::time::Duration = std::time::Duration::from_secs(10);

/// The second half of installing a tool that fetches the rest of itself. A failure here leaves the
/// tool on PATH but unusable, so it counts as the install failing rather than as a warning.
#[expect(
    clippy::print_stdout,
    reason = "each install is said as it ends, since the next one can take minutes"
)]
pub(super) fn run_setup(pin: &Pin, pause: &dyn Fn(u32)) -> bool {
    let Some(line) = &pin.setup else {
        return true;
    };
    let Some((program, args)) = setup_command(line) else {
        println!(
            "  FAILED    {} — {} pins an empty setup line",
            pin.crate_name, pin.key
        );
        return false;
    };
    let finishes = |again: u32| {
        pause(again);
        let ran = Command::new(program).args(&args).status();
        ran.is_ok_and(|status| status.success())
    };
    if (0..SETUP_ATTEMPTS).any(finishes) {
        println!("  set up    {} — {line}", pin.crate_name);
        true
    } else {
        println!("  FAILED    {} — `{line}` did not finish", pin.crate_name);
        false
    }
}

pub(super) fn install_global(
    pins: &[Pin],
    start: Start<'_>,
    root: Option<&Path>,
) -> Result<(), Error> {
    let installed = crate::setup::doctor::installed_on_this_machine();
    // Asked per pin: the pin file installs cargo-binstall first, and every tool after it uses it.
    let mut failed: Vec<String> = pins
        .iter()
        .filter(|pin| !installs(pin, &installed, start))
        .map(|pin| pin.crate_name.clone())
        .collect();
    // Miri is no pin: it is a part of the `nightly` toolchain, so every machine gets it.
    let miri = crate::setup::miri::install(start, Path::new("."));
    failed.extend((!reported(&miri)).then(|| crate::setup::miri::NAME.to_string()));
    // Nor is the oldest Rust the crate promises, which the `msrv` gate builds with.
    let msrv = crate::setup::msrv::install(start, root);
    failed.extend((!msrv.as_ref().is_none_or(reported)).then(|| crate::setup::msrv::NAME.into()));
    if failed.is_empty() {
        Ok(())
    } else {
        Err(Error::NotInstalled(failed))
    }
}

/// The installs' result, with the download pins that failed added to it.
pub(super) fn joined(installed: Result<(), Error>, failed: Vec<String>) -> Result<(), Error> {
    match installed {
        Err(Error::NotInstalled(mut crates)) => {
            crates.extend(failed);
            Err(Error::NotInstalled(crates))
        }
        Ok(()) if !failed.is_empty() => Err(Error::NotInstalled(failed)),
        other => other,
    }
}

/// Download pins, each checked against its SHA-256. A gate's own tool comes only while the project
/// has that gate on, so a project without `tla` never fetches 200 MB of Java.
#[expect(
    clippy::print_stdout,
    reason = "each download is said as it is decided, since the next one can take minutes"
)]
pub(super) fn fetch_downloads(
    downloads: &[crate::setup::downloads::Download],
    root: Option<&Path>,
    tools: &crate::gates::proofs::Tools,
) -> Vec<String> {
    let config = root.and_then(|root| project::document::read::<Config>(root).ok().flatten());
    let on = |gate: &str| config.as_ref().is_some_and(|held| held.is_on(gate));
    let wanted: Vec<_> = downloads
        .iter()
        .filter(|pin| match pin.gate().filter(|gate| !on(gate)) {
            Some(gate) => {
                println!("  skipped   {} — the `{gate}` gate is off", pin.name);
                false
            }
            None => true,
        })
        .collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    let Some(home) = tools.home.as_deref() else {
        println!("  FAILED    downloads — no home directory holds them; set CHOCK_TOOLS");
        return wanted.iter().map(|pin| pin.name.clone()).collect();
    };
    wanted
        .into_iter()
        .filter(|pin| {
            !(reported(&crate::setup::downloads::fetch(pin, home, tools)) && set_up(pin, home))
        })
        .map(|pin| pin.name.clone())
        .collect()
}

/// A download pin's `_SETUP` line, run in the directory that holds the fetched file.
#[expect(
    clippy::print_stdout,
    reason = "each setup is said as it ends, since the next one can take minutes"
)]
pub(super) fn set_up(pin: &crate::setup::downloads::Download, home: &Path) -> bool {
    let (Some(line), Ok(Some(file))) = (&pin.setup, pin.here()) else {
        return true;
    };
    let dir = pin.dir(home, &file.sha256);
    let ran = setup_command(line)
        .map(|(program, args)| Command::new(program).args(&args).current_dir(&dir).status());
    let done = ran.is_some_and(|status| status.is_ok_and(|status| status.success()));
    if done {
        println!("  set up    {} — {line}", pin.name);
    } else {
        println!("  FAILED    {} — `{line}` did not finish", pin.name);
    }
    done
}

/// One pin: Outpost's fork of mutest-rs is built from its repository, any other tool is fetched.
#[expect(
    clippy::print_stdout,
    reason = "the build is said before it starts, since it can take minutes"
)]
pub(super) fn installs(
    pin: &Pin,
    installed: &dyn Fn(&Pin) -> Option<String>,
    start: Start<'_>,
) -> bool {
    if !crate::setup::mutest::is_pin(pin, std::env::consts::OS) {
        return install_one(pin, installed, binstall_here());
    }
    println!("{}", crate::setup::mutest::starting());
    reported(&crate::setup::mutest::install(start, &std::env::temp_dir()))
}

/// Prints what an install that is no crate fetch did; `false` is one that failed.
#[expect(
    clippy::print_stdout,
    reason = "each install is said as it ends, since the next one can take minutes"
)]
pub(super) fn reported(done: &Result<String, String>) -> bool {
    let (Ok(line) | Err(line)) = done;
    println!("{line}");
    done.is_ok()
}

/// Whether `cargo binstall` answers here: it takes a tool's own prebuilt release, seconds where
/// compiling the crate takes minutes.
fn binstall_here() -> Fetch {
    let answers = Command::new("cargo")
        .args(["binstall", "-V"])
        .output()
        .is_ok_and(|out| out.status.success());
    if answers {
        Fetch::Binstall
    } else {
        Fetch::Compile
    }
}

/// A binstall that fails for any reason falls back to the compile, whose errors `install_one`
/// already knows how to read. binstall fetches releases, so a git pin always compiles.
pub(super) fn fetch(pin: &Pin, how: Fetch) -> std::io::Result<std::process::Output> {
    let fetched = (how == Fetch::Binstall)
        .then_some(pin)
        .filter(|pin| pin.git.is_none());
    let fetched = fetched.map(|pin| {
        Command::new("cargo")
            .args(fetch_args(pin, Fetch::Binstall))
            .output()
    });
    match fetched {
        Some(Ok(out)) if out.status.success() => Ok(out),
        _ => Command::new("cargo")
            .args(fetch_args(pin, Fetch::Compile))
            .output(),
    }
}

/// One tool, fetched and then set up. A tool that is not on crates.io has no coordinate to fetch,
/// so it is reported rather than attempted, and that is not a failure.
#[expect(
    clippy::print_stdout,
    reason = "each install is said as it goes, since one can take minutes"
)]
fn install_one(pin: &Pin, installed: &dyn Fn(&Pin) -> Option<String>, how: Fetch) -> bool {
    if let Some(said) = not_fetched(pin, std::env::consts::OS) {
        println!("{said}");
        return true;
    }
    let have = installed(pin);
    if let Some(said) = kept(pin, have.as_deref()) {
        println!("{said}");
        return run_setup(pin, &|again| std::thread::sleep(SETUP_PAUSE * again));
    }
    if let Some(said) = replacing(pin, have.as_deref()) {
        println!("{said}");
    }
    println!("{}", starting(pin, how));
    let Ok(out) = fetch(pin, how) else {
        println!("  FAILED    {} {}", pin.crate_name, pin.want);
        return false;
    };
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        if not_a_crate(&said) {
            println!(
                "  skipped   {} {} — crates.io does not have it, so chock did not install it",
                pin.crate_name, pin.want
            );
            return true;
        }
        print!("{said}");
        println!("  FAILED    {} {}", pin.crate_name, pin.want);
        return false;
    }
    println!("  installed {} {}", pin.crate_name, pin.want);
    run_setup(pin, &|again| std::thread::sleep(SETUP_PAUSE * again))
}
