//! What mutest's code for a mutant needs at the root of its crate. mutest's own build has it, so
//! the copy gives it to the roots of that mutant's package while Kani checks the mutant.

/// The feature that lets a type name its allocator, as mutest's types do.
const FEATURE: &str = "#![feature(allocator_api)] ";
/// The crate that a path from `::alloc` names.
const ALLOC: &str = "extern crate alloc;";

/// `root`, the text of a crate root, with what `code` needs and `root` lacks; `None` where it
/// lacks nothing. The feature goes in front of the first line and the crate after the last.
pub(super) fn equipped(root: &str, code: &[&str]) -> Option<String> {
    let names = |part: &str| code.iter().any(|code| code.contains(part));
    let feature = names("::alloc::Global") && !root.contains("allocator_api");
    let alloc = names("::alloc::") && !root.contains(ALLOC);
    if !feature && !alloc {
        return None;
    }
    let mut text = String::new();
    if feature {
        text.push_str(FEATURE);
    }
    text.push_str(root);
    if alloc {
        text.extend(["\n", ALLOC, "\n"]);
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TYPED: &str = "{\n    let _: ::alloc::vec::Vec<u8, ::alloc::alloc::Global> = f();\n}";

    #[test]
    fn a_root_gets_the_feature_and_the_crate_and_each_line_keeps_its_place() {
        let root = "//! Docs.\nmod a;\n// last";
        let now = equipped(root, &["x", TYPED]).unwrap_or_default();
        assert_eq!(
            now,
            "#![feature(allocator_api)] //! Docs.\nmod a;\n// last\nextern crate alloc;\n"
        );
        for (at, line) in root.lines().enumerate() {
            assert!(now.lines().nth(at).is_some_and(|now| now.ends_with(line)));
        }
    }

    #[test]
    fn a_root_gets_only_what_it_lacks_and_code_that_names_neither_changes_nothing() {
        let declared = "#![feature(allocator_api)]\nextern crate alloc;\n";
        assert_eq!(equipped(declared, &[TYPED]), None);
        let std_only = "mod a;\n";
        let path = "::alloc::string::String::new()";
        assert_eq!(
            equipped(std_only, &[path]).as_deref(),
            Some("mod a;\n\nextern crate alloc;\n")
        );
        let featured = "#![feature(allocator_api)]\n";
        let now = equipped(featured, &[TYPED]);
        assert_eq!(
            now.as_deref(),
            Some("#![feature(allocator_api)]\n\nextern crate alloc;\n")
        );
        assert_eq!(
            equipped(std_only, &["::std::vec::Vec::new()", "x + 1"]),
            None
        );
        assert_eq!(equipped(std_only, &[]), None);
    }
}
