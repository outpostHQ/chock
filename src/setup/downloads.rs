//! Download pins: a tool that is no crate, fetched from its URL and held to its SHA-256. Its
//! directory's name holds the hash, so a new pin never finds the file of an old one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::gates::proofs::Tools;
use crate::setup::pins::{self, ParseError};

const URL: &str = "_URL";
const SHA256: &str = "_SHA256";

/// The file of a fetched tool that says which hash it was checked against; written last.
pub(crate) const STAMP: &str = ".chock-sha256";

/// The download pins a gate needs. `init --global` fetches one only where its gate is on, so a
/// project without that gate never downloads hundreds of MB for nothing.
pub const CLAIMED: [(&str, &str); 4] = [
    ("TLA_TOOLS", "tla"),
    ("APALACHE", "tla"),
    ("TLAPM", "tla"),
    ("VERUS", "verus"),
];

/// The end of the reason a gate gives when the project pins no download it needs.
pub const NOT_PINNED: &str = " is not pinned in tool-versions.env, so this gate has nothing to run";

/// The end of the reason a gate gives when the project's pin has no file for this machine.
pub const NO_FILE: &str = ", so it cannot run on this machine";

/// One file of a download pin: for every system when `platform` is empty, else for those it names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct File {
    /// The system suffix of its keys, such as `x86_linux`, or empty for every system.
    pub platform: String,
    /// The `_URL` value, where the pin sets one.
    pub url: Option<String>,
    /// The `_SHA256` value, where the pin sets one.
    pub sha256: Option<String>,
}

/// A tool fetched as a file: `X_VERSION` with `X_URL` and `X_SHA256`, or the same two keys
/// with a platform suffix, such as `X_URL_x86_linux`, once for each system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    /// The key before `_VERSION`, such as `TLA_TOOLS`.
    pub stem: String,
    /// The stem as a command is named, `tla-tools` for `TLA_TOOLS`.
    pub name: String,
    /// The `_VERSION` value.
    pub want: String,
    /// The files of the pin, one for each system it names.
    pub files: Vec<File>,
    /// The `_SETUP` command that runs after the download, where the pin sets one.
    pub setup: Option<String>,
}

/// The file this machine takes: its URL and the hash it must have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    /// Where the file is fetched from, always `https://`.
    pub url: String,
    /// The hash the file must have, in lowercase hex.
    pub sha256: String,
}

/// Whether a key belongs to a download pin rather than a crate pin.
#[must_use]
pub fn is_download_key(key: &str) -> bool {
    split(key).is_some()
}

/// A `_URL` or `_SHA256` key as its stem, its suffix, and the platform after the suffix.
fn split(key: &str) -> Option<(&str, &'static str, &str)> {
    [URL, SHA256].into_iter().find_map(|suffix| {
        if let Some(stem) = key.strip_suffix(suffix) {
            return Some((stem, suffix, ""));
        }
        let at = key.find(&format!("{suffix}_"))?;
        Some((&key[..at], suffix, &key[at + suffix.len() + 1..]))
    })
}

/// `value` with each `${NAME}` replaced by what an earlier line set NAME to, as bash sourcing the
/// file does, so a version bump edits one line. `Err` names the first NAME no line set.
fn expanded(value: &str, set: &BTreeMap<&str, String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = value;
    while let Some(at) = rest.find("${") {
        let Some(end) = rest[at..].find('}') else {
            break;
        };
        let name = &rest[at + 2..at + end];
        let Some(said) = set.get(name) else {
            return Err(name.to_string());
        };
        out.push_str(&rest[..at]);
        out.push_str(said);
        rest = &rest[at + end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Every download pin in a pin file, in the order of their `_VERSION` lines. A hash with no
/// `_URL`, or a stem with no `_VERSION`, is for the project's own scripts unless a gate claims it.
/// # Errors
/// When a line of the pin file is wrong.
pub fn parse(text: &str) -> Result<Vec<Download>, ParseError> {
    let mut versions: Vec<(String, String)> = Vec::new();
    let mut setups: BTreeMap<String, String> = BTreeMap::new();
    let mut files: BTreeMap<String, BTreeMap<String, File>> = BTreeMap::new();
    let mut set: BTreeMap<&str, String> = BTreeMap::new();
    for (i, raw) in text.lines().enumerate() {
        let Some((key, value)) = pins::assignment(raw, i + 1)? else {
            continue;
        };
        let value = expanded(value, &set).map_err(|name| ParseError::Refused {
            line: i + 1,
            key: key.to_string(),
            why: format!("uses ${{{name}}}, which no earlier line of the file sets"),
        })?;
        set.insert(key, value.clone());
        let value = value.as_str();
        if let Some(stem) = key.strip_suffix("_VERSION") {
            versions.push((stem.to_string(), value.to_string()));
        } else if let Some(stem) = key.strip_suffix("_SETUP") {
            setups.insert(stem.to_string(), value.to_string());
        } else if let Some((stem, suffix, platform)) = split(key) {
            let file = files
                .entry(stem.to_string())
                .or_default()
                .entry(platform.to_string())
                .or_insert_with(|| File {
                    platform: platform.to_string(),
                    ..File::default()
                });
            match suffix {
                URL => file.url = Some(value.to_string()),
                _ => file.sha256 = Some(value.to_string()),
            }
        }
    }
    Ok(versions
        .into_iter()
        .filter_map(|(stem, want)| {
            let files: Vec<File> = files.remove(&stem)?.into_values().collect();
            let claimed = CLAIMED.iter().any(|(owned, _)| *owned == stem);
            if !claimed && files.iter().all(|file| file.url.is_none()) {
                return None;
            }
            Some(Download {
                name: stem.to_lowercase().replace('_', "-"),
                setup: setups.get(&stem).cloned(),
                stem,
                want,
                files,
            })
        })
        .collect())
}

/// The stems of a pin file's download pins, which are no crate for `cargo install` to fetch.
#[must_use]
pub fn stems(text: &str) -> Vec<String> {
    let keys = text
        .lines()
        .filter_map(|raw| raw.split_once('='))
        .map(|(key, _)| key.trim());
    let mut stems: Vec<String> = keys
        .filter_map(|key| split(key).map(|(stem, _, _)| stem.to_string()))
        .collect();
    stems.dedup();
    stems
}

const ARCHES: [(&str, &[&str]); 2] = [
    ("x86_64", &["x86", "x64", "amd64"]),
    ("aarch64", &["arm64", "aarch64"]),
];

const SYSTEMS: [(&str, &[&str]); 3] = [
    ("linux", &["linux"]),
    ("macos", &["macos", "darwin", "apple", "osx", "mac"]),
    ("windows", &["windows", "win", "win64", "msvc"]),
];

/// Whether a platform suffix names this machine: where it names a system or an architecture, it
/// names this one. Words that name neither, such as `gnu`, change nothing.
fn names_machine(platform: &str, os: &str, arch: &str) -> Result<bool, String> {
    let lower = platform.to_lowercase();
    let words: Vec<&str> = lower.split(['_', '-']).collect();
    let says = |table: &[(&str, &[&str])], own: &str| -> (bool, bool) {
        let named = |aliases: &[&str]| words.iter().any(|word| aliases.contains(word));
        let any = table.iter().any(|(_, aliases)| named(aliases));
        let ours = table
            .iter()
            .any(|(name, aliases)| *name == own && named(aliases));
        (any, ours)
    };
    let (any_arch, our_arch) = says(&ARCHES, arch);
    let (any_os, our_os) = says(&SYSTEMS, os);
    if !any_arch && !any_os {
        return Err(format!(
            "`{platform}` names no system and no architecture; use words such as linux, macos, windows, x86_64 or arm64"
        ));
    }
    Ok((!any_arch || our_arch) && (!any_os || our_os))
}

impl Download {
    /// The file for a machine; `Ok(None)` where the pin has none for it.
    /// # Errors
    /// When a platform name is unknown, two files name the machine, or the file's keys are wrong.
    pub fn chosen(&self, os: &str, arch: &str) -> Result<Option<Chosen>, String> {
        let mut ours = Vec::new();
        for file in &self.files {
            if file.platform.is_empty() || names_machine(&file.platform, os, arch)? {
                ours.push(file);
            }
        }
        let file = match ours.as_slice() {
            [] => return Ok(None),
            [one] => *one,
            many => {
                let names: Vec<&str> = many.iter().map(|file| file.platform.as_str()).collect();
                return Err(format!(
                    "{} all name {os} on {arch}; keep one",
                    names.join(", ")
                ));
            }
        };
        let key = |suffix: &str| match file.platform.as_str() {
            "" => format!("{}{suffix}", self.stem),
            platform => format!("{}{suffix}_{platform}", self.stem),
        };
        let url = file.url.clone().ok_or_else(|| unset_url(&key(URL)))?;
        let sha256 = file
            .sha256
            .clone()
            .ok_or_else(|| format!("{} is missing", key(SHA256)))?;
        if !url.starts_with("https://") {
            return Err(format!("{} must start with https://", key(URL)));
        }
        let hex = sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit());
        if !hex {
            return Err(format!("{} must be 64 hex digits", key(SHA256)));
        }
        Ok(Some(Chosen {
            url,
            sha256: sha256.to_lowercase(),
        }))
    }

    /// The file for the machine that runs.
    /// # Errors
    /// When `chosen` fails for this machine.
    pub fn here(&self) -> Result<Option<Chosen>, String> {
        self.chosen(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// Where the file with this hash is unpacked.
    #[must_use]
    pub fn dir(&self, home: &Path, sha256: &str) -> PathBuf {
        let want: String = self
            .want
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        home.join(format!("{}-{want}-{}", self.name, &sha256[..12]))
    }

    /// The gate whose tool this is, for a pin that only one gate uses.
    #[must_use]
    pub fn gate(&self) -> Option<&'static str> {
        CLAIMED
            .iter()
            .find(|(stem, _)| *stem == self.stem)
            .map(|(_, gate)| *gate)
    }
}

/// chock's tools directory: `CHOCK_TOOLS`, else `chock-tools` in cargo's home. A relative path
/// is read from the directory that chock runs in, as the shell reads it.
#[must_use]
pub fn home() -> Option<PathBuf> {
    home_in(&|name| std::env::var_os(name)).and_then(|dir| std::path::absolute(dir).ok())
}

/// `home`, with `var` in place of the environment.
fn home_in(var: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|value| !value.is_empty());
    if let Some(dir) = set("CHOCK_TOOLS") {
        return Some(PathBuf::from(dir));
    }
    let cargo = set("CARGO_HOME").map(PathBuf::from).or_else(|| {
        set("HOME")
            .or_else(|| set("USERPROFILE"))
            .map(|home| PathBuf::from(home).join(".cargo"))
    })?;
    Some(cargo.join("chock-tools"))
}

/// Whether the file with this hash was fetched, checked and unpacked in `dir`.
#[must_use]
pub fn fetched(dir: &Path, sha256: &str) -> bool {
    std::fs::read_to_string(dir.join(STAMP)).is_ok_and(|stamp| stamp.trim() == sha256)
}

/// A missing URL key, with the line chock's own pin file holds for it, where it holds one.
fn unset_url(key: &str) -> String {
    let shipped = super::init::PIN_FILE
        .lines()
        .find(|line| line.split_once('=').is_some_and(|(name, _)| name == key));
    match shipped {
        Some(line) => format!("{key} is missing; add the line chock ships: {line}"),
        None => format!("{key} is missing"),
    }
}

/// The path of `wanted`, a file name such as `tla2tools.jar`, inside the download a gate needs.
/// # Errors
/// When the pin is missing or wrong, has no file here, or its download is not fetched or lacks it.
pub fn located(
    root: &Path,
    home: Option<&Path>,
    stem: &str,
    wanted: &str,
) -> Result<PathBuf, String> {
    let name = stem.to_lowercase().replace('_', "-");
    let text = std::fs::read_to_string(root.join(crate::project::PIN_FILE)).unwrap_or_default();
    let downloads = parse(&text).map_err(|e| format!("{}: {e}", crate::project::PIN_FILE))?;
    let Some(pin) = downloads.iter().find(|pin| pin.stem == stem) else {
        return Err(format!("`{name}`{NOT_PINNED}"));
    };
    let Some(file) = pin.here()? else {
        return Err(format!(
            "the {stem} pin has no file for {} on {}{NO_FILE}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
    };
    let home = home.ok_or("no home directory holds chock's tools; set CHOCK_TOOLS")?;
    let dir = pin.dir(home, &file.sha256);
    if !fetched(&dir, &file.sha256) {
        return Err(crate::gates::fixes::not_fetched(&name));
    }
    find(&dir, wanted, 4).ok_or_else(|| format!("{} holds no {wanted}", dir.display()))
}

/// The version the project pins `stem` at, such as `1.7.4` for `TLA_TOOLS_VERSION=1.7.4`.
pub fn version(root: &Path, stem: &str) -> Option<String> {
    let text = std::fs::read_to_string(root.join(crate::project::PIN_FILE)).ok()?;
    let pin = parse(&text)
        .ok()?
        .into_iter()
        .find(|pin| pin.stem == stem)?;
    Some(pin.want)
}

/// The first `wanted` at most `depth` directories down, in name order so every run finds the same.
/// `wanted` is a file name, or one with a `*` for any text, such as `tla2tools*.jar`.
fn matches(name: &str, wanted: &str) -> bool {
    match wanted.split_once('*') {
        Some((head, tail)) => {
            name.len() >= head.len() + tail.len() && name.starts_with(head) && name.ends_with(tail)
        }
        None => name == wanted,
    }
}

fn find(dir: &Path, wanted: &str, depth: u32) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();
    // A file only: a download often holds a directory named as its program, such as `tlapm/`.
    let named = |path: &&PathBuf| {
        path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| matches(name, wanted))
    };
    if let Some(hit) = entries.iter().find(named) {
        return Some(hit.clone());
    }
    let mut deeper = entries
        .iter()
        .filter(|path| crate::project::descends_into(path));
    (depth > 0)
        .then(|| deeper.find_map(|path| find(path, wanted, depth - 1)))
        .flatten()
}

/// Fetches one pin's file for this machine with curl, checks its hash, and unpacks it. The line it
/// returns says what happened.
/// # Errors
/// When the fetch, the hash check or the unpack fails; the `Err` holds the line to print.
pub fn fetch(pin: &Download, home: &Path, tools: &Tools) -> Result<String, String> {
    let failed = |why: String| format!("  FAILED    {} {} — {why}", pin.name, pin.want);
    let Some(file) = pin.here().map_err(failed)? else {
        let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
        return Ok(format!(
            "  skipped   {} — its pin has no file for {os} on {arch}",
            pin.name
        ));
    };
    let dir = pin.dir(home, &file.sha256);
    if fetched(&dir, &file.sha256) {
        return Ok(format!(
            "  kept      {} {} — already fetched and checked",
            pin.name, pin.want
        ));
    }
    let part = dir.with_added_extension("part");
    let clear = |dir: &Path| crate::gates::proofs::cleared(dir).map_err(failed);
    clear(&part)?;
    let io = |e: std::io::Error| failed(format!("cannot write {}: {e}", part.display()));
    crate::gates::proofs::made(&part).map_err(io)?;
    let unpacked = fetch_into(&file, &part, tools).and_then(|path| unpack(&path, &part, tools));
    if let Err(why) = unpacked {
        let also = clear(&part).err().map(|also| format!("\n{also}"));
        return Err(format!("{}{}", failed(why), also.unwrap_or_default()));
    }
    clear(&dir)?;
    let stamped = crate::project::document::write(&part.join(STAMP), &file.sha256);
    stamped
        .and_then(|()| std::fs::rename(&part, &dir))
        .map_err(io)?;
    Ok(format!(
        "  fetched   {} {} — its SHA-256 matches the pin",
        pin.name, pin.want
    ))
}

/// The file at the URL, in `part`, once its hash is the pin's.
fn fetch_into(file: &Chosen, part: &Path, tools: &Tools) -> Result<PathBuf, String> {
    let name = file.url.rsplit('/').next().unwrap_or_default();
    let name = name.split(['?', '#']).next().unwrap_or_default();
    let path = part.join(if name.is_empty() { "download" } else { name });
    let target = path.to_string_lossy().into_owned();
    let args = [
        "-fsSL", "--retry", "3", "--proto", "=https", "-o", &target, &file.url,
    ];
    let out = tools
        .run("curl", &args, part)
        .map_err(|e| format!("curl did not run: {e}"))?;
    if !out.success() {
        let why = out.why_it_failed();
        return Err(format!("curl could not fetch {}: {why}", file.url));
    }
    let got = super::sha256::of_file(&path).map_err(|e| format!("cannot read {target}: {e}"))?;
    if got != file.sha256 {
        let (url, want) = (&file.url, &file.sha256);
        return Err(format!(
            "{url} has SHA-256 {got}, and the pin says {want}; chock did not keep it"
        ));
    }
    Ok(path)
}

/// An archive unpacked in place of itself; any other file stays as it came. The archive is in `into`
/// and goes by its bare name, because the GNU tar of Git for Windows reads `C:` as a host.
fn unpack(path: &Path, into: &Path, tools: &Tools) -> Result<(), String> {
    let archive = path.file_name().unwrap_or_default().to_string_lossy();
    let name = archive.to_lowercase();
    let run = |program: &str, args: &[&str]| {
        tools
            .run(program, args, into)
            .is_ok_and(|out| out.success())
    };
    let ext = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    let done = if name.ends_with(".tar.gz") || ext == "tgz" {
        run("tar", &["-xzf", &archive])
    } else if name.ends_with(".tar.xz") {
        run("tar", &["-xJf", &archive])
    } else if ext == "zip" {
        // Windows and macOS tar read a zip; Linux tar does not, and unzip does.
        run("unzip", &["-q", &archive]) || run("tar", &["-xf", &archive])
    } else {
        return Ok(());
    };
    if !done {
        return Err(format!("cannot unpack {}", path.display()));
    }
    std::fs::remove_file(path).map_err(|e| format!("cannot remove {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "b781b53bba32fac14ac2e02b0332e1dc2edc64cb7b850c0608d8142cc7aa2ef4";

    #[test]
    fn a_url_and_a_hash_make_a_download_pin_and_no_crate_pin() {
        let text = format!(
            "# pins\n\nAPALACHE_VERSION=0.62.4\nAPALACHE_URL=https://x/apalache-0.62.4.tgz\nAPALACHE_SHA256={SHA}\nCARGO_DENY_VERSION=0.20.2\n"
        );
        let downloads = parse(&text).unwrap();
        let names: Vec<&str> = downloads.iter().map(|pin| pin.name.as_str()).collect();
        assert_eq!(names, ["apalache"]);
        let chosen = downloads[0].chosen("linux", "x86_64").unwrap().unwrap();
        assert_eq!(chosen.url, "https://x/apalache-0.62.4.tgz");
        let crates = pins::parse(&text).unwrap();
        let crates: Vec<String> = crates.into_iter().map(|pin| pin.crate_name).collect();
        assert_eq!(crates, ["cargo-deny"]);
        assert_eq!(
            pins::additional(&text).unwrap().unchecked,
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_platform_suffix_picks_the_file_of_this_machine_by_its_words() {
        let text = format!(
            "V_VERSION=1\nV_URL_x86_linux=https://l\nV_SHA256_x86_linux={SHA}\nV_URL_arm64_macos=https://m\nV_SHA256_arm64_macos={SHA}\n"
        );
        let pin = &parse(&text).unwrap()[0];
        let url = |os, arch| pin.chosen(os, arch).unwrap().map(|file| file.url);
        assert_eq!(url("linux", "x86_64").as_deref(), Some("https://l"));
        assert_eq!(url("macos", "aarch64").as_deref(), Some("https://m"));
        assert_eq!(url("windows", "x86_64"), None);
        assert_eq!(url("linux", "aarch64"), None);
        assert_eq!(
            names_machine("x86_64_linux_gnu", "linux", "x86_64"),
            Ok(true)
        );
        assert_eq!(names_machine("arm64_darwin", "macos", "aarch64"), Ok(true));
        assert!(names_machine("gnu", "linux", "x86_64").is_err());
        assert_eq!(names_machine("linux", "linux", "aarch64"), Ok(true));
        assert_eq!(names_machine("macos", "linux", "x86_64"), Ok(false));
    }

    #[test]
    fn a_url_names_an_earlier_line_as_bash_does_and_an_unset_name_is_refused() {
        let text = format!(
            "E_VERSION=2.1\nE_URL_linux=\"https://x/v${{E_VERSION}}/e-${{E_VERSION}}.zip\"\nE_SHA256_linux={SHA}\n"
        );
        let url = parse(&text).unwrap()[0]
            .chosen("linux", "x86_64")
            .unwrap()
            .unwrap()
            .url;
        assert_eq!(url, "https://x/v2.1/e-2.1.zip");
        let unset = parse("E_URL=https://x/${E_VERSION}\nE_VERSION=2.1\n").unwrap_err();
        assert_eq!(
            unset.to_string(),
            "line 1: E_URL uses ${E_VERSION}, which no earlier line of the file sets"
        );
        let set = BTreeMap::from([("V", "1".to_string())]);
        assert_eq!(expanded("a${V}b${V", &set), Ok("a1b${V".into()));
        assert_eq!(expanded("${W}", &set), Err("W".into()));
    }

    #[test]
    fn a_half_pin_a_plain_url_and_a_short_hash_are_each_named() {
        let pin = |extra: &str| parse(&format!("T_VERSION=1\n{extra}")).unwrap()[0].here();
        let claimed = parse(&format!("VERUS_VERSION=1\nVERUS_SHA256={SHA}\n")).unwrap();
        assert!(
            claimed[0]
                .here()
                .unwrap_err()
                .contains("VERUS_URL is missing")
        );
        let tla = parse(&format!("TLA_TOOLS_VERSION=1\nTLA_TOOLS_SHA256={SHA}\n")).unwrap();
        assert_eq!(
            tla[0].here().unwrap_err(),
            "TLA_TOOLS_URL is missing; add the line chock ships: TLA_TOOLS_URL=https://github.com/\
             tlaplus/tlaplus/releases/download/v${TLA_TOOLS_VERSION}/tla2tools.jar"
        );
        assert!(
            pin("T_URL=https://x\n")
                .unwrap_err()
                .contains("T_SHA256 is missing")
        );
        let plain = pin(&format!("T_URL=http://x\nT_SHA256={SHA}\n")).unwrap_err();
        assert!(plain.contains("https://"));
        assert!(
            pin("T_URL=https://x\nT_SHA256=abc\n")
                .unwrap_err()
                .contains("64 hex")
        );
    }

    #[test]
    fn a_hash_with_no_url_or_no_version_is_left_to_the_project_s_own_scripts() {
        let text = format!(
            "FIXPOINT_SHA256_x86_64_linux_gnu={SHA}\nESBMC_VERSION=8.5\nESBMC_SHA256_linux={SHA}\nCARGO_DENY_VERSION=0.20.2\n"
        );
        assert_eq!(parse(&text).unwrap(), Vec::new());
        let crates: Vec<String> = pins::parse(&text)
            .unwrap()
            .into_iter()
            .map(|pin| pin.crate_name)
            .collect();
        assert_eq!(crates, ["cargo-deny"]);
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_new_hash_gets_a_new_directory_and_only_a_stamped_one_counts() {
        let text = format!(
            "TLA_TOOLS_VERSION=1.8.0-20261015.1\nTLA_TOOLS_URL=https://x/t.jar\nTLA_TOOLS_SHA256={SHA}\n"
        );
        let pin = &parse(&text).unwrap()[0];
        let home = crate::testdir::make("downloads-dir");
        let dir = pin.dir(&home, SHA);
        assert_eq!(
            dir.file_name().unwrap(),
            "tla-tools-1.8.0-20261015.1-b781b53bba32"
        );
        assert_eq!(pin.gate(), Some("tla"));
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib").join("t.jar"), "").unwrap();
        assert!(!fetched(&dir, SHA));
        std::fs::write(dir.join(STAMP), SHA).unwrap();
        assert!(fetched(&dir, SHA));
        assert_eq!(find(&dir, "t.jar", 4), Some(dir.join("lib").join("t.jar")));
        assert_eq!(find(&dir, "t*.jar", 4), Some(dir.join("lib").join("t.jar")));
        assert!(!matches("t.jar", "tt*.jar") && !matches("t.ja", "t*.jar"));
        // `a*a` needs two letters, and `b*a` needs its head.
        assert!(!matches("a", "a*a") && !matches("xa", "b*a"));
        assert_eq!(find(&dir, "t.jar", 0), None);
    }

    #[test]
    fn two_files_for_one_machine_a_setup_line_and_an_odd_version_are_each_read() {
        let text = format!(
            "VERUS_VERSION=1+a\nVERUS_SETUP=\"sh -c true\"\nVERUS_SHA256_linux={SHA}\nT_VERSION=1\nT_URL_linux=https://x/${{T\nT_SHA256_linux={SHA}\nT_URL_x86_64_linux=https://y\nT_SHA256_x86_64_linux={SHA}\n"
        );
        let pins = parse(&text).unwrap();
        assert_eq!(pins[0].setup.as_deref(), Some("sh -c true"));
        let missing = pins[0].chosen("linux", "x86_64").unwrap_err();
        assert_eq!(missing, "VERUS_URL_linux is missing");
        let dir = pins[0].dir(Path::new("t"), SHA);
        assert_eq!(dir.file_name().unwrap(), "verus-1-a-b781b53bba32");
        let both = pins[1].chosen("linux", "x86_64").unwrap_err();
        assert_eq!(
            both,
            "linux, x86_64_linux all name linux on x86_64; keep one"
        );
        let open = pins[1].chosen("linux", "aarch64").unwrap().unwrap();
        assert_eq!(open.url, "https://x/${T");
    }

    #[test]
    fn the_tools_directory_is_chock_tools_else_one_in_cargo_s_home() {
        let home = |vars: &[(&str, &str)]| {
            let vars: BTreeMap<&str, &str> = vars.iter().copied().collect();
            home_in(&|name| vars.get(name).map(|value| (*value).into()))
        };
        let both = [("CHOCK_TOOLS", "t"), ("CARGO_HOME", "c")];
        assert_eq!(home(&both), Some(PathBuf::from("t")));
        let cargo = Some(Path::new("c").join("chock-tools"));
        assert_eq!(home(&[("CHOCK_TOOLS", ""), ("CARGO_HOME", "c")]), cargo);
        let user = |home: &str| Some(Path::new(home).join(".cargo").join("chock-tools"));
        assert_eq!(home(&[("HOME", "h"), ("USERPROFILE", "u")]), user("h"));
        assert_eq!(home(&[("USERPROFILE", "u")]), user("u"));
        assert_eq!(home(&[]), None);
        assert!(super::home().is_some_and(|dir| dir.is_absolute()));
    }

    /// The other system, whose file no machine that runs this test can use.
    const OTHER: &str = if cfg!(windows) { "linux" } else { "windows" };

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_finds_its_file_only_in_a_fetched_download_of_its_own_pin() {
        let root = crate::testdir::make("downloads-located");
        let home = root.join("tools");
        let at = |stem: &str, wanted: &str| located(&root, Some(&home), stem, wanted);
        assert_eq!(
            at("TLA_TOOLS", "t.jar").unwrap_err(),
            format!("`tla-tools`{NOT_PINNED}")
        );
        assert_eq!(version(&root, "TLA_TOOLS"), None);
        let pin = crate::testdir::fetched(&root, "TLA_TOOLS", "t.jar");
        let other = format!("E_VERSION=2\nE_URL_{OTHER}=https://x\nE_SHA256_{OTHER}={SHA}\n");
        std::fs::write(root.join(crate::project::PIN_FILE), format!("{pin}{other}")).unwrap();
        let dir = parse(&pin).unwrap()[0].dir(&home, SHA);
        assert_eq!(at("TLA_TOOLS", "t*.jar").unwrap(), dir.join("t.jar"));
        let pinned = |stem: &str| version(&root, stem);
        assert_eq!(pinned("TLA_TOOLS").as_deref(), Some("1"));
        assert_eq!((pinned("E").as_deref(), pinned("F")), (Some("2"), None));
        let none = format!("{} holds no x.jar", dir.display());
        assert_eq!(at("TLA_TOOLS", "x.jar").unwrap_err(), none);
        let homeless = located(&root, None, "TLA_TOOLS", "t.jar").unwrap_err();
        assert_eq!(
            homeless,
            "no home directory holds chock's tools; set CHOCK_TOOLS"
        );
        let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
        let elsewhere = format!("the E pin has no file for {os} on {arch}{NO_FILE}");
        assert_eq!(at("E", "e").unwrap_err(), elsewhere);
        std::fs::remove_file(dir.join(STAMP)).unwrap();
        let missing = crate::gates::fixes::not_fetched("tla-tools");
        assert_eq!(at("TLA_TOOLS", "t.jar").unwrap_err(), missing);
        std::fs::write(root.join(crate::project::PIN_FILE), "E_URL=${NOPE}\n").unwrap();
        assert!(at("E", "e").unwrap_err().starts_with("tool-versions.env: "));
    }

    /// A curl that writes `abc`, but fails on a URL with `bad` in it; a tar that unpacks a file
    /// `unpacked/tool`, but fails on an archive with `broken` in its name; an unzip that fails.
    const FAKE: &str = r#"program=$1; shift
case "$program $*" in
  "curl "*bad*) echo 'curl: (22) 404' >&2; exit 22 ;;
  "curl "*) for arg; do [ "$last" = -o ] && printf abc > "$arg"; last=$arg; done ;;
  unzip*) exit 9 ;;
  *broken*) exit 2 ;;
  tar*) mkdir -p unpacked && : > unpacked/tool ;;
esac
"#;

    /// The SHA-256 of `abc`.
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_fetch_keeps_a_file_whose_hash_matches_and_unpacks_an_archive() {
        let root = crate::testdir::make("downloads-fetch");
        let tools = crate::testdir::faked(&root, FAKE);
        let home = root.join("tools");
        let pin = |stem: &str, url: &str| {
            let text = format!("{stem}_VERSION=1\n{stem}_URL={url}\n{stem}_SHA256={ABC}\n");
            parse(&text).unwrap().remove(0)
        };
        let jar = pin("J", "https://x/j.jar");
        let dir = jar.dir(&home, ABC);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stale"), "").unwrap();
        let fetched = fetch(&jar, &home, &tools).unwrap();
        assert_eq!(fetched, "  fetched   j 1 — its SHA-256 matches the pin");
        assert_eq!(std::fs::read_to_string(dir.join("j.jar")).unwrap(), "abc");
        assert!(
            !dir.join("stale").exists(),
            "the unstamped directory stayed"
        );
        let kept = fetch(&jar, &home, &tools).unwrap();
        assert_eq!(kept, "  kept      j 1 — already fetched and checked");
        for (stem, url) in [("G", "https://x/g.tgz"), ("X", "https://x/x.tar.xz")] {
            let archive = pin(stem, url);
            fetch(&archive, &home, &tools).unwrap();
            let dir = archive.dir(&home, ABC);
            assert!(
                dir.join("unpacked/tool").is_file(),
                "{url} was not unpacked"
            );
            assert!(
                !dir.join(url.rsplit('/').next().unwrap()).exists(),
                "{url} stayed"
            );
        }
        let zip = pin("Z", "https://x/z.zip");
        fetch(&zip, &home, &tools).unwrap();
        assert!(zip.dir(&home, ABC).join("unpacked/tool").is_file());
        let bare = pin("N", "https://x/");
        fetch(&bare, &home, &tools).unwrap();
        assert!(bare.dir(&home, ABC).join("download").is_file());
        assert_eq!(unpack(Path::new("j.jar"), &root, &tools), Ok(()));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start a process")]
    fn a_fetch_that_fails_says_why_and_leaves_nothing_behind() {
        let root = crate::testdir::make("downloads-refused");
        let tools = crate::testdir::faked(&root, FAKE);
        let home = root.join("tools");
        let fetched = |text: String| fetch(&parse(&text).unwrap()[0], &home, &tools);
        let pin = |url: &str, sha: &str| format!("P_VERSION=1\nP_URL={url}\nP_SHA256={sha}\n");
        let part = parse(&pin("https://x", ABC)).unwrap()[0].dir(&home, ABC);
        let part = part.with_added_extension("part");
        let unpacked = fetched(pin("https://x/broken.tgz", ABC)).unwrap_err();
        let archive = part.join("broken.tgz");
        assert_eq!(
            unpacked,
            format!("  FAILED    p 1 — cannot unpack {}", archive.display())
        );
        assert!(!part.exists(), "the half fetch stayed");
        let bad = parse(&pin("https://x/bad.jar", ABC)).unwrap().remove(0);
        let refused = fetch_into(&bad.here().unwrap().unwrap(), &root, &tools).unwrap_err();
        assert_eq!(
            refused,
            "curl could not fetch https://x/bad.jar: curl: (22) 404"
        );
        let other = fetched(pin("https://x/w.jar", SHA)).unwrap_err();
        let said = format!("https://x/w.jar has SHA-256 {ABC}, and the pin says {SHA}");
        assert_eq!(
            other,
            format!("  FAILED    p 1 — {said}; chock did not keep it")
        );
        let plain = fetched(pin("http://x/p.jar", ABC)).unwrap_err();
        assert_eq!(plain, "  FAILED    p 1 — P_URL must start with https://");
        let elsewhere = format!("P_VERSION=1\nP_URL_{OTHER}=https://x\nP_SHA256_{OTHER}={ABC}\n");
        let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
        let skipped = format!("  skipped   p — its pin has no file for {os} on {arch}");
        assert_eq!(fetched(elsewhere).unwrap(), skipped);
        let gone = Tools {
            home: None,
            launcher: vec!["chock-no-such-program".into()],
        };
        let pin = parse(&pin("https://x/p.jar", ABC)).unwrap().remove(0);
        let lost = fetch(&pin, &home, &gone).unwrap_err();
        assert!(
            lost.starts_with("  FAILED    p 1 — curl did not run: "),
            "{lost}"
        );
    }
}
