//! String literals too long for one line: code or data, such as minified JS or CSS, kept in a Rust
//! string. The fix is a file in its own language, read in with `include_str!`.

use proc_macro2::{Literal, TokenStream, TokenTree};
use syn::visit::Visit;

/// The most characters a string literal may hold on one source line.
pub(crate) const MOST: usize = 300;

/// What a string literal's text starts with before its quote, less any `#` of a raw one.
const PREFIXES: [&str; 6] = ["", "r", "b", "br", "c", "cr"];

/// The line of each string literal with more than `MOST` characters on one line, once a literal.
/// Doc comments are left out; a literal in a macro call or an attribute is read from its tokens.
#[must_use]
pub(crate) fn lines(file: &syn::File) -> Vec<u32> {
    let mut found = Literals { lines: Vec::new() };
    found.visit_file(file);
    found.lines
}

struct Literals {
    lines: Vec<u32>,
}

impl Literals {
    /// Notes `token` if it is a plain, raw, byte or C string with a line too long. Characters are
    /// counted, not bytes.
    fn literal(&mut self, token: &Literal) {
        let text = token.to_string();
        let (Some(open), Some(close)) = (text.find('"'), text.rfind('"')) else {
            return;
        };
        let prefix = text.get(..open).unwrap_or_default().trim_end_matches('#');
        if !PREFIXES.contains(&prefix) {
            return;
        }
        let inside = text.get(open.saturating_add(1)..close).unwrap_or_default();
        let Some(offset) = inside
            .split('\n')
            .position(|line| line.chars().count() > MOST)
        else {
            return;
        };
        let start = token.span().start().line.saturating_add(offset);
        self.lines.push(u32::try_from(start).unwrap_or(u32::MAX));
    }

    /// Each literal in a token stream and its groups. The text of a `doc = "..."` pair is a doc
    /// comment, which a macro's tokens carry that way.
    fn tokens(&mut self, stream: &TokenStream) {
        let mut doc = 0_u8;
        for tree in stream.clone() {
            let documents = doc == 2;
            doc = match &tree {
                TokenTree::Ident(ident) if ident == "doc" => 1,
                TokenTree::Punct(punct) if punct.as_char() == '=' && doc == 1 => 2,
                _ => 0,
            };
            match tree {
                TokenTree::Group(group) => self.tokens(&group.stream()),
                TokenTree::Literal(literal) if !documents => self.literal(&literal),
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Literals {
    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        // `///` and `//!` arrive as `#[doc = "..."]`.
        if !node.path().is_ident("doc") {
            syn::visit::visit_attribute(self, node);
        }
    }

    fn visit_meta_list(&mut self, node: &'ast syn::MetaList) {
        syn::visit::visit_meta_list(self, node);
        self.tokens(&node.tokens);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        syn::visit::visit_macro(self, node);
        self.tokens(&node.tokens);
    }

    fn visit_lit_str(&mut self, node: &'ast syn::LitStr) {
        self.literal(&node.token());
    }

    fn visit_lit_byte_str(&mut self, node: &'ast syn::LitByteStr) {
        self.literal(&node.token());
    }

    fn visit_lit_cstr(&mut self, node: &'ast syn::LitCStr) {
        self.literal(&node.token());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(source: &str) -> Vec<u32> {
        lines(&syn::parse_file(source).unwrap())
    }

    /// Contract: a literal of 301 characters on one line counts once, and one of 300 does not.
    /// Mutant: `> MOST` as `>= MOST` counts the 300 too.
    #[test]
    fn a_literal_one_character_over_the_most_counts_once_and_one_at_the_most_does_not() {
        let over = "a".repeat(MOST + 1);
        let most = "b".repeat(MOST);
        let source = format!("const A: &str = \"{over}\";\nconst B: &str = \"{most}\";\n");
        assert_eq!(found(&source), vec![1]);
    }

    #[test]
    fn a_number_or_a_character_in_a_macro_is_no_string() {
        let source = "made! {\n    1, b'\"', '\"'\n}\n";
        assert_eq!(found(source), Vec::<u32>::new());
    }

    #[test]
    fn raw_byte_and_c_strings_count_as_plain_ones_do() {
        let over = "a".repeat(MOST + 1);
        let source = format!(
            "const A: &str = r#\"{over}\"#;\nconst B: &[u8] = b\"{over}\";\n\
             const C: &[u8] = br\"{over}\";\nconst D: &CStr = c\"{over}\";\n"
        );
        assert_eq!(found(&source), vec![1, 2, 3, 4]);
    }

    #[test]
    fn a_literal_inside_a_macro_call_counts_once_however_deep() {
        let over = "a".repeat(MOST + 1);
        let source = format!("fn f() {{\n    let _ = vec![format!(\"{over}\")];\n}}\n");
        assert_eq!(found(&source), vec![2]);
    }

    #[test]
    fn a_doc_comment_never_counts_even_inside_a_macro() {
        let over = "a".repeat(MOST + 1);
        let source = format!(
            "//! {over}\n/// {over}\n#[doc = \"{over}\"]\nfn f() {{}}\n\
             made! {{\n    /// {over}\n    struct S;\n}}\n"
        );
        assert_eq!(found(&source), Vec::<u32>::new());
    }

    #[test]
    fn characters_are_counted_and_not_bytes() {
        let wide = "é".repeat(MOST);
        let over = "é".repeat(MOST + 1);
        let source = format!("const A: &str = \"{wide}\";\nconst B: &str = \"{over}\";\n");
        assert_eq!(found(&source), vec![2]);
    }

    #[test]
    fn a_literal_over_many_lines_counts_on_its_long_line_and_only_once() {
        let over = "a".repeat(MOST + 1);
        let short = "b".repeat(MOST);
        let source =
            format!("const A: &str = \"{short}\n{over}\n{over}\";\nconst B: &str = \"x\";\n");
        assert_eq!(found(&source), vec![2]);
    }

    #[test]
    fn a_literal_in_an_attribute_counts_but_a_number_or_a_character_never_does() {
        let over = "a".repeat(MOST + 1);
        let digits = "1".repeat(MOST + 1);
        let source = format!("#[deprecated(note = \"{over}\")]\nconst A: u128 = {digits};\n");
        assert_eq!(found(&source), vec![1]);
    }
}
