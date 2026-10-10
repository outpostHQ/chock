//! The commands that read a tree or a program chock does not own: no config and no record.

use std::path::PathBuf;
use std::process::ExitCode;

use super::args::split_json;
use super::{VERSION, cannot_run, emit, usage};
use crate::gates::metrics::lean::report;
use crate::run::{Ctx, baseline::Baseline, report::Verdict};
use crate::{oracle, slop, sweep};

/// Why a command stopped with no exit code of its own: its words were wrong, or it could not read.
enum Stop {
    Usage(String),
    CannotRun(String),
}

impl From<String> for Stop {
    fn from(why: String) -> Self {
        Self::CannotRun(why)
    }
}

impl Stop {
    /// The exit of the command `name` that stopped.
    fn exit(self, name: &str) -> ExitCode {
        match self {
            Self::Usage(why) => usage(&why),
            Self::CannotRun(why) => cannot_run(&format!("{name}: {why}")),
        }
    }
}

/// Runs the command `name`, which `parse` already knows.
pub(super) fn reads(name: &str, args: &[&str]) -> ExitCode {
    let (rest, json) = split_json(args);
    let done = match name {
        "lean" => lean(&rest, json),
        "oracle" => compare(&rest, json),
        "sweep" | "moved" => proof(name, &rest, json),
        _ => long_comments(&rest, json),
    };
    done.map_or_else(|stop| stop.exit(name), ExitCode::from)
}

/// The directory a command named, or the one chock was started in.
fn dir_or_here(named: Option<&str>) -> Result<PathBuf, String> {
    let dir = match named {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()
            .map_err(|e| format!("cannot read the current directory: {e}"))?,
    };
    // outpost: ignore[path-is-dir-follows-symlinks] a link the user names is its directory.
    match dir.is_dir() {
        true => Ok(dir),
        false => Err(format!("{} is not a directory", dir.display())),
    }
}

/// A report on stdout, as JSON or as text for a person.
fn shown(json: bool, as_json: &str, as_text: &str) {
    if json {
        emit(&format!("{as_json}\n"));
    } else {
        emit(as_text);
    }
}

fn long_comments(rest: &[&str], json: bool) -> Result<u8, Stop> {
    let ([] | [_]) = rest else {
        return Err(Stop::Usage("slop takes at most one path".to_string()));
    };
    let root = dir_or_here(rest.first().copied())
        .map_err(|why| format!("{why}; `chock edited PATH` checks one file"))?;
    let hits = slop::scan(&root)?;
    let as_json = slop::render_json(&hits, &root, VERSION);
    shown(json, &as_json, &slop::render(&hits, &root));
    Ok(hits.first().map_or(0, |_| Verdict::Tripped.code()))
}

/// `chock lean`: a report and never a verdict, so it exits zero unless a file did not read.
fn lean(rest: &[&str], json: bool) -> Result<u8, Stop> {
    let (asked, path) = report::asked(rest).map_err(Stop::Usage)?;
    let ctx = Ctx::for_root(dir_or_here(path)?, Baseline::empty(VERSION));
    let found = report::survey(&ctx, asked, VERSION)?;
    shown(json, &found.render_json(), &found.render());
    found.whole().map_err(Stop::CannotRun)
}

fn compare(rest: &[&str], json: bool) -> Result<u8, Stop> {
    let asked = oracle::asked(rest).map_err(Stop::Usage)?;
    let found = oracle::run(&asked)?;
    shown(json, &found.render_json(VERSION), &found.render());
    Ok(found.code())
}

/// `chock sweep` and `chock moved`: a report for a person, tripped when code changed or was lost.
fn proof(name: &str, rest: &[&str], json: bool) -> Result<u8, Stop> {
    if json {
        return Err(Stop::Usage(format!("{name} has no `--json` report")));
    }
    let (rev, paths) = sweep::asked(name, rest).map_err(Stop::Usage)?;
    let here = dir_or_here(None)?;
    let (report, tripped) = match name {
        "moved" => sweep::moved(&here, rev, paths)?,
        _ => sweep::sweep(&here, rev, paths)?,
    };
    emit(&report);
    Ok(match tripped {
        true => Verdict::Tripped.code(),
        false => 0,
    })
}
