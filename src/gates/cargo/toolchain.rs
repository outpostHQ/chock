//! The `toolchain` gate: each rust-toolchain file pins one exact compiler, so every machine and
//! every CI run builds with the same one, and a new release cannot change a result unseen.

use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

/// Fails a rust-toolchain file whose channel moves, such as `stable` or `nightly`.
pub const GATE: Gate = Gate {
    name: "toolchain",
    about: "each rust-toolchain file names an exact release or a dated nightly, not a channel that \
            moves",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Binary(check),
};

/// The names rustup reads a toolchain from, in any directory.
const FILES: [&str; 2] = ["rust-toolchain.toml", "rust-toolchain"];

fn check(ctx: &Ctx) -> Result<Outcome, String> {
    let files = ctx.texts(&|name, _| FILES.contains(&name))?;
    if files.is_empty() {
        return Err(
            "no `rust-toolchain.toml` or `rust-toolchain` file here, so nothing pins the toolchain"
                .into(),
        );
    }
    let (mut found, mut notes) = (Vec::new(), Vec::new());
    for (shown, text) in files {
        if let Some(message) = said(&text) {
            found.push(Finding::at(&shown, &message));
        }
        if let Some(message) = channel(&meant(&text)).as_deref().and_then(faulted) {
            notes.push(Finding::at(&shown, &message).item("advisory, cargo security fix"));
        }
    }
    Ok(Outcome {
        passed: found.is_empty(),
        ..Outcome::noted([found, notes].concat())
    })
}

/// The Cargo security faults of 2026, each with the first release that fixes it.
const CARGO_FAULTS: [(&str, [u32; 3]); 3] = [
    ("CVE-2026-33056", [1, 94, 1]),
    ("CVE-2026-5222", [1, 96, 0]),
    ("CVE-2026-5223", [1, 96, 0]),
];

/// What is wrong with the pin a toolchain file holds, or `None` when it pins one compiler.
fn said(text: &str) -> Option<String> {
    let lines = meant(text);
    if lines.iter().any(|line| value(line, "path").is_some()) {
        return Some(
            "names a toolchain at a local path, which holds whatever each machine put there and \
             runs on each cargo command: name a release such as `1.98.1`"
                .into(),
        );
    }
    match channel(&lines) {
        Some(channel) if exact(&channel) => None,
        Some(channel) => Some(format!(
            "names `{channel}`, which moves: name a release such as `1.98.1` or a dated nightly \
             such as `nightly-2026-09-17`"
        )),
        None => Some(
            "names no channel, so each machine builds with its own default: add `channel = \
             \"1.98.1\"` under `[toolchain]`"
                .into(),
        ),
    }
}

/// The lines of a toolchain file that say something: no blank line and no comment.
fn meant(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// The channel a toolchain file names. The file without `.toml` may hold it bare on its only line.
fn channel(lines: &[&str]) -> Option<String> {
    match lines {
        [only] if !only.contains('=') && !only.starts_with('[') => Some((*only).to_string()),
        _ => lines.iter().find_map(|line| value(line, "channel")),
    }
}

/// The Cargo security faults a numbered release still has, and the release that fixes them all.
fn faulted(channel: &str) -> Option<String> {
    let release = channel.split('-').next().unwrap_or_default();
    let numbers: Vec<u32> = release
        .split('.')
        .map_while(|part| part.parse().ok())
        .collect();
    let open: Vec<&(&str, [u32; 3])> = CARGO_FAULTS
        .iter()
        .filter(|(_, fixed)| numbers.len() == 3 && numbers.as_slice() < fixed.as_slice())
        .collect();
    let fixed = open.iter().map(|(_, fixed)| fixed).max()?;
    let names: Vec<&str> = open.iter().map(|(name, _)| *name).collect();
    Some(format!(
        "names `{channel}`, whose Cargo still has {}: name `{}.{}.{}` or later",
        names.join(", "),
        fixed[0],
        fixed[1],
        fixed[2]
    ))
}

/// The value of `key = "value"` on this line, quoted either way, less a trailing comment.
fn value(line: &str, key: &str) -> Option<String> {
    let (name, value) = line.split_once('=')?;
    if name.trim() != key {
        return None;
    }
    let value = value.split('#').next().unwrap_or_default().trim();
    Some(value.trim_matches('"').trim_matches('\'').to_string())
}

/// `1.98.1`, or `nightly`, `beta` or `stable` with a date; a host may follow either.
fn exact(channel: &str) -> bool {
    let digits = |part: &str, len: Option<usize>| {
        !part.is_empty()
            && part.bytes().all(|b| b.is_ascii_digit())
            && len.is_none_or(|len| part.len() == len)
    };
    let release = channel.split('-').next().unwrap_or_default();
    let numbers: Vec<&str> = release.split('.').collect();
    if numbers.len() == 3 && numbers.iter().all(|part| digits(part, None)) {
        return true;
    }
    let Some((name, rest)) = channel.split_once('-') else {
        return false;
    };
    let date: Vec<&str> = rest.splitn(4, '-').take(3).collect();
    matches!(name, "nightly" | "beta" | "stable")
        && date.len() == 3
        && [4, 2, 2]
            .into_iter()
            .zip(&date)
            .all(|(len, part)| digits(part, Some(len)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_or_a_dated_channel_is_exact_and_a_bare_channel_moves() {
        let pinned = "1.98.1 1.99.0-x86_64-unknown-linux-gnu nightly-2026-09-17 beta-2026-09-01 \
                      nightly-2026-09-17-aarch64-apple-darwin";
        // The leading space adds the empty channel.
        let moving = " stable nightly 1.98 1 stable-x86_64-unknown-linux-gnu nightly-2026-9-17 \
                      nightly-2026-09 dev-2026-09-17";
        let channels = pinned.split(' ').chain(moving.split(' '));
        let exact_ones: Vec<&str> = channels.filter(|it| exact(it)).collect();
        assert_eq!(exact_ones, pinned.split(' ').collect::<Vec<_>>());
    }

    #[test]
    fn the_channel_is_read_from_either_file_form() {
        let pinned =
            "[toolchain]\nchannel = \"1.99.0\" # the release CI tests\ncomponents = [\"clippy\"]\n";
        assert_eq!(said(pinned), None);
        assert_eq!(said("[toolchain]\nchannel = '1.99.0'\n"), None);
        assert_eq!(said("# pinned\nnightly-2026-09-17\n"), None);
        let path = said("[toolchain]\npath = \"/opt/rust\"\n").unwrap();
        assert!(
            path.starts_with("names a toolchain at a local path"),
            "{path}"
        );
        let moving = said("[toolchain]\nchannel = \"nightly\"\n").unwrap();
        assert!(
            moving.starts_with("names `nightly`, which moves"),
            "{moving}"
        );
        assert!(said("stable\n").unwrap().starts_with("names `stable`"));
        let missing = said("[toolchain]\ncomponents = [\"rustfmt\"]\n").unwrap();
        assert!(missing.starts_with("names no channel"), "{missing}");
        let header = said("[toolchain]\n").unwrap();
        assert!(header.starts_with("names no channel"), "{header}");
    }

    #[test]
    fn a_release_before_a_cargo_security_fix_names_each_fault_it_still_has() {
        let faults = |channel| faulted(channel).unwrap_or_default();
        let oldest = "names `1.94.0-x86_64-unknown-linux-gnu`, whose Cargo still has \
                      CVE-2026-33056, CVE-2026-5222, CVE-2026-5223: name `1.96.0` or later";
        assert_eq!(faults("1.94.0-x86_64-unknown-linux-gnu"), oldest);
        let newer = faults("1.95.0");
        assert!(newer.starts_with("names `1.95.0`, whose Cargo still has CVE-2026-5222"));
        let fixed = "1.96.0 1.99.1 1.100.0 nightly-2026-09-17 stable 1.95";
        for channel in fixed.split(' ') {
            assert_eq!(faulted(channel), None, "{channel}");
        }
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn each_toolchain_file_in_the_tree_is_held_to_one_compiler() {
        let ctx = crate::testdir::Held::tree(
            "gate-toolchain",
            &[
                ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.99.0\"\n"),
                (
                    "probe/rust-toolchain.toml",
                    "[toolchain]\nchannel = \"nightly\"\n",
                ),
                ("target/rust-toolchain", "stable\n"),
            ],
        );
        let outcome = check(&ctx).unwrap();
        assert!(!outcome.passed);
        let files: Vec<&str> = outcome.findings.iter().map(|it| it.file.as_str()).collect();
        assert_eq!(files, ["probe/rust-toolchain.toml"]);
        let pinned =
            crate::testdir::Held::tree("gate-toolchain-pinned", &[("rust-toolchain", "1.99.0\n")]);
        assert!(check(&pinned).unwrap().passed);
        let old =
            crate::testdir::Held::tree("gate-toolchain-old", &[("rust-toolchain", "1.95.0\n")]);
        let noted = check(&old).unwrap();
        assert!(noted.passed);
        let items: Vec<_> = noted.findings.iter().map(|it| it.item.as_deref()).collect();
        assert_eq!(items, [Some("advisory, cargo security fix")]);
        let bare = crate::testdir::Held::tree("gate-toolchain-none", &[("Cargo.toml", "")]);
        assert!(
            check(&bare)
                .unwrap_err()
                .starts_with("no `rust-toolchain.toml`")
        );
    }
}
