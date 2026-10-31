//! The Rust release a crate promises in `rust-version`, and the release that brought each feature
//! chock's advice names, so no gate asks a crate for code its promised Rust cannot build.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use super::workspace::Metadata;

/// A Rust release by major and minor number. A patch release brings no feature, so `1.81.2` reads
/// as `1.81`. The order is by number: `1.9` comes before `1.81`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Release {
    /// The major number, `1` in `1.81`.
    pub major: u32,
    /// The minor number, `81` in `1.81`.
    pub minor: u32,
}

impl Release {
    /// The release with these numbers.
    #[must_use]
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// `1.81` or `1.81.0`, as Cargo takes `rust-version`; `None` for any other text.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.').map(number);
        let release = Self::new(parts.next()??, parts.next()??);
        match (parts.next(), parts.next()) {
            (None | Some(Some(_)), None) => Some(release),
            _ => None,
        }
    }
}

impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// A run of ASCII digits as a number; `+1`, which `u32::from_str` takes, is no part of a release.
fn number(part: &str) -> Option<u32> {
    let digits = !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    digits.then(|| part.parse().ok()).flatten()
}

/// Something chock's advice can ask a crate to write, and the release that first builds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feature {
    /// What the advice asks for, as the advice names it.
    pub what: &'static str,
    /// The first release that builds it.
    pub since: Release,
}

impl Feature {
    /// Whether a crate that promises `promise` can use it. A crate with no promise builds on the
    /// Rust it pins, which chock takes as the newest.
    #[must_use]
    pub fn open_to(self, promise: Option<Release>) -> bool {
        promise.is_none_or(|promise| promise >= self.since)
    }
}

/// `[lints]` in Cargo.toml. An older Cargo warns of an unused key and builds without the lints.
pub const LINT_TABLE: Feature = Feature {
    what: "the `[lints]` table",
    since: Release::new(1, 74),
};

/// `#[expect(…)]` and `reason = "…"` in a lint attribute. Clippy turns on `allow_attributes` and
/// `allow_attributes_without_reason` only for a crate that promises this release or later.
pub const LINT_REASONS: Feature = Feature {
    what: "`#[expect(…)]` and `reason = \"…\"`",
    since: Release::new(1, 81),
};

/// Every feature above, for the table in docs/gates.md.
pub const FEATURES: [Feature; 2] = [LINT_TABLE, LINT_REASONS];

/// The release each workspace member promises, keyed by its directory relative to the root (`""`
/// for a member at the root). A member with no `rust-version`, or an unreadable one, is absent.
#[must_use]
pub fn promises(metadata: &Metadata) -> BTreeMap<String, Release> {
    let root = Path::new(&metadata.workspace_root);
    metadata
        .packages
        .iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
        .filter_map(|package| {
            let promise = Release::parse(package.rust_version.as_deref()?)?;
            let manifest = super::relative(root, Path::new(&package.manifest_path));
            Some((super::crate_dir(&manifest)?, promise))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_compare_by_number_so_1_9_comes_before_1_81() {
        let parse = |text| Release::parse(text);
        assert!(parse("1.9") < parse("1.81"));
        assert!(parse("1.81") < parse("2.0"));
        assert_eq!(parse("1.81.2"), parse("1.81"));
    }

    #[test]
    fn only_two_or_three_runs_of_digits_are_a_release() {
        for bad in [
            "",
            "1",
            "1.",
            ".81",
            "1.81.",
            "1.81.0.1",
            "1.x",
            "+1.81",
            "stable",
            "1.81-beta",
        ] {
            assert_eq!(Release::parse(bad), None, "{bad}");
        }
        assert_eq!(Release::parse("1.81.0"), Some(Release::new(1, 81)));
        assert_eq!(Release::new(1, 81).to_string(), "1.81");
    }

    #[test]
    fn a_feature_is_open_from_its_release_on_and_to_a_crate_with_no_promise() {
        let open = |promise| LINT_REASONS.open_to(Release::parse(promise));
        assert!(!open("1.80"));
        assert!(open("1.81"));
        assert!(open("2.0"));
        assert!(LINT_REASONS.open_to(None));
    }

    #[test]
    fn each_member_is_keyed_by_its_directory_and_one_with_no_promise_is_left_out() {
        let metadata: Metadata = serde_json::from_str(
            r#"{"workspace_root": "/w", "workspace_members": ["a", "b", "c"], "packages": [
                {"id": "a", "manifest_path": "/w/Cargo.toml", "rust_version": "1.70"},
                {"id": "b", "manifest_path": "/w/crates/b/Cargo.toml", "rust_version": "1.85.1"},
                {"id": "c", "manifest_path": "/w/crates/c/Cargo.toml", "rust_version": null},
                {"id": "d", "manifest_path": "/r/d/Cargo.toml", "rust_version": "1.60"}]}"#,
        )
        .unwrap();
        let keyed = [
            (String::new(), Release::new(1, 70)),
            ("crates/b".to_string(), Release::new(1, 85)),
        ];
        assert_eq!(promises(&metadata), BTreeMap::from(keyed));
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Raising `rust-version` never closes a feature: a feature open to a promise is open to every
    /// later one, and to a crate with no promise.
    #[kani::proof]
    fn a_later_promise_keeps_every_feature_open() {
        let since = Release::new(kani::any(), kani::any());
        let feature = Feature { what: "", since };
        let (low, high) = (
            Release::new(kani::any(), kani::any()),
            Release::new(kani::any(), kani::any()),
        );
        if low <= high && feature.open_to(Some(low)) {
            assert!(feature.open_to(Some(high)));
        }
        assert!(feature.open_to(None));
        assert!(feature.open_to(Some(since)));
    }

    /// A later major release with a smaller minor number still comes after: 2.0 is after 1.81.
    #[kani::proof]
    fn the_major_number_decides_before_the_minor() {
        let (a, b) = (
            Release::new(kani::any(), kani::any()),
            Release::new(kani::any(), kani::any()),
        );
        if a.major < b.major {
            assert!(a < b);
        }
    }
}
