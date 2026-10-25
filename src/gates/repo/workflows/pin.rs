//! Whether a workflow names the code it runs by an identifier its owner cannot move: a full commit
//! SHA for an action or a reusable workflow, a `sha256` digest for a container image.

/// How a `uses:` value names the code a step or a job runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pin {
    /// `$/path`: the same repository at the running commit; `./path`: the checked-out workspace.
    Local,
    /// `owner/repo[/path]@` and 40 hexadecimal digits: one commit.
    Commit,
    /// `docker://image@sha256:` and 64 hexadecimal digits: one image.
    Digest,
    /// A tag, a branch, a short SHA or any other name that its owner can point at new code.
    Movable,
}

/// How `uses` names its code.
pub(super) fn action(uses: &[u8]) -> Pin {
    if uses.starts_with(b"./") || uses.starts_with(b"$/") {
        return Pin::Local;
    }
    if let Some(reference) = uses.strip_prefix(b"docker://") {
        return if image(reference) {
            Pin::Digest
        } else {
            Pin::Movable
        };
    }
    match after_last_at(uses) {
        Some(version) if hex(version, 40) => Pin::Commit,
        _ => Pin::Movable,
    }
}

/// Whether a container `image` names one image by its `sha256` digest.
pub(super) fn image(reference: &[u8]) -> bool {
    after_last_at(reference)
        .and_then(|version| version.strip_prefix(b"sha256:"))
        .is_some_and(|digits| hex(digits, 64))
}

/// What follows the last `@`, when a name stands before it.
fn after_last_at(value: &[u8]) -> Option<&[u8]> {
    match value.iter().rposition(|&byte| byte == b'@') {
        Some(at) if at > 0 => value.get(at + 1..),
        _ => None,
    }
}

/// Whether `digits` is exactly `count` hexadecimal digits.
fn hex(digits: &[u8], count: usize) -> bool {
    digits.len() == count && digits.iter().all(u8::is_ascii_hexdigit)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "8e5e7e5ab8b370d6c329ec480221332ada57f0ab";
    const DIGEST: &str = "sha256:2b5e7e5ab8b370d6c329ec480221332ada57f0ab8e5e7e5ab8b370d6c329ec48";

    fn pin(uses: &str) -> Pin {
        action(uses.as_bytes())
    }

    #[test]
    fn a_full_commit_sha_pins_an_action_and_a_reusable_workflow() {
        assert_eq!(pin(&format!("actions/checkout@{SHA}")), Pin::Commit);
        assert_eq!(
            pin(&format!("org/repo/.github/workflows/build.yml@{SHA}")),
            Pin::Commit
        );
        assert_eq!(
            pin(&format!("org/repo/sub/dir@{}", SHA.to_uppercase())),
            Pin::Commit
        );
    }

    #[test]
    fn a_tag_a_branch_or_a_short_sha_can_move() {
        assert_eq!(pin("actions/checkout@v4"), Pin::Movable);
        assert_eq!(pin("cargo-bins/cargo-binstall@main"), Pin::Movable);
        assert_eq!(
            pin(&format!("actions/checkout@{}", &SHA[..7])),
            Pin::Movable
        );
        assert_eq!(pin(&format!("actions/checkout@{SHA}0")), Pin::Movable);
        assert_eq!(
            pin(&format!("actions/checkout@{}g", &SHA[..39])),
            Pin::Movable
        );
        assert_eq!(pin("actions/checkout"), Pin::Movable);
        assert_eq!(pin(&format!("@{SHA}")), Pin::Movable);
    }

    #[test]
    fn a_path_in_the_same_repository_is_local() {
        assert_eq!(pin("./.github/actions/setup"), Pin::Local);
        assert_eq!(pin("$/.github/actions/setup"), Pin::Local);
        assert_eq!(pin("$/.github/workflows/build.yml"), Pin::Local);
        assert_eq!(pin("$.github/actions/setup@v1"), Pin::Movable);
    }

    #[test]
    fn a_docker_action_pins_only_by_digest() {
        assert_eq!(pin(&format!("docker://alpine@{DIGEST}")), Pin::Digest);
        assert_eq!(pin("docker://alpine:3.20"), Pin::Movable);
        assert_eq!(pin(&format!("docker://alpine:3.20@{DIGEST}")), Pin::Digest);
    }

    #[test]
    fn an_image_pins_only_by_a_whole_sha256_digest() {
        assert!(image(format!("ghcr.io/org/image@{DIGEST}").as_bytes()));
        assert!(!image(b"node:20"));
        assert!(!image(format!("node@{}", &DIGEST[..70]).as_bytes()));
        assert!(!image(format!("node@sha512:{}", &DIGEST[7..]).as_bytes()));
        assert!(!image(DIGEST.as_bytes()));
    }
}

/// Kani proofs over every input up to a bound, built only under `cargo kani`.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// `@` and 40 digits need 41 bytes; one more byte lets a name stand in front of them.
    const ACTION_BOUND: usize = 44;
    /// `@sha256:` and 64 digits need 72 bytes; two more let a name stand in front of them.
    const IMAGE_BOUND: usize = 74;

    /// A commit pin is 40 hexadecimal digits after an `@` that has a name before it.
    #[kani::proof]
    #[kani::unwind(46)]
    fn a_commit_pin_is_forty_hex_digits_after_a_named_action() {
        let (bytes, len) = crate::bounded::bytes::<ACTION_BOUND>();
        let uses = &bytes[..len];
        if action(uses) == Pin::Commit {
            assert!(len > 41);
            assert!(uses[len - 41] == b'@');
            let mut i = len - 40;
            while i < len {
                assert!(uses[i].is_ascii_hexdigit());
                i += 1;
            }
        }
    }

    /// Only a path in the same repository, after `./` or `$/`, passes without a commit or a digest.
    #[kani::proof]
    #[kani::unwind(46)]
    fn a_local_pin_starts_with_dot_slash_or_dollar_slash() {
        let (bytes, len) = crate::bounded::bytes::<ACTION_BOUND>();
        let uses = &bytes[..len];
        if action(uses) == Pin::Local {
            assert!(len >= 2 && uses[1] == b'/' && (uses[0] == b'.' || uses[0] == b'$'));
        }
    }

    /// Any name that is not a local path and has no `@`, then `@` and 40 hexadecimal digits, is a
    /// commit pin.
    #[kani::proof]
    #[kani::unwind(46)]
    fn forty_hex_digits_after_a_name_always_pin_a_commit() {
        let name_len: usize = kani::any();
        kani::assume(name_len >= 1 && name_len <= ACTION_BOUND - 41);
        let mut bytes: [u8; ACTION_BOUND] = kani::any();
        let mut i = 0;
        while i < name_len {
            kani::assume(bytes[i] != b'@');
            i += 1;
        }
        bytes[name_len] = b'@';
        let mut i = name_len + 1;
        while i < name_len + 41 {
            kani::assume(bytes[i].is_ascii_hexdigit());
            i += 1;
        }
        kani::assume(!(bytes[1] == b'/' && (bytes[0] == b'.' || bytes[0] == b'$')));
        assert!(action(&bytes[..name_len + 41]) == Pin::Commit);
    }

    /// A digest pin is `@sha256:` and 64 hexadecimal digits after a name.
    #[kani::proof]
    #[kani::unwind(76)]
    fn a_digest_pin_is_sixty_four_hex_digits_after_sha256() {
        let (bytes, len) = crate::bounded::bytes::<IMAGE_BOUND>();
        let reference = &bytes[..len];
        if image(reference) {
            assert!(len > 72);
            let mut i = 0;
            while i < 8 {
                assert!(reference[len - 72 + i] == b"@sha256:"[i]);
                i += 1;
            }
            let mut i = len - 64;
            while i < len {
                assert!(reference[i].is_ascii_hexdigit());
                i += 1;
            }
        }
    }
}
