//! Shapes that span files: a module that only re-exports, a trait that one type implements, and
//! two structs with the same fields and a `From` between them. Each is an estimate for a person.

use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{TokenStream, TokenTree};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Fields, GenericArgument, Item, ItemImpl, Macro, PathArguments, Type, Visibility};

use super::report::Cut;
use crate::gates::metrics::prodlines;

/// A struct or a trait: its name, where it is, and a struct's field names in order of name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Named {
    name: String,
    file: String,
    line: u32,
    last: u32,
    fields: Vec<String>,
}

/// For each trait, what each impl is for: the name of one type, or `None` for an impl that
/// stands for many types, which is a generic one or one that a macro writes.
type Impls = BTreeMap<String, Vec<Option<String>>>;

/// What the production code of every file defines, and the types and the impls of all code,
/// test code included.
#[derive(Debug, Default)]
pub struct Seen {
    structs: Vec<Named>,
    traits: Vec<Named>,
    types: BTreeSet<String>,
    impls: Impls,
    conversions: Vec<(String, Named)>,
    reexports: Vec<String>,
}

fn lines_of(node: &impl Spanned) -> (u32, u32) {
    let span = node.span();
    let line = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
    (line(span.start().line), line(span.end().line))
}

/// The last name of a type's path, as `B` for `a::B<T>`.
fn named(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    Some(path.path.segments.last()?.ident.to_string())
}

/// The first type between the angle brackets of a path's last segment, as `A` for `From<A>`.
fn first_argument(path: &syn::Path) -> Option<String> {
    let PathArguments::AngleBracketed(given) = &path.segments.last()?.arguments else {
        return None;
    };
    given.args.iter().find_map(|it| match it {
        GenericArgument::Type(ty) => named(ty),
        _ => None,
    })
}

/// The name of the type that an item declares.
fn declared(item: &Item) -> Option<String> {
    let name = match item {
        Item::Struct(it) => &it.ident,
        Item::Enum(it) => &it.ident,
        Item::Union(it) => &it.ident,
        Item::Type(it) => &it.ident,
        _ => return None,
    };
    Some(name.to_string())
}

/// Each trait that tokens implement, as `Keyed` for `impl Keyed for $t`: the last name outside
/// angle brackets between an `impl` and its `for`. A group ends an `impl` that names no trait.
fn written(tokens: TokenStream, traits: &mut Vec<String>) {
    let (mut open, mut depth, mut name) = (false, 0_u32, None);
    for token in tokens {
        if let TokenTree::Group(group) = &token {
            written(group.stream(), traits);
            open &= depth > 0;
            continue;
        }
        let word = token.to_string();
        match word.as_str() {
            "impl" => (open, depth, name) = (true, 0, None),
            "<" => depth += 1,
            ">" => depth = depth.saturating_sub(1),
            "for" if open && depth == 0 => {
                traits.extend(name.take());
                open = false;
            }
            _ if open && depth == 0 && matches!(token, TokenTree::Ident(_)) => name = Some(word),
            _ => {}
        }
    }
}

/// Reads the impls that the macros of a file write, in a definition or in a call.
struct Macros<'a>(&'a mut Impls);

impl<'ast> Visit<'ast> for Macros<'_> {
    fn visit_macro(&mut self, node: &'ast Macro) {
        let mut traits = Vec::new();
        written(node.tokens.clone(), &mut traits);
        for name in traits {
            self.0.entry(name).or_default().push(None);
        }
    }
}

/// A struct's field names in order of name; a tuple struct has none.
fn field_names(fields: &Fields) -> Vec<String> {
    let mut names: Vec<String> = (fields.iter())
        .filter_map(|it| Some(it.ident.as_ref()?.to_string()))
        .collect();
    names.sort();
    names
}

impl Seen {
    /// Reads one file; `test` is a file of test code. A production file whose every item is a
    /// `pub use` is a re-export module, unless it is `lib.rs`, the crate's interface.
    pub fn read(&mut self, shown: &str, src: &str, test: bool) {
        let Ok(file) = prodlines::parse_rust(src) else {
            return;
        };
        let public = |item: &Item| match item {
            Item::Use(it) => !matches!(it.vis, Visibility::Inherited),
            _ => false,
        };
        let only_uses = !file.items.is_empty() && file.items.iter().all(public);
        if only_uses && !test && !shown.ends_with("lib.rs") {
            self.reexports.push(shown.to_string());
        }
        Macros(&mut self.impls).visit_file(&file);
        let gated = test || prodlines::is_test_gated(&file.attrs);
        self.items(&file.items, shown, gated);
    }

    fn items(&mut self, items: &[Item], shown: &str, test: bool) {
        for item in items {
            self.types.extend(declared(item));
            match item {
                Item::Struct(it) if !test => {
                    let fields = field_names(&it.fields);
                    (self.structs).push(place(&it.ident.to_string(), shown, it, fields));
                }
                Item::Trait(it) if !test => {
                    (self.traits).push(place(&it.ident.to_string(), shown, it, Vec::new()));
                }
                Item::Impl(it) => self.implemented(it, shown, test),
                Item::Mod(it) => {
                    let inner = it.content.as_ref().map(|(_, items)| items.as_slice());
                    let gated = test || prodlines::is_test_gated(&it.attrs);
                    self.items(inner.unwrap_or_default(), shown, gated);
                }
                _ => {}
            }
        }
    }

    /// Keeps what the trait's impl is for, which is no one type where the impl is generic, and
    /// keeps a production `impl From<A> for B`.
    fn implemented(&mut self, node: &ItemImpl, shown: &str, test: bool) {
        let path = node.trait_.as_ref().map(|(path, _)| path);
        let Some((path, last)) = path.and_then(|it| Some((it, it.segments.last()?))) else {
            return;
        };
        let one = named(&node.self_ty).filter(|_| node.generics.params.is_empty());
        let impls = self.impls.entry(last.ident.to_string()).or_default();
        impls.push(one);
        if last.ident == "From"
            && !test
            && let (Some(from), Some(into)) = (first_argument(path), named(&node.self_ty))
        {
            (self.conversions).push((from, place(&into, shown, node, Vec::new())));
        }
    }

    /// Each estimate with its file. `lines` gives a re-export module's length.
    pub fn cuts(&self, lines: &BTreeMap<String, u64>) -> Vec<(String, Cut)> {
        let mut cuts = Vec::new();
        for shown in &self.reexports {
            let whole = lines.get(shown).copied().unwrap_or(0);
            let fix = "name each item by the module that defines it, then remove this module";
            let last = u32::try_from(whole).unwrap_or(u32::MAX);
            cuts.push((
                shown.clone(),
                Cut::estimate("reexport_module", (1, last), whole, fix),
            ));
        }
        // A type of another crate takes no inherent `impl`, so its one trait is an extension.
        let sole = |it: &&Named| match self.impls.get(&it.name).map(Vec::as_slice) {
            Some([Some(ty)]) => self.types.contains(ty),
            _ => false,
        };
        for it in self.traits.iter().filter(sole) {
            let fix = format!(
                "one type implements `{0}`: move its methods into an inherent `impl`, then remove \
                 `{0}`",
                it.name
            );
            let whole = u64::from(it.last + 1 - it.line);
            let cut = Cut::estimate("single_impl_trait", (it.line, it.last), whole, &fix);
            cuts.push((it.file.clone(), cut));
        }
        cuts.extend(
            self.conversions
                .iter()
                .filter_map(|(from, by)| self.mirror(from, by)),
        );
        cuts
    }

    /// The struct a `From` builds, where it has the fields of the struct it is built from.
    fn mirror(&self, from: &str, by: &Named) -> Option<(String, Cut)> {
        let find = |name: &str| self.structs.iter().find(|it| it.name == name);
        let (source, copy) = (find(from)?, find(&by.name)?);
        if source.fields != copy.fields || copy.fields.len() < 2 || source.name == copy.name {
            return None;
        }
        let fix = format!(
            "`{0}` has the fields of `{1}`: use `{1}` where `{0}` is used, then remove `{0}` and \
             its `From`",
            copy.name, source.name
        );
        let whole = u64::from(copy.last + 1 - copy.line) + u64::from(by.last + 1 - by.line);
        let mut cut = Cut::estimate("mirror_type", (copy.line, copy.last), whole, &fix);
        cut.twin = Some(format!("{}:{}", source.file, source.line));
        Some((copy.file.clone(), cut))
    }
}

fn place(name: &str, shown: &str, node: &impl Spanned, fields: Vec<String>) -> Named {
    let (line, last) = lines_of(node);
    Named {
        name: name.to_string(),
        file: shown.to_string(),
        line,
        last,
        fields,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    /// The cuts of files given as (path, source), each file's length being its line count.
    fn cuts(files: &[(&str, &str)]) -> Vec<(String, Cut)> {
        let mut seen = Seen::default();
        let mut lines = BTreeMap::new();
        for (shown, src) in files {
            seen.read(shown, src, shown.starts_with("tests/"));
            lines.insert((*shown).to_string(), src.lines().count() as u64);
        }
        seen.cuts(&lines)
    }

    fn kinds(files: &[(&str, &str)]) -> Vec<(String, &'static str, u32, u32, u64)> {
        (cuts(files).into_iter())
            .map(|(file, cut)| (file, cut.kind, cut.line, cut.end_line, cut.removable_lines))
            .collect()
    }

    #[test]
    fn a_file_of_only_pub_use_lines_is_a_reexport_module_of_its_whole_length() {
        let found = kinds(&[(
            "src/names.rs",
            "pub use crate::a::A;\npub(crate) use crate::b::B;\n",
        )]);
        assert_eq!(
            found,
            [("src/names.rs".to_string(), "reexport_module", 1, 2, 2)]
        );
    }

    #[test]
    fn a_private_use_another_item_an_empty_file_or_a_lib_rs_is_not_a_reexport_module() {
        assert!(kinds(&[("src/a.rs", "pub use crate::a::A;\nuse crate::b::B;\n")]).is_empty());
        assert!(kinds(&[("src/a.rs", "pub use crate::a::A;\nfn f() {}\n")]).is_empty());
        assert!(kinds(&[("src/a.rs", "// nothing\n")]).is_empty());
        assert!(kinds(&[("src/lib.rs", "pub use crate::a::A;\n")]).is_empty());
        assert!(kinds(&[("src/a.rs", "pub use crate::a::A;\nfn broken( {\n")]).is_empty());
    }

    #[test]
    fn a_trait_one_type_implements_is_named_with_the_lines_of_its_declaration() {
        let shape = "\npub trait Shape {\n    fn area(&self) -> u32;\n}\n";
        let square = "struct Square;\nimpl crate::a::Shape for Square {\n}\n";
        let found = cuts(&[("src/a.rs", shape), ("src/b.rs", square)]);
        let files: Vec<&str> = found.iter().map(|(file, _)| file.as_str()).collect();
        assert_eq!(files, ["src/a.rs"]);
        let cut = &found[0].1;
        assert_eq!(
            (cut.kind, cut.line, cut.end_line, cut.removable_lines),
            ("single_impl_trait", 2, 4, 3)
        );
        assert_eq!(cut.evidence, "estimate");
        assert_eq!(
            cut.fix,
            "one type implements `Shape`: move its methods into an inherent `impl`, then remove \
             `Shape`"
        );
    }

    #[test]
    fn a_trait_with_no_impl_two_impls_a_generic_impl_or_a_test_impl_is_not_named() {
        let shape = "trait Shape {}\n";
        assert!(kinds(&[("src/a.rs", shape)]).is_empty());
        let two =
            "trait Shape {}\nstruct A;\nstruct B;\nimpl Shape for A {}\nimpl Shape for B {}\n";
        assert!(kinds(&[("src/a.rs", two)]).is_empty());
        let generic = "trait Shape {}\nstruct T;\nimpl<T: Clone> Shape for T {}\n";
        assert!(kinds(&[("src/a.rs", generic)]).is_empty());
        let mock = "trait Shape {}\nstruct A;\nimpl Shape for A {}\n#[cfg(test)]\nmod tests {\n    \
                    struct Mock;\n    impl super::Shape for Mock {}\n}\n";
        assert!(kinds(&[("src/a.rs", mock)]).is_empty());
    }

    /// One trait with one impl for a type of the tree, which `single_impl_trait` names at line 1.
    const SHAPE: &str = "trait Shape {}\nstruct A;\nimpl Shape for A {}\n";

    fn named_at_line_one(files: &[(&str, &str)]) -> bool {
        let one = ("src/a.rs".to_string(), "single_impl_trait", 1, 1, 1);
        kinds(files) == [one]
    }

    #[test]
    fn an_impl_in_a_file_of_test_code_is_an_impl_and_the_file_declares_nothing_to_remove() {
        assert!(named_at_line_one(&[("src/a.rs", SHAPE)]));
        let fake = "struct Fake;\nimpl crate::Shape for Fake {}\n";
        assert!(kinds(&[("src/a.rs", SHAPE), ("tests/fake.rs", fake)]).is_empty());
        let own = "pub trait Probe {}\npub struct One;\nimpl Probe for One {}\n";
        assert!(named_at_line_one(&[
            ("src/a.rs", SHAPE),
            ("tests/own.rs", own)
        ]));
        let uses = "pub use a::A;\n";
        assert!(named_at_line_one(&[
            ("src/a.rs", SHAPE),
            ("tests/uses.rs", uses)
        ]));
    }

    #[test]
    fn a_trait_whose_one_impl_is_for_a_type_of_another_crate_is_an_extension() {
        for foreign in [
            "clap::ArgMatches",
            "ArgMatches",
            "str",
            "[A]",
            "&A",
            "(A, A)",
        ] {
            let src = format!("trait Reader {{}}\nstruct A;\nimpl Reader for {foreign} {{}}\n");
            assert!(kinds(&[("src/a.rs", &src)]).is_empty(), "{foreign}");
        }
        for own in [
            "enum A {}",
            "union A { x: u8 }",
            "type A = u8;",
            "struct A<T>(T);",
        ] {
            let src = format!("trait Shape {{}}\n{own}\nimpl Shape for A {{}}\n");
            assert!(named_at_line_one(&[("src/a.rs", &src)]), "{own}");
        }
        let in_tests = "trait Shape {}\n#[cfg(test)]\nmod tests {\n    struct Fake;\n    \
                        impl super::Shape for Fake {}\n}\n";
        assert!(named_at_line_one(&[("src/a.rs", in_tests)]));
    }

    #[test]
    fn a_trait_that_a_macro_implements_stands_for_many_types() {
        let free = |with: &str| kinds(&[("src/a.rs", &format!("{SHAPE}{with}\n"))]).is_empty();
        assert!(free("macro_rules! m { ($t:ty) => {impl Shape for $t {}} }"));
        assert!(free("m! { impl<T> $crate::Shape<(T, u8)> for $t {} }"));
        assert!(free("m! { impl<F: for<'a> Fn(&'a u8)> Shape for B {} }"));
        assert!(free("fn derive() { quote! { impl Shape for #name {} }; }"));
        assert!(free("#[test]\nfn t() { m! { impl Shape for B {} }; }"));
    }

    #[test]
    fn a_macro_that_implements_another_trait_or_no_trait_leaves_the_trait_named() {
        let named = |with: &str| named_at_line_one(&[("src/a.rs", &format!("{SHAPE}{with}\n"))]);
        assert!(named("macro_rules! m { () => { impl Other for B {} } }"));
        assert!(named("m! { impl Wrap<Shape> for $t {} }"));
        assert!(named("m! { fn f(x: impl Shape) { for a in x {} } }"));
        assert!(named("m! { fn f() -> impl Shape { for a in b {} } }"));
        assert!(named("m! { impl A {} Shape for B }"));
        assert!(named("m! { Shape for B }"));
    }

    #[test]
    fn a_trait_declared_in_test_code_is_not_named() {
        let gated = "#[cfg(test)]\nmod tests {\n    trait Shape {}\n    impl Shape for A {}\n}\n";
        assert!(kinds(&[("src/a.rs", gated)]).is_empty());
        let whole = "#![cfg(test)]\ntrait Shape {}\nimpl Shape for A {}\n";
        assert!(kinds(&[("src/a.rs", whole)]).is_empty());
    }

    const POINT: &str = "pub struct Point {\n    pub x: u32,\n    pub y: u32,\n}\n";

    #[test]
    fn a_struct_built_from_one_with_the_same_fields_is_a_mirror_of_it() {
        let copy = "mod inner {\n    struct Dot {\n        y: u32,\n        x: u32,\n    }\n}\n\
                    impl From<crate::a::Point> for Dot {\n    fn from(p: Point) -> Self {\n        \
                    Dot { x: p.x, y: p.y }\n    }\n}\n";
        let found = cuts(&[("src/a.rs", POINT), ("src/b.rs", copy)]);
        let files: Vec<&str> = found.iter().map(|(file, _)| file.as_str()).collect();
        assert_eq!(files, ["src/b.rs"]);
        let cut = &found[0].1;
        assert_eq!(
            (cut.kind, cut.line, cut.end_line, cut.removable_lines),
            ("mirror_type", 2, 5, 9)
        );
        assert_eq!(cut.twin.as_deref(), Some("src/a.rs:1"));
        assert_eq!(
            cut.fix,
            "`Dot` has the fields of `Point`: use `Point` where `Dot` is used, then remove `Dot` \
             and its `From`"
        );
    }

    #[test]
    fn a_struct_with_other_fields_one_field_or_no_from_is_not_a_mirror() {
        let from = "impl From<Point> for Dot {}\n";
        let other = format!("struct Dot {{\n    x: u32,\n    z: u32,\n}}\n{from}");
        assert!(kinds(&[("src/a.rs", POINT), ("src/b.rs", &other)]).is_empty());
        let one = format!(
            "struct One {{ x: u32 }}\nstruct Dot {{ x: u32 }}\n{}",
            from.replace("Point", "One")
        );
        assert!(kinds(&[("src/b.rs", &one)]).is_empty());
        let none = "struct Dot {\n    x: u32,\n    y: u32,\n}\n";
        assert!(kinds(&[("src/a.rs", POINT), ("src/b.rs", none)]).is_empty());
        let tuple = format!("struct Dot(u32, u32);\n{from}");
        assert!(kinds(&[("src/a.rs", POINT), ("src/b.rs", &tuple)]).is_empty());
    }

    #[test]
    fn a_from_in_test_code_a_from_of_itself_or_a_from_of_a_reference_is_not_a_mirror() {
        let dot = "struct Dot {\n    x: u32,\n    y: u32,\n}\n";
        let gated =
            format!("{dot}#[cfg(test)]\nmod tests {{\n    impl From<Point> for Dot {{}}\n}}\n");
        assert!(kinds(&[("src/a.rs", POINT), ("src/b.rs", &gated)]).is_empty());
        let own = format!("{dot}impl From<Dot> for Dot {{}}\n");
        assert!(kinds(&[("src/b.rs", &own)]).is_empty());
        let borrowed = format!("{dot}impl From<&Point> for Dot {{}}\n");
        assert!(kinds(&[("src/a.rs", POINT), ("src/b.rs", &borrowed)]).is_empty());
        let lifetime = format!("{dot}impl From<Wrap<'static>> for Dot {{}}\n");
        assert!(kinds(&[("src/a.rs", POINT), ("src/b.rs", &lifetime)]).is_empty());
    }

    #[test]
    fn a_struct_declared_only_in_test_code_is_not_the_mirror_a_production_from_builds() {
        let built = "impl From<Point> for Dot {}\n#[cfg(test)]\nmod tests {\n    \
                     struct Dot {\n        x: u32,\n        y: u32,\n    }\n}\n";
        assert!(kinds(&[("src/a.rs", POINT), ("src/b.rs", built)]).is_empty());
    }

    #[test]
    fn a_from_that_names_no_type_builds_no_mirror_and_an_inherent_impl_is_no_trait_impl() {
        let dot = "struct Dot {\n    x: u32,\n    y: u32,\n}\n";
        for from in ["From", "From<'static>", "From<3>"] {
            let built = format!("{dot}impl {from} for Dot {{}}\n");
            let found = kinds(&[("src/a.rs", POINT), ("src/b.rs", &built)]);
            assert!(found.is_empty(), "{from}: {found:?}");
        }
        let inherent = format!("{SHAPE}impl A {{}}\n");
        assert!(named_at_line_one(&[("src/a.rs", &inherent)]));
    }
}
