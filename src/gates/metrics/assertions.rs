//! Tests that assert how many items a result has rather than what they are. `found.len() == 1`
//! still passes when a bug corrupts the one item found.

use std::collections::BTreeMap;

use syn::parse::Parser;
use syn::visit::Visit;

use crate::gates::metrics::prodlines::{for_each_source, keyed, parse_rust};
use crate::run::baseline::{Keys, Series};
use crate::run::{Ctx, Gate, Group, Kind};

pub const GATE: Gate = Gate {
    name: "assertions",
    about: "tests asserting a length or a count rather than the values, per test",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::Ratchet {
        measure,
        keys: Keys::Items,
        unit: "length assertion(s)",
    },
};

/// Methods that answer "how many" and nothing about what.
const COUNTING: [&str; 3] = ["len", "count", "is_empty"];

/// One key per test, spelled `file#test`, so a finding names the test to fix.
fn measure(ctx: &Ctx) -> Result<Series, String> {
    Ok(keyed(for_each_source(ctx, &counted)?))
}

/// Each function in one source holding a count-only assertion, and how many it holds.
fn counted(src: &str) -> Result<Vec<(String, u64)>, String> {
    let file = parse_rust(src)?;
    let mut count = Count::default();
    count.visit_file(&file);
    Ok(count.found.into_iter().collect())
}

#[derive(Default)]
struct Count {
    /// The functions being walked, innermost last; an assertion is charged to the innermost.
    inside: Vec<String>,
    found: BTreeMap<String, u64>,
    /// How many enclosing items are test code; outside tests an assertion is an invariant.
    testing: usize,
    /// Each local of the function being walked, with the values the test passed as it made it.
    given: Given,
}

type Given = BTreeMap<String, Vec<String>>;

impl<'ast> Visit<'ast> for Count {
    /// Charges a count-only assertion in test code; a count outside an assertion is ordinary code.
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let Some(name) = node.path.segments.last() else {
            return;
        };
        let called = name.ident.to_string();
        if self.testing > 0 && counts(&called, node.tokens.clone(), &self.given) {
            self.charge();
        }
    }

    fn visit_local(&mut self, node: &'ast syn::Local) {
        if let (Some(name), Some(init)) = (bound_name(&node.pat), &node.init) {
            self.given.insert(name, passed(&init.expr));
        }
        syn::visit::visit_local(self, node);
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let test = node
            .attrs
            .iter()
            .any(crate::gates::metrics::prodlines::is_test_gate);
        self.testing += usize::from(test);
        syn::visit::visit_item_mod(self, node);
        self.testing -= usize::from(test);
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.within(&node.attrs, node.sig.ident.to_string(), &|me| {
            syn::visit::visit_item_fn(me, node);
        });
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.within(&node.attrs, node.sig.ident.to_string(), &|me| {
            syn::visit::visit_impl_item_fn(me, node);
        });
    }
}

impl Count {
    fn charge(&mut self) {
        let item = self.inside.last().cloned().unwrap_or_default();
        let held = self.found.entry(item).or_insert(0);
        *held = held.saturating_add(1);
    }

    /// Walks one function under its own name, as test code when its attributes say it is.
    fn within(&mut self, attrs: &[syn::Attribute], name: String, walk: &dyn Fn(&mut Self)) {
        let test = usize::from(is_test(attrs));
        self.testing += test;
        self.inside.push(name);
        let outer = std::mem::take(&mut self.given);
        walk(self);
        self.given = outer;
        self.inside.pop();
        self.testing -= test;
    }
}

/// The values passed to every call in the chain that makes a local: `Cache::new(2)`, and
/// `with_capacity(CAP)` inside `Pool::builder().with_capacity(CAP).build()`.
fn passed(expr: &syn::Expr) -> Vec<String> {
    match expr {
        syn::Expr::Call(call) => call.args.iter().filter_map(plain_value).collect(),
        syn::Expr::MethodCall(call) => {
            let mut found = passed(&call.receiver);
            found.extend(call.args.iter().filter_map(plain_value));
            found
        }
        syn::Expr::Try(inner) => passed(&inner.expr),
        syn::Expr::Paren(inner) => passed(&inner.expr),
        _ => Vec::new(),
    }
}

fn bound_name(pat: &syn::Pat) -> Option<String> {
    match pat {
        syn::Pat::Ident(bound) => Some(bound.ident.to_string()),
        syn::Pat::Type(typed) => bound_name(&typed.pat),
        _ => None,
    }
}

/// An integer or a bare name, the two ways a test states a size.
fn plain_value(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(int),
            ..
        }) => Some(int.base10_digits().to_string()),
        syn::Expr::Path(path) if path.qself.is_none() => {
            path.path.get_ident().map(ToString::to_string)
        }
        _ => None,
    }
}

/// Whether `count` checks a size the test itself passed in, as in `Cache::new(2)` then
/// `cache.len() == 2`. That is the claim, not a proxy for one.
fn sized_by_setup(count: &syn::Expr, expected: &syn::Expr, given: &Given) -> bool {
    let receiver = match count {
        syn::Expr::MethodCall(call) if COUNTING.contains(&call.method.to_string().as_str()) => {
            plain_value(&call.receiver)
        }
        _ => None,
    };
    receiver
        .and_then(|name| given.get(&name))
        .zip(plain_value(expected))
        .is_some_and(|(passed, expected)| passed.contains(&expected))
}

/// `sized_by_setup` over an assertion's two operands in either order, or the two sides of an `==`.
fn set_up_size(compared: &[syn::Expr], given: &Given) -> bool {
    let pair = match compared {
        [left, right] => Some((left, right)),
        [syn::Expr::Binary(eq)] if matches!(eq.op, syn::BinOp::Eq(_)) => {
            Some((eq.left.as_ref(), eq.right.as_ref()))
        }
        _ => None,
    };
    pair.is_some_and(|(left, right)| {
        sized_by_setup(left, right, given) || sized_by_setup(right, left, given)
    })
}

/// A test attribute, `#[test]` or `#[tokio::test]`, or a `cfg` that builds the item only for tests.
fn is_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        crate::gates::metrics::prodlines::is_test_gate(attr)
            || attr
                .path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "test")
    })
}

/// Whether an assertion macro compares a count. Only the compared values are read, never the panic
/// message, and a size the test set up is excused.
fn counts(macro_name: &str, tokens: proc_macro2::TokenStream, given: &Given) -> bool {
    let name = macro_name.strip_prefix("debug_").unwrap_or(macro_name);
    assertion_kind(name).is_some_and(|(arity, judge)| {
        (|input: syn::parse::ParseStream| assertion_args(input, arity))
            .parse2(tokens)
            .is_ok_and(|compared| compared.iter().any(judge) && !set_up_size(&compared, given))
    })
}

type ExprPredicate = fn(&syn::Expr) -> bool;

fn assertion_kind(name: &str) -> Option<(usize, ExprPredicate)> {
    match name {
        "assert" => Some((1, count_equality)),
        "assert_eq" | "assert_ne" => Some((2, count_value)),
        _ => None,
    }
}

fn assertion_args(input: syn::parse::ParseStream, arity: usize) -> syn::Result<Vec<syn::Expr>> {
    let mut compared = vec![input.parse()?];
    if arity == 2 {
        input.parse::<syn::Token![,]>()?;
        compared.push(input.parse()?);
    }
    let _: proc_macro2::TokenStream = input.parse()?;
    Ok(compared)
}

fn count_value(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Paren(expr) => count_value(&expr.expr),
        syn::Expr::Group(expr) => count_value(&expr.expr),
        syn::Expr::Cast(expr) => count_value(&expr.expr),
        syn::Expr::MethodCall(call) => {
            call.args.is_empty() && COUNTING.contains(&call.method.to_string().as_str())
        }
        syn::Expr::Binary(expr) => count_value(&expr.left) || count_value(&expr.right),
        _ => false,
    }
}

fn count_equality(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Paren(expr) => count_equality(&expr.expr),
        syn::Expr::Group(expr) => count_equality(&expr.expr),
        syn::Expr::Binary(expr) if matches!(expr.op, syn::BinOp::Eq(_) | syn::BinOp::Ne(_)) => {
            count_value(&expr.left) || count_value(&expr.right)
        }
        syn::Expr::Binary(expr)
            if matches!(
                expr.op,
                syn::BinOp::And(_)
                    | syn::BinOp::Or(_)
                    | syn::BinOp::BitAnd(_)
                    | syn::BinOp::BitOr(_)
                    | syn::BinOp::BitXor(_)
            ) =>
        {
            count_equality(&expr.left) || count_equality(&expr.right)
        }
        _ => false,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    fn found(src: &str) -> u64 {
        counted(&in_tests(src))
            .unwrap()
            .iter()
            .map(|(_, n)| n)
            .sum()
    }

    /// Wraps `src` in a `#[cfg(test)]` module, where assertions are charged.
    fn in_tests(src: &str) -> String {
        format!("#[cfg(test)]\nmod tests {{\n{src}}}\n")
    }

    #[test]
    fn an_assertion_about_how_many_is_counted() {
        assert_eq!(found("fn t() { assert_eq!(v.len(), 1); }\n"), 1);
        assert_eq!(found("fn t() { assert_eq!(1, v.len()); }\n"), 1);
        assert_eq!(found("fn t() { assert_eq!(v.iter().count(), 3); }\n"), 1);
    }

    #[test]
    fn bounds_content_checks_and_panic_messages_are_not_count_only_proxies() {
        let code = r#"fn t() {
            assert!(result.len() > 0 && result.len() < 1024 * 1024);
            assert!(cache.len() <= 4096);
            assert_eq!(result, expected, "count {}", result.len());
            assert!(result == expected, "count {}", result.iter().count());
            assert!(result.contains(&wanted));
            assert_eq!(". len ()", ". count ()");
        }"#;
        assert_eq!(found(code), 0);
        assert_eq!(found("fn t() { assert!(ready && v.len() == 1); }"), 1);
        assert_eq!(
            found("fn t() { assert!((v.len() as u64) != 2 || ready); }"),
            1
        );
        assert_eq!(found("fn t() { assert_eq!(v.len() + 1, 4); }"), 1);
        assert_eq!(found("fn t() { assert_custom!(v.len()); }"), 0);
    }

    #[test]
    fn count_values_require_counting_methods_but_preserve_casts_and_arithmetic() {
        for (source, expected) in [
            ("v.len()", true),
            ("(v.len())", true),
            ("v.len() as u64", true),
            ("v.len() + 1", true),
            ("1 + v.iter().count()", true),
            ("v.is_empty()", true),
            ("42", false),
            ("(expected)", false),
            ("expected as u64", false),
            ("1 + 2", false),
            ("v.capacity()", false),
            ("v.len(context)", false),
            ("v.length()", false),
            ("v.len", false),
            ("len(v)", false),
        ] {
            let expr = syn::parse_str(source).unwrap();
            assert_eq!(count_value(&expr), expected, "{source}");
        }
    }

    #[test]
    fn count_equalities_distinguish_both_operands_from_bounds_and_content() {
        for (source, expected) in [
            ("v.len() == 2", true),
            ("2 != v.len()", true),
            ("(ready && v.len() == 2)", true),
            ("(v.len() == 2) || ready", true),
            ("ready && (steady || v.len() == 2)", true),
            ("(v.len() == 2) & ready", true),
            ("ready | (v.len() == 2)", true),
            ("(v.len() != 2) ^ ready", true),
            ("(v.len() <= 4096) & ready", false),
            ("ready | steady", false),
            ("ready ^ steady", false),
            ("(v.len() == 2) < ready", false),
            ("v.len() > 0", false),
            ("cache.len() <= 4096", false),
            ("v.len() > 0 && v.len() < 1024", false),
            ("result == expected", false),
            ("ready || steady", false),
            ("v.is_empty()", false),
        ] {
            let expr = syn::parse_str(source).unwrap();
            assert_eq!(count_equality(&expr), expected, "{source}");
        }
    }

    #[test]
    fn assertion_arguments_keep_compared_values_not_panic_message_tokens() {
        for (arity, source, expected) in [
            (1, "ready", vec!["ready"]),
            (1, "ready,", vec!["ready"]),
            (1, r#"ready, "count {}", v.len()"#, vec!["ready"]),
            (2, "actual, expected,", vec!["actual", "expected"]),
            (
                2,
                r#"actual, expected, "count {}", v.iter().count()"#,
                vec!["actual", "expected"],
            ),
        ] {
            let compared = (|input: syn::parse::ParseStream| assertion_args(input, arity))
                .parse2(source.parse().unwrap())
                .unwrap();
            let names = compared
                .into_iter()
                .map(|expr| match expr {
                    syn::Expr::Path(path) => path.path.get_ident().map(ToString::to_string),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>();
            assert_eq!(
                names,
                Some(expected.into_iter().map(String::from).collect()),
                "{source}"
            );
        }
    }

    #[test]
    fn malformed_assertion_arguments_are_not_invented_comparisons() {
        for source in ["", "v.len() 2", "v.len(),"] {
            let tokens: proc_macro2::TokenStream = source.parse().unwrap();
            assert!(
                (|input: syn::parse::ParseStream| assertion_args(input, 2))
                    .parse2(tokens.clone())
                    .is_err(),
                "{source}"
            );
            assert!(!counts("assert_eq", tokens, &Given::new()));
        }
        assert!(!counts("assert", "".parse().unwrap(), &Given::new()));
        assert_eq!(found("fn t() { assert!((v.len() == 2)); }"), 1);
        assert_eq!(found("fn t() { assert_eq!((v.len()), 2); }"), 1);
        assert_eq!(found("fn t() { assert_eq!(v.len(context), 2); }"), 0);
    }

    #[test]
    fn a_size_the_test_set_up_is_a_claim_and_one_it_did_not_is_a_count() {
        let evicts = "fn t() { let mut cache = Cache::new(2); cache.put(3); \
                      assert_eq!(cache.len(), 2); assert!(cache.len() == 2); }";
        assert_eq!(found(evicts), 0);
        let named = "fn t() { let pool: Pool = Pool::builder().limit(CAP).build(); \
                     assert_eq!(CAP, pool.len()); }";
        assert_eq!(found(named), 0);
        let fallible = "fn t() { let c = (Cache::open(4)?); assert_eq!(c.len(), 4); }";
        assert_eq!(found(fallible), 0, "through `?` and parentheses");
        let pair = "fn t() { let (a, b) = split(2); assert_eq!(a.len(), 2); }";
        assert_eq!(found(pair), 1, "a destructured local names no container");
        let proxy = "fn t() { let found = scan(2); assert_eq!(found.len(), 1); }";
        assert_eq!(found(proxy), 1);
        let literal = "fn t() { let v = vec![1, 2]; assert_eq!(v.len(), 2); }";
        assert_eq!(found(literal), 1, "an element is not a size the test set");
        let elsewhere = "fn t() { let a = Cache::new(2); let b = load(); assert_eq!(b.len(), 2); }";
        assert_eq!(found(elsewhere), 1, "the size was given to another local");
        let scoped = "fn t() { let c = Cache::new(2); } fn u() { assert_eq!(c.len(), 2); }";
        assert_eq!(found(scoped), 1, "a local does not outlive its function");
    }

    #[test]
    fn invisible_macro_groups_preserve_the_expression_being_compared() {
        for (source, value, equality) in [
            ("v.len()", true, false),
            ("(v.len() == 2)", true, true),
            ("ready", false, false),
            ("v.len() < 2", true, false),
        ] {
            let mut expr = syn::parse_str(source).unwrap();
            for _ in 0..3 {
                expr = syn::Expr::Group(syn::ExprGroup {
                    attrs: Vec::new(),
                    group_token: Default::default(),
                    expr: Box::new(expr),
                });
                assert_eq!(count_value(&expr), value, "{source}");
                assert_eq!(count_equality(&expr), equality, "{source}");
            }
        }
    }

    #[test]
    fn a_macro_without_a_name_cannot_be_mistaken_for_an_assertion() {
        let mut node: syn::Macro = syn::parse_str("assert_eq!(v.len(), 2)").unwrap();
        let mut count = Count {
            inside: vec!["example".to_string()],
            found: BTreeMap::new(),
            testing: 1,
            given: Given::new(),
        };
        count.visit_macro(&node);
        assert_eq!(count.found, BTreeMap::from([("example".to_string(), 1)]));
        count.found.clear();
        node.path.segments.clear();
        count.visit_macro(&node);
        assert_eq!(count.found, BTreeMap::new());
    }

    /// Calls the visitor directly so mutation testing reaches it.
    #[test]
    fn the_macro_visitor_charges_nothing_outside_test_code() {
        let node: syn::Macro = syn::parse_str("assert_eq!(v.len(), 2)").unwrap();
        let mut shipped = Count {
            inside: vec!["finalize".to_string()],
            found: BTreeMap::new(),
            testing: 0,
            given: Given::new(),
        };
        shipped.visit_macro(&node);
        assert_eq!(shipped.found, BTreeMap::new());
        let content: syn::Macro = syn::parse_str("assert_eq!(v, [1])").unwrap();
        let mut testing = Count {
            inside: vec!["t".to_string()],
            found: BTreeMap::new(),
            testing: 1,
            given: Given::new(),
        };
        testing.visit_macro(&content);
        assert_eq!(testing.found, BTreeMap::new());
    }

    #[test]
    fn an_assertion_about_the_values_is_not_counted() {
        assert_eq!(found("fn t() { assert_eq!(v, [\"a\", \"b\"]); }\n"), 0);
        assert_eq!(found("fn t() { assert_eq!(v, Vec::<u8>::new()); }\n"), 0);
    }

    #[test]
    fn a_comma_inside_the_call_being_asserted_is_not_a_comparison() {
        assert_eq!(
            found("fn t() { assert!(polite(\"git\", true).is_empty()); }"),
            0
        );
        // Later `assert!` arguments are the panic message.
        assert_eq!(found("fn t() { assert!(v.is_empty(), \"{v:?}\"); }"), 0);
        // A comparison against emptiness, or against a count, is still a length claim.
        assert_eq!(found("fn t() { assert_eq!(f(a, b).is_empty(), true); }"), 1);
        assert_eq!(found("fn t() { assert_eq!(v.len(), 1); }"), 1);
        assert_eq!(found("fn t() { assert!(v.len() == 1); }"), 1);
    }

    /// `is_empty()` alone is a full claim; comparing it to a value is still a count.
    #[test]
    fn asserting_a_collection_is_empty_is_a_claim_about_its_contents() {
        assert_eq!(found("fn t() { assert!(v.is_empty()); }\n"), 0);
        assert_eq!(found("fn t() { assert_eq!(v.is_empty(), false); }\n"), 1);
    }

    #[test]
    fn counting_outside_an_assertion_is_ordinary_code() {
        assert_eq!(found("fn f() { if v.len() == 1 { g(); } }\n"), 0);
        assert_eq!(found("fn f() { let n = v.len(); }\n"), 0);
    }

    #[test]
    fn every_assertion_macro_is_read_not_only_assert_eq() {
        assert_eq!(found("fn t() { assert_ne!(v.len(), 0); }\n"), 1);
        assert_eq!(found("fn t() { assert!(v.len() > 2); }\n"), 0);
        assert_eq!(found("fn t() { debug_assert_eq!(v.len(), 2); }\n"), 1);
    }

    #[test]
    fn a_length_assertion_is_charged_to_the_test_it_is_written_in() {
        let src = "#[test]\nfn a_name_is_the_key() { assert_eq!(v.len(), 1); }\n";
        assert_eq!(
            counted(src).unwrap(),
            vec![("a_name_is_the_key".to_string(), 1)]
        );
    }

    #[test]
    fn a_method_on_an_impl_is_named_the_same_way_a_free_function_is() {
        let src = in_tests("impl T { fn reads(&self) { assert_eq!(v.len(), 2); } }\n");
        assert_eq!(counted(&src).unwrap(), vec![("reads".to_string(), 1)]);
    }

    #[test]
    fn two_functions_of_one_name_in_a_file_are_charged_to_one_key_together() {
        let src = in_tests(
            "mod a { fn t() { assert_eq!(v.len(), 1); } }\nmod b { fn t() { assert!(v.len() == 2); } }\n",
        );
        assert_eq!(counted(&src).unwrap(), vec![("t".to_string(), 2)]);
    }

    #[test]
    fn an_assertion_in_a_nested_helper_is_charged_to_the_helper() {
        let src = in_tests("fn outer() { fn inner() { assert_eq!(v.len(), 1); } inner(); }\n");
        assert_eq!(counted(&src).unwrap(), vec![("inner".to_string(), 1)]);
    }

    #[test]
    fn a_production_invariant_is_not_a_test_asserting_a_length() {
        let src = "fn finalize(buf: &[u8]) { debug_assert_eq!(buf.len(), HEADER_SIZE); \
                   assert!(buf.len() == HEADER_SIZE); }\n";
        assert_eq!(counted(src).unwrap(), Vec::<(String, u64)>::new());
    }

    #[test]
    fn a_test_attribute_or_a_test_cfg_makes_its_assertions_count() {
        let src = "#[tokio::test]\nasync fn a() { assert_eq!(v.len(), 1); }\n\
                   #[cfg(test)]\nfn helper() { debug_assert_eq!(v.len(), 2); }\n\
                   fn shipped() { assert_eq!(v.len(), 3); }\n";
        assert_eq!(
            counted(src).unwrap(),
            vec![("a".to_string(), 1), ("helper".to_string(), 1)]
        );
    }

    #[test]
    fn a_file_with_no_length_assertion_yields_no_key_at_all() {
        let read = counted("fn t() { assert_eq!(v, [1]); }\n").unwrap();
        assert_eq!(read, Vec::<(String, u64)>::new());
    }

    #[test]
    fn a_key_names_the_file_and_the_test_together() {
        let read = vec![
            ("src/a.rs".to_string(), vec![("t".to_string(), 2)]),
            ("src/b.rs".to_string(), vec![("t".to_string(), 1)]),
        ];
        let series = keyed(read);
        assert_eq!(series.get("src/a.rs#t"), Some(2));
        assert_eq!(series.get("src/b.rs#t"), Some(1));
    }

    #[test]
    fn a_walk_that_read_no_length_assertion_anywhere_records_nothing() {
        let read = vec![("src/a.rs".to_string(), Vec::<(String, u64)>::new())];
        assert_eq!(keyed(read), Series::new());
    }

    #[test]
    fn a_file_the_parser_rejects_stops_the_assertions_gate() {
        assert!(counted("fn f( {\n").is_err());
    }
}
