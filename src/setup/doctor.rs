//! Installed tools and hook declarations must answer the project's pins without executing hooks.
//! An unreadable prerequisite remains distinct from one inspected and found missing.

use std::fmt::Write as _;

use crate::run::report::{Finding, GateReport, Run, Verdict, plural};
use crate::setup::pins::Pin;
use crate::setup::version;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Matches(String),
    Drifted {
        have: String,
        want: String,
    },
    Missing {
        want: String,
    },
    /// Present, and built from a checkout rather than fetched, so there is no version to compare.
    Unpinned(String),
    /// Absent, and `cargo install` cannot fetch it. Every gate that needs one of these is opt-in.
    Unbuilt,
    /// Wired into an editor and seen to answer, however long ago.
    Firing(String),
    /// Wired into an editor and never seen to answer, so a tree it never read looks clean.
    Silent,
    /// A git hook from an older chock. It runs until `chock init --local` replaces or retires it.
    Stale(String),
    /// Declared to outpost, which runs a hook only while its hash is trusted; an edit untrusts it.
    Untrusted(String),
    /// Holding a record in a unit this chock does not produce, so the gate refuses until re-recorded.
    Recounted {
        have: String,
        want: String,
    },
    Inspected {
        verdict: Verdict,
        message: String,
    },
}

impl Status {
    fn broken(message: String) -> Self {
        Self::Inspected {
            verdict: Verdict::Tripped,
            message,
        }
    }

    fn unreadable(message: String) -> Self {
        Self::Inspected {
            verdict: Verdict::CannotRun,
            message,
        }
    }

    fn available(message: String) -> Self {
        Self::Inspected {
            verdict: Verdict::Pass,
            message,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub command: String,
    pub status: Status,
}

impl Row {
    #[must_use]
    pub fn is_failure(&self) -> bool {
        self.verdict() != Verdict::Pass
    }

    fn verdict(&self) -> Verdict {
        match self.status {
            Status::Inspected { verdict, .. } => verdict,
            Status::Matches(_) | Status::Unpinned(_) | Status::Unbuilt | Status::Firing(_) => {
                Verdict::Pass
            }
            _ => Verdict::Tripped,
        }
    }
}

/// The row for the hook chock declared to outpost, from the standing outpost reports.
#[must_use]
pub fn outpost_hooks(standing: Option<&str>, file: &str) -> Vec<Row> {
    // A trusted hook gets a row too, so it reads differently from a hook nobody looked at.
    let status = match standing {
        Some("untrusted" | "changed") => Status::Untrusted(file.to_string()),
        Some("trusted") => Status::Matches("trusted".to_string()),
        _ => return Vec::new(),
    };
    vec![Row {
        command: "outpost hook".to_string(),
        status,
    }]
}

/// The row for the hook chock wired into an editor, or `None` where it wired none: a project
/// that never asked for a hook does not have a broken one.
#[must_use]
pub fn editor_hook(configured: bool, since: Option<std::time::Duration>) -> Option<Row> {
    if !configured {
        return None;
    }
    let status = match since {
        Some(ago) => Status::Firing(format!("last answered {}", how_long(ago))),
        None => Status::Silent,
    };
    Some(Row {
        command: EDITOR_HOOK.to_string(),
        status,
    })
}

/// Named like a tool, because doctor reports it in a row like one.
pub const EDITOR_HOOK: &str = "editor hook";

/// Coarse on purpose: the question is whether it ever fires, not exactly when.
pub fn how_long(ago: std::time::Duration) -> String {
    const MINUTE: usize = 60;
    const HOUR: usize = 60 * MINUTE;
    const DAY: usize = 24 * HOUR;
    let seconds = usize::try_from(ago.as_secs()).unwrap_or(usize::MAX);
    match seconds {
        0..MINUTE => "just now".to_string(),
        MINUTE..HOUR => format!("{} ago", plural(seconds / MINUTE, "minute")),
        HOUR..DAY => format!("{} ago", plural(seconds / HOUR, "hour")),
        _ => format!("{} ago", plural(seconds / DAY, "day")),
    }
}

/// A row per git hook in the working tree from an older chock. It uses `init`'s test, so doctor
/// calls stale exactly what `init` replaces.
#[must_use]
pub fn git_hooks(installed: &[(String, Option<String>)]) -> Vec<Row> {
    installed
        .iter()
        .filter_map(|(name, held)| Some((name, held.as_ref()?)))
        .filter(|(name, held)| crate::setup::hooks::outgrown(name, held))
        .map(|(name, _)| Row {
            command: "git hook".to_string(),
            status: Status::Stale(format!(".chock/hooks/{name}")),
        })
        .collect()
}

/// Commands a toolchain itself ships, the nightly's miri among them. Nobody pins these; `cargo`
/// stands for the toolchain.
const TOOLCHAIN: [&str; 5] = [
    "cargo",
    "rustfmt",
    "clippy-driver",
    "rustdoc",
    "cargo +nightly miri",
];

/// Tools a gate runs without declaring them for its verdict cache. `test` and `coverage` are absent
/// on purpose: a project may replace either command, and then the default tool is not needed.
const UNDECLARED: [(&str, &str); 4] = [
    ("sort", "cargo-sort"),
    ("typos", "typos"),
    ("unused", "cargo-machete"),
    ("deps", "cargo-deny"),
];

/// Every tool a gate needs installed, from what it declares plus the list above.
fn tools_of(gate: &crate::run::Gate) -> Vec<&'static str> {
    let declared = gate.reads.map(|reads| reads.tools).unwrap_or_default();
    let undeclared = UNDECLARED
        .iter()
        .filter(|(name, _)| *name == gate.name)
        .map(|(_, tool)| *tool);
    declared
        .iter()
        .copied()
        .chain(undeclared)
        .filter(|tool| !TOOLCHAIN.contains(tool))
        .collect()
}

/// A switched-on gate whose tool no pin names, so doctor would check a tool nothing runs.
#[must_use]
pub fn unpinned(enabled: &dyn Fn(&str) -> bool, pins: &[Pin]) -> Vec<Row> {
    let pinned = |tool: &str| {
        pins.iter()
            .any(|pin| pin.command == tool || pin.crate_name == tool)
    };
    let mut needed = std::collections::BTreeMap::<&str, Vec<&str>>::new();
    for gate in crate::gates::registry()
        .iter()
        .filter(|gate| enabled(gate.name))
    {
        for tool in tools_of(gate).into_iter().filter(|tool| !pinned(tool)) {
            needed.entry(tool).or_default().push(gate.name);
        }
    }
    needed
        .into_iter()
        .map(|(tool, gates)| Row {
            command: tool.to_string(),
            status: Status::broken(format!(
                "switched on and run by {}, but {} does not pin it",
                gates.join(", "),
                crate::project::PIN_FILE
            )),
        })
        .collect()
}

/// `unpinned` for the project's config. No config switches nothing on; an unreadable one gets a
/// row, or the check would say every pin was fine.
#[must_use]
pub fn unpinned_in(
    config: &Result<Option<crate::project::config::Config>, String>,
    pins: &[Pin],
) -> Vec<Row> {
    match config {
        Ok(Some(config)) => unpinned(&|gate| config.is_on(gate), pins),
        Ok(None) => Vec::new(),
        Err(why) => vec![Row {
            command: crate::project::config::FILE.to_string(),
            status: Status::unreadable(format!("cannot tell which gates need a pin: {why}")),
        }],
    }
}

/// The fork's row says how its build stands against `main`: it has no version to compare.
fn judged(rows: Vec<Row>, standing: &dyn Fn() -> (Verdict, String)) -> Vec<Row> {
    let fork = |row: &Row| {
        row.command == crate::setup::mutest::CRATE
            && matches!(row.status, Status::Unbuilt | Status::Unpinned(_))
    };
    rows.into_iter()
        .map(|row| match fork(&row) {
            true => inspected(&row.command, standing()),
            false => row,
        })
        .collect()
}

/// Miri's row, for a project that switched the `miri` check on: no pin names Miri.
fn miri_in(
    config: &Result<Option<crate::project::config::Config>, String>,
    standing: &dyn Fn() -> (Verdict, String),
) -> Option<Row> {
    let name = crate::setup::miri::NAME;
    let on = matches!(config, Ok(Some(config)) if config.is_on(name));
    on.then(|| inspected(name, standing()))
}

fn inspected(command: &str, (verdict, message): (Verdict, String)) -> Row {
    Row {
        command: command.to_string(),
        status: Status::Inspected { verdict, message },
    }
}

/// Every row `doctor` reports for this tree: the pins, the config, the machine, and the hooks.
pub fn gathered(
    root: &std::path::Path,
    pins: &[Pin],
    extra: &crate::setup::pins::AdditionalPins,
) -> Vec<Row> {
    use crate::project::{document, vcs};
    let start: crate::exec::Start = &crate::exec::run_env;
    let rows = check(pins, &installed_on_this_machine());
    let mut rows = judged(rows, &|| crate::setup::mutest::standing(start, root).row());
    let config = document::read::<crate::project::config::Config>(root).map_err(|e| e.to_string());
    rows.extend(unpinned_in(&config, pins));
    rows.extend(undecided_in(&config));
    rows.extend(manual_in(&config));
    rows.extend(miri_in(&config, &|| {
        crate::setup::miri::standing(start, root)
    }));
    rows.extend(machine_health(root, extra));
    rows.extend(editor_hook(
        wired_into_an_editor(root),
        crate::edited::since_firing(root),
    ));
    rows.extend(git_hooks(&crate::setup::hooks::installed_hooks(root)));
    // Only where Outpost holds this tree: elsewhere Outpost answers for an ancestor repository.
    let outpost_holds = vcs::holders(root).contains(&vcs::Kind::Outpost);
    rows.extend(outpost_hooks(
        outpost_holds
            .then(|| vcs::declared_hook_standing(root))
            .flatten()
            .as_deref(),
        crate::setup::hooks::DECLARED,
    ));
    // Asked here so an upgrade names every gate to re-record at once, before a commit refuses.
    if let Ok(Some(held)) = document::read::<crate::run::baseline::Baseline>(root) {
        rows.extend(recounted(&held, &ratchet_units()));
    }
    rows
}

/// Whether chock's own editor hook is in the settings it writes. Doctor does not report on a hook
/// that chock did not wire.
fn wired_into_an_editor(root: &std::path::Path) -> bool {
    std::fs::read_to_string(root.join(".claude/settings.json"))
        .is_ok_and(|held| held.contains(crate::setup::agents::ON_EDIT))
}

/// Default gates neither on nor recorded as off, which an older `init` dropped. `enable` or
/// `disable` records the decision.
#[must_use]
pub fn undecided_in(config: &Result<Option<crate::project::config::Config>, String>) -> Vec<Row> {
    let Ok(Some(config)) = config else {
        return Vec::new();
    };
    let defaults: Vec<&str> = crate::gates::registry()
        .iter()
        .filter(|gate| {
            !matches!(
                gate.group,
                crate::run::Group::OptIn | crate::run::Group::Instrument
            )
        })
        .map(|gate| gate.name)
        .collect();
    config
        .undecided(&defaults)
        .into_iter()
        .map(|gate| Row {
            command: gate.to_string(),
            status: Status::broken(format!(
                "a default gate that is off with no recorded reason: `chock enable {gate}` \
                 or `chock disable {gate}` records the decision"
            )),
        })
        .collect()
}

/// Gates that are on and staged `manual`. No hook and no CI job runs them, so each is named.
#[must_use]
pub fn manual_in(config: &Result<Option<crate::project::config::Config>, String>) -> Vec<Row> {
    let Ok(Some(config)) = config else {
        return Vec::new();
    };
    config
        .stage
        .iter()
        .filter(|(gate, stage)| {
            **stage == crate::project::config::Stage::Manual && config.is_on(gate)
        })
        .map(|(gate, _)| Row {
            command: gate.clone(),
            status: Status::available(
                "staged `manual`: no hook and no CI job runs it, only `chock run`".to_string(),
            ),
        })
        .collect()
}

/// Machine queries stay at the edge; the checks below consume only the captured answers.
pub fn machine_health(
    root: &std::path::Path,
    extra: &crate::setup::pins::AdditionalPins,
) -> Vec<Row> {
    let listing = if extra.toolchains.is_empty() {
        Err(String::new())
    } else {
        crate::exec::run("rustup", &["toolchain", "list"], root).map_err(|e| e.to_string())
    };
    let mut rows = additional(extra, &listing);
    if crate::project::vcs::holders(root).contains(&crate::project::vcs::Kind::Git) {
        let declarations = crate::project::vcs::git_hook_declarations(root);
        let path = std::env::var_os("PATH");
        rows.extend(declared_git_hooks(&declarations, &|program| {
            let path = path.as_ref().ok_or("PATH is not available")?;
            crate::setup::hooks::executable(root, program, path)
        }));
    }
    rows
}

/// A failed query is not an empty installation; neither may pass as a missing comparison.
pub fn additional(
    pins: &crate::setup::pins::AdditionalPins,
    listing: &Result<crate::exec::Output, String>,
) -> Vec<Row> {
    let mut rows: Vec<Row> = pins
        .toolchains
        .iter()
        .map(|pin| {
            let status = match listing {
                Ok(out) if out.success() && !out.truncated => {
                    toolchain_status(&pin.want, &out.stdout)
                }
                Ok(out) => Status::unreadable(format!(
                    "rustup toolchain list failed: {}",
                    out.why_it_failed()
                )),
                Err(why) => Status::unreadable(why.clone()),
            };
            Row {
                command: pin.key.clone(),
                status,
            }
        })
        .collect();
    rows.extend(pins.unchecked.iter().map(|key| Row {
        command: key.clone(),
        status: Status::broken("unrecognized pin key; supported suffixes are _VERSION, _BIN, _SETUP and _NIGHTLY; this value was not executed or checked".to_string()),
    }));
    rows
}

fn toolchain_status(want: &str, stdout: &str) -> Status {
    let unreadable =
        || Status::unreadable("rustup toolchain list returned an unrecognized listing".to_string());
    if stdout.trim() == "no installed toolchains" {
        return missing_toolchain(want);
    }
    let Some((wanted, host)) = toolchain_name(want) else {
        return unreadable();
    };
    if stdout.trim().is_empty() {
        return unreadable();
    }
    let mut found = false;
    // Linked custom toolchains are opaque names; only official names are normalized for matching.
    for line in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let mut words = line.splitn(2, char::is_whitespace);
        let name = words.next().unwrap_or_default();
        let installed = toolchain_name(name);
        let annotation = words.next().unwrap_or_default().trim();
        if !matches!(
            annotation,
            "" | "(active)" | "(default)" | "(active, default)" | "(default, active)"
        ) {
            return unreadable();
        }
        found |= installed.is_some_and(|(name, installed_host)| {
            name == wanted && (host.is_none() || host == installed_host)
        });
    }
    if found {
        Status::available(format!("pinned toolchain {want} is installed"))
    } else {
        missing_toolchain(want)
    }
}

fn missing_toolchain(want: &str) -> Status {
    Status::broken(format!(
        "pinned toolchain {want} is not installed; run `rustup toolchain install {want}`"
    ))
}

fn toolchain_name(name: &str) -> Option<(&str, Option<&str>)> {
    let (channel, rest) = name.split_once('-').unwrap_or((name, ""));
    if !toolchain_channel(channel) {
        return None;
    }
    let (base, host) = dated_toolchain(name, channel, rest)?;
    if host.is_empty() {
        return (!name.ends_with('-')).then_some((base, None));
    }
    toolchain_host(host).then_some((base, Some(host)))
}

fn toolchain_channel(channel: &str) -> bool {
    matches!(channel, "stable" | "beta" | "nightly")
        || version::from_version_output(channel).as_deref() == Some(channel)
}

fn dated_toolchain<'a>(
    name: &'a str,
    channel: &'a str,
    rest: &'a str,
) -> Option<(&'a str, &'a str)> {
    if !rest.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return Some((channel, rest));
    }
    let date = rest.get(..10)?;
    if !date.bytes().enumerate().all(|(i, b)| match i {
        4 | 7 => b == b'-',
        _ => b.is_ascii_digit(),
    }) {
        return None;
    }
    let tail = rest.get(10..)?;
    let host = match tail {
        "" => "",
        _ => tail.strip_prefix('-')?,
    };
    Some((name.get(..channel.len() + 11)?, host))
}

fn toolchain_host(host: &str) -> bool {
    let parts: Vec<&str> = host.split('-').collect();
    (3..=4).contains(&parts.len())
        && host.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && parts.iter().all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
}

/// A declaration may exist while its command cannot be started. Checking never executes the hook.
pub fn declared_git_hooks(
    declarations: &Result<Vec<crate::project::vcs::GitHook>, String>,
    available: &dyn Fn(&str) -> Result<bool, String>,
) -> Vec<Row> {
    match declarations {
        Err(why) => vec![Row {
            command: "git hook declarations".to_string(),
            status: Status::unreadable(why.clone()),
        }],
        Ok(hooks) => hooks
            .iter()
            .filter_map(|hook| Some((hook, hook.name.strip_prefix("chock-")?)))
            .map(|(hook, name)| Row {
                command: format!("git hook {}", hook.name),
                status: declared_status(hook, name, available),
            })
            .collect(),
    }
}

fn declared_status(
    hook: &crate::project::vcs::GitHook,
    name: &str,
    available: &dyn Fn(&str) -> Result<bool, String>,
) -> Status {
    if !crate::setup::hooks::wired()
        .iter()
        .any(|(event, _)| *event == name)
        || !hook.events.iter().any(|event| event == name)
    {
        return Status::broken(format!(
            "declaration has no {name} event; run `chock init --local`"
        ));
    }
    let Some(command) = &hook.command else {
        return Status::broken(
            "declaration names no command; run `chock init --local`".to_string(),
        );
    };
    let program = if command == &crate::setup::hooks::command(name) {
        "chock"
    } else if command.trim_start_matches("./") == format!("{}/{name}", crate::setup::hooks::DIR) {
        command
    } else {
        return Status::broken(
            "declaration is not a verifiable chock command; run `chock init --local`".to_string(),
        );
    };
    match available(program) {
        Ok(true) => Status::available("declared command is available (not executed)".to_string()),
        Ok(false) => Status::broken(format!(
            "command {program} is missing or not executable; run `chock init --local`"
        )),
        Err(why) => Status::unreadable(why),
    }
}

/// Compare each pin against what `installed` reports, in the order the pin file wrote them.
pub fn check(pins: &[Pin], installed: &dyn Fn(&Pin) -> Option<String>) -> Vec<Row> {
    check_on(pins, installed, std::env::consts::OS)
}

/// A tool made for other systems passes with a note, because it cannot be installed here.
fn check_on(pins: &[Pin], installed: &dyn Fn(&Pin) -> Option<String>, os: &str) -> Vec<Row> {
    pins.iter()
        .map(|pin| match pin.elsewhere(os) {
            Some(message) => Row {
                command: pin.command.clone(),
                status: Status::available(message),
            },
            None => row(pin, installed),
        })
        .collect()
}

fn row(pin: &Pin, installed: &dyn Fn(&Pin) -> Option<String>) -> Row {
    let status = match installed(pin) {
        None if pin.unpublished() => Status::Unbuilt,
        None => Status::Missing {
            want: pin.want.clone(),
        },
        Some(have) if pin.unpublished() => Status::Unpinned(have),
        Some(have) if have == pin.want => Status::Matches(have),
        Some(have) => Status::Drifted {
            have,
            want: pin.want.clone(),
        },
    };
    Row {
        command: pin.command.clone(),
        status,
    }
}

/// Every gate whose record is in a unit this chock does not produce, named at once after an upgrade.
#[must_use]
pub fn recounted(held: &crate::run::baseline::Baseline, gates: &[(&str, &str)]) -> Vec<Row> {
    gates
        .iter()
        .filter_map(|(name, want)| {
            let have = held.unit(name)?;
            (have != *want).then(|| Row {
                command: (*name).to_string(),
                status: Status::Recounted {
                    have: have.to_string(),
                    want: (*want).to_string(),
                },
            })
        })
        .collect()
}

/// Each ratchet with the unit it counts in. It uses `gates::ratchets`, not `enforced`, so opt-in
/// gates such as `boundaries` are included.
#[must_use]
pub fn ratchet_units() -> Vec<(&'static str, &'static str)> {
    crate::gates::ratchets()
        .into_iter()
        .filter_map(|gate| Some((gate.name, gate.counts_in()?)))
        .collect()
}

/// No rows means nothing was checked, so it is a tool error, not a pass.
#[must_use]
pub fn verdict(rows: &[Row]) -> Verdict {
    if rows.is_empty() {
        return Verdict::CannotRun;
    }
    Verdict::worst(rows.iter().map(Row::verdict))
}

fn inspection_label(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Pass => "ok",
        Verdict::Tripped => "BROKEN",
        Verdict::CannotRun => "ERROR",
    }
}

#[must_use]
pub fn render(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "tool-versions.env pins no tools — nothing was checked.\n".to_string();
    }
    let mut out = String::new();
    for row in rows {
        let line = match &row.status {
            Status::Matches(have) => format!("  ok       {:<16} {have}", row.command),
            Status::Drifted { have, want } => {
                format!("  DRIFTED  {:<16} have {have:<10} want {want}", row.command)
            }
            Status::Missing { want } => {
                format!("  MISSING  {:<16} want {want}", row.command)
            }
            Status::Firing(when) => format!("  ok       {:<16} {when}", row.command),
            Status::Stale(which) => format!(
                "  stale    {:<16} {which} calls chock but is not what this chock writes — \
                 `chock init --local` replaces it",
                row.command
            ),
            Status::Untrusted(which) => format!(
                "  UNTRUSTED {:<15} {which} is declared but not trusted, so no commit runs it — \
                 run `outpost hooks trust`",
                row.command
            ),
            Status::Silent => format!(
                "  SILENT   {:<16} wired, and has never answered an edit",
                row.command
            ),
            Status::Unpinned(have) => {
                format!(
                    "  local    {:<16} {have:<10} built from a checkout",
                    row.command
                )
            }
            Status::Unbuilt => {
                format!("  absent   {:<16} build it from its checkout", row.command)
            }
            Status::Recounted { have, want } => format!(
                "  RECOUNTED {:<15} recorded in {have}, counts {want} — `chock baseline {}`",
                row.command, row.command
            ),
            Status::Inspected { verdict, message } => {
                format!(
                    "  {:<8} {:<16} {message}",
                    inspection_label(*verdict),
                    row.command
                )
            }
        };
        // Writing to a String cannot fail; the result is discarded rather than unwrapped.
        let _ = writeln!(out, "{line}");
    }
    out.push_str(&summary(rows));
    out
}

fn summary(rows: &[Row]) -> String {
    let mut out = String::new();
    let failures = rows.iter().filter(|r| r.is_failure()).count();
    let _ = if failures == 0 {
        // Not `{matched} of {total}`: rows that are not pins made that fraction read as failures.
        let matched = rows
            .iter()
            .filter(|r| matches!(r.status, Status::Matches(_)))
            .count();
        writeln!(
            out,
            "{} ok. Versions that match tool-versions.env: {matched}.",
            plural(rows.len(), "check")
        )
    } else {
        writeln!(
            out,
            "Needs attention: {failures} of {}. `chock init --global` installs the crates; \
             a tool in tool-versions.env that is not a crate is yours to install.",
            plural(rows.len(), "check")
        )
    };
    out
}

/// What is installed here: one `cargo install --list` for all pins, then `--version` for a pin
/// the listing lacks.
pub fn installed_on_this_machine() -> impl Fn(&Pin) -> Option<String> {
    let root = std::env::current_dir().unwrap_or_default();
    let listing = crate::exec::run("cargo", &["install", "--list"], &root).ok();
    move |pin| {
        read_installation(pin, listing.as_ref(), &|| {
            crate::exec::run(&pin.command, &["--version"], &root).ok()
        })
    }
}

fn read_installation(
    pin: &Pin,
    listing: Option<&crate::exec::Output>,
    version: &dyn Fn() -> Option<crate::exec::Output>,
) -> Option<String> {
    listing
        .filter(|out| out.success() && !out.truncated)
        .and_then(|out| version::from_cargo_listing(&out.stdout, &pin.crate_name))
        .or_else(|| {
            let out = version()?;
            if !out.success() || out.truncated {
                return None;
            }
            version::from_version_output(&out.stdout)
        })
}

/// A row in one clause, for a machine reader with no columns.
#[must_use]
fn describe(status: &Status) -> String {
    match status {
        Status::Matches(have) => format!("matches the pin at {have}"),
        Status::Drifted { have, want } => format!("installed {have}, pinned {want}"),
        Status::Missing { want } => {
            format!("not installed, pinned {want} — a crate is fetched by `chock init --global`")
        }
        Status::Unpinned(have) => {
            format!("{have}, built from a checkout rather than pinned to a version")
        }
        Status::Unbuilt => {
            "not installed, and not fetchable — build it from its checkout".to_string()
        }
        Status::Firing(when) => format!("wired into an editor, {when}"),
        Status::Stale(which) | Status::Untrusted(which) => hook_description(status, which),
        Status::Recounted { have, want } => format!(
            "recorded in {have} where this chock counts {want}, so the gate refuses until \
             `chock baseline` records it again"
        ),
        Status::Silent => {
            "wired into an editor and never seen to answer, so nothing is checked on an edit"
                .to_string()
        }
        Status::Inspected { message, .. } => message.clone(),
    }
}

fn hook_description(status: &Status, which: &str) -> String {
    if matches!(status, Status::Untrusted(_)) {
        return format!(
            "{which} is declared to outpost but not trusted, so no commit runs it; run `outpost hooks trust`"
        );
    }
    format!("{which} is a hook an older chock wrote; `chock init --local` replaces it")
}

fn unmeasured(rows: &[Row]) -> Option<String> {
    if rows.is_empty() {
        return Some("tool-versions.env pins no tools".to_string());
    }
    let reasons: Vec<&str> = rows
        .iter()
        .filter_map(|row| match &row.status {
            Status::Inspected {
                verdict: Verdict::CannotRun,
                message,
            } => Some(message.as_str()),
            _ => None,
        })
        .collect();
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

/// The same `Run` shape every gate reports in, so an agent parses one format and not two.
#[must_use]
pub fn render_json(rows: &[Row], chock_version: &str) -> String {
    let rerun = "chock doctor";
    let mut report = unmeasured(rows).map_or_else(
        || GateReport::new("doctor", verdict(rows), rerun),
        |why| GateReport::cannot_run("doctor", rerun, &why),
    );
    // Every row, not only the failures: what is installed is the answer `doctor` is asked for.
    report.findings = rows
        .iter()
        .map(|row| Finding::at(crate::project::PIN_FILE, &describe(&row.status)).item(&row.command))
        .collect();
    Run::new(chock_version, vec![report]).render_json()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;

    #[test]
    fn a_toolchain_host_requires_both_an_architecture_and_a_complete_triple() {
        for host in ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"] {
            assert!(toolchain_host(host), "{host}");
        }
        for host in [
            "",
            "x86_64",
            "a-b",
            "a-b-c-d-e",
            "1-unknown-linux",
            "-unknown-linux",
            "a--linux",
        ] {
            assert!(!toolchain_host(host), "{host}");
        }
    }

    fn listing(text: &str) -> Result<crate::exec::Output, String> {
        Ok(crate::exec::Output {
            code: Some(0),
            stdout: text.to_string(),
            stderr: String::new(),
            truncated: false,
        })
    }

    fn declared(command: Option<&str>) -> crate::project::vcs::GitHook {
        crate::project::vcs::GitHook {
            name: "chock-pre-commit".to_string(),
            events: vec!["pre-commit".to_string()],
            command: command.map(str::to_string),
        }
    }

    #[test]
    fn a_switched_on_gate_whose_tool_is_not_pinned_is_named_once_per_tool() {
        // Mutation testing on, and the other mutation engine pinned.
        let pins = crate::setup::pins::parse("CARGO_MUTANTS_VERSION=25.0.0\n").unwrap();
        let on = |gate: &str| matches!(gate, "mutest" | "scan" | "padding" | "lint");
        let rows = unpinned(&on, &pins);
        assert_eq!(
            rows,
            vec![
                Row {
                    command: "cargo-mutest".into(),
                    status: Status::broken(
                        "switched on and run by mutest, but tool-versions.env does not pin it".into()
                    ),
                },
                Row {
                    command: "outpost".into(),
                    status: Status::broken(
                        "switched on and run by padding, scan, but tool-versions.env does not pin it"
                            .into()
                    ),
                },
            ]
        );
    }

    #[test]
    fn a_pinned_tool_a_toolchain_command_and_a_switched_off_gate_need_nothing() {
        let pins = crate::setup::pins::parse(
            "CARGO_MUTEST_VERSION=0.0.0\nTYPOS_CLI_VERSION=1.0.0\nTYPOS_CLI_BIN=typos\n",
        )
        .unwrap();
        let on = |gate: &str| matches!(gate, "mutest" | "typos" | "lint" | "doc");
        assert_eq!(unpinned(&on, &pins), Vec::new());
        assert_eq!(unpinned(&|_| false, &[]), Vec::new());
    }

    /// The nightly ships miri, and a project may replace the coverage command, so neither is a
    /// pin. What `miri`, `crap` and `bsize` run beside them is.
    #[test]
    fn a_gate_that_keeps_a_verdict_needs_a_pin_for_each_tool_no_toolchain_ships() {
        let on = |gate: &str| matches!(gate, "miri" | "crap" | "bsize" | "coverage");
        let rows = unpinned(&on, &[]);
        let named: Vec<&str> = rows.iter().map(|row| row.command.as_str()).collect();
        assert_eq!(named, ["cargo-bsize", "cargo-crap", "cargo-nextest"]);
    }

    /// A manual gate is the project's choice, so its row passes; naming it keeps it in sight.
    #[test]
    fn a_manual_gate_that_is_on_is_named_and_one_that_is_off_is_not() {
        use crate::project::config::{Config, Stage};
        let mut config = Config::of(["binsize"]);
        config.place("binsize", true, Stage::Manual);
        config.place("bsize", true, Stage::Manual);
        let rows = manual_in(&Ok(Some(config)));
        let named: Vec<&str> = rows.iter().map(|row| row.command.as_str()).collect();
        assert_eq!(named, ["binsize"]);
        assert!(!rows[0].is_failure());
        assert_eq!(manual_in(&Ok(None)), Vec::new());
    }

    #[test]
    fn a_default_gate_off_with_no_recorded_reason_is_named_and_a_decided_one_is_not() {
        let defaults: Vec<&str> = crate::gates::registry()
            .iter()
            .filter(|gate| {
                !matches!(
                    gate.group,
                    crate::run::Group::OptIn | crate::run::Group::Instrument
                )
            })
            .map(|gate| gate.name)
            .filter(|name| !matches!(*name, "slop" | "typos"))
            .collect();
        let mut config = crate::project::config::Config::of(defaults);
        config.disable("typos", None);
        let named: Vec<String> = undecided_in(&Ok(Some(config)))
            .iter()
            .map(|row| format!("{} {}", row.command, describe(&row.status)))
            .collect();
        assert_eq!(
            named,
            [
                "slop a default gate that is off with no recorded reason: `chock enable slop` or \
              `chock disable slop` records the decision"
            ]
        );
        assert_eq!(undecided_in(&Ok(None)), Vec::new());
        assert_eq!(undecided_in(&Err("unreadable".into())), Vec::new());
    }

    #[test]
    fn an_unreadable_config_is_reported_and_an_absent_one_switches_nothing_on() {
        assert_eq!(unpinned_in(&Ok(None), &[]), Vec::new());
        assert_eq!(
            unpinned_in(&Err("line 3: expected `,`".into()), &[]),
            vec![Row {
                command: ".chock/config.json".into(),
                status: Status::unreadable(
                    "cannot tell which gates need a pin: line 3: expected `,`".into()
                ),
            }]
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn machine_health_without_toolchains_or_a_repository_needs_no_process() {
        let root = crate::testdir::make("doctor-no-machine-queries");
        assert_eq!(
            machine_health(&root, &crate::setup::pins::AdditionalPins::default()),
            Vec::new()
        );
        let extra = crate::setup::pins::additional("TOOL_UNKNOWN=value").unwrap();
        let rows = machine_health(&root, &extra);
        assert_eq!(rows[0].command, "line 1: TOOL_UNKNOWN");
        assert_eq!(verdict(&rows), Verdict::Tripped);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn machine_health_queries_a_named_toolchain_instead_of_silently_skipping_it() {
        let dir = crate::testdir::make("doctor-positive-query");
        let extra = crate::setup::pins::AdditionalPins {
            toolchains: vec![crate::setup::pins::ToolchainPin {
                key: "TEST_NIGHTLY".into(),
                want: "nightly-1900-01-01".into(),
            }],
            unchecked: Vec::new(),
        };
        let rows = machine_health(&dir, &extra);
        assert_eq!(rows, vec![Row { command: "TEST_NIGHTLY".into(), status: Status::broken("pinned toolchain nightly-1900-01-01 is not installed; run `rustup toolchain install nightly-1900-01-01`".into()) }]);
    }

    #[test]
    fn installation_readers_accept_only_successful_complete_version_answers() {
        let tool = pin("example", "1.2.3");
        let installed = listing("example v1.2.3:\n    example\n").unwrap();
        assert_eq!(
            read_installation(&tool, Some(&installed), &|| None),
            Some("1.2.3".to_string())
        );
        let fallback = listing("example 1.2.3\n").unwrap();
        assert_eq!(
            read_installation(&tool, None, &|| Some(fallback.clone())),
            Some("1.2.3".to_string())
        );
        let mut incomplete = fallback;
        incomplete.truncated = true;
        assert_eq!(
            read_installation(&tool, None, &|| Some(incomplete.clone())),
            None
        );
        incomplete.truncated = false;
        incomplete.code = Some(1);
        assert_eq!(
            read_installation(&tool, None, &|| Some(incomplete.clone())),
            None
        );
        assert_eq!(read_installation(&tool, Some(&incomplete), &|| None), None);
        assert_eq!(
            read_installation(&tool, None, &|| listing("no version").ok()),
            None
        );
    }

    /// A tool cargo did not install, such as `cargo` itself, answers through its own `--version`.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn one_reading_of_this_machine_answers_for_each_pin_it_is_asked_about() {
        let installed = installed_on_this_machine();
        let here = std::env::current_dir().unwrap();
        let said = crate::exec::run("cargo", &["--version"], &here).unwrap();
        let cargo = Pin {
            crate_name: "chock-no-such-crate".to_string(),
            ..pin("cargo", "0.0.1")
        };
        let answered = installed(&cargo);
        assert!(answered.is_some(), "cargo answered no version");
        assert_eq!(answered, version::from_version_output(&said.stdout));
        assert_eq!(installed(&pin("chock-no-such-tool", "1.0.0")), None);
    }

    #[test]
    fn every_health_status_reaches_both_output_formats() {
        let rows = vec![
            Row {
                command: "drift".into(),
                status: Status::Drifted {
                    have: "1".into(),
                    want: "2".into(),
                },
            },
            Row {
                command: "editor".into(),
                status: Status::Firing("recently".into()),
            },
            Row {
                command: "silent".into(),
                status: Status::Silent,
            },
            Row {
                command: "local".into(),
                status: Status::Unpinned("0.1.0".into()),
            },
            Row {
                command: "ready".into(),
                status: Status::available("available".into()),
            },
        ];
        let human = render(&rows);
        for expected in [
            "DRIFTED",
            "recently",
            "SILENT",
            "built from a checkout",
            "available",
        ] {
            assert!(human.contains(expected), "{human}");
        }
        let json = render_json(&rows, "0.1.0");
        for expected in [
            "installed 1, pinned 2",
            "wired into an editor, recently",
            "never seen to answer",
            "built from a checkout",
            "available",
        ] {
            assert!(json.contains(expected), "{json}");
        }
        let empty: serde_json::Value = serde_json::from_str(&render_json(&[], "0.1.0")).unwrap();
        assert_eq!(
            empty["gates"][0]["cannot_run_reason"],
            "tool-versions.env pins no tools"
        );
        assert_eq!(inspection_label(Verdict::Pass), "ok");
    }

    #[test]
    fn custom_toolchain_names_do_not_hide_an_installed_official_pin() {
        let status = toolchain_status(
            "nightly-2026-09-26",
            "custom-debug\nnightly-2026-09-26-x86_64-unknown-linux-gnu\n",
        );
        assert_eq!(
            status,
            Status::available("pinned toolchain nightly-2026-09-26 is installed".to_string())
        );
        assert_eq!(
            toolchain_status("nightly", "custom-debug\n"),
            missing_toolchain("nightly")
        );
        assert_eq!(
            toolchain_status("unrecognized", "nightly\n"),
            Status::unreadable(
                "rustup toolchain list returned an unrecognized listing".to_string()
            )
        );
        for name in [
            "nightly-2026-9-26",
            "nightly-",
            "nightly-x86_64--linux-gnu",
            "nightly-2026-09-26x86_64-unknown-linux-gnu",
            "nightly-2026-09-2x",
        ] {
            assert_eq!(toolchain_name(name), None, "{name}");
        }
    }

    #[test]
    fn a_toolchain_pin_matches_its_date_and_not_an_unrelated_installed_nightly() {
        let pins = crate::setup::pins::additional("MUTEST_NIGHTLY=nightly-2026-09-26").unwrap();
        let good = additional(
            &pins,
            &listing("nightly-2026-09-26-x86_64-unknown-linux-gnu (default)\n"),
        );
        assert_eq!(
            good[0].status,
            Status::available("pinned toolchain nightly-2026-09-26 is installed".to_string())
        );
        assert_eq!(verdict(&good), Verdict::Pass);
        for text in [
            "nightly-2026-09-25-x86_64-unknown-linux-gnu\n",
            "no installed toolchains\n",
        ] {
            let missing = additional(&pins, &listing(text));
            assert_eq!(verdict(&missing), Verdict::Tripped);
            assert!(
                describe(&missing[0].status)
                    .contains("rustup toolchain install nightly-2026-09-26")
            );
        }
        assert!(matches!(
            toolchain_status("nightly", "nightly-2026-09-26-x86_64-unknown-linux-gnu\n"),
            Status::Inspected {
                verdict: Verdict::Tripped,
                ..
            }
        ));
        assert!(matches!(
            toolchain_status(
                "nightly",
                "nightly-aarch64-apple-darwin (active, default)\n"
            ),
            Status::Inspected {
                verdict: Verdict::Pass,
                ..
            }
        ));
    }

    #[test]
    fn empty_or_malformed_toolchain_lists_never_mean_the_pin_is_missing() {
        for text in [
            "",
            "  \n",
            "unrecognized output",
            "nightly-x86_64-unknown-linux-gnu (unexpected)",
            "nightly-x86_64-unknown-linux-gnu\nunrecognized output",
        ] {
            assert!(
                matches!(
                    toolchain_status("nightly", text),
                    Status::Inspected {
                        verdict: Verdict::CannotRun,
                        ..
                    }
                ),
                "{text:?}"
            );
        }
        assert_eq!(
            toolchain_name("1.98.1-x86_64-unknown-linux-gnu"),
            Some(("1.98.1", Some("x86_64-unknown-linux-gnu")))
        );
        assert_eq!(
            toolchain_name("nightly-2026-09-26"),
            Some(("nightly-2026-09-26", None))
        );
        assert!(matches!(
            toolchain_status(
                "nightly-2026-09-26-x86_64-unknown-linux-gnu",
                "nightly-2026-09-26-aarch64-apple-darwin"
            ),
            Status::Inspected {
                verdict: Verdict::Tripped,
                ..
            }
        ));
        assert!(matches!(
            toolchain_status(
                "nightly-2026-09-26-x86_64-unknown-linux-gnu",
                "nightly-2026-09-26-x86_64-unknown-linux-gnu-extra"
            ),
            Status::Inspected {
                verdict: Verdict::Tripped,
                ..
            }
        ));
    }

    #[test]
    fn unreadable_toolchain_queries_and_unknown_keys_never_report_healthy() {
        let pins =
            crate::setup::pins::additional("MUTEST_NIGHTLY=nightly\nTOOL_VERSIONX=1").unwrap();
        let mut failed = listing("nightly-x86_64-unknown-linux-gnu\n").unwrap();
        failed.code = Some(1);
        failed.stderr = "rustup failed".to_string();
        let rows = additional(&pins, &Ok(failed));
        assert_eq!(verdict(&rows), Verdict::CannotRun);
        assert!(describe(&rows[0].status).contains("rustup failed"));
        assert_eq!(rows[1].command, "line 2: TOOL_VERSIONX");
        assert!(describe(&rows[1].status).contains("not executed or checked"));
        let doc: serde_json::Value = serde_json::from_str(&render_json(&rows, "0.1.0")).unwrap();
        assert_eq!(doc["gates"][0]["verdict"], "cannot_run");
        assert!(
            doc["gates"][0]["cannot_run_reason"]
                .as_str()
                .unwrap()
                .contains("rustup failed")
        );
        assert!(render(&rows).contains("ERROR"));
        assert_eq!(
            verdict(&additional(&pins, &Err("no rustup".into()))),
            Verdict::CannotRun
        );
        assert!(matches!(
            toolchain_status("nightly", "unexpected output format\n"),
            Status::Inspected {
                verdict: Verdict::CannotRun,
                ..
            }
        ));
        let mut cut = listing("nightly-x86_64-unknown-linux-gnu\n").unwrap();
        cut.truncated = true;
        assert_eq!(verdict(&additional(&pins, &Ok(cut))), Verdict::CannotRun);
    }

    #[test]
    fn a_declared_binary_is_available_but_a_missing_legacy_path_is_broken() {
        let current = declared(Some("chock hook pre-commit"));
        let rows = declared_git_hooks(&Ok(vec![current]), &|program| {
            assert_eq!(program, "chock");
            Ok(true)
        });
        assert_eq!(verdict(&rows), Verdict::Pass);
        assert!(describe(&rows[0].status).contains("not executed"));
        assert!(render(&rows).contains("declared command is available"));
        let legacy = declared(Some(".chock/hooks/pre-commit"));
        let rows = declared_git_hooks(&Ok(vec![legacy.clone()]), &|program| {
            assert_eq!(program, ".chock/hooks/pre-commit");
            Ok(false)
        });
        assert_eq!(verdict(&rows), Verdict::Tripped);
        assert!(describe(&rows[0].status).contains("missing or not executable"));
        assert!(render(&rows).contains("BROKEN"));
        assert_eq!(
            verdict(&declared_git_hooks(&Ok(vec![legacy]), &|_| Ok(true))),
            Verdict::Pass
        );
    }

    #[test]
    fn invalid_or_unreadable_declarations_cannot_hide_behind_existing_hook_files() {
        for mut hook in [
            declared(None),
            declared(Some("")),
            declared(Some("echo ignored; chock hook pre-commit")),
            declared(Some("unrelated")),
        ] {
            let row = declared_git_hooks(&Ok(vec![hook.clone()]), &|_| Ok(true));
            assert_eq!(verdict(&row), Verdict::Tripped);
            hook.events.clear();
            assert_eq!(
                verdict(&declared_git_hooks(&Ok(vec![hook]), &|_| Ok(true))),
                Verdict::Tripped
            );
        }
        let unread = declared_git_hooks(&Err("Git config unreadable".to_string()), &|_| Ok(true));
        assert_eq!(verdict(&unread), Verdict::CannotRun);
        let unavailable =
            declared_git_hooks(&Ok(vec![declared(Some("chock hook pre-commit"))]), &|_| {
                Err("cannot inspect binary".to_string())
            });
        assert_eq!(verdict(&unavailable), Verdict::CannotRun);
        let mut unrelated = declared(Some("anything"));
        unrelated.name = "other-pre-commit".to_string();
        let inspected = std::cell::Cell::new(false);
        let observe = |_: &str| {
            inspected.set(true);
            Ok(true)
        };
        assert_eq!(
            declared_git_hooks(&Ok(vec![unrelated]), &observe),
            Vec::new()
        );
        assert_eq!(declared_git_hooks(&Ok(Vec::new()), &observe), Vec::new());
        assert!(!inspected.get());
        assert_eq!(
            declared_git_hooks(&Ok(vec![declared(Some("chock hook pre-commit"))]), &observe)[0]
                .verdict(),
            Verdict::Pass
        );
        assert!(inspected.get());
    }

    #[test]
    fn a_record_in_a_unit_this_chock_does_not_produce_is_named_with_the_command_to_fix_it() {
        let mut held = crate::run::baseline::Baseline::empty("0.1.0");
        let empty = crate::run::baseline::Series::new;
        held.record("boundaries", "point(s) off one concern", empty());
        held.record("assertions", "length assertion(s)", empty());
        let gates = [
            ("boundaries", "file(s) off one concern, tree-wide"),
            ("assertions", "length assertion(s)"),
        ];
        let found = recounted(&held, &gates);
        assert_eq!(
            found,
            vec![Row {
                command: "boundaries".to_string(),
                status: Status::Recounted {
                    have: "point(s) off one concern".to_string(),
                    want: "file(s) off one concern, tree-wide".to_string(),
                },
            }]
        );
        assert!(
            found[0].is_failure(),
            "a refusal at commit time is a failure"
        );
        assert!(describe(&found[0].status).contains("chock baseline"));
        // The printed line names the gate twice: once as the row, once in the command that fixes it.
        let printed = render(&found);
        assert!(printed.contains("RECOUNTED"), "{printed}");
        assert!(printed.contains("chock baseline boundaries"), "{printed}");
    }

    /// Otherwise every upgrade would ask to re-record gates the project never ran.
    #[test]
    fn a_gate_holding_no_record_at_all_is_not_reported_as_recounted() {
        let held = crate::run::baseline::Baseline::empty("0.1.0");
        assert_eq!(
            recounted(&held, &[("sort", "unsorted manifest(s)")]),
            vec![]
        );
    }

    /// `boundaries` is opt-in, so the list must come from every ratchet, not the enforced set.
    #[test]
    fn every_ratchet_the_binary_has_reports_the_unit_it_counts_in() {
        let units = ratchet_units();
        assert!(units.iter().all(|(_, unit)| !unit.is_empty()));
        assert!(
            units.contains(&("assertions", "length assertion(s)")),
            "{units:?}"
        );
        assert!(
            units.contains(&("boundaries", "file(s) off one concern, tree-wide")),
            "{units:?}"
        );
        // A pass/fail check has no unit to compare, so it is not in the list at all.
        assert!(!units.iter().any(|(name, _)| *name == "test"));
        assert!(units.contains(&("modcheck", "orphan module file(s)")));
    }

    #[test]
    fn a_hook_an_older_chock_wrote_is_reported_and_somebody_elses_is_left_alone() {
        let installed = [
            (
                "pre-commit".to_string(),
                Some("chock run || exit 1".to_string()),
            ),
            (
                "pre-push".to_string(),
                Some(crate::setup::hooks::stub("pre-push")),
            ),
            (
                "commit-msg".to_string(),
                Some("exec ./bin/lint-message".to_string()),
            ),
            ("post-commit".to_string(), None),
        ];
        let rows = git_hooks(&installed);
        assert_eq!(
            rows.iter().map(|r| r.status.clone()).collect::<Vec<_>>(),
            vec![Status::Stale(".chock/hooks/pre-commit".to_string())]
        );
        // A stale hook is a failure: nothing else makes anybody delete it.
        assert!(rows[0].is_failure());
        assert_eq!(verdict(&rows), Verdict::Tripped);

        // Both surfaces name the file and the one command that fixes it.
        let text = render(&rows);
        assert!(text.contains(".chock/hooks/pre-commit"), "{text}");
        assert!(text.contains("chock init --local"), "{text}");
        let doc: serde_json::Value = serde_json::from_str(&render_json(&rows, "0.1.0")).unwrap();
        let said = doc["gates"][0]["findings"][0]["message"].as_str().unwrap();
        assert!(said.contains("an older chock wrote"), "{said}");
        assert!(said.contains("chock init --local"), "{said}");
    }

    #[test]
    fn a_report_with_nothing_wrong_says_so_rather_than_a_fraction_that_reads_as_a_failure() {
        // A published pin that matches, and an unpublished one built from a checkout. Neither is a
        // failure, and only the first is a version tool-versions.env pins.
        let mut rows = check(
            &[
                pin("just", "1.58.0"),
                pin("outpost", crate::setup::pins::UNPUBLISHED),
            ],
            &|p| (p.command == "just").then(|| "1.58.0".to_string()),
        );
        rows.extend(editor_hook(true, Some(std::time::Duration::from_secs(60))));
        assert_eq!(verdict(&rows), Verdict::Pass);
        let text = render(&rows);
        assert!(
            text.ends_with("3 checks ok. Versions that match tool-versions.env: 1.\n"),
            "{text}"
        );
    }

    #[test]
    fn the_json_carries_every_row_and_not_only_the_failures() {
        let rows = check(&[pin("just", "1.58.0")], &|_| Some("1.58.0".to_string()));
        let doc: serde_json::Value = serde_json::from_str(&render_json(&rows, "0.1.0")).unwrap();
        let gate = &doc["gates"][0];
        assert_eq!(gate["verdict"], serde_json::json!("passed"));
        assert_eq!(gate["findings"][0]["item"], serde_json::json!("just"));
        assert_eq!(
            gate["findings"][0]["message"],
            serde_json::json!("matches the pin at 1.58.0")
        );
    }

    #[test]
    fn a_hook_wired_and_never_seen_to_answer_is_a_failure() {
        let row = editor_hook(true, None).unwrap();
        assert_eq!(row.command, EDITOR_HOOK);
        assert_eq!(row.status, Status::Silent);
        assert!(row.is_failure());
    }

    #[test]
    fn a_hook_that_has_answered_is_not_a_failure_and_says_when() {
        let row = editor_hook(true, Some(std::time::Duration::from_secs(90))).unwrap();
        assert_eq!(
            row.status,
            Status::Firing("last answered 1 minute ago".to_string())
        );
        assert!(!row.is_failure());
    }

    #[test]
    fn a_project_with_no_editor_hook_is_not_asked_about_one() {
        assert_eq!(editor_hook(false, None), None);
        assert_eq!(
            editor_hook(false, Some(std::time::Duration::from_secs(1))),
            None
        );
    }

    #[test]
    fn how_long_ago_is_said_in_the_largest_unit_that_fits() {
        use std::time::Duration;
        assert_eq!(how_long(Duration::from_secs(5)), "just now");
        assert_eq!(how_long(Duration::from_secs(60)), "1 minute ago");
        assert_eq!(how_long(Duration::from_secs(3600)), "1 hour ago");
        assert_eq!(how_long(Duration::from_secs(86_400 * 3)), "3 days ago");
    }

    #[test]
    fn a_silent_hook_makes_the_whole_report_fail() {
        let rows = vec![editor_hook(true, None).unwrap()];
        assert_eq!(verdict(&rows), Verdict::Tripped);
    }

    #[test]
    fn the_row_of_the_fork_says_how_its_build_stands_and_every_other_row_stays() {
        let behind = || (Verdict::Tripped, "built from 0123456".to_string());
        let stands = inspected("cargo-mutest", behind());
        let pins = [
            pin("cargo-mutest", "0.0.0"),
            pin("outpost", "0.0.0"),
            pin("just", "1.58.0"),
        ];
        let here = |pin: &Pin| (pin.command != "outpost").then(|| "0.0.0".to_string());
        assert_eq!(
            judged(check(&pins, &here), &behind),
            vec![
                stands.clone(),
                Row {
                    command: "outpost".into(),
                    status: Status::Unbuilt,
                },
                Row {
                    command: "just".into(),
                    status: Status::Drifted {
                        have: "0.0.0".into(),
                        want: "1.58.0".into(),
                    },
                },
            ]
        );
        assert_eq!(judged(check(&pins[..1], &|_| None), &behind), [stands]);
        // A pin with a version is a release, and its row compares versions.
        let released = judged(check(&[pin("cargo-mutest", "1.2.3")], &|_| None), &behind);
        let want = "1.2.3".to_string();
        assert_eq!(released[0].status, Status::Missing { want });
    }

    #[test]
    fn miri_gets_a_row_only_where_the_project_switched_its_check_on() {
        use crate::project::config::Config;
        let asked = std::cell::Cell::new(0);
        let lacking = || {
            asked.set(asked.get() + 1);
            (Verdict::Tripped, "no `rust-src`".to_string())
        };
        assert_eq!(miri_in(&Ok(Some(Config::of(["lint"]))), &lacking), None);
        assert_eq!(miri_in(&Ok(None), &lacking), None);
        assert_eq!(miri_in(&Err("unreadable".into()), &lacking), None);
        assert_eq!(
            asked.get(),
            0,
            "a project without the check starts no rustup"
        );
        let row = miri_in(&Ok(Some(Config::of(["miri"]))), &lacking).unwrap();
        assert_eq!(asked.get(), 1);
        assert_eq!(
            (row.command.as_str(), row.verdict()),
            ("miri", Verdict::Tripped)
        );
        assert_eq!(describe(&row.status), "no `rust-src`");
    }

    fn pin(command: &str, want: &str) -> Pin {
        Pin {
            key: format!("{}_VERSION", command.to_uppercase()),
            crate_name: command.to_string(),
            command: command.to_string(),
            want: want.to_string(),
            setup: None,
            systems: None,
        }
    }

    #[test]
    fn a_matching_version_is_the_only_passing_outcome() {
        let pins = [pin("just", "1.58.0")];
        assert_eq!(
            check(&pins, &|_| Some("1.58.0".to_string())),
            vec![Row {
                command: "just".to_string(),
                status: Status::Matches("1.58.0".to_string()),
            }]
        );
    }

    #[test]
    fn a_tool_made_for_other_systems_passes_and_says_so_rather_than_reading_as_missing() {
        let acl = Pin {
            systems: Some(vec!["linux".to_string()]),
            ..pin("cargo-acl", "0.9.0")
        };
        let absent = |_: &Pin| None;
        assert_eq!(
            check_on(std::slice::from_ref(&acl), &absent, "macos"),
            vec![Row {
                command: "cargo-acl".to_string(),
                status: Status::Inspected {
                    verdict: Verdict::Pass,
                    message: "runs only on linux, so macos has no use for it".to_string(),
                },
            }]
        );
        assert_eq!(
            check_on(&[acl], &absent, "linux")[0].status,
            Status::Missing {
                want: "0.9.0".to_string()
            }
        );
    }

    #[test]
    fn a_different_version_reports_both_sides_rather_than_only_failing() {
        let pins = [pin("just", "1.58.0")];
        assert_eq!(
            check(&pins, &|_| Some("1.40.0".to_string())),
            vec![Row {
                command: "just".to_string(),
                status: Status::Drifted {
                    have: "1.40.0".to_string(),
                    want: "1.58.0".to_string(),
                },
            }]
        );
    }

    #[test]
    fn a_version_that_could_not_be_read_is_missing_and_never_a_pass() {
        let pins = [pin("just", "1.58.0")];
        assert_eq!(
            check(&pins, &|_| None),
            vec![Row {
                command: "just".to_string(),
                status: Status::Missing {
                    want: "1.58.0".to_string()
                },
            }]
        );
    }

    #[test]
    fn rows_follow_the_order_the_pin_file_wrote() {
        let pins = [pin("b", "1.0.0"), pin("a", "1.0.0"), pin("c", "1.0.0")];
        let rows = check(&pins, &|_| Some("1.0.0".to_string()));
        assert_eq!(
            rows.iter().map(|r| r.command.as_str()).collect::<Vec<_>>(),
            ["b", "a", "c"]
        );
    }

    #[test]
    fn every_status_is_a_failure_except_a_match() {
        let pins = [pin("a", "1.0.0"), pin("b", "1.0.0"), pin("c", "1.0.0")];
        let rows = check(&pins, &|p| match p.command.as_str() {
            "a" => Some("1.0.0".to_string()),
            "b" => Some("2.0.0".to_string()),
            _ => None,
        });
        assert_eq!(
            rows.iter().map(Row::is_failure).collect::<Vec<_>>(),
            [false, true, true]
        );
    }

    #[test]
    fn a_green_report_names_the_file_that_was_matched_against() {
        let rows = check(&[pin("just", "1.58.0")], &|_| Some("1.58.0".to_string()));
        assert_eq!(
            render(&rows),
            "  ok       just             1.58.0\n\
             1 check ok. Versions that match tool-versions.env: 1.\n"
        );
    }

    #[test]
    fn a_red_report_names_both_versions_and_the_command_that_fixes_it() {
        let rows = check(&[pin("just", "1.58.0")], &|_| Some("1.40.0".to_string()));
        assert_eq!(
            render(&rows),
            "  DRIFTED  just             have 1.40.0     want 1.58.0\n\
             Needs attention: 1 of 1 check. `chock init --global` installs the crates; a tool in tool-versions.env that is not a crate is yours to install.\n"
        );
    }

    #[test]
    fn a_missing_tool_reports_what_was_wanted_rather_than_only_that_it_is_absent() {
        let rows = check(&[pin("kani", "0.67.0")], &|_| None);
        assert_eq!(
            render(&rows),
            "  MISSING  kani             want 0.67.0\n\
             Needs attention: 1 of 1 check. `chock init --global` installs the crates; a tool in tool-versions.env that is not a crate is yours to install.\n"
        );
    }

    #[test]
    fn a_pin_file_that_pins_nothing_says_so_rather_than_reporting_green() {
        assert_eq!(
            render(&[]),
            "tool-versions.env pins no tools — nothing was checked.\n"
        );
    }

    #[test]
    fn the_three_verdicts_map_to_the_three_exit_codes() {
        let pass = check(&[pin("a", "1.0.0")], &|_| Some("1.0.0".to_string()));
        let tripped = check(&[pin("a", "1.0.0")], &|_| Some("2.0.0".to_string()));
        assert_eq!(verdict(&pass), Verdict::Pass);
        assert_eq!(verdict(&tripped), Verdict::Tripped);
        assert_eq!(verdict(&[]), Verdict::CannotRun);
        assert_eq!(
            [
                Verdict::Pass.code(),
                Verdict::Tripped.code(),
                Verdict::CannotRun.code()
            ],
            [0, 1, 2]
        );
    }

    #[test]
    fn a_missing_tool_trips_the_gate_just_as_a_drifted_one_does() {
        let missing = check(&[pin("a", "1.0.0")], &|_| None);
        assert_eq!(verdict(&missing), Verdict::Tripped);
    }

    #[test]
    fn a_tool_nobody_can_fetch_is_absent_rather_than_a_failure() {
        let rows = check(&[pin("outpost", "0.0.0")], &|_| None);
        assert_eq!(rows[0].status, Status::Unbuilt);
        assert!(!rows[0].is_failure());
        assert_eq!(verdict(&rows), Verdict::Pass);
        assert_eq!(
            render(&rows),
            "  absent   outpost          build it from its checkout\n\
             1 check ok. Versions that match tool-versions.env: 0.\n"
        );
    }

    #[test]
    fn an_unfetchable_tool_says_in_words_why_it_is_not_a_failure() {
        assert_eq!(
            describe(&Status::Unbuilt),
            "not installed, and not fetchable — build it from its checkout"
        );
        assert_eq!(
            describe(&Status::Unpinned("0.1.0".to_string())),
            "0.1.0, built from a checkout rather than pinned to a version"
        );
    }

    #[test]
    fn a_tool_that_is_on_crates_io_is_still_a_failure_when_it_is_missing() {
        let rows = check(&[pin("typos", "1.50.2")], &|_| None);
        assert!(rows[0].is_failure());
        assert_eq!(verdict(&rows), Verdict::Tripped);
    }

    #[test]
    fn a_declared_outpost_hook_that_no_commit_runs_is_reported() {
        let rows = |standing| outpost_hooks(standing, ".outposthooks.toml");
        assert_eq!(
            rows(Some("changed")),
            vec![Row {
                command: "outpost hook".to_string(),
                status: Status::Untrusted(".outposthooks.toml".to_string())
            }]
        );
        assert_eq!(rows(Some("untrusted")), rows(Some("changed")));
        // A trusted hook says so: silence would read the same as a hook nobody looked at.
        assert_eq!(
            rows(Some("trusted")),
            vec![Row {
                command: "outpost hook".to_string(),
                status: Status::Matches("trusted".to_string())
            }]
        );
        // Absent is a tree chock never wired, which is not a hook in a bad state.
        assert_eq!(rows(Some("absent")), Vec::new());
        assert_eq!(rows(None), Vec::new());
    }

    /// A row nothing runs is a failure, or `doctor` exits zero over a hook that checks nothing.
    #[test]
    fn an_untrusted_hook_is_a_failure_and_says_what_to_run() {
        let row = Row {
            command: "outpost hook".to_string(),
            status: Status::Untrusted(".outposthooks.toml".to_string()),
        };
        assert!(row.is_failure());
        let shown = render(std::slice::from_ref(&row));
        assert!(shown.contains("outpost hooks trust"), "{shown}");
        let json = render_json(std::slice::from_ref(&row), "0.1.0");
        assert!(json.contains("not trusted"), "{json}");
    }
}
