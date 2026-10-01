//! Which version control system holds this tree, and everything chock asks it: what is tracked,
//! what is committed but unsent, and where git keeps its hooks. No other module runs git.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::exec;

/// A version control system, serialised lower-case as a project writes it in its config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Git,
    Outpost,
}

impl Kind {
    /// The first system holding the tree, git first; `live_holder` decides between two.
    #[must_use]
    pub fn of(root: &Path) -> Option<Self> {
        holders(root).into_iter().next()
    }

    /// The file each tool reads for paths to ignore; Outpost does not read `.gitignore`.
    #[must_use]
    pub fn ignore_file(self) -> &'static str {
        match self {
            Self::Git => ".gitignore",
            Self::Outpost => ".outpostignore",
        }
    }

    #[must_use]
    pub fn command(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Outpost => "outpost",
        }
    }
}

/// Every system holding this tree, git first. Both can, since `outpost setup` leaves git in place.
#[must_use]
pub fn holders(root: &Path) -> Vec<Kind> {
    let mut held = Vec::new();
    // A worktree's `.git` is a file rather than a directory, so existence is the question.
    if root.join(".git").exists() {
        held.push(Kind::Git);
    }
    if root.join(".outpost").join(OUTPOST_MARKER).is_file() {
        held.push(Kind::Outpost);
    }
    held
}

/// The file that makes `.outpost` a repository. Outpost passes over a `.outpost` without it and
/// searches upward.
const OUTPOST_MARKER: &str = "config.toml";

/// Makes a test directory read as Outpost-held, without spawning outpost inside chock's own tree.
#[cfg(test)]
pub fn pretend_outpost_holds(root: &Path) {
    let held = root.join(".outpost");
    let _ = std::fs::create_dir_all(&held);
    let _ = std::fs::write(held.join(OUTPOST_MARKER), "");
}

/// Runs `outpost hooks trust`. Outpost runs a declared hook only once its file's hash is trusted,
/// and an edit untrusts it.
pub fn trust_declared_hooks(root: &Path) -> bool {
    exec::run("outpost", &["hooks", "trust"], root).is_ok_and(|out| out.success())
}

/// What outpost makes of the declared hooks: `absent`, `untrusted`, `trusted` or `changed`, or
/// `None` where outpost did not answer.
#[must_use]
pub fn declared_hook_standing(root: &Path) -> Option<String> {
    standing(&exec::run("outpost", &["hooks", "list", "--json"], root).ok()?)
}

#[derive(Deserialize)]
struct Declared {
    standing: String,
}

/// The standing in `hooks list --json` stdout, read even on a non-zero exit.
fn standing(out: &exec::Output) -> Option<String> {
    serde_json::from_str::<Declared>(&out.stdout)
        .ok()
        .map(|read| read.standing)
}

/// The most tracked files chock enumerates.
pub const MAX_TRACKED: usize = 50_000;

/// Every file the repository tracks, relative to the root; `Err` outside a repository.
pub fn tracked(root: &Path, held: Option<Kind>) -> Result<Vec<String>, String> {
    match held {
        None => Err("no repository here, so nothing says which files are tracked".to_string()),
        Some(Kind::Git) => {
            let out = exec::run("git", &["ls-files", "-z"], root).map_err(|e| e.to_string())?;
            read_git(&out)
        }
        Some(Kind::Outpost) => {
            let out = exec::run("outpost", &["tree", "--json"], root).map_err(|e| e.to_string())?;
            read_outpost(&out)
        }
    }
}

/// Every path, or an error past `MAX_TRACKED` rather than a silently truncated list.
fn enumerated<'a>(paths: impl Iterator<Item = &'a str>) -> Result<Vec<String>, String> {
    let held: Vec<String> = paths.take(MAX_TRACKED + 1).map(str::to_string).collect();
    if held.len() > MAX_TRACKED {
        return Err(format!(
            "this repository tracks more than {MAX_TRACKED} files, which chock will not enumerate"
        ));
    }
    Ok(held)
}

/// `git ls-files -z`, separated by NUL so a path holding a newline stays one path.
fn read_git(out: &exec::Output) -> Result<Vec<String>, String> {
    if !out.success() {
        return Err(reason("git ls-files read no repository here", out));
    }
    enumerated(out.stdout.split('\0').filter(|path| !path.is_empty()))
}

/// `outpost tree --json`, which nests a directory's entries under it rather than listing paths.
fn read_outpost(out: &exec::Output) -> Result<Vec<String>, String> {
    if !out.success() {
        return Err(reason("outpost could not read the tree", out));
    }
    let tree: Node = serde_json::from_str(&out.stdout)
        .map_err(|e| format!("outpost printed a tree chock cannot read: {e}"))?;
    let mut found = Vec::new();
    paths(&tree, "", &mut found);
    found.sort();
    enumerated(found.iter().map(String::as_str))
}

/// One node of `outpost tree --json`, of the two variants that carry a path.
#[derive(Deserialize)]
struct Node {
    node: Entry,
    #[serde(default)]
    children: Vec<Node>,
}

/// A node's payload. `Other` takes the `Commit` and `VNode` wrappers, which carry payloads too.
#[derive(Deserialize)]
enum Entry {
    File {
        node: Named,
    },
    Directory {
        node: Named,
    },
    #[serde(untagged)]
    Other(serde::de::IgnoredAny),
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

/// Collects the file paths under `node`. The root directory is named `""`, so `join` skips it.
fn paths(node: &Node, at: &str, into: &mut Vec<String>) {
    let here = match &node.node {
        Entry::File { node } => {
            into.push(join(at, &node.name));
            return;
        }
        Entry::Directory { node } => join(at, &node.name),
        Entry::Other(_) => at.to_string(),
    };
    for child in &node.children {
        paths(child, &here, into);
    }
}

fn join(at: &str, name: &str) -> String {
    match (at.is_empty(), name.is_empty()) {
        (true, _) => name.to_string(),
        (_, true) => at.to_string(),
        _ => format!("{at}/{name}"),
    }
}

/// How many commits the system holding this tree can walk back from HEAD.
pub fn commits(root: &Path, held: Option<Kind>) -> Result<u64, String> {
    match held {
        None => Err("no repository here, so nothing says how many commits there are".to_string()),
        Some(kind) => commits_of(root, kind),
    }
}

/// The commit count in one named system, so a tree both hold can ask each.
fn commits_of(root: &Path, kind: Kind) -> Result<u64, String> {
    match kind {
        Kind::Git => {
            let out = exec::run("git", &["rev-list", "--count", "HEAD"], root)
                .map_err(|e| e.to_string())?;
            read_git_count(&out)
        }
        Kind::Outpost => {
            let out = exec::run("outpost", &["log", "--oneline", "-n", COUNTED], root)
                .map_err(|e| e.to_string())?;
            read_outpost_count(&out)
        }
    }
}

/// Which repository a run reads: the config's `vcs` if set, else `live_holder`.
#[must_use]
pub fn holding(root: &Path, said: Option<&crate::project::config::Config>) -> Option<Kind> {
    said.and_then(|set| set.vcs).or_else(|| live_holder(root))
}

/// Which system's history a gate should read. Where both hold the tree, the longer history is live
/// and the shorter a mirror.
#[must_use]
pub fn live_holder(root: &Path) -> Option<Kind> {
    match holders(root).as_slice() {
        [] => enclosed_by_git(root).then_some(Kind::Git),
        [only] => Some(*only),
        // Measured rather than preferring one: either system can be the mirror.
        _ => Some(live(
            commits_of(root, Kind::Git).ok(),
            commits_of(root, Kind::Outpost).ok(),
        )),
    }
}

/// Whether git finds a repository above a project holding none itself, as a nested crate does.
/// Git's own walk stops at `GIT_CEILING_DIRECTORIES`, which keeps test scratch out of this one.
fn enclosed_by_git(root: &Path) -> bool {
    exec::run("git", &["rev-parse", "--show-toplevel"], root).is_ok_and(|out| out.success())
}

/// The repository's root directory; a path outside it is missing from a fresh clone.
#[must_use]
pub fn repository_root(root: &Path, held: Option<Kind>) -> Option<std::path::PathBuf> {
    match held? {
        Kind::Git => exec::run("git", &["rev-parse", "--show-toplevel"], root)
            .ok()
            .filter(exec::Output::success)
            .map(|out| std::path::PathBuf::from(out.stdout.trim())),
        Kind::Outpost => root
            .ancestors()
            .find(|dir| dir.join(".outpost").join(OUTPOST_MARKER).is_file())
            .map(Path::to_path_buf),
    }
}

/// The pathspec a project's commits are read over: none at the repository root, `.` below it, so a
/// nested crate answers only for its own commits.
fn own_paths(root: &Path) -> &'static [&'static str] {
    let below = exec::run("git", &["rev-parse", "--show-prefix"], root)
        .is_ok_and(|out| out.success() && !out.stdout.trim().is_empty());
    if below { &["--", "."] } else { &[] }
}

/// The system with the longer history, git when level, since a mirror falls behind its source.
#[must_use]
fn live(git: Option<u64>, outpost: Option<u64>) -> Kind {
    match (git, outpost) {
        (Some(git), Some(outpost)) if outpost > git => Kind::Outpost,
        // A git that cannot count is an empty export beside outpost's history.
        (None, Some(_)) => Kind::Outpost,
        _ => Kind::Git,
    }
}

/// Where counting outpost's history stops.
const COUNTED: &str = "100000";

/// `git rev-list --count HEAD` prints the number and nothing else. A repository holding no commit
/// has no HEAD and git refuses, rather than answering zero.
fn read_git_count(out: &exec::Output) -> Result<u64, String> {
    if !out.success() {
        return Err(reason("git could not count the commits behind HEAD", out));
    }
    let printed = out.stdout.trim();
    printed
        .parse()
        .map_err(|e| format!("git printed a commit count chock cannot read: {printed:?}: {e}"))
}

/// One line per commit, so the lines are the count; truncated output is an error, not fewer.
fn read_outpost_count(out: &exec::Output) -> Result<u64, String> {
    if !out.success() {
        return Err(reason("outpost could not count this history", out));
    }
    if out.truncated {
        return Err("outpost printed more history than chock keeps; the count is a prefix".into());
    }
    let walked = out.stdout.lines().filter(|line| !line.is_empty()).count();
    Ok(u64::try_from(walked).unwrap_or(u64::MAX))
}

/// `core.hooksPath`, or `None` where it is unset or git could not answer.
pub fn hooks_path(root: &Path) -> Option<String> {
    let out = exec::run("git", &["config", "--get", "core.hooksPath"], root).ok()?;
    read_hooks_path(&out)
}

/// The configured hooks path; an unset key (a non-zero exit) or an empty value is `None`.
fn read_hooks_path(out: &exec::Output) -> Option<String> {
    if !out.success() {
        return None;
    }
    let set = out.stdout.trim();
    (!set.is_empty()).then(|| set.to_string())
}

/// One `hook.chock-*` declaration in git config.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GitHook {
    pub name: String,
    pub events: Vec<String>,
    pub command: Option<String>,
}

fn require_declared_hook_support(version: &exec::Output) -> Result<(), String> {
    if declares_hooks(version) {
        Ok(())
    } else {
        Err("cannot confirm Git supports the configured hook declarations; Git 2.54 or later is required".to_string())
    }
}

/// chock's hook declarations in git config, read with `--null` so a command holding a newline
/// stays whole.
pub fn git_hook_declarations(root: &Path) -> Result<Vec<GitHook>, String> {
    if !holders(root).contains(&Kind::Git) {
        return Ok(Vec::new());
    }
    let out = exec::run(
        "git",
        &[
            "config",
            "--null",
            "--get-regexp",
            r"^hook\.chock-.*\.(event|command)$",
        ],
        root,
    )
    .map_err(|e| e.to_string())?;
    confirm_declared_hooks(read_git_hook_declarations(&out)?, || {
        exec::run("git", &["--version"], root).map_err(|e| e.to_string())
    })
}

fn confirm_declared_hooks(
    hooks: Vec<GitHook>,
    version: impl FnOnce() -> Result<exec::Output, String>,
) -> Result<Vec<GitHook>, String> {
    if !hooks.is_empty() {
        require_declared_hook_support(&version()?)?;
    }
    Ok(hooks)
}

fn read_git_hook_declarations(out: &exec::Output) -> Result<Vec<GitHook>, String> {
    if out.truncated {
        return Err("Git hook declarations were truncated".to_string());
    }
    if out.code == Some(1) && out.stdout.is_empty() && out.stderr.is_empty() {
        return Ok(Vec::new());
    }
    if !out.success() {
        return Err(format!(
            "cannot read Git hook declarations: {}",
            out.why_it_failed()
        ));
    }
    if !out.stdout.ends_with('\0') {
        return Err("Git hook declarations have no complete record".to_string());
    }
    let mut hooks = std::collections::BTreeMap::<String, GitHook>::new();
    for record in out.stdout.split_terminator('\0') {
        let (key, value) = record
            .split_once('\n')
            .ok_or("unreadable Git hook declaration")?;
        let Some(rest) = key.strip_prefix("hook.chock-") else {
            continue;
        };
        let (name, field) = rest.rsplit_once('.').ok_or("unreadable Git hook key")?;
        let name = format!("chock-{name}");
        let hook = hooks.entry(name.clone()).or_insert_with(|| GitHook {
            name,
            ..GitHook::default()
        });
        match field {
            "event" => hook.events.push(value.to_string()),
            "command" => hook.command = Some(value.to_string()),
            _ => return Err("unrecognized Git hook field".to_string()),
        }
    }
    Ok(hooks.into_values().collect())
}

/// The first git that runs hooks declared in config, alongside the hooks directory.
const DECLARED_HOOKS_SINCE: (u32, u32) = (2, 54);

/// Whether the installed git runs hooks declared in config.
#[must_use]
pub fn runs_declared_hooks(root: &Path) -> bool {
    exec::run("git", &["--version"], root).is_ok_and(|out| declares_hooks(&out))
}

/// Whether a `git --version` answer is at least `DECLARED_HOOKS_SINCE`. Apart from the spawn, so a
/// test can reach both answers.
fn declares_hooks(out: &exec::Output) -> bool {
    read_version(out).is_some_and(|found| found >= DECLARED_HOOKS_SINCE)
}

/// `git version 2.55.0.782.g1630431f32`, of which only the first two numbers decide.
fn read_version(out: &exec::Output) -> Option<(u32, u32)> {
    if !out.success() {
        return None;
    }
    let rest = out.stdout.trim().strip_prefix("git version ")?;
    let mut parts = rest.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Declares one hook in git config, alongside the hooks directory. Refuses where git does not hold
/// the tree, since `git config` would write to a repository above it.
pub fn declare_hook(root: &Path, name: &str, event: &str, command: &str) -> bool {
    if !holders(root).contains(&Kind::Git) {
        return false;
    }
    let keyed = |key: &str, value: &str| {
        exec::run("git", &["config", key, value], root).is_ok_and(|out| out.success())
    };
    keyed(&format!("hook.{name}.event"), event) && keyed(&format!("hook.{name}.command"), command)
}

/// Sets `core.hooksPath` to `dir` and says whether git took it; refuses off a git tree.
pub fn point_hooks_at(root: &Path, dir: &str) -> bool {
    holders(root).contains(&Kind::Git)
        && exec::run("git", &["config", "core.hooksPath", dir], root).is_ok_and(|out| out.success())
}

/// Unsets `core.hooksPath` only while it still names `dir`: a path somebody else chose is theirs.
pub fn clear_hooks_path(root: &Path, dir: &str) -> bool {
    holders(root).contains(&Kind::Git)
        && hooks_path(root).as_deref() == Some(dir)
        && exec::run("git", &["config", "--unset", "core.hooksPath"], root)
            .is_ok_and(|out| out.success())
}

/// Where git keeps the message being composed. A declared hook gets no arguments, so `commit-msg`
/// asks git rather than reading `$1`.
#[must_use]
pub fn message_being_composed(root: &Path) -> Option<std::path::PathBuf> {
    if Kind::of(root) != Some(Kind::Git) {
        return None;
    }
    let out = exec::run("git", &["rev-parse", "--git-path", "COMMIT_EDITMSG"], root).ok()?;
    if !out.success() {
        return None;
    }
    let path = root.join(out.stdout.trim());
    path.is_file().then_some(path)
}

/// Commits not yet on the remote, as (id, message): only those can still be rewritten.
pub fn unpushed(root: &Path, held: Option<Kind>) -> Result<Vec<(String, String)>, String> {
    match held {
        None => Err(NO_REPOSITORY.to_string()),
        Some(Kind::Git) => {
            let upstream = exec::run("git", &["rev-parse", "--abbrev-ref", "@{upstream}"], root)
                .map_err(|e| e.to_string())?;
            if !upstream.success() {
                return Ok(Vec::new());
            }
            let mut args = vec!["log", "@{upstream}..HEAD", "--format=%H%x00%B%x01"];
            args.extend(own_paths(root));
            let out = exec::run("git", &args, root).map_err(|e| e.to_string())?;
            read_git_log(&out)
        }
        Some(Kind::Outpost) => outpost_unpushed(root),
    }
}

/// The files this change touched, relative to the root: since the fork from the upstream, or in CI
/// the commit under test. An untracked file is not listed; its debt is new, and new debt fails.
pub fn changed(root: &Path, held: Option<Kind>, ci: bool) -> Result<Vec<String>, String> {
    match held {
        None => Err(NO_REPOSITORY.to_string()),
        Some(Kind::Git) => {
            let fork = if ci {
                "HEAD^1".to_string()
            } else {
                git_fork(root)?
            };
            let mut args = vec!["diff", "--name-only", "--relative", "-z", fork.as_str()];
            // A CI checkout has no upstream, so the commit is compared with its parent.
            args.extend(ci.then_some("HEAD"));
            let out = exec::run("git", &args, root).map_err(|e| e.to_string())?;
            read_changed(&out)
        }
        Some(Kind::Outpost) => outpost_changed(root),
    }
}

/// Where the local change starts: the fork from the upstream, or HEAD on a branch without one.
fn git_fork(root: &Path) -> Result<String, String> {
    let fork = exec::run("git", &["merge-base", "@{upstream}", "HEAD"], root)
        .map_err(|e| e.to_string())?;
    Ok(if fork.success() {
        fork.stdout.trim().to_string()
    } else {
        "HEAD".to_string()
    })
}

fn read_changed(out: &exec::Output) -> Result<Vec<String>, String> {
    if !out.success() {
        return Err(reason(
            "git could not list the changed files; a CI checkout needs two commits (`fetch-depth: 2`)",
            out,
        ));
    }
    enumerated(out.stdout.split('\0').filter(|path| !path.is_empty()))
}

/// The working tree against HEAD, and HEAD against the remote's tip where a remote is set.
fn outpost_changed(root: &Path) -> Result<Vec<String>, String> {
    let mut found = outpost_diff(root, None)?;
    if let Some(tip) = remote_tip(root)? {
        found.extend(outpost_diff(root, Some(&tip))?);
    }
    Ok(found)
}

fn outpost_diff(root: &Path, from: Option<&str>) -> Result<Vec<String>, String> {
    let mut args = vec!["diff", "--name-only"];
    args.extend(from);
    read_diff(&exec::run("outpost", &args, root).map_err(|e| e.to_string())?)
}

/// `outpost diff --name-only`: one path to a line.
fn read_diff(out: &exec::Output) -> Result<Vec<String>, String> {
    if !out.success() {
        return Err(reason("outpost could not list the changed files", out));
    }
    enumerated(out.stdout.lines().filter(|path| !path.is_empty()))
}

fn read_git_log(out: &exec::Output) -> Result<Vec<(String, String)>, String> {
    if !out.success() {
        return Err(reason("git could not list the commits since upstream", out));
    }
    Ok(split_git_log(&out.stdout))
}

/// `%x00` between the hash and the message, `%x01` after each commit, so a message holding blank
/// lines and colons cannot be mistaken for a boundary.
fn split_git_log(log: &str) -> Vec<(String, String)> {
    log.split('\u{1}')
        .filter_map(|entry| entry.trim_start_matches('\n').split_once('\u{0}'))
        .map(|(commit, message)| (commit.to_string(), message.to_string()))
        .collect()
}

/// Outpost's unpushed commits, without agent turns, which Outpost wrote and nobody can rewrite.
fn outpost_unpushed(root: &Path) -> Result<Vec<(String, String)>, String> {
    let Some(tip) = remote_tip(root)? else {
        return Ok(Vec::new());
    };
    let depth = HISTORY.to_string();
    let log =
        exec::run("outpost", &["log", "--json", "-n", &depth], root).map_err(|e| e.to_string())?;
    let agent = exec::run(
        "outpost",
        &["log", "--only-agent", "--json", "-n", &depth],
        root,
    )
    .map_err(|e| e.to_string())?;
    read_outpost_log(&log, &agent, &tip)
}

fn read_outpost_log(
    log: &exec::Output,
    agent: &exec::Output,
    tip: &str,
) -> Result<Vec<(String, String)>, String> {
    if !log.success() {
        return Err(reason("outpost could not read the log", log));
    }
    if !agent.success() {
        return Err(reason("outpost could not list the agent commits", agent));
    }
    since(&log.stdout, &agent.stdout, tip)
}

/// How far back to look for the remote's tip; past it, `since` is an error rather than a prefix.
const HISTORY: usize = 200;

/// The tip of the same-named remote branch, or `None` when no remote is configured.
fn remote_tip(root: &Path) -> Result<Option<String>, String> {
    let current =
        exec::run("outpost", &["branch", "--show-current"], root).map_err(|e| e.to_string())?;
    let listed = exec::run("outpost", &["branch", "-r", REMOTE, "--json"], root)
        .map_err(|e| e.to_string())?;
    read_remote_tip(&current, &listed)
}

/// The remote name every outpost clone gets. No command lists remotes, so this is a convention.
const REMOTE: &str = "origin";

const NO_REMOTE: &str = "No remote named";

/// `None` when no remote is configured. Any other failure is an error, since chock then cannot
/// tell what is published.
fn read_remote_tip(
    current: &exec::Output,
    listed: &exec::Output,
) -> Result<Option<String>, String> {
    if !current.success() {
        return Err(reason("outpost could not name the current branch", current));
    }
    if !listed.success() {
        if listed.stderr.contains(NO_REMOTE) {
            return Ok(None);
        }
        return Err(reason(
            "outpost could not reach the remote, so chock cannot tell what is published",
            listed,
        ));
    }
    let branch = current.stdout.trim();
    let branches: Vec<Branch> = serde_json::from_str(&listed.stdout)
        .map_err(|e| format!("outpost printed branches chock cannot read: {e}"))?;
    Ok(branches
        .into_iter()
        .find(|b| b.name == branch || b.name == format!("{REMOTE}/{branch}"))
        .map(|b| b.commit_id))
}

/// The commits between the remote's tip and HEAD, with the agent's own turns left out.
fn since(log_json: &str, agent_json: &str, tip: &str) -> Result<Vec<(String, String)>, String> {
    let commits: Vec<Commit> = serde_json::from_str(log_json)
        .map_err(|e| format!("outpost printed a log chock cannot read: {e}"))?;
    let agent: Vec<Commit> = serde_json::from_str(agent_json)
        .map_err(|e| format!("outpost printed a log chock cannot read: {e}"))?;
    let written_by_agent: Vec<&str> = agent.iter().map(|c| c.id.as_str()).collect();
    // An agent list as long as the log means the filter was ignored; subtracting it drops all.
    if !commits.is_empty() && written_by_agent.len() >= commits.len() {
        return Err(
            "outpost answered `--only-agent` with the whole log, so chock cannot tell which \
             commits a person wrote"
                .to_string(),
        );
    }
    let mut found = Vec::new();
    for commit in &commits {
        if commit.id == tip {
            return Ok(found);
        }
        if !written_by_agent.contains(&commit.id.as_str()) {
            found.push((commit.id.clone(), commit.message.clone()));
        }
    }
    Err(format!(
        "the remote's tip is not in the last {HISTORY} commits, so chock cannot tell which are unpushed"
    ))
}

const NO_REPOSITORY: &str = "no repository here, so nothing says what changed";

#[derive(Deserialize)]
struct Commit {
    id: String,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct Branch {
    name: String,
    commit_id: String,
}

fn reason(what: &str, out: &exec::Output) -> String {
    format!("{what}: {}", out.why_it_failed())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn the_message_git_is_composing_is_named_by_git_and_nowhere_else() {
        let root = repo("vcs-editmsg");
        let path = root.join(".git").join("COMMIT_EDITMSG");
        // The helper's own commit left one; start with none.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(message_being_composed(&root), None);
        std::fs::write(&path, "A subject being composed\n").unwrap();
        assert_eq!(message_being_composed(&root), Some(path));
        // A tree git does not hold has none, whatever files it holds.
        let bare = crate::testdir::make("vcs-editmsg-bare");
        pretend_outpost_holds(&bare);
        assert_eq!(message_being_composed(&bare), None);
    }

    #[test]
    fn the_commits_a_push_would_carry_are_the_ones_the_upstream_lacks() {
        let root = repo("vcs-upstream");
        let bare = crate::testdir::make("vcs-upstream-remote");
        let git = |args: &[&str], at: &Path| {
            let done = std::process::Command::new("git")
                .args(args)
                .current_dir(at)
                .output()
                .unwrap();
            assert!(done.status.success(), "{args:?}: {done:?}");
        };
        git(&["init", "--bare", "-q", "."], &bare);
        git(
            &["remote", "add", "origin", &bare.display().to_string()],
            &root,
        );
        git(&["push", "-q", "-u", "origin", "HEAD"], &root);
        assert_eq!(unpushed(&root, Some(Kind::Git)).unwrap(), Vec::new());
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        git(&["add", "b.txt"], &root);
        git(
            &["commit", "-q", "-m", "A commit the remote has not seen"],
            &root,
        );
        let held = unpushed(&root, Some(Kind::Git)).unwrap();
        assert_eq!(
            held.iter().map(|(_, m)| m.trim()).collect::<Vec<_>>(),
            vec!["A commit the remote has not seen"]
        );
    }

    #[test]
    fn a_tree_both_systems_hold_names_both_of_them() {
        let dir = crate::testdir::make("vcs-holders");
        assert_eq!(holders(&dir), Vec::new());
        pretend_outpost_holds(&dir);
        assert_eq!(holders(&dir), vec![Kind::Outpost]);
        std::fs::write(dir.join(".git"), "gitdir: elsewhere\n").unwrap();
        assert_eq!(holders(&dir), vec![Kind::Git, Kind::Outpost]);
        assert_eq!(Kind::of(&dir), Some(Kind::Git));
    }

    #[test]
    fn each_system_names_the_ignore_file_it_actually_reads() {
        assert_eq!(Kind::Git.ignore_file(), ".gitignore");
        assert_eq!(Kind::Outpost.ignore_file(), ".outpostignore");
    }

    #[test]
    fn a_declared_hook_standing_is_read_even_where_outpost_refused() {
        let refused = exec::Output {
            code: Some(1),
            stdout: r#"{"local":[],"declared":[],"standing":"changed"}"#.to_string(),
            stderr: String::new(),
            truncated: false,
        };
        assert_eq!(standing(&refused), Some("changed".to_string()));
        assert_eq!(
            standing(&exec::Output {
                stdout: "not json".to_string(),
                ..refused
            }),
            None
        );
    }

    /// A scratch directory sits inside chock's repository, whose config `git config` would write.
    #[test]
    fn a_tree_git_does_not_hold_is_never_written_to() {
        let dir = crate::testdir::make("vcs-not-a-repo");
        assert!(!declare_hook(
            &dir,
            "chock-pre-commit",
            "pre-commit",
            ".chock/hooks/pre-commit"
        ));
        assert!(!point_hooks_at(&dir, ".chock/hooks"));
    }

    #[test]
    fn a_declared_hook_is_recorded_without_touching_the_hooks_directory() {
        let root = repo("vcs-declared");
        assert!(declare_hook(
            &root,
            "chock-pre-commit",
            "pre-commit",
            ".chock/hooks/pre-commit"
        ));
        let event = exec::run(
            "git",
            &["config", "--get", "hook.chock-pre-commit.event"],
            &root,
        )
        .unwrap();
        assert_eq!(event.stdout.trim(), "pre-commit");
        let command = exec::run(
            "git",
            &["config", "--get", "hook.chock-pre-commit.command"],
            &root,
        )
        .unwrap();
        assert_eq!(command.stdout.trim(), ".chock/hooks/pre-commit");
        assert_eq!(
            hooks_path(&root),
            None,
            "the hooks directory was taken over"
        );
    }

    #[test]
    fn a_git_old_enough_to_lack_declared_hooks_is_told_from_one_that_has_them() {
        assert!(declares_hooks(&said(
            "git version 2.55.0.782.g1630431f32\n"
        )));
        assert!(declares_hooks(&said("git version 2.54.0\n")));
        assert!(!declares_hooks(&said("git version 2.43.0\n")));
        assert!(!declares_hooks(&said("git version 1.9.5\n")));
    }

    /// Assuming it does would declare a hook nothing runs.
    #[test]
    fn a_git_whose_version_cannot_be_read_is_not_assumed_to_declare_hooks() {
        assert!(!declares_hooks(&said("not a version\n")));
        assert!(!declares_hooks(&refused("not a git repository")));
    }

    #[test]
    fn a_git_that_runs_declared_hooks_is_told_apart_from_one_that_does_not() {
        assert_eq!(
            read_version(&said("git version 2.55.0.782.g1630431f32\n")),
            Some((2, 55))
        );
        assert_eq!(read_version(&said("git version 2.54.0\n")), Some((2, 54)));
        assert_eq!(read_version(&said("git version 2.43.0\n")), Some((2, 43)));
        assert!(Some((2, 55)) >= Some(DECLARED_HOOKS_SINCE));
        assert!(Some((2, 43)) < Some(DECLARED_HOOKS_SINCE));
    }

    #[test]
    fn a_version_that_cannot_be_read_is_not_taken_for_a_newer_git() {
        assert_eq!(read_version(&said("not a version\n")), None);
        assert_eq!(read_version(&said("git version two\n")), None);
        assert_eq!(read_version(&refused("not a git directory")), None);
    }

    #[test]
    fn a_repository_with_more_files_than_chock_enumerates_is_refused() {
        let many: Vec<String> = (0..=MAX_TRACKED).map(|n| format!("src/f{n}.rs")).collect();
        let err = enumerated(many.iter().map(String::as_str)).unwrap_err();
        assert_eq!(
            err,
            format!(
                "this repository tracks more than {MAX_TRACKED} files, which chock will not enumerate"
            )
        );
    }

    #[test]
    fn a_repository_at_the_limit_is_still_enumerated_whole() {
        let many: Vec<String> = (0..MAX_TRACKED).map(|n| format!("src/f{n}.rs")).collect();
        let held = enumerated(many.iter().map(String::as_str)).unwrap();
        assert_eq!(held.first().map(String::as_str), Some("src/f0.rs"));
        assert_eq!(
            held.last().map(String::as_str),
            Some(format!("src/f{}.rs", MAX_TRACKED - 1).as_str())
        );
    }

    #[test]
    fn an_ordinary_listing_comes_back_in_the_order_it_arrived() {
        let held = enumerated(["src/b.rs", "src/a.rs"].into_iter()).unwrap();
        assert_eq!(held, vec!["src/b.rs".to_string(), "src/a.rs".to_string()]);
    }

    fn dir(name: &str, children: Vec<Node>) -> Node {
        Node {
            node: Entry::Directory {
                node: Named {
                    name: name.to_string(),
                },
            },
            children,
        }
    }

    fn file(name: &str) -> Node {
        Node {
            node: Entry::File {
                node: Named {
                    name: name.to_string(),
                },
            },
            children: vec![],
        }
    }

    fn walked(tree: &Node) -> Vec<String> {
        let mut found = Vec::new();
        paths(tree, "", &mut found);
        found
    }

    fn git(root: &Path, args: &[&str]) {
        let out = exec::run("git", args, root).unwrap();
        assert!(out.success(), "git {args:?}: {}", out.stderr);
    }

    fn repo(name: &str) -> crate::testdir::Scratch {
        let root = crate::testdir::make(name);
        git(&root, &["init", "--quiet"]);
        git(&root, &["config", "user.email", "t@example.com"]);
        git(&root, &["config", "user.name", "t"]);
        std::fs::write(root.join("a.rs"), "fn f() -> u8 { 1 }\n").unwrap();
        git(&root, &["add", "a.rs"]);
        git(&root, &["commit", "--quiet", "-m", "first"]);
        std::fs::write(root.join("a.rs"), "fn f() -> u8 { 2 }\n").unwrap();
        git(&root, &["add", "a.rs"]);
        git(&root, &["commit", "--quiet", "-m", "second"]);
        root
    }

    /// A crate two levels below a repository root with an upstream, and two unpushed commits: one
    /// inside the crate and one outside it.
    fn nested(name: &str) -> (crate::testdir::Scratch, std::path::PathBuf) {
        let root = repo(name);
        let bare = root.join("remote.git");
        git(&root, &["init", "--bare", "--quiet", "remote.git"]);
        git(
            &root,
            &["remote", "add", "origin", &bare.display().to_string()],
        );
        git(&root, &["push", "--quiet", "-u", "origin", "HEAD"]);
        let crate_dir = root.join("crates/c");
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::write(crate_dir.join("src/lib.rs"), "").unwrap();
        git(&root, &["add", "crates"]);
        git(&root, &["commit", "--quiet", "-m", "In the crate"]);
        std::fs::write(root.join("a.rs"), "fn f() -> u8 { 3 }\n").unwrap();
        git(
            &root,
            &["commit", "--quiet", "-am", "In the rest of the tree"],
        );
        (root, crate_dir)
    }

    #[test]
    fn a_crate_below_its_repository_root_is_read_as_its_own_part_of_it() {
        let (root, crate_dir) = nested("vcs-nested");
        assert_eq!(
            holders(&crate_dir),
            Vec::new(),
            "nothing sits at the crate itself"
        );
        assert_eq!(live_holder(&crate_dir), Some(Kind::Git));
        assert_eq!(
            tracked(&crate_dir, Some(Kind::Git)).unwrap(),
            ["src/lib.rs"]
        );
        let messages = |at: &Path| -> Vec<String> {
            unpushed(at, Some(Kind::Git))
                .unwrap()
                .into_iter()
                .map(|(_, message)| message.trim().to_string())
                .collect()
        };
        assert_eq!(messages(&crate_dir), ["In the crate"]);
        assert_eq!(messages(&root), ["In the rest of the tree", "In the crate"]);
        // Scratch sits inside this repository, and git's ceiling keeps it from being found there.
        assert_eq!(live_holder(&crate::testdir::make("vcs-nested-none")), None);
    }

    #[test]
    fn the_root_a_clone_starts_from_is_found_for_either_system_and_none_without_one() {
        let root = repo("vcs-repository-root");
        let inner = root.join("crates/inner");
        std::fs::create_dir_all(&inner).unwrap();
        let top = repository_root(&inner, Some(Kind::Git)).unwrap();
        assert_eq!(top.canonicalize().unwrap(), root.canonicalize().unwrap());
        let held = crate::testdir::make("vcs-outpost-root");
        pretend_outpost_holds(&held);
        std::fs::create_dir_all(held.join("sub")).unwrap();
        assert_eq!(
            repository_root(&held.join("sub"), Some(Kind::Outpost)),
            Some(held.to_path_buf())
        );
        assert_eq!(repository_root(&root, None), None);
    }

    #[test]
    fn a_git_tree_is_recognised_and_a_bare_directory_is_not() {
        let root = repo("vcs-kind");
        assert_eq!(Kind::of(&root), Some(Kind::Git));
        assert_eq!(Kind::of(&crate::testdir::make("vcs-none")), None);
    }

    /// `.cargo/config.toml` sets a git ceiling at the scratch root.
    #[test]
    fn git_run_in_a_scratch_directory_cannot_reach_the_repository_around_it() {
        let dir = crate::testdir::make("vcs-fenced");
        let out = exec::run("git", &["rev-parse", "--git-dir"], &dir).unwrap();
        assert_eq!(
            (out.success(), out.stdout.trim()),
            (false, ""),
            "{}",
            out.stderr
        );
    }

    #[test]
    fn the_changed_files_are_those_since_the_upstream_fork_and_in_ci_those_of_the_commit() {
        let (root, crate_dir) = nested("vcs-changed");
        std::fs::write(crate_dir.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        assert_eq!(
            changed(&crate_dir, Some(Kind::Git), false).unwrap(),
            ["src/lib.rs"]
        );
        assert_eq!(
            changed(&root, Some(Kind::Git), false).unwrap(),
            ["a.rs", "crates/c/src/lib.rs"]
        );
        assert_eq!(changed(&root, Some(Kind::Git), true).unwrap(), ["a.rs"]);
        assert_eq!(
            changed(&crate_dir, Some(Kind::Git), true).unwrap(),
            Vec::<String>::new()
        );
        assert!(changed(&root, None, false).is_err());
    }

    #[test]
    fn a_branch_without_an_upstream_changes_only_what_is_not_committed() {
        let root = repo("vcs-changed-local");
        assert_eq!(
            changed(&root, Some(Kind::Git), false).unwrap(),
            Vec::<String>::new()
        );
        std::fs::write(root.join("a.rs"), "fn f() -> u8 { 4 }\n").unwrap();
        assert_eq!(changed(&root, Some(Kind::Git), false).unwrap(), ["a.rs"]);
    }

    #[test]
    fn a_ci_checkout_of_one_commit_cannot_say_what_changed() {
        let root = crate::testdir::make("vcs-changed-shallow");
        git(&root, &["init", "--quiet"]);
        git(&root, &["config", "user.email", "t@example.com"]);
        git(&root, &["config", "user.name", "t"]);
        std::fs::write(root.join("a.rs"), "").unwrap();
        git(&root, &["add", "a.rs"]);
        git(&root, &["commit", "--quiet", "-m", "only"]);
        let why = changed(&root, Some(Kind::Git), true).unwrap_err();
        assert!(why.contains("fetch-depth: 2"), "{why}");
    }

    #[test]
    fn outpost_changes_are_the_edits_since_head() {
        // Skipped where outpost is not installed; `read_diff` is held without it.
        let Some(root) = outpost_repo("vcs-changed-outpost") else {
            return;
        };
        std::fs::write(root.join("a.rs"), "").unwrap();
        let run = |args: &[&str]| exec::run("outpost", args, &root).unwrap();
        assert!(run(&["add", "."]).success() && run(&["commit", "-m", "first"]).success());
        std::fs::write(root.join("a.rs"), "fn f() {}\n").unwrap();
        assert_eq!(
            changed(&root, Some(Kind::Outpost), false).unwrap(),
            ["a.rs"]
        );
    }

    #[test]
    fn an_outpost_diff_is_read_one_path_to_a_line_and_a_refusal_is_an_error() {
        let read = read_diff(&said("src/a.rs\n\nsrc/b.rs\n")).unwrap();
        assert_eq!(read, ["src/a.rs", "src/b.rs"]);
        assert!(read_diff(&refused("no repository")).is_err());
    }

    #[test]
    fn a_branch_with_no_upstream_reports_no_unpushed_commits_rather_than_all_of_them() {
        assert_eq!(
            unpushed(&repo("vcs-unpushed"), Some(Kind::Git)).unwrap(),
            Vec::<(String, String)>::new()
        );
    }

    #[test]
    fn a_bare_outpost_directory_is_not_taken_for_a_repository() {
        let root = crate::testdir::make("vcs-outpost-bare");
        std::fs::create_dir_all(root.join(".outpost")).unwrap();
        assert_eq!(holders(&root), Vec::new());
        assert_eq!(live_holder(&root), None);
        // Asked anyway, every question is refused.
        assert!(tracked(&root, None).is_err());
        assert!(unpushed(&root, None).is_err());
        assert!(commits(&root, None).is_err());
    }

    /// A repository `outpost init` made, or `None` where outpost is not installed.
    fn outpost_repo(name: &str) -> Option<crate::testdir::Scratch> {
        made_by("outpost", name)
    }

    /// `outpost_repo` with the program as a parameter, so a test can reach the absent-tool arm.
    fn made_by(program: &str, name: &str) -> Option<crate::testdir::Scratch> {
        let dir = crate::testdir::make(name);
        let made = std::process::Command::new(program)
            .args(["init"])
            .current_dir(&dir)
            .output()
            .ok()?;
        assert!(made.status.success(), "{program} init");
        Some(dir)
    }

    /// CI runners have no outpost, so this arm decides whether the outpost tests run.
    #[test]
    fn a_tool_nothing_installed_makes_no_repository_rather_than_failing() {
        assert!(made_by("outpost-that-nothing-installs", "vcs-no-such-tool").is_none());
    }

    #[test]
    fn a_repository_holding_no_commit_refuses_every_question_rather_than_answering_empty() {
        // Skipped where outpost is not installed.
        if let Some(root) = outpost_repo("vcs-outpost-empty") {
            assert_eq!(holders(&root), vec![Kind::Outpost]);
            assert!(tracked(&root, Some(Kind::Outpost)).is_err());
            assert!(commits(&root, Some(Kind::Outpost)).is_err());
            // No remote and no branch, so there is nothing published to compare against.
            assert_eq!(unpushed(&root, Some(Kind::Outpost)).unwrap(), Vec::new());
            // Nothing declared here, which outpost reports rather than refusing.
            assert_eq!(declared_hook_standing(&root), Some("absent".to_string()));
        }
    }

    /// Out of outpost's reach, an empty answer would read as clean, so each question is refused.
    #[test]
    fn an_outpost_out_of_reach_refuses_every_question_rather_than_answering_empty() {
        let gone = crate::testdir::make("vcs-outpost-gone").join("gone");
        assert!(tracked(&gone, Some(Kind::Outpost)).is_err());
        assert!(changed(&gone, Some(Kind::Outpost), false).is_err());
        assert!(unpushed(&gone, Some(Kind::Outpost)).is_err());
        assert_eq!(declared_hook_standing(&gone), None);
    }

    /// A sole holder is the answer without counting, so this runs without outpost installed.
    #[test]
    fn a_repository_outpost_really_made_is_recognised_by_its_marker() {
        let root = crate::testdir::make("vcs-outpost-marked");
        std::fs::create_dir_all(root.join(".outpost")).unwrap();
        std::fs::write(root.join(".outpost").join(OUTPOST_MARKER), "").unwrap();
        assert_eq!(holders(&root), vec![Kind::Outpost]);
        assert_eq!(live_holder(&root), Some(Kind::Outpost));
    }

    #[test]
    fn the_longer_history_is_the_live_one_and_level_reads_as_git() {
        assert_eq!(live(Some(41), Some(46)), Kind::Outpost);
        assert_eq!(live(Some(1277), Some(213)), Kind::Git);
        // Level means an up-to-date mirror, so either answer is the same.
        assert_eq!(live(Some(9), Some(9)), Kind::Git);
    }

    /// With both silent, the gates fail with the tool's own reason.
    #[test]
    fn a_history_that_cannot_be_counted_is_not_the_live_one() {
        assert_eq!(live(None, Some(1)), Kind::Outpost);
        assert_eq!(live(Some(1), None), Kind::Git);
        assert_eq!(live(None, None), Kind::Git);
    }

    #[test]
    fn a_tree_both_hold_is_read_as_whichever_of_them_answers() {
        let root = repo("vcs-both");
        pretend_outpost_holds(&root);
        assert_eq!(holders(&root), vec![Kind::Git, Kind::Outpost]);
        assert_eq!(live_holder(&root), Some(Kind::Git));
        assert!(tracked(&root, Some(Kind::Git)).is_ok());
    }

    #[test]
    fn a_directory_that_is_no_repository_is_refused_by_every_question() {
        let root = crate::testdir::make("vcs-bare");
        assert!(tracked(&root, None).is_err());
        assert!(unpushed(&root, None).is_err());
    }

    #[test]
    fn a_hooks_path_is_read_only_when_git_both_answered_and_named_one() {
        assert_eq!(
            read_hooks_path(&said(".chock/hooks\n")),
            Some(".chock/hooks".to_string())
        );
        assert_eq!(read_hooks_path(&said("")), None);
        assert_eq!(read_hooks_path(&said("   \n")), None);
        assert_eq!(read_hooks_path(&refused("not a repository")), None);
    }

    #[test]
    fn a_real_declared_hook_reaches_machine_health_without_being_executed() {
        let dir = repo("declared-health");
        git(
            &dir,
            &["config", "hook.chock-pre-commit.event", "pre-commit"],
        );
        git(
            &dir,
            &[
                "config",
                "hook.chock-pre-commit.command",
                "chock-missing-fixture-command",
            ],
        );
        let rows = crate::setup::doctor::machine_health(
            &dir,
            &crate::setup::pins::AdditionalPins::default(),
        );
        assert!(
            rows.iter()
                .any(|row| row.command.contains("chock-pre-commit") && row.is_failure()),
            "{rows:?}"
        );
    }

    #[test]
    fn declarations_require_version_support_but_empty_lists_do_not_query_it() {
        let hook = GitHook {
            name: "chock-pre-commit".into(),
            events: vec!["pre-commit".into()],
            command: Some("chock hook pre-commit".into()),
        };
        assert_eq!(
            confirm_declared_hooks(Vec::new(), || Err("must not query".into())),
            Ok(Vec::new())
        );
        assert_eq!(
            confirm_declared_hooks(vec![hook.clone()], || Ok(said("git version 2.54.0\n"))),
            Ok(vec![hook.clone()])
        );
        for version in ["git version 2.53.0\n", "not a version"] {
            assert_eq!(confirm_declared_hooks(vec![hook.clone()], || Ok(said(version))), Err("cannot confirm Git supports the configured hook declarations; Git 2.54 or later is required".into()));
        }
        assert_eq!(
            confirm_declared_hooks(vec![hook], || Err("query failed".into())),
            Err("query failed".into())
        );
    }

    #[test]
    fn pointing_a_real_repository_at_hooks_reports_the_persisted_setting() {
        let dir = repo("point-hooks");
        assert!(point_hooks_at(&dir, ".chock/fixture-hooks"));
        assert_eq!(hooks_path(&dir), Some(".chock/fixture-hooks".into()));
    }

    #[test]
    fn nul_delimited_declarations_keep_multiline_commands_whole() {
        let out = said(
            "hook.other.command\nignored\0hook.chock-pre-commit.event\npre-commit\0hook.chock-pre-commit.command\nchock hook pre-commit\0",
        );
        assert_eq!(
            read_git_hook_declarations(&out).unwrap(),
            vec![GitHook {
                name: "chock-pre-commit".to_string(),
                events: vec!["pre-commit".to_string()],
                command: Some("chock hook pre-commit".to_string()),
            }]
        );
        let out = said("hook.chock-pre-push.command\nfirst\nsecond\0");
        assert_eq!(
            read_git_hook_declarations(&out).unwrap(),
            vec![GitHook {
                name: "chock-pre-push".to_string(),
                events: Vec::new(),
                command: Some("first\nsecond".to_string()),
            }]
        );
    }

    #[test]
    fn an_absent_declaration_is_distinct_from_failed_or_truncated_config_output() {
        assert_eq!(
            read_git_hook_declarations(&refused("")).unwrap(),
            Vec::new()
        );
        assert!(
            read_git_hook_declarations(&refused("cannot read config"))
                .unwrap_err()
                .contains("cannot read config")
        );
        for text in [
            "",
            "hook.chock-pre-push.command\nx",
            "no separator\0",
            "hook.chock-other\nx\0",
            "hook.chock-pre-push.unknown\nx\0",
        ] {
            assert!(read_git_hook_declarations(&said(text)).is_err(), "{text:?}");
        }
        let mut out = said("hook.chock-pre-commit.event\npre-commit\0");
        out.truncated = true;
        assert_eq!(
            read_git_hook_declarations(&out),
            Err("Git hook declarations were truncated".to_string())
        );
    }

    #[test]
    fn a_declaration_query_cannot_read_an_ancestor_repository() {
        let dir = crate::testdir::make("hook-no-repository");
        assert_eq!(git_hook_declarations(&dir).unwrap(), Vec::new());
    }

    fn said(stdout: &str) -> exec::Output {
        exec::Output {
            code: Some(0),
            stdout: stdout.to_string(),
            stderr: String::new(),
            truncated: false,
        }
    }

    fn refused(stderr: &str) -> exec::Output {
        exec::Output {
            code: Some(1),
            stdout: String::new(),
            stderr: stderr.to_string(),
            truncated: false,
        }
    }

    #[test]
    fn a_log_that_failed_is_refused_rather_than_read_as_no_commits() {
        assert!(read_git_log(&refused("no upstream")).is_err());
        assert!(read_outpost_log(&refused("no log"), &said("[]"), "aaa").is_err());
        assert!(read_outpost_log(&said("[]"), &refused("no log"), "aaa").is_err());
    }

    /// An unread log also parses as nothing, so only this catches an inverted success check.
    #[test]
    fn a_log_that_succeeded_is_read_rather_than_refused() {
        let json = log(&[("bbb", &["aaa"], "second"), ("aaa", &[], "first")]);
        assert_eq!(
            read_outpost_log(&said(&json), &said("[]"), "aaa").unwrap(),
            vec![("bbb".to_string(), "second".to_string())]
        );
        assert_eq!(
            read_git_log(&said("aaa\u{0}Subject\n\u{1}")).unwrap(),
            vec![("aaa".to_string(), "Subject\n".to_string())]
        );
    }

    #[test]
    fn the_commits_behind_head_are_counted_in_the_repository_that_holds_them() {
        assert_eq!(commits(&repo("vcs-commits"), Some(Kind::Git)), Ok(2));
        assert_eq!(
            commits(&crate::testdir::make("vcs-uncounted"), None),
            Err("no repository here, so nothing says how many commits there are".to_string())
        );
    }

    #[test]
    fn a_commit_count_is_read_from_what_each_system_printed() {
        assert_eq!(read_git_count(&said("1277\n")), Ok(1277));
        assert_eq!(
            read_outpost_count(&said("ddeb556 turn 207\n3eb1e41 turn 204\n\n")),
            Ok(2)
        );
    }

    #[test]
    fn a_history_that_could_not_be_counted_is_refused_rather_than_counted_as_empty() {
        assert!(read_git_count(&refused("not a git repository")).is_err());
        assert!(read_outpost_count(&refused("no repository here")).is_err());
    }

    #[test]
    fn a_history_chock_had_to_truncate_is_refused_rather_than_counted_short() {
        let mut cut = said("ddeb556 turn 207\n");
        cut.truncated = true;
        assert!(read_outpost_count(&cut).is_err());
    }

    #[test]
    fn a_count_that_is_not_a_number_is_refused_rather_than_read_as_no_commits() {
        assert!(
            read_git_count(&said("HEAD\n"))
                .unwrap_err()
                .starts_with("git printed a commit count chock cannot read")
        );
    }

    #[test]
    fn an_agent_log_that_succeeded_is_read_rather_than_refused() {
        let all = log(&[("bbb", &["aaa"], "a person"), ("aaa", &[], "first")]);
        let agent = log(&[("bbb", &["aaa"], "a person")]);
        assert_eq!(
            read_outpost_log(&said(&all), &said(&agent), "aaa").unwrap(),
            Vec::<(String, String)>::new()
        );
    }

    #[test]
    fn a_repository_with_no_remote_has_no_tip_rather_than_an_error() {
        assert_eq!(
            read_remote_tip(&said("main"), &refused("No remote named 'origin' is set.")).unwrap(),
            None
        );
    }

    #[test]
    fn a_tree_that_cannot_name_its_own_branch_is_refused_before_the_remote_is_asked() {
        let err = read_remote_tip(&refused("not a repository"), &said("[]")).unwrap_err();
        assert!(err.contains("could not name the current branch"), "{err}");
    }

    #[test]
    fn a_remote_that_could_not_be_reached_is_refused_rather_than_read_as_no_remote() {
        let err = read_remote_tip(
            &said("main"),
            &refused("Network error: error sending request"),
        )
        .unwrap_err();
        assert!(err.contains("could not reach the remote"), "{err}");
    }

    fn log(commits: &[(&str, &[&str], &str)]) -> String {
        let entries: Vec<String> = commits
            .iter()
            .map(|(id, parents, message)| {
                let parents: Vec<String> = parents.iter().map(|p| format!("\"{p}\"")).collect();
                format!(
                    "{{\"id\":\"{id}\",\"parent_ids\":[{}],\"message\":{}}}",
                    parents.join(","),
                    serde_json::to_string(message).unwrap()
                )
            })
            .collect();
        format!("[{}]", entries.join(","))
    }

    #[test]
    fn the_remote_tip_is_found_whether_the_branch_carries_the_remote_name_or_not() {
        let plain = r#"[{"name":"main","commit_id":"aaa","is_current":false}]"#;
        let prefixed = r#"[{"name":"origin/main","commit_id":"bbb","is_current":false}]"#;
        assert_eq!(
            read_remote_tip(&said("main"), &said(plain)).unwrap(),
            Some("aaa".to_string())
        );
        assert_eq!(
            read_remote_tip(&said("main"), &said(prefixed)).unwrap(),
            Some("bbb".to_string())
        );
    }

    #[test]
    fn a_branch_the_remote_does_not_have_leaves_nothing_to_compare_against() {
        let json = r#"[{"name":"main","commit_id":"aaa","is_current":false}]"#;
        assert_eq!(
            read_remote_tip(&said("feature"), &said(json)).unwrap(),
            None
        );
    }

    #[test]
    fn a_filter_that_answered_with_the_whole_log_is_refused_rather_than_subtracted() {
        let all = log(&[("bbb", &["aaa"], "second"), ("aaa", &[], "first")]);
        let err = since(&all, &all, "aaa").unwrap_err();
        assert!(err.contains("`--only-agent` with the whole log"), "{err}");
    }

    #[test]
    fn a_filter_that_named_some_of_the_log_is_still_subtracted() {
        let all = log(&[("bbb", &["aaa"], "second"), ("aaa", &[], "first")]);
        let agent = log(&[("bbb", &["aaa"], "second")]);
        assert_eq!(since(&all, &agent, "aaa").unwrap(), vec![]);
    }

    #[test]
    fn the_commits_since_the_remote_tip_stop_at_the_tip_itself() {
        let all = log(&[
            ("ccc", &["bbb"], "third"),
            ("bbb", &["aaa"], "second"),
            ("aaa", &[], "first"),
        ]);
        assert_eq!(
            since(&all, "[]", "aaa").unwrap(),
            vec![
                ("ccc".to_string(), "third".to_string()),
                ("bbb".to_string(), "second".to_string()),
            ]
        );
    }

    #[test]
    fn a_commit_an_agent_wrote_is_left_out_of_what_the_commits_gate_reads() {
        let all = log(&[
            ("ccc", &["bbb"], "a person wrote this"),
            ("bbb", &["aaa"], "[claude-code] turn 136"),
            ("aaa", &[], "first"),
        ]);
        let agent = log(&[("bbb", &["aaa"], "[claude-code] turn 136")]);
        assert_eq!(
            since(&all, &agent, "aaa").unwrap(),
            vec![("ccc".to_string(), "a person wrote this".to_string())]
        );
    }

    #[test]
    fn a_tip_beyond_the_history_read_is_refused_rather_than_reported_as_a_prefix() {
        let all = log(&[("ccc", &["bbb"], "third"), ("bbb", &["aaa"], "second")]);
        assert!(since(&all, "[]", "zzz").is_err());
    }

    #[test]
    fn each_commit_is_read_out_of_the_git_log_with_its_own_message() {
        let log = "aaa\u{0}Subject one\n\nbody one\n\u{1}\nbbb\u{0}Subject two\n\u{1}";
        assert_eq!(
            split_git_log(log),
            vec![
                ("aaa".to_string(), "Subject one\n\nbody one\n".to_string()),
                ("bbb".to_string(), "Subject two\n".to_string()),
            ]
        );
    }

    #[test]
    fn a_tree_yields_every_file_under_the_directory_holding_it() {
        let tree = dir("", vec![dir("src", vec![file("lib.rs"), file("main.rs")])]);
        assert_eq!(walked(&tree), ["src/lib.rs", "src/main.rs"]);
    }

    #[test]
    fn a_file_at_the_root_is_named_without_a_leading_slash() {
        let tree = dir("", vec![file("Cargo.toml")]);
        assert_eq!(walked(&tree), ["Cargo.toml"]);
    }

    #[test]
    fn nesting_is_followed_to_any_depth() {
        let tree = dir("", vec![dir("a", vec![dir("b", vec![file("c.rs")])])]);
        assert_eq!(walked(&tree), ["a/b/c.rs"]);
    }

    #[test]
    fn a_node_that_is_neither_a_file_nor_a_directory_is_stepped_over() {
        let commit = Node {
            node: Entry::Other(serde::de::IgnoredAny),
            children: vec![dir("", vec![file("x.rs")])],
        };
        assert_eq!(walked(&commit), ["x.rs"]);
    }

    #[test]
    fn a_tree_with_no_repository_under_it_is_not_claimed_by_either() {
        let dir = crate::testdir::make("vcs-none");
        assert_eq!(live_holder(&dir), None);
        assert!(tracked(&dir, None).is_err());
    }

    #[test]
    fn a_git_checkout_is_answered_by_git_and_an_outpost_one_by_outpost() {
        let git = crate::testdir::make("vcs-git");
        std::fs::create_dir_all(git.join(".git")).unwrap();
        assert_eq!(Kind::of(&git), Some(Kind::Git));
        assert_eq!(Kind::of(&git).unwrap().command(), "git");

        let outpost = crate::testdir::make("vcs-outpost");
        pretend_outpost_holds(&outpost);
        assert_eq!(Kind::of(&outpost), Some(Kind::Outpost));
        assert_eq!(Kind::of(&outpost).unwrap().command(), "outpost");
    }

    #[test]
    fn a_worktree_whose_git_is_a_file_is_still_a_git_checkout() {
        let dir = crate::testdir::make("vcs-worktree");
        std::fs::write(dir.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(Kind::of(&dir), Some(Kind::Git));
    }

    fn ran(stdout: &str, code: Option<i32>) -> exec::Output {
        exec::Output {
            code,
            stdout: stdout.to_string(),
            stderr: "no repository".to_string(),
            truncated: false,
        }
    }

    #[test]
    fn git_separates_paths_by_nul_so_a_newline_in_one_stays_in_it() {
        let listed = ran("src/a.rs\0src/odd\nname.rs\0", Some(0));
        assert_eq!(read_git(&listed).unwrap(), ["src/a.rs", "src/odd\nname.rs"]);
    }

    #[test]
    fn a_git_that_found_no_repository_is_an_error_not_an_empty_tree() {
        assert_eq!(
            read_git(&ran("", Some(128))),
            Err("git ls-files read no repository here: no repository".to_string())
        );
    }

    #[test]
    fn an_outpost_that_failed_is_an_error_not_an_empty_tree() {
        assert_eq!(
            read_outpost(&ran("", Some(1))),
            Err("outpost could not read the tree: no repository".to_string())
        );
    }

    #[test]
    fn a_tree_wrapped_in_the_nodes_outpost_adds_is_still_read() {
        let tree = r#"{
          "node": {"Commit": {"node": {"hash": "aa", "message": "m"}}},
          "children": [{
            "node": {"Directory": {"node": {"name": ""}}},
            "children": [{
              "node": {"VNode": {"node": {"hash": "bb"}}},
              "children": [
                {"node": {"File": {"node": {"name": "Cargo.toml"}}}},
                {
                  "node": {"Directory": {"node": {"name": "src"}}},
                  "children": [{
                    "node": {"VNode": {"node": {"hash": "cc"}}},
                    "children": [{"node": {"File": {"node": {"name": "lib.rs"}}}}]
                  }]
                }
              ]
            }]
          }]
        }"#;
        assert_eq!(
            read_outpost(&ran(tree, Some(0))).unwrap(),
            vec!["Cargo.toml".to_string(), "src/lib.rs".to_string()]
        );
    }

    #[test]
    fn a_tree_outpost_printed_in_a_shape_chock_does_not_know_stops_the_gate() {
        assert!(
            read_outpost(&ran("not json", Some(0)))
                .unwrap_err()
                .starts_with("outpost printed a tree chock cannot read")
        );
    }

    #[test]
    fn joining_leaves_out_the_empty_half_whichever_half_it_is() {
        assert_eq!(join("", "Cargo.toml"), "Cargo.toml");
        assert_eq!(join("src", ""), "src");
        assert_eq!(join("src", "lib.rs"), "src/lib.rs");
        assert_eq!(join("", ""), "");
    }
}
