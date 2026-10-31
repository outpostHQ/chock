//! The git hooks chock declares: the hooks path, the declared-hooks support of git, and the install and removal of the hooks.

use std::path::Path;

use super::{Kind, holders};
use crate::exec;

/// `core.hooksPath`, or `None` where it is unset or git could not answer.
pub fn hooks_path(root: &Path) -> Option<String> {
    let out = exec::run("git", &["config", "--get", "core.hooksPath"], root).ok()?;
    read_hooks_path(&out)
}

/// The configured hooks path; an unset key (a non-zero exit) or an empty value is `None`.
pub(super) fn read_hooks_path(out: &exec::Output) -> Option<String> {
    if !out.success() {
        return None;
    }
    let set = out.stdout.trim();
    (!set.is_empty()).then(|| set.to_string())
}

/// One `hook.chock-*` declaration in git config.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GitHook {
    /// The declaration's name, after `hook.`.
    pub name: String,
    /// The git events that run it, such as `pre-commit`.
    pub events: Vec<String>,
    /// The command it runs; `None` where the declaration gives none.
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
/// # Errors
/// When git cannot read its config, or is older than the hook declarations need.
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

pub(super) fn confirm_declared_hooks(
    hooks: Vec<GitHook>,
    version: impl FnOnce() -> Result<exec::Output, String>,
) -> Result<Vec<GitHook>, String> {
    if !hooks.is_empty() {
        require_declared_hook_support(&version()?)?;
    }
    Ok(hooks)
}

pub(super) fn read_git_hook_declarations(out: &exec::Output) -> Result<Vec<GitHook>, String> {
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
pub(super) const DECLARED_HOOKS_SINCE: (u32, u32) = (2, 54);

/// Whether the installed git runs hooks declared in config.
#[must_use]
pub fn runs_declared_hooks(root: &Path) -> bool {
    exec::run("git", &["--version"], root).is_ok_and(|out| declares_hooks(&out))
}

/// Whether a `git --version` answer is at least `DECLARED_HOOKS_SINCE`. Apart from the spawn, so a
/// test can reach both answers.
#[must_use]
pub fn declares_hooks(out: &exec::Output) -> bool {
    read_version(out).is_some_and(|found| found >= DECLARED_HOOKS_SINCE)
}

/// `git version 2.55.0.782.g1630431f32`, of which only the first two numbers decide.
pub(super) fn read_version(out: &exec::Output) -> Option<(u32, u32)> {
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
