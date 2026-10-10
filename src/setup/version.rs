//! Reads an installed tool's version: from `cargo install --list` where cargo installed it,
//! otherwise from its `--version` output.

/// A crate's version in `cargo install --list`. Only unindented `<crate> v<version>:` lines count,
/// since the indented lines beneath them name binaries.
#[must_use]
pub fn from_cargo_listing(listing: &str, crate_name: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        if line.starts_with(char::is_whitespace) {
            return None;
        }
        let rest = line.strip_prefix(crate_name)?.strip_prefix(" v")?;
        let version = rest.strip_suffix(':').unwrap_or(rest);
        version.split_whitespace().next().map(str::to_string)
    })
}

/// The first three-part version in a tool's `--version` output.
#[must_use]
pub fn from_version_output(output: &str) -> Option<String> {
    let bytes = output.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit()
            && (i == 0 || !matches!(bytes[i - 1], b'0'..=b'9' | b'.'))
            && let Some(end) = semver_end(bytes, i)
        {
            return Some(output[i..end].to_string());
        }
        i += 1;
    }
    None
}

/// The end of a `digits.digits.digits` run starting at `start`, or `None` if there is not one.
fn semver_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    for component in 0..3 {
        if component > 0 {
            if bytes.get(i) != Some(&b'.') {
                return None;
            }
            i += 1;
        }
        let digits_from = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == digits_from {
            return None;
        }
    }
    // A fourth component cannot be compared against a three-part pin.
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    Some(i)
}

/// Two `major.minor.patch` versions in order. `None` where either has another shape, such as a
/// pre-release, since chock cannot tell which of two such pins is the newer.
#[must_use]
pub fn compare(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let parts = |version: &str| {
        let mut each = version.split('.').map(|part| part.parse::<u64>().ok());
        let three = [each.next()??, each.next()??, each.next()??];
        each.next().is_none().then_some(three)
    };
    Some(parts(a)?.cmp(&parts(b)?))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing, which is the point"
)]
mod tests {
    use super::*;

    const LISTING: &str = "\
cargo-crap v0.4.3:
    cargo-crap
cargo-nextest v0.9.143:
    cargo-nextest
kani-verifier v0.67.0:
    cargo-kani
    kani
";

    #[test]
    fn versions_compare_by_number_and_only_in_three_parts() {
        use std::cmp::Ordering::{Equal, Greater, Less};
        assert_eq!(compare("0.1.0", "0.1.1"), Some(Less));
        assert_eq!(compare("0.10.0", "0.9.9"), Some(Greater));
        assert_eq!(compare("1.2.3", "1.2.3"), Some(Equal));
        for odd in ["0.2.0-beta.1", "1.2", "1.2.3.4", ""] {
            assert_eq!(compare(odd, "0.1.0"), None, "{odd}");
            assert_eq!(compare("0.1.0", odd), None, "{odd}");
        }
    }

    #[test]
    fn a_listed_crate_yields_the_version_beside_its_name() {
        assert_eq!(
            from_cargo_listing(LISTING, "cargo-nextest"),
            Some("0.9.143".to_string())
        );
    }

    #[test]
    fn a_crate_the_listing_does_not_hold_yields_nothing() {
        assert_eq!(from_cargo_listing(LISTING, "cargo-deny"), None);
    }

    #[test]
    fn an_indented_binary_name_is_not_read_as_an_installed_crate() {
        assert_eq!(from_cargo_listing(LISTING, "kani"), None);
    }

    #[test]
    fn a_crate_whose_name_prefixes_another_does_not_match_it() {
        assert_eq!(from_cargo_listing(LISTING, "cargo"), None);
    }

    #[test]
    fn the_four_formats_the_pinned_tools_actually_print() {
        assert_eq!(
            from_version_output("cargo-deny 0.20.2"),
            Some("0.20.2".to_string())
        );
        assert_eq!(
            from_version_output("Version: 0.0.2"),
            Some("0.0.2".to_string())
        );
        assert_eq!(from_version_output("0.9.2"), Some("0.9.2".to_string()));
        assert_eq!(
            from_version_output("cargo-nextest 0.9.143 (60fa45f63 2026-08-04)"),
            Some("0.9.143".to_string())
        );
    }

    #[test]
    fn the_shortest_version_fits_exactly_and_shorter_suffixes_do_not() {
        assert_eq!(from_version_output(""), None);
        assert_eq!(from_version_output("1.23"), None);
        assert_eq!(from_version_output("0.0.0"), Some("0.0.0".into()));
        assert_eq!(from_version_output("tool 0.0.0"), Some("0.0.0".into()));
        assert_eq!(from_version_output("é0.0.0"), Some("0.0.0".into()));
        assert_eq!(from_version_output("0.0.0é"), Some("0.0.0".into()));
        assert_eq!(from_version_output("tool 1.2"), None);
    }

    #[test]
    fn a_build_hash_after_the_version_is_not_read_as_the_version() {
        assert_eq!(
            from_version_output("tool 1.2.3 (abc1234 2026-01-02)"),
            Some("1.2.3".to_string())
        );
    }

    #[test]
    fn a_date_is_not_a_version() {
        assert_eq!(from_version_output("built 2026-08-04"), None);
    }

    #[test]
    fn a_two_component_number_is_not_a_version() {
        assert_eq!(from_version_output("tool 1.2"), None);
    }

    #[test]
    fn a_four_component_number_is_refused_rather_than_truncated() {
        assert_eq!(from_version_output("tool 1.2.3.4"), None);
    }

    #[test]
    fn a_digit_inside_a_tool_name_does_not_start_a_version() {
        assert_eq!(
            from_version_output("py3lint 2.0.1"),
            Some("2.0.1".to_string())
        );
    }

    #[test]
    fn output_with_no_version_in_it_yields_nothing() {
        assert_eq!(from_version_output("command not found"), None);
    }

    #[test]
    fn a_trailing_dot_with_no_fourth_number_is_still_a_version() {
        assert_eq!(
            from_version_output("tool 1.2.3."),
            Some("1.2.3".to_string())
        );
    }

    #[test]
    fn a_dot_followed_by_a_non_digit_does_not_make_it_a_fourth_component() {
        assert_eq!(
            from_version_output("tool 1.2.3.x"),
            Some("1.2.3".to_string())
        );
    }

    #[test]
    fn a_prerelease_suffix_is_not_part_of_the_compared_version() {
        assert_eq!(
            from_version_output("tool 1.2.3-rc1"),
            Some("1.2.3".to_string())
        );
    }
}

/// Kani proofs over every input up to a bound, built only under `cargo kani`.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// Input bounds; proof cost grows steeply with length, so the end-to-end proof gets less.
    const INDEX_BOUND: usize = 16;
    const TEXT_BOUND: usize = 6;

    /// A slice panics past its end or inside a character; this proof and the next cover one each.
    #[kani::proof]
    #[kani::unwind(18)]
    fn semver_end_never_points_past_the_bytes_it_read() {
        let len: usize = kani::any();
        kani::assume(len <= INDEX_BOUND);
        let bytes: [u8; INDEX_BOUND] = kani::any();
        let start: usize = kani::any();
        kani::assume(start < len);
        if let Some(end) = semver_end(&bytes[..len], start) {
            assert!(end <= len);
            assert!(end > start);
        }
    }

    /// An ASCII byte is a whole character, so the slice cannot split one.
    #[kani::proof]
    #[kani::unwind(18)]
    fn semver_end_consumes_only_single_byte_characters() {
        let len: usize = kani::any();
        kani::assume(len <= INDEX_BOUND);
        let bytes: [u8; INDEX_BOUND] = kani::any();
        let start: usize = kani::any();
        kani::assume(start < len);
        if let Some(end) = semver_end(&bytes[..len], start) {
            let mut i = start;
            while i < end {
                assert!(bytes[i].is_ascii_digit() || bytes[i] == b'.');
                i += 1;
            }
        }
    }

    /// The whole function, without assuming the two proofs above.
    #[kani::proof]
    #[kani::unwind(8)]
    fn no_text_up_to_six_bytes_makes_from_version_output_panic() {
        let len: usize = kani::any();
        kani::assume(len <= TEXT_BOUND);
        let bytes: [u8; TEXT_BOUND] = kani::any();
        if let Ok(text) = core::str::from_utf8(&bytes[..len]) {
            let _ = from_version_output(text);
        }
    }
}
