//! Each function with a body in a Rust file, named under what holds it, with a hash of its text.
use proc_macro2::LineColumn;
use syn::spanned::Spanned;
use syn::visit::Visit;

/// One function with a body: its name, where its item starts and ends, and the hash of its text.
pub(super) struct Function {
    pub(super) name: String,
    pub(super) start: LineColumn,
    pub(super) end: LineColumn,
    pub(super) hash: String,
}

/// Every function with a body in `text`, the file at `shown`.
pub(super) fn defined(shown: &str, text: &str) -> Vec<Function> {
    let Ok(file) = syn::parse_file(text) else {
        return Vec::new();
    };
    let mut named = Named {
        file: shown,
        text,
        path: Vec::new(),
        found: Vec::new(),
    };
    named.visit_file(&file);
    named.found
}

/// The SHA-256 of a function's text as the file holds it, from the start of the line of its `fn`
/// to its closing brace. Attributes and doc comments above that line are not in it.
fn body_hash(text: &str, fn_token: proc_macro2::Span, block: &syn::Block) -> String {
    let at = fn_token.byte_range().start;
    let first = text
        .get(..at)
        .and_then(|it| it.rfind('\n'))
        .map_or(0, |it| it + 1);
    let last = block.brace_token.span.close().byte_range().end;
    sha256(text.get(first..last).unwrap_or_default())
}

/// The SHA-256 of `text`, in 64 lowercase hex digits.
fn sha256(text: &str) -> String {
    crate::setup::sha256::of(text.as_bytes())
}

/// Collects each function under the modules, types and functions that hold it.
struct Named<'a> {
    file: &'a str,
    text: &'a str,
    path: Vec<String>,
    found: Vec<Function>,
}

impl Named<'_> {
    fn take(&mut self, item: proc_macro2::Span, sig: &syn::Signature, block: &syn::Block) {
        let parts = std::iter::once(self.file.to_string()).chain(self.path.iter().cloned());
        let name = parts.chain([sig.ident.to_string()]).collect::<Vec<_>>();
        self.found.push(Function {
            name: name.join("::"),
            start: item.start(),
            end: block.brace_token.span.close().end(),
            hash: body_hash(self.text, sig.fn_token.span, block),
        });
    }

    /// Visits what `visit` reaches under `name`.
    fn under(&mut self, name: String, visit: impl FnOnce(&mut Self)) {
        self.path.push(name);
        visit(self);
        self.path.pop();
    }

    /// The type an `impl` is for, by its last name: `Counter` for `impl<T> Show for a::Counter<T>`.
    fn type_name(&self, ty: &syn::Type) -> String {
        if let syn::Type::Path(path) = ty
            && let Some(last) = path.path.segments.last()
        {
            return last.ident.to_string();
        }
        let written = self.text.get(ty.span().byte_range()).unwrap_or_default();
        written.split_whitespace().collect()
    }
}

impl<'ast> Visit<'ast> for Named<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.under(node.ident.to_string(), |it| {
            syn::visit::visit_item_mod(it, node);
        });
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let name = self.type_name(&node.self_ty);
        self.under(name, |it| syn::visit::visit_item_impl(it, node));
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        self.under(node.ident.to_string(), |it| {
            syn::visit::visit_item_trait(it, node);
        });
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.take(node.span(), &node.sig, &node.block);
        self.under(node.sig.ident.to_string(), |it| {
            syn::visit::visit_item_fn(it, node);
        });
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.take(node.span(), &node.sig, &node.block);
        self.under(node.sig.ident.to_string(), |it| {
            syn::visit::visit_impl_item_fn(it, node);
        });
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        if let Some(block) = &node.default {
            self.take(node.span(), &node.sig, block);
        }
        self.under(node.sig.ident.to_string(), |it| {
            syn::visit::visit_trait_item_fn(it, node);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trait's default method is named under the trait, and an `impl` for a type that is no path
    /// by its text; a file that does not parse defines nothing. Mutant: skipping traits.
    #[test]
    fn a_trait_default_and_an_impl_for_a_written_type_are_named_too() {
        let text = "trait Show {\n    fn show(&self) -> bool { true }\n    fn bare(&self);\n}\n\n\
                    impl Show for [u8; 2] {\n    fn bare(&self) {}\n}\n";
        let found = defined("src/lib.rs", text).into_iter();
        let names: Vec<String> = found.map(|it| it.name).collect();
        assert_eq!(
            names,
            ["src/lib.rs::Show::show", "src/lib.rs::[u8;2]::bare"]
        );
        assert!(defined("src/lib.rs", "fn (").is_empty());
    }

    /// The hash is SHA-256 in 64 lowercase hex digits, as the standard's test vectors give it.
    /// Mutant: std's `DefaultHasher` gives 16 digits that no vector matches.
    #[test]
    fn a_body_hash_is_the_sha_256_of_the_text() {
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256("abc"), abc);
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(sha256(""), empty);
    }
}
