//! The `toolchain` gate: each rust-toolchain file pins one exact compiler, so every machine and
//! every CI run builds with the same one, and a new release cannot change a result unseen.

use crate::project;
use crate::run::report::Finding;
use crate::run::{Ctx, Gate, Group, Kind, Outcome};

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
    let entered = |dir: &str| !project::SKIPPED.contains(&dir);
    let named = |name: &str, _: &std::path::Path| FILES.contains(&name);
    let files = project::walked(&ctx.listing, &ctx.root, &entered, &named)?;
    if files.is_empty() {
        return Err(
            "no `rust-toolchain.toml` or `rust-toolchain` file here, so nothing pins the toolchain"
                .into(),
        );
    }
    let mut found = Vec::new();
    for path in files {
        let shown = project::relative(&ctx.root, &path);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("could not read {shown}: {e}"))?;
        if let Some(message) = said(&text) {
            found.push(Finding::at(&shown, &message));
        }
    }
    Ok(if found.is_empty() {
        Outcome::passed()
    } else {
        Outcome::failed(found)
    })
}

/// What is wrong with the pin a toolchain file holds, or `None` when it pins one compiler.
fn said(text: &str) -> Option<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    // The file without `.toml` may hold the bare channel on its only line.
    let channel = match lines.as_slice() {
        [only] if !only.contains('=') && !only.starts_with('[') => Some((*only).to_string()),
        // A toolchain at a local path is whatever that directory holds, which the file names.
        _ if lines.iter().any(|line| value(line, "path").is_some()) => return None,
        _ => lines.iter().find_map(|line| value(line, "channel")),
    };
    match channel {
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
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn a_release_or_a_dated_channel_is_exact_and_a_bare_channel_moves() {
        let pinned = "1.98.1 1.99.0-x86_64-unknown-linux-gnu nightly-2026-09-17 beta-2026-09-01 \
                      nightly-2026-09-17-aarch64-apple-darwin";
        // The leading space adds the empty channel.
        let moving = " stable nightly 1.98 1 stable-x86_64-unknown-linux-gnu nightly-2026-9-17";
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
        assert_eq!(said("[toolchain]\npath = \"/opt/rust\"\n"), None);
        let moving = said("[toolchain]\nchannel = \"nightly\"\n").unwrap();
        assert!(
            moving.starts_with("names `nightly`, which moves"),
            "{moving}"
        );
        assert!(said("stable\n").unwrap().starts_with("names `stable`"));
        let missing = said("[toolchain]\ncomponents = [\"rustfmt\"]\n").unwrap();
        assert!(missing.starts_with("names no channel"), "{missing}");
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
        let bare = crate::testdir::Held::tree("gate-toolchain-none", &[("Cargo.toml", "")]);
        assert!(
            check(&bare)
                .unwrap_err()
                .starts_with("no `rust-toolchain.toml`")
        );
    }
}
