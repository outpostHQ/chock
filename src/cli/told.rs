//! What a run says around its table: what the table leaves out, each verdict that changed, and the
//! gates the next stage adds.

use super::{Asker, OS, Tier, VERSION, for_tier, here, select};
use crate::project::config::Config;
use crate::run::report::{Leaves, Run};
use crate::run::{self, Ctx};

/// Each gate whose verdict changed since the last run, with the files modified since then.
#[expect(clippy::print_stderr, reason = "the CLI tells the user on stderr")]
pub(super) fn told_after(
    before: &std::collections::BTreeMap<String, (run::report::Verdict, u64)>,
    result: &Run,
    ctx: &Ctx,
) {
    let modified = |at| {
        crate::project::listed_once(&ctx.listing, &ctx.root).map(|tree| tree.modified_since(at))
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    for line in run::since::told(before, &result.gates, &modified, now) {
        eprintln!("{line}");
    }
}

/// The chock a hook ran, from where, and what the next stage adds, so that a pass at the commit
/// is not read as a pass at the push. `None` outside a hook's tier.
pub(super) fn reach_line(
    tier: Tier,
    chosen: &[&run::Gate],
    config: Option<&Config>,
) -> Option<String> {
    let (judged, next, later) = match tier {
        Tier::Committing => ("the commit's", Tier::Pushing, "the push"),
        Tier::Pushing => ("the push's", Tier::Everything, "`chock run`"),
        Tier::Ci { .. } | Tier::Everything => return None,
    };
    let (after, _) = here(for_tier(select(&[], config).ok()?, next, config), OS);
    let ran = |name: &str| chosen.iter().any(|gate| gate.name == name);
    let added: Vec<&str> = after
        .iter()
        .map(|gate| gate.name)
        .filter(|name| !ran(name))
        .collect();
    let exe =
        std::env::current_exe().map_or_else(|_| "chock".into(), |at| at.display().to_string());
    let gates = run::report::plural(chosen.len(), "gate");
    let rest = if added.is_empty() {
        format!("{later} adds none")
    } else {
        format!("{later} adds {}: {}", added.len(), added.join(", "))
    };
    Some(format!(
        "chock {VERSION} at {exe} judged {judged} {gates}; {rest}"
    ))
}

/// What the report leaves out: the blocks progress put on its stream, and a hook's advisories.
pub(super) fn leaves(asker: Asker) -> Leaves {
    Leaves {
        advisories: asker != Asker::Person,
        ..on_one_stream()
    }
}

/// `said` where stdout and stderr reach one file, pipe or terminal, as a git hook's do.
// mutest: a unit test shares the harness's streams; `tests/cli.rs` runs the binary on one file.
#[cfg(unix)]
#[cfg_attr(mutest, mutest::skip)]
fn on_one_stream() -> Leaves {
    let said = same_target("/dev/fd/1".as_ref(), "/dev/fd/2".as_ref());
    Leaves {
        said,
        ..Leaves::default()
    }
}

#[cfg(not(unix))]
fn on_one_stream() -> Leaves {
    Leaves::default()
}

/// Whether two paths name one file; a path that cannot be read names none.
#[cfg(unix)]
fn same_target(one: &std::path::Path, other: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let id = |path: &std::path::Path| std::fs::metadata(path).map(|meta| (meta.dev(), meta.ino()));
    matches!((id(one), id(other)), (Ok(left), Ok(right)) if left == right)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a hook counts the advisories; a person reads each one. Mutant: `!=` turned to `==`.
    #[test]
    fn a_hook_counts_the_advisories_and_a_person_reads_them() {
        assert!(!leaves(Asker::Person).advisories);
        assert!(leaves(Asker::Hook).advisories);
        assert!(leaves(Asker::Commit).advisories);
    }

    /// A hard link names the file it links; two files, or two missing paths, name none.
    /// Mutants: `same_target` always true; two failed reads compared as equal.
    #[test]
    #[cfg(unix)]
    fn two_paths_name_one_file_only_when_their_device_and_inode_match() {
        let dir = crate::testdir::tree("one-stream", &[("a", "one"), ("b", "one")]);
        let (a, b, link, missing) = (
            dir.join("a"),
            dir.join("b"),
            dir.join("link"),
            dir.join("missing"),
        );
        std::fs::hard_link(&a, &link).unwrap();
        assert!(same_target(&a, &a));
        assert!(same_target(&a, &link));
        assert!(!same_target(&a, &b));
        assert!(!same_target(&a, &missing));
        assert!(!same_target(&missing, &missing));
    }
}
