//! The Rust toolchain that doctor reports: its name, channel, date and host.

use super::Status;
use crate::setup::version;

pub(super) fn toolchain_status(want: &str, stdout: &str) -> Status {
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

pub(super) fn missing_toolchain(want: &str) -> Status {
    Status::broken(format!(
        "pinned toolchain {want} is not installed; run `rustup toolchain install {want}`"
    ))
}

pub(super) fn toolchain_name(name: &str) -> Option<(&str, Option<&str>)> {
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

pub(super) fn toolchain_host(host: &str) -> bool {
    let parts: Vec<&str> = host.split('-').collect();
    (3..=4).contains(&parts.len())
        && host.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && parts.iter().all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
}
