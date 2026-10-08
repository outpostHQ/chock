//! Wires chock into commits: a declared git hook where git supports one, a one-line stub file
//! where it does not, and an entry in Outpost's versioned hook file.

use std::fs;
use std::path::Path;

/// The stub directory `core.hooksPath` names on a git that cannot declare hooks. In the working
/// tree, not `.git/hooks`, so the stubs are versioned.
pub const DIR: &str = ".chock/hooks";

/// Outpost's versioned hook file, so every clone runs the same hooks; `.outpost/hooks.toml` is
/// never cloned.
pub const DECLARED: &str = ".outposthooks.toml";

/// The argv a hook runs. git's shell line and outpost's argv both derive from it, so they agree.
#[must_use]
pub fn invocation(name: &str) -> [String; 3] {
    ["chock".to_string(), "hook".to_string(), name.to_string()]
}

/// The shell line git runs for a hook. The logic lives in chock, so upgrading chock upgrades it.
#[must_use]
pub fn command(name: &str) -> String {
    invocation(name).join(" ")
}

/// The shell line git runs, from the top of its repository, for a project at `below` within it.
#[must_use]
pub fn command_below(name: &str, below: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "/._-".contains(c);
    let word = match below.chars().all(safe) {
        true => below.to_string(),
        false => format!("'{}'", below.replace('\'', r"'\''")),
    };
    format!("{} --project {word}", command(name))
}

/// chock's `.outposthooks.toml` entry. Outpost runs hooks only at `pre-commit`, and takes an argv.
/// A project at `below` within the repository has its own entry, which moves into it.
#[must_use]
pub fn declaration(below: Option<&str>) -> String {
    let mut argv = invocation("pre-commit").to_vec();
    argv.extend(
        below
            .iter()
            .flat_map(|at| ["--project".to_string(), at.to_string()]),
    );
    let argv = argv
        .iter()
        .map(|word| format!("\"{word}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[[pre-commit]]\nname = \"{}\"\ncommand = [{argv}]\n",
        named(below)
    )
}

/// The name chock declares a hook under: its own, and the project's path where it sits below.
fn named(below: Option<&str>) -> String {
    below.map_or_else(|| "chock".to_string(), |at| format!("chock@{at}"))
}

/// What `.outposthooks.toml` needs. `ours` says whether chock wrote all of it, since chock trusts
/// only a file that is entirely its own.
#[derive(Debug, PartialEq, Eq)]
pub enum Declaring {
    Write {
        text: String,
        ours: bool,
    },
    Declared {
        ours: bool,
    },
    /// Names chock, but not what chock runs now: a hand edit or an older chock.
    Outgrown,
}

/// What the file's current text needs. Not parsed: an appended array-of-tables entry is valid
/// after any valid TOML.
#[must_use]
pub fn declaring(held: Option<&str>, below: Option<&str>) -> Declaring {
    let want = declaration(below);
    let Some(text) = held.filter(|text| !text.trim().is_empty()) else {
        return Declaring::Write {
            text: want,
            ours: true,
        };
    };
    if text.contains(want.trim()) {
        return Declaring::Declared {
            ours: text.trim() == want.trim(),
        };
    }
    if text.contains(&format!("name = \"{}\"\n", named(below))) {
        return Declaring::Outgrown;
    }
    let sep = if text.ends_with('\n') { "" } else { "\n" };
    Declaring::Write {
        text: format!("{text}{sep}\n{want}"),
        ours: false,
    }
}

/// Every hook chock wires, and the shell line git runs for each.
#[must_use]
pub fn wired() -> [(&'static str, String); 3] {
    ["pre-commit", "commit-msg", "pre-push"].map(|name| (name, command(name)))
}

/// The stub file for git before 2.54, which cannot declare hooks: one line delegating to chock.
#[must_use]
pub fn stub(name: &str) -> String {
    let args = match name {
        // git passes `commit-msg` the message file.
        "commit-msg" => " \"$1\"",
        _ => "",
    };
    format!(
        "#!/usr/bin/env bash\nset -euo pipefail\n# Written by chock. Every rule lives in chock \
         itself, so this line is all there is to keep.\nexec {}{args}\n",
        command(name)
    )
}

/// Each wired hook's stub file as it is on disk, `None` where it is absent.
#[must_use]
pub fn installed_hooks(root: &Path) -> Vec<(String, Option<String>)> {
    wired()
        .map(|(name, _)| {
            let held = std::fs::read_to_string(root.join(DIR).join(name)).ok();
            (name.to_string(), held)
        })
        .to_vec()
}

/// Whether `program` resolves to an executable file, without running it. A missing file is
/// `Ok(false)`; one that cannot be inspected is `Err`.
pub fn executable(root: &Path, program: &str, path: &std::ffi::OsStr) -> Result<bool, String> {
    if program.contains(['/', '\\']) {
        return runnable(&root.join(program));
    }
    for candidate in crate::run::verdicts::on_path(program, path) {
        if runnable(&root.join(candidate))? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn runnable(path: &Path) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(info) => Ok(info.is_file() && has_execute_permission(&info)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("cannot inspect {}: {e}", path.display())),
    }
}

#[cfg(unix)]
fn has_execute_permission(info: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    info.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn has_execute_permission(_info: &fs::Metadata) -> bool {
    true
}

/// Whether a hook file names chock but not the command chock writes now, so chock may replace it.
#[must_use]
pub fn outgrown(name: &str, held: &str) -> bool {
    held.contains("chock") && !held.contains(&command(name))
}

/// Wires chock into every repository holding this tree, git and Outpost alike, or into the
/// repository above a project that sits below its top.
pub fn install_hooks(root: &Path) -> Result<String, crate::setup::init::Error> {
    let held = crate::project::vcs::holders(root);
    match held.is_empty() {
        true => install_from_above(root),
        false => install_where_held(root, held),
    }
}

fn install_where_held(
    root: &Path,
    held: Vec<crate::project::vcs::Kind>,
) -> Result<String, crate::setup::init::Error> {
    let mut report = String::new();
    for kind in held {
        report.push_str(&match kind {
            crate::project::vcs::Kind::Git => {
                install_git_hooks(root, crate::project::vcs::runs_declared_hooks(root))?
            }
            crate::project::vcs::Kind::Outpost => install_outpost_hook(root, None)?,
        });
    }
    Ok(report)
}

/// Wires a project below the top of its git repository, as a crate among others is: each hook
/// declared at the top under the project's own name, moving into the project before it runs.
fn install_from_above(root: &Path) -> Result<String, crate::setup::init::Error> {
    let Some((top, below)) = crate::project::vcs::enclosing_git(root) else {
        return Ok(String::new());
    };
    let mut report = from_above(&top, &below, crate::project::vcs::runs_declared_hooks(&top));
    if crate::project::vcs::holders(&top).contains(&crate::project::vcs::Kind::Outpost) {
        report.push_str(&install_outpost_hook(&top, Some(&below))?);
    }
    Ok(report)
}

/// The git hooks of a project at `below`, declared at `top` where git can run them from there.
fn from_above(top: &Path, below: &str, declared: bool) -> String {
    match declared {
        true => declare_each(top, Some(below)),
        // A stub directory would replace the hooks of every other project in the repository.
        false => format!(
            "  note      {below} sits below its repository's top, and only git 2.54 or later can \
             run its hooks from there; chock wired none\n"
        ),
    }
}

/// Declares chock in `.outposthooks.toml`, since `outpost commit` runs no git hook.
fn install_outpost_hook(
    root: &Path,
    below: Option<&str>,
) -> Result<String, crate::setup::init::Error> {
    let path = root.join(DECLARED);
    let plan = declaring(
        Some(crate::setup::init::held_or_empty(&path)?.as_str()),
        below,
    );
    write_declared(&path, &plan)?;
    // Trusting the file trusts everything in it, so chock does that only where it wrote it all.
    let trusted = match plan {
        Declaring::Write { ours: true, .. } | Declaring::Declared { ours: true } => {
            Some(crate::project::vcs::trust_declared_hooks(root))
        }
        _ => None,
    };
    Ok(wired_into_outpost(&plan, trusted, below))
}

fn write_declared(path: &Path, plan: &Declaring) -> Result<(), crate::setup::init::Error> {
    if let Declaring::Write { text, .. } = plan {
        crate::project::document::write(path, text)
            .map_err(|e| crate::setup::init::unwritable(path, &e))?;
    }
    Ok(())
}

/// What `init` reports about the Outpost wiring, given the plan and whether trusting it worked.
fn wired_into_outpost(plan: &Declaring, trusted: Option<bool>, below: Option<&str>) -> String {
    let runs = below.map_or_else(
        || command("pre-commit"),
        |at| command_below("pre-commit", at),
    );
    let said = match (plan, trusted) {
        (Declaring::Outgrown, _) => format!(
            "  note      {DECLARED} names {} but not `{runs}`; drop that entry and re-run\n",
            named(below)
        ),
        (_, Some(true)) => format!("  outpost   {DECLARED} runs `{runs}`, trusted\n"),
        (_, Some(false)) => {
            format!("  note      could not trust {DECLARED}; run: outpost hooks trust\n")
        }
        (_, None) => format!(
            "  note      {DECLARED} also declares hooks chock did not write. Read it, then run: \
             outpost hooks trust\n"
        ),
    };
    said + crate::setup::init::OUTPOST_HAS_ONE_MOMENT
}

/// Wires the git hooks: declared where git can, otherwise stub files in `DIR` and `core.hooksPath`.
fn install_git_hooks(root: &Path, declared: bool) -> Result<String, crate::setup::init::Error> {
    // A declaring git needs no stub files, so any old ones are retired.
    if declared {
        return Ok(retire(root)? + &declare_each(root, None));
    }
    let dir = root.join(DIR);
    fs::create_dir_all(&dir).map_err(|e| crate::setup::init::unwritable(&dir, &e))?;
    let mut written = Vec::new();
    for (name, _) in wired() {
        let path = dir.join(name);
        let held = fs::read_to_string(&path).unwrap_or_default();
        // A hook the project wrote is left alone; an outgrown chock stub is replaced.
        if path.exists() && !outgrown(name, &held) {
            continue;
        }
        crate::project::document::write(&path, &stub(name))
            .map_err(|e| crate::setup::init::unwritable(&path, &e))?;
        make_executable(&path)?;
        written.push(name);
    }
    let mut report = String::new();
    if !written.is_empty() {
        report.push_str(&format!("  git hook  {}\n", written.join(", ")));
    }
    report.push_str(&point_git_at_the_hooks(root, declared));
    Ok(report)
}

/// Sets `core.hooksPath`, which is per clone. Setting it silences `.git/hooks`, so a project with
/// hooks there is left alone.
fn point_git_at_the_hooks(root: &Path, declared: bool) -> String {
    let how = wiring(
        declared,
        crate::project::vcs::hooks_path(root),
        hooks_already_in_git(root),
    );
    match how {
        Wiring::Declare => declare_and_clear(root),
        Wiring::PointAt => pointed(crate::project::vcs::point_hooks_at(root, DIR)),
        settled => said(&settled),
    }
}

/// Declares the hooks and clears a `core.hooksPath` still naming the retired stub directory.
fn declare_and_clear(root: &Path) -> String {
    let mut report = declare_each(root, None);
    if crate::project::vcs::clear_hooks_path(root, DIR) {
        report.push_str(&format!(
            "  git       core.hooksPath no longer names {DIR}\n"
        ));
    }
    report
}

/// What `init` reports about a wiring, given whether pointing git at `DIR` worked.
fn said(how: &Wiring) -> String {
    match how {
        // Not news: chock set this path, or declaring and pointing report themselves.
        Wiring::Ours | Wiring::Declare | Wiring::PointAt => String::new(),
        Wiring::Elsewhere(set) => {
            format!("  note      core.hooksPath is {set}; chock's hooks in {DIR} are not run\n")
        }
        Wiring::Theirs(theirs) => format!(
            "  note      .git/hooks holds {theirs}, so chock's are not run.\n  note      use them with: git config core.hooksPath {DIR}\n"
        ),
    }
}

/// What `init` says once it has tried to point `core.hooksPath` at chock's hooks.
fn pointed(done: bool) -> String {
    if done {
        format!("  git       core.hooksPath -> {DIR}\n")
    } else {
        format!("  note      could not set core.hooksPath; run: git config core.hooksPath {DIR}\n")
    }
}

/// How chock's git hooks get run.
#[derive(Debug, PartialEq, Eq)]
enum Wiring {
    /// Declared hooks, which run alongside `.git/hooks`.
    Declare,
    /// `core.hooksPath` already points here.
    Ours,
    /// `core.hooksPath` points elsewhere, and chock leaves it.
    Elsewhere(String),
    /// The project keeps its own hooks and this git cannot run two sets.
    Theirs(String),
    PointAt,
}

/// Picks the wiring. Declaring wins where git supports it, since it displaces no other hooks.
fn wiring(declared: bool, set: Option<String>, theirs: Option<String>) -> Wiring {
    if declared {
        return Wiring::Declare;
    }
    match (set, theirs) {
        (Some(set), _) if set == DIR => Wiring::Ours,
        (Some(set), _) => Wiring::Elsewhere(set),
        (None, Some(theirs)) => Wiring::Theirs(theirs),
        (None, None) => Wiring::PointAt,
    }
}

/// Deletes the stub files chock wrote, once git runs declared hooks instead.
fn retire(root: &Path) -> Result<String, crate::setup::init::Error> {
    let mut gone = Vec::new();
    for (name, _) in wired() {
        let path = root.join(DIR).join(name);
        let Ok(held) = fs::read_to_string(&path) else {
            continue;
        };
        if !held.contains("chock") {
            continue;
        }
        fs::remove_file(&path).map_err(|e| crate::setup::init::unwritable(&path, &e))?;
        gone.push(name);
    }
    if gone.is_empty() {
        return Ok(String::new());
    }
    Ok(format!(
        "  retired   {} — git runs chock directly now, so there is no file to keep\n",
        gone.join(", ")
    ))
}

/// One declaration per hook, each under its own name so git keeps them apart. A project at `below`
/// within the repository declares its hooks at the top, under names of its own.
fn declare_each(top: &Path, below: Option<&str>) -> String {
    let named: Vec<&str> = wired()
        .into_iter()
        .filter(|(name, command)| {
            let (key, line) = match below {
                Some(at) => (format!("chock-{name}@{at}"), command_below(name, at)),
                None => (format!("chock-{name}"), command.clone()),
            };
            crate::project::vcs::declare_hook(top, &key, name, &line)
        })
        .map(|(name, _)| name)
        .collect();
    if named.is_empty() {
        return format!(
            "  note      could not declare chock's hooks; run: git config core.hooksPath {DIR}\n"
        );
    }
    let whence = below.map_or_else(String::new, |at| format!(" at the top for {at}"));
    format!(
        "  git hook  {} declared{whence}, and .git/hooks still run\n",
        named.join(", ")
    )
}

/// The hooks a project wrote itself, which are the ones `core.hooksPath` would silence.
fn hooks_already_in_git(root: &Path) -> Option<String> {
    let listing = fs::read_dir(root.join(".git/hooks")).ok()?;
    let mut theirs: Vec<String> = listing
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.ends_with(".sample"))
        .collect();
    theirs.sort();
    (!theirs.is_empty()).then(|| theirs.join(", "))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), crate::setup::init::Error> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .map_err(|e| crate::setup::init::unwritable(path, &e))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).map_err(|e| crate::setup::init::unwritable(path, &e))
}

/// Nothing to set: git on Windows runs a hook through its own shell whatever its permissions say.
#[cfg(not(unix))]
fn make_executable(path: &Path) -> Result<(), crate::setup::init::Error> {
    fs::metadata(path)
        .map(|_| ())
        .map_err(|e| crate::setup::init::unwritable(path, &e))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap or panic in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;

    /// A scratch directory with its own `git init`, so a `git config` run there never reaches the
    /// chock repository it sits inside.
    fn git_repo(name: &str) -> crate::testdir::Scratch {
        let dir = crate::testdir::make(name);
        let _ = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status();
        dir
    }

    /// A scratch repository whose `core.hooksPath` already names `path`.
    fn hooks_path_set(name: &str, path: &str) -> crate::testdir::Scratch {
        let dir = git_repo(name);
        let set = std::process::Command::new("git")
            .args(["config", "core.hooksPath", path])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(set.success(), "{set:?}");
        dir
    }

    #[test]
    fn a_hook_file_is_outgrown_only_when_chock_wrote_it_and_chock_has_moved_on() {
        assert!(outgrown("pre-commit", "exec chock run --fast\n"));
        assert!(!outgrown("pre-commit", &stub("pre-commit")));
        assert!(!outgrown("pre-commit", "exec cargo fmt --check\n"));
    }

    /// A second `init` neither rewrites it nor reports it.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_hook_file_already_current_is_left_where_it_is() {
        let dir = git_repo("hooks-already-current");
        let first = install_git_hooks(&dir, false).unwrap();
        assert!(
            first.ends_with(&format!("  git       core.hooksPath -> {DIR}\n")),
            "{first}"
        );
        let path = dir.join(DIR).join("pre-commit");
        let again = install_git_hooks(&dir, false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), stub("pre-commit"));
        assert!(!again.contains("git hook"), "it was rewritten: {again}");
    }

    /// Goes through the wiring rather than `said`, since the dispatch is what can be wrong.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_hooks_path_chock_set_is_kept_for_an_older_git_and_cleared_once_hooks_are_declared() {
        let dir = hooks_path_set("hooks-path-ours", DIR);
        assert_eq!(point_git_at_the_hooks(&dir, false), String::new());
        assert_eq!(crate::project::vcs::hooks_path(&dir).as_deref(), Some(DIR));
        let said = point_git_at_the_hooks(&dir, true);
        assert_eq!(
            said,
            format!(
                "{}  git       core.hooksPath no longer names {DIR}\n",
                declare_each(&dir, None)
            )
        );
        assert_eq!(crate::project::vcs::hooks_path(&dir), None);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_hooks_path_somebody_else_set_survives_declaring() {
        let dir = hooks_path_set("hooks-path-theirs", "tools/hooks");
        assert_eq!(point_git_at_the_hooks(&dir, true), declare_each(&dir, None));
        assert_eq!(
            crate::project::vcs::hooks_path(&dir).as_deref(),
            Some("tools/hooks")
        );
    }

    /// Called directly, since a git of any one version reaches only some of these states.
    #[test]
    fn what_init_says_about_a_git_wiring_is_one_line_per_state_it_can_be_in() {
        assert_eq!(said(&Wiring::Ours), String::new());
        assert_eq!(said(&Wiring::Declare), String::new());
        assert_eq!(said(&Wiring::PointAt), String::new());
        let elsewhere = said(&Wiring::Elsewhere("other/hooks".to_string()));
        assert!(
            elsewhere.contains("core.hooksPath is other/hooks"),
            "{elsewhere}"
        );
        let theirs = said(&Wiring::Theirs("pre-commit".to_string()));
        assert!(theirs.contains(".git/hooks holds pre-commit"), "{theirs}");
        assert!(theirs.contains("git config core.hooksPath"), "{theirs}");
        let set = pointed(true);
        assert!(set.contains("core.hooksPath -> .chock/hooks"), "{set}");
        let refused = pointed(false);
        assert!(
            refused.contains("could not set core.hooksPath"),
            "{refused}"
        );
    }

    /// An untrusted file is a hook that does not run, so each trust outcome is reported.
    #[test]
    fn what_init_says_about_an_outpost_wiring_names_whether_the_hook_will_run() {
        let ours = Declaring::Write {
            text: declaration(None),
            ours: true,
        };
        let trusted = wired_into_outpost(&ours, Some(true), None);
        assert!(
            trusted.contains("runs `chock hook pre-commit`, trusted"),
            "{trusted}"
        );
        let refused = wired_into_outpost(&ours, Some(false), None);
        assert!(refused.contains("could not trust"), "{refused}");
        let theirs = wired_into_outpost(&Declaring::Declared { ours: false }, None, None);
        assert!(
            theirs.contains("declares hooks chock did not write"),
            "{theirs}"
        );
        let edited = wired_into_outpost(&Declaring::Outgrown, None, None);
        assert!(edited.contains("drop that entry and re-run"), "{edited}");
        for said in [trusted, refused, theirs, edited] {
            assert!(
                said.contains("outpost runs only a pre-commit hook"),
                "{said}"
            );
        }
    }

    /// Told git cannot declare hooks, rather than finding such a git.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_git_that_cannot_declare_a_hook_gets_a_file_for_each_of_them() {
        let dir = git_repo("hooks-older-git");
        let report = install_git_hooks(&dir, false).unwrap();
        for (name, _) in wired() {
            let path = dir.join(DIR).join(name);
            assert_eq!(fs::read_to_string(&path).unwrap(), stub(name), "{name}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&path).unwrap().permissions().mode();
                assert_eq!(mode & 0o111, 0o111, "{name} is not executable");
            }
        }
        assert!(
            report.contains("pre-commit, commit-msg, pre-push"),
            "{report}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_tree_outpost_holds_gets_the_hook_outpost_will_actually_run() {
        let dir = crate::testdir::make("init-outpost-held");
        crate::project::vcs::pretend_outpost_holds(&dir);
        let said = install_hooks(&dir).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join(DECLARED)).unwrap(),
            "[[pre-commit]]\nname = \"chock\"\ncommand = [\"chock\", \"hook\", \"pre-commit\"]\n"
        );
        assert!(
            said.contains("outpost runs only a pre-commit hook"),
            "{said}"
        );
        assert!(
            !dir.join(DIR).exists(),
            "git hooks nothing here invokes were written"
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_declaration_file_chock_cannot_read_is_left_byte_for_byte_and_refused() {
        let dir = crate::testdir::make("init-outpost-unreadable");
        crate::project::vcs::pretend_outpost_holds(&dir);
        let path = dir.join(DECLARED);
        let theirs = [b'[', 0xff, b']', b'\n'];
        fs::write(&path, theirs).unwrap();
        let refused = install_hooks(&dir);
        assert!(
            matches!(&refused, Err(crate::setup::init::Error::Unreadable { path: named, .. })
                if named.ends_with(DECLARED)),
            "{refused:?}"
        );
        assert_eq!(fs::read(&path).unwrap(), theirs);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_declaration_file_already_there_is_added_to_or_left_exactly_alone() {
        let dir = crate::testdir::make("init-outpost-existing");
        crate::project::vcs::pretend_outpost_holds(&dir);
        let path = dir.join(DECLARED);
        let theirs = "[[pre-commit]]\nname = \"fmt\"\ncommand = [\"cargo\", \"fmt\"]\n";
        fs::write(&path, theirs).unwrap();
        let beside = install_hooks(&dir).unwrap();
        assert!(
            beside.contains("declares hooks chock did not write"),
            "{beside}"
        );
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(after, format!("{theirs}\n{}", declaration(None)));

        // Already chock's, so not rewritten: outpost untrusts a rewritten file.
        fs::write(&path, declaration(None)).unwrap();
        let again = install_hooks(&dir).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), declaration(None));
        // Trust needs a real outpost tree, so its outcomes are tested on `wired_into_outpost`.
        assert!(
            again.contains("outpost runs only a pre-commit hook"),
            "{again}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_tree_git_and_outpost_both_hold_is_wired_for_each_of_them() {
        let dir = git_repo("init-both-hold");
        crate::project::vcs::pretend_outpost_holds(&dir);
        let said = install_hooks(&dir).unwrap();
        assert!(
            dir.join(DECLARED).exists(),
            "outpost was left ungated: {said}"
        );
        assert!(said.contains("pre-commit"), "{said}");
    }

    #[test]
    fn a_declaration_beside_somebody_elses_hooks_is_not_trusted_for_them() {
        let theirs = "[[pre-commit]]\nname = \"fmt\"\ncommand = [\"cargo\", \"fmt\"]\n";
        let want = declaration(None);
        // Compared whole, so another variant shows what it was instead of panicking.
        assert_eq!(
            declaring(Some(theirs), None),
            Declaring::Write {
                text: format!("{theirs}\n{want}"),
                ours: false
            }
        );
    }

    #[test]
    fn a_declaration_already_there_is_left_exactly_as_it_is() {
        assert_eq!(
            declaring(Some(&declaration(None)), None),
            Declaring::Declared { ours: true }
        );
        let beside = format!(
            "{}{}",
            "[[pre-commit]]\nname = \"f\"\ncommand = [\"x\"]\n",
            declaration(None)
        );
        assert_eq!(
            declaring(Some(&beside), None),
            Declaring::Declared { ours: false }
        );
    }

    #[test]
    fn an_absent_file_is_written_whole_and_an_edited_one_is_handed_back() {
        let want = Declaring::Write {
            text: declaration(None),
            ours: true,
        };
        assert_eq!(declaring(None, None), want);
        assert_eq!(declaring(Some("  \n"), None), want);
        assert_eq!(
            declaring(
                Some("[[pre-commit]]\nname = \"chock\"\ncommand = [\"chock\", \"run\"]\n"),
                None
            ),
            Declaring::Outgrown
        );
    }

    #[test]
    fn both_wirings_name_the_same_invocation() {
        assert_eq!(command("pre-commit"), "chock hook pre-commit");
        let declared = declaration(None);
        assert!(
            declared.contains(r#"["chock", "hook", "pre-commit"]"#),
            "{declared}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_tree_no_repository_holds_is_told_nothing_about_commit_hooks() {
        let dir = crate::testdir::make("init-unheld");
        assert_eq!(install_hooks(&dir).unwrap(), String::new());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_git_that_declares_its_hooks_is_left_no_file_to_go_stale() {
        let dir = git_repo("init-hooks");
        let report = install_hooks(&dir).unwrap();
        assert!(report.contains("declared"), "{report}");
        for name in ["pre-commit", "commit-msg", "pre-push"] {
            let path = dir.join(DIR).join(name);
            assert!(!path.is_file(), "{name} was written anyway:\n{report}");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_hook_file_an_older_chock_wrote_is_retired_and_somebody_elses_is_kept() {
        let dir = git_repo("init-retire");
        let hooks = dir.join(DIR);
        fs::create_dir_all(&hooks).unwrap();
        fs::write(
            hooks.join("pre-commit"),
            "#!/usr/bin/env bash\nchock run --fast\n",
        )
        .unwrap();
        fs::write(
            hooks.join("pre-push"),
            "#!/usr/bin/env bash\nexec ./bin/our-own-check\n",
        )
        .unwrap();

        let report = install_hooks(&dir).unwrap();
        assert!(report.contains("retired   pre-commit"), "{report}");
        assert!(!hooks.join("pre-commit").is_file(), "{report}");
        assert_eq!(
            fs::read_to_string(hooks.join("pre-push")).unwrap(),
            "#!/usr/bin/env bash\nexec ./bin/our-own-check\n"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn declaring_records_every_hook_and_names_them() {
        let dir = git_repo("init-declare");
        let said = declare_each(&dir, None);
        assert!(said.contains("pre-commit, commit-msg, pre-push"), "{said}");
        assert!(said.contains(".git/hooks still run"), "{said}");
        for name in ["pre-commit", "commit-msg", "pre-push"] {
            let key = format!("hook.chock-{name}.command");
            let out = crate::exec::run("git", &["config", "--get", &key], &dir).unwrap();
            assert_eq!(out.stdout.trim(), command(name));
        }
    }

    #[test]
    fn declaring_in_a_tree_git_will_not_answer_for_says_so() {
        // Not a scratch directory: those sit inside chock's own repository, whose config this
        // would write.
        let said = declare_each(Path::new("/nonexistent"), None);
        assert!(said.contains("could not declare chock's hooks"), "{said}");
        assert!(!said.contains("still run"), "{said}");
    }

    /// git ships `.sample` files in every new repository.
    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn the_samples_git_ships_are_not_hooks_a_project_wrote() {
        let dir = crate::testdir::make("init-hooks-samples");
        let hooks = dir.join(".git/hooks");
        fs::create_dir_all(&hooks).unwrap();
        fs::write(hooks.join("pre-commit.sample"), "").unwrap();
        fs::write(hooks.join("pre-push.sample"), "").unwrap();
        assert_eq!(hooks_already_in_git(&dir), None);
        fs::write(hooks.join("pre-commit"), "theirs\n").unwrap();
        assert_eq!(hooks_already_in_git(&dir), Some("pre-commit".to_string()));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_tree_with_no_hooks_directory_holds_no_hooks_of_its_own() {
        let dir = crate::testdir::make("init-hooks-none");
        assert_eq!(hooks_already_in_git(&dir), None);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn every_hook_a_project_wrote_is_named() {
        let dir = crate::testdir::make("init-hooks-several");
        let hooks = dir.join(".git/hooks");
        fs::create_dir_all(&hooks).unwrap();
        fs::write(hooks.join("pre-push"), "").unwrap();
        fs::write(hooks.join("commit-msg"), "").unwrap();
        assert_eq!(
            hooks_already_in_git(&dir),
            Some("commit-msg, pre-push".to_string())
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_project_with_its_own_hooks_keeps_them_untouched() {
        let dir = git_repo("init-hooks-theirs");
        std::fs::create_dir_all(dir.join(".git/hooks")).unwrap();
        std::fs::write(dir.join(".git/hooks/pre-commit"), "theirs\n").unwrap();
        install_hooks(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(".git/hooks/pre-commit")).unwrap(),
            "theirs\n"
        );
    }

    #[test]
    fn a_git_that_can_declare_a_hook_displaces_nothing() {
        assert_eq!(
            wiring(true, None, Some("pre-commit".to_string())),
            Wiring::Declare
        );
        assert_eq!(
            wiring(true, Some("elsewhere".to_string()), None),
            Wiring::Declare
        );
    }

    #[test]
    fn an_older_git_leaves_the_projects_own_hooks_alone_and_says_so() {
        assert_eq!(
            wiring(false, None, Some("pre-commit".to_string())),
            Wiring::Theirs("pre-commit".to_string())
        );
        assert_eq!(wiring(false, None, None), Wiring::PointAt);
    }

    #[test]
    fn a_hooks_path_already_set_is_read_rather_than_overwritten() {
        assert_eq!(wiring(false, Some(DIR.to_string()), None), Wiring::Ours);
        assert_eq!(
            wiring(false, Some("other/hooks".to_string()), None),
            Wiring::Elsewhere("other/hooks".to_string())
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn executable_lookup_resolves_relative_paths_without_running_them() {
        let dir = crate::testdir::make("hook-executable");
        let bin = dir.join("bin");
        fs::create_dir(&bin).unwrap();
        let program = bin.join("chock");
        fs::write(&program, "not a real program").unwrap();
        make_executable(&program).unwrap();
        let path = std::env::join_paths([Path::new("absent"), Path::new("bin")]).unwrap();
        assert_eq!(executable(&dir, "chock", &path), Ok(true));
        assert_eq!(
            executable(&dir, "bin/chock", std::ffi::OsStr::new("")),
            Ok(true)
        );
        assert_eq!(
            executable(&dir, &program.to_string_lossy(), std::ffi::OsStr::new("")),
            Ok(true)
        );
        assert_eq!(executable(&dir, "missing", &path), Ok(false));
        assert_eq!(executable(&dir, "bin", std::ffi::OsStr::new("")), Ok(false));
    }

    #[cfg(unix)]
    #[test]
    fn a_nonexecutable_file_cannot_answer_for_a_hook_command() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::testdir::make("hook-not-executable");
        fs::write(dir.join("chock"), "unused").unwrap();
        fs::set_permissions(dir.join("chock"), fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            executable(&dir, "./chock", std::ffi::OsStr::new("")),
            Ok(false)
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_command_target_is_not_reported_missing() {
        let dir = crate::testdir::make("hook-unreadable");
        std::os::unix::fs::symlink("loop", dir.join("loop")).unwrap();
        assert!(
            executable(&dir, "./loop", std::ffi::OsStr::new(""))
                .unwrap_err()
                .contains("cannot inspect")
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_tree_that_is_not_a_git_repository_gets_no_hooks_and_no_complaint() {
        let dir = crate::testdir::make("init-nogit");
        assert_eq!(install_hooks(&dir).unwrap(), String::new());
    }

    #[test]
    fn a_project_below_the_top_is_entered_by_its_path_quoted_only_where_the_shell_needs_it() {
        assert_eq!(
            command_below("pre-push", "crates/tova"),
            "chock hook pre-push --project crates/tova"
        );
        assert_eq!(
            command_below("pre-commit", "my crate's"),
            r"chock hook pre-commit --project 'my crate'\''s'"
        );
    }

    #[test]
    fn a_project_below_the_top_declares_outpost_its_own_entry_beside_the_tops() {
        let below = declaration(Some("crates/a"));
        assert!(below.contains(r#""--project", "crates/a""#), "{below}");
        assert!(below.contains(r#"name = "chock@crates/a""#), "{below}");
        // The top's own entry neither hides a subfolder's nor is outgrown by it.
        let both = format!("{}\n{below}", declaration(None));
        assert_eq!(
            declaring(Some(&both), None),
            Declaring::Declared { ours: false }
        );
        assert_eq!(
            declaring(Some(&declaration(None)), Some("crates/a")),
            Declaring::Write {
                text: both,
                ours: false
            }
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_project_below_the_top_of_its_repository_declares_its_hooks_at_the_top_under_its_own_names()
    {
        let dir = git_repo("init-below");
        let below = dir.join("sub");
        fs::create_dir_all(&below).unwrap();
        let report = install_hooks(&below).unwrap();
        let declared = crate::project::vcs::runs_declared_hooks(&dir);
        assert_eq!(report.contains("at the top for sub"), declared, "{report}");
        assert!(!below.join(DIR).exists(), "{report}");
        assert!(!dir.join(DECLARED).exists(), "{report}");
        // Once Outpost holds the top too, the project's entry joins the file it reads there.
        crate::project::vcs::pretend_outpost_holds(&dir);
        install_hooks(&below).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join(DECLARED)).unwrap(),
            declaration(Some("sub"))
        );
        // Declared as git 2.54 would, whatever git this machine has.
        let report = from_above(&dir, "sub", true);
        assert!(report.contains("at the top for sub"), "{report}");
        for (name, _) in wired() {
            let key = format!("hook.chock-{name}@sub.command");
            let held = std::process::Command::new("git")
                .args(["config", "--get", &key])
                .current_dir(&dir)
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&held.stdout).trim(),
                command_below(name, "sub")
            );
        }
    }

    #[test]
    fn a_git_too_old_to_run_hooks_from_the_top_leaves_every_projects_hooks_alone() {
        let said = from_above(Path::new("never-read"), "crates/a", false);
        assert!(said.contains("crates/a sits below"), "{said}");
        assert!(said.contains("only git 2.54 or later"), "{said}");
    }

    #[test]
    fn an_outpost_entry_outgrown_below_the_top_names_the_projects_own_entry() {
        let said = wired_into_outpost(&Declaring::Outgrown, None, Some("crates/a"));
        assert_eq!(
            said.lines().next(),
            Some(
                format!(
                    "  note      {DECLARED} names chock@crates/a but not \
                     `chock hook pre-commit --project crates/a`; drop that entry and re-run"
                )
                .as_str()
            )
        );
    }
}
