//! What `chock init --global` does with one pin before cargo runs: whether it fetches the pin
//! at all, what it asks cargo for, and the line that says so.

use crate::setup::pins::Pin;
use crate::setup::version;

/// Whether cargo says crates.io lacks the pin. That is not a failed install: a pin file can
/// name a Haskell binary or a Python tool beside the crates.
pub(super) fn not_a_crate(said: &str) -> bool {
    said.contains("could not find") && said.contains("registry")
}

/// A pin this machine already has at its version, and the line saying so: nothing is fetched, so
/// a second `chock init --global` installs nothing. Its setup line still runs, and it is cheap.
pub(super) fn kept(pin: &Pin, installed: Option<&str>) -> Option<String> {
    (installed == Some(pin.want.as_str())).then(|| {
        format!(
            "  kept      {} {} — already at its pin",
            pin.crate_name, pin.want
        )
    })
}

/// What installing this pin changes for every project on the machine. Whether a recorded number
/// moves depends on the release, which is why this is said rather than refused.
pub(super) fn replacing(pin: &Pin, installed: Option<&str>) -> Option<String> {
    let have = installed?;
    if have == pin.want {
        return None;
    }
    Some(format!(
        "  replacing {} {have} with {}, which every project on this machine then measures with",
        pin.crate_name, pin.want
    ))
}

/// A pin not fetched here, and the line saying why: one built from its own checkout, a chock older
/// than this one, or one whose tool does not run on this system. None is a failed install.
pub(super) fn not_fetched(pin: &Pin, os: &str) -> Option<String> {
    built_locally(pin)
        .or_else(|| older_chock(pin))
        .or_else(|| for_another_system(pin, os))
}

fn built_locally(pin: &Pin) -> Option<String> {
    let name = &pin.crate_name;
    pin.unpublished()
        .then(|| format!("  local     {name} — not on crates.io; build it from its checkout"))
}

/// A chock older than the one that runs, which installing would put in its place.
fn older_chock(pin: &Pin) -> Option<String> {
    let running = env!("CARGO_PKG_VERSION");
    let older = version::compare(&pin.want, running) == Some(std::cmp::Ordering::Less);
    (pin.crate_name == "chock" && older).then(|| {
        format!(
            "  kept      chock {running} — this project pins {}; `chock init --local` updates \
             its pins to this chock",
            pin.want
        )
    })
}

fn for_another_system(pin: &Pin, os: &str) -> Option<String> {
    let why = pin.elsewhere(os)?;
    Some(format!("  skipped   {} — {why}", pin.crate_name))
}

/// Printed before each tool, because a compile takes minutes and silence reads as hung.
pub(super) fn starting(pin: &Pin, binstall: bool) -> String {
    let how = match binstall {
        true => "its own prebuilt release where it has one, else compiled",
        false => "compiled from crates.io",
    };
    format!("  installing {} {} — {how}", pin.crate_name, pin.want)
}

/// What cargo is asked for one pin. binstall never takes a third-party build cache, only the
/// release the tool publishes itself, and falls back to compiling when there is none.
pub(super) fn fetch_args(pin: &Pin, binstall: bool) -> Vec<String> {
    let at = format!("{}@{}", pin.crate_name, pin.want);
    let args = match binstall {
        true => vec![
            "binstall",
            "-y",
            "--locked",
            "--disable-strategies",
            "quick-install",
            &at,
        ],
        false => vec![
            "install",
            &pin.crate_name,
            "--version",
            &pin.want,
            "--locked",
        ],
    };
    args.into_iter().map(str::to_string).collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;
    use crate::setup::pins::{self, version_pinned};

    #[test]
    fn binstall_takes_the_tools_own_release_and_a_compile_takes_the_crate_at_its_pin() {
        let pin = &pins::parse("CARGO_DENY_VERSION=0.20.2\n").unwrap()[0];
        assert_eq!(
            fetch_args(pin, true),
            [
                "binstall",
                "-y",
                "--locked",
                "--disable-strategies",
                "quick-install",
                "cargo-deny@0.20.2"
            ]
        );
        assert_eq!(
            fetch_args(pin, false),
            ["install", "cargo-deny", "--version", "0.20.2", "--locked"]
        );
    }

    #[test]
    fn a_tool_built_from_a_checkout_or_made_for_another_system_is_not_fetched() {
        let parsed = pins::parse(
            "CARGO_ACL_VERSION=0.9.0\nCARGO_ACL_OS=linux\nOUTPOST_VERSION=0.0.0\nCARGO_SORT_VERSION=2.1.4\n",
        )
        .unwrap();
        let (acl, outpost, sort) = (&parsed[0], &parsed[1], &parsed[2]);
        assert_eq!(not_fetched(acl, "linux"), None);
        assert_eq!(
            not_fetched(acl, "macos").as_deref(),
            Some("  skipped   cargo-acl — runs only on linux, so macos has no use for it")
        );
        assert_eq!(
            not_fetched(outpost, "linux").as_deref(),
            Some("  local     outpost — not on crates.io; build it from its checkout")
        );
        assert_eq!(not_fetched(sort, "windows"), None);
    }

    #[test]
    fn an_older_chock_pin_never_replaces_the_chock_that_runs() {
        let running = env!("CARGO_PKG_VERSION");
        let kept = format!(
            "  kept      chock {running} — this project pins 0.0.1; `chock init --local` updates its \
             pins to this chock"
        );
        let older = version_pinned("chock", "0.0.1");
        assert_eq!(not_fetched(&older, "linux"), Some(kept));
        assert_eq!(
            not_fetched(&version_pinned("chock", running), "linux"),
            None
        );
        assert_eq!(
            not_fetched(&version_pinned("cargo-deny", "0.0.1"), "linux"),
            None
        );
    }

    #[test]
    fn each_tool_is_announced_before_it_starts_with_how_it_will_arrive() {
        let pin = &pins::parse("CARGO_DENY_VERSION=0.20.2\n").unwrap()[0];
        assert_eq!(
            starting(pin, true),
            "  installing cargo-deny 0.20.2 — its own prebuilt release where it has one, else compiled"
        );
        assert_eq!(
            starting(pin, false),
            "  installing cargo-deny 0.20.2 — compiled from crates.io"
        );
    }

    #[test]
    fn a_pin_that_is_not_a_crate_is_not_a_failed_install() {
        assert!(not_a_crate(
            "error: could not find `shellcheck` in registry `crates-io` with version `=0.11.0`\n"
        ));
        assert!(not_a_crate(
            "error: could not find `ruff` in registry `crates-io`\n"
        ));
    }

    /// A crate that exists and would not build is a real failure, and reading it as a missing crate
    /// would report a tool as skipped while nothing installed it.
    #[test]
    fn a_crate_that_failed_to_build_is_still_a_failure() {
        assert!(!not_a_crate("error[E0432]: unresolved import `foo::bar`\n"));
        assert!(!not_a_crate(
            "error: failed to compile `cargo-deny v0.20.2`\n"
        ));
        assert!(!not_a_crate(""));
    }

    /// Each phrase alone is some other error: a missing manifest names no registry, and a registry
    /// that cannot be reached names no missing crate.
    #[test]
    fn a_message_holding_only_one_of_the_two_phrases_is_not_a_missing_crate() {
        assert!(!not_a_crate(
            "error: could not find `Cargo.toml` in `/w` or any parent directory\n"
        ));
        assert!(!not_a_crate(
            "error: failed to update registry `crates-io`\n"
        ));
    }

    /// The `--global`/`--local` split holds that sharing executables makes two projects comparable.
    /// That needs them to agree on the version, so a change is worth naming even when scores hold.
    #[test]
    fn replacing_a_version_another_project_pins_is_said_out_loud() {
        let pin = version_pinned("cargo-crap", "0.5.0");
        let said = replacing(&pin, Some("0.4.3")).unwrap();
        assert_eq!(
            said,
            "  replacing cargo-crap 0.4.3 with 0.5.0, which every project on this machine then measures with"
        );
    }

    #[test]
    fn a_tool_at_its_pin_is_kept_and_any_other_is_fetched() {
        let pin = version_pinned("cargo-crap", "0.5.0");
        assert_eq!(
            kept(&pin, Some("0.5.0")).as_deref(),
            Some("  kept      cargo-crap 0.5.0 — already at its pin")
        );
        assert_eq!(kept(&pin, Some("0.4.3")), None);
        assert_eq!(kept(&pin, None), None);
    }

    #[test]
    fn installing_the_version_already_there_replaces_nothing() {
        assert_eq!(
            replacing(&version_pinned("cargo-crap", "0.5.0"), Some("0.5.0")),
            None
        );
    }

    #[test]
    fn installing_a_tool_this_machine_does_not_have_replaces_nothing() {
        assert_eq!(
            replacing(&version_pinned("cargo-crap", "0.5.0"), None),
            None
        );
    }
}
