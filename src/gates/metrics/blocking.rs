//! Blocking `std::fs`, `std::thread::sleep` and `std::net` calls inside an `async fn`, which stall
//! the executor's thread; skipped inside a wrapper such as `spawn_blocking` that offloads it.

use syn::Expr;
use syn::spanned::Spanned;
use syn::visit::Visit;

use crate::gates::rules::{is_test, line_of};
use crate::run::baseline::Keys;
use crate::run::{Ctx, Gate, Group, Kind, Measurement};

use super::prodlines::{IntoHits, for_each_source, tallied, visited};

/// The `blocking-async` gate: a blocking filesystem, sleep or network call inside an async fn.
pub const GATE: Gate = Gate {
    name: "blocking-async",
    about: "a blocking `std::fs`, `std::thread::sleep` or `std::net` call inside an `async fn`",
    group: Group::Quality,
    builds: false,
    reads: None,
    kind: Kind::AnnotatedRatchet {
        measure,
        keys: Keys::Items,
        unit: "call(s)",
    },
};

fn measure(ctx: &Ctx) -> Result<Measurement, String> {
    let read = for_each_source(ctx, &|src| hits(src))?;
    let (series, details) = tallied(read, "call");
    Ok(Measurement::detailed(series, details))
}

const BLOCKING_FS_FUNCTIONS: [&str; 14] = [
    "read_to_string",
    "read",
    "write",
    "create_dir",
    "create_dir_all",
    "remove_file",
    "remove_dir",
    "remove_dir_all",
    "rename",
    "copy",
    "metadata",
    "read_dir",
    "canonicalize",
    "read_link",
];

const BLOCKING_NET_TYPES: [&str; 3] = ["TcpStream", "TcpListener", "UdpSocket"];

/// Functions that correctly offload a blocking call to its own thread; a call inside one of
/// these is not flagged.
const WRAPPERS: [&str; 3] = ["asyncify", "spawn_blocking", "block_in_place"];

/// Every blocking call inside an async fn, in a file's production code, by pattern and line.
fn hits(src: &str) -> Result<Vec<(&'static str, u32)>, String> {
    visited(
        src,
        Blocking {
            asyncs: 0,
            wrappers: 0,
            hits: Vec::new(),
        },
    )
}

struct Blocking {
    /// How many `async fn` ancestors this point in the walk sits inside; a sync fn nested inside
    /// one does not leave it, since an ancestor further up still is one.
    asyncs: u32,
    /// How many wrapper calls, such as `spawn_blocking`, this point sits inside the arguments of.
    wrappers: u32,
    hits: Vec<(&'static str, u32)>,
}

impl IntoHits for Blocking {
    fn into_hits(self) -> Vec<(&'static str, u32)> {
        self.hits
    }
}

impl Blocking {
    fn within_async(&mut self, body: impl FnOnce(&mut Self)) {
        self.asyncs += 1;
        body(self);
        self.asyncs -= 1;
    }

    fn within_wrapper(&mut self, body: impl FnOnce(&mut Self)) {
        self.wrappers += 1;
        body(self);
        self.wrappers -= 1;
    }

    fn fn_item(
        &mut self,
        attrs: &[syn::Attribute],
        asyncness: Option<&syn::token::Async>,
        body: impl FnOnce(&mut Self),
    ) {
        if is_test(attrs) {
            return;
        }
        if asyncness.is_some() {
            self.within_async(body);
        } else {
            body(self);
        }
    }
}

impl<'ast> Visit<'ast> for Blocking {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.fn_item(&node.attrs, node.sig.asyncness.as_ref(), |me| {
            syn::visit::visit_item_fn(me, node);
        });
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.fn_item(&node.attrs, node.sig.asyncness.as_ref(), |me| {
            syn::visit::visit_impl_item_fn(me, node);
        });
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        if node.default.is_some() {
            self.fn_item(&node.attrs, node.sig.asyncness.as_ref(), |me| {
                syn::visit::visit_trait_item_fn(me, node);
            });
        }
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if is_wrapper_call(node) {
            self.within_wrapper(|me| syn::visit::visit_expr_call(me, node));
            return;
        }
        if self.asyncs > 0
            && self.wrappers == 0
            && let Some(pattern) = path_of(node).and_then(|parts| classify(&parts))
        {
            self.hits.push((pattern, line_of(node.func.span())));
        }
        syn::visit::visit_expr_call(self, node);
    }
}

fn is_wrapper_call(node: &syn::ExprCall) -> bool {
    let Expr::Path(path) = &*node.func else {
        return false;
    };
    path.path
        .segments
        .last()
        .is_some_and(|last| WRAPPERS.contains(&last.ident.to_string().as_str()))
}

fn path_of(call: &syn::ExprCall) -> Option<Vec<String>> {
    match &*call.func {
        Expr::Path(path) if path.qself.is_none() => Some(
            path.path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect(),
        ),
        _ => None,
    }
}

fn classify(parts: &[String]) -> Option<&'static str> {
    if is_blocking_fs(parts) {
        Some("fs-in-async")
    } else if is_blocking_sleep(parts) {
        Some("sleep-in-async")
    } else if is_blocking_net(parts) {
        Some("net-in-async")
    } else {
        None
    }
}

fn is_blocking_fs(parts: &[String]) -> bool {
    match parts {
        [a, b, c] => a == "std" && b == "fs" && BLOCKING_FS_FUNCTIONS.contains(&c.as_str()),
        [a, b] => a == "fs" && BLOCKING_FS_FUNCTIONS.contains(&b.as_str()),
        _ => false,
    }
}

fn is_blocking_sleep(parts: &[String]) -> bool {
    match parts {
        [a, b, c] => a == "std" && b == "thread" && c == "sleep",
        [a, b] => a == "thread" && b == "sleep",
        _ => false,
    }
}

fn is_blocking_net(parts: &[String]) -> bool {
    match parts {
        [a, b, c, ..] if a == "std" => b == "net" && BLOCKING_NET_TYPES.contains(&c.as_str()),
        [a, b, ..] if a == "net" => BLOCKING_NET_TYPES.contains(&b.as_str()),
        [a, ..] => BLOCKING_NET_TYPES.contains(&a.as_str()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(src: &str, pattern: &str) -> Vec<u32> {
        hits(src)
            .unwrap()
            .into_iter()
            .filter(|(found, _)| *found == pattern)
            .map(|(_, line)| line)
            .collect()
    }

    #[test]
    fn a_blocking_fs_call_in_an_async_fn_is_caught_fully_qualified_or_short() {
        let full = "async fn f() {\n    std::fs::read_to_string(\"x\").unwrap();\n}\n";
        assert_eq!(lines(full, "fs-in-async"), [2]);
        let short = "use std::fs;\nasync fn f() {\n    fs::read_to_string(\"x\").unwrap();\n}\n";
        assert_eq!(lines(short, "fs-in-async"), [3]);
    }

    #[test]
    fn a_bare_fs_function_brought_in_by_name_is_not_caught() {
        let src = "use std::fs::read_to_string;\nasync fn f() {\n    read_to_string(\"x\").unwrap();\n}\n";
        assert_eq!(lines(src, "fs-in-async"), Vec::<u32>::new());
    }

    #[test]
    fn a_blocking_sleep_in_an_async_fn_is_caught() {
        let src = "async fn f() {\n    std::thread::sleep(std::time::Duration::ZERO);\n}\n";
        assert_eq!(lines(src, "sleep-in-async"), [2]);
    }

    #[test]
    fn a_blocking_net_call_is_caught_qualified_or_by_its_bare_type() {
        let qualified = "async fn f() {\n    std::net::TcpStream::connect(\"a\").unwrap();\n}\n";
        assert_eq!(lines(qualified, "net-in-async"), [2]);
        let bare = "use std::net::TcpStream;\nasync fn f() {\n    TcpStream::connect(\"a\").unwrap();\n}\n";
        assert_eq!(lines(bare, "net-in-async"), [3]);
    }

    #[test]
    fn a_path_of_no_segments_is_never_a_blocking_net_type() {
        assert!(!is_blocking_net(&[]));
    }

    fn parts(segments: &[&str]) -> Vec<String> {
        segments.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn is_blocking_net_classifies_every_arm_by_its_prefix_and_length() {
        assert!(is_blocking_net(&parts(&["std", "net", "TcpStream"])));
        assert!(!is_blocking_net(&parts(&["std", "io", "TcpStream"])));
        assert!(is_blocking_net(&parts(&["net", "TcpStream"])));
        assert!(!is_blocking_net(&parts(&["other", "TcpStream"])));
        assert!(is_blocking_net(&parts(&["TcpStream"])));
        assert!(!is_blocking_net(&parts(&["Other"])));
    }

    #[test]
    fn a_call_whose_callee_is_not_a_path_is_not_classified() {
        let src = "async fn f(obj: &Obj) {\n    (obj.callback)();\n}\n";
        assert_eq!(hits(src).unwrap(), Vec::<(&str, u32)>::new());
    }

    #[test]
    fn a_blocking_call_outside_any_async_fn_is_not_caught() {
        let src = "fn f() {\n    std::fs::read_to_string(\"x\").unwrap();\n}\n";
        assert_eq!(hits(src).unwrap(), Vec::<(&str, u32)>::new());
    }

    #[test]
    fn a_blocking_call_wrapped_in_spawn_blocking_is_not_caught() {
        let src = "async fn f() {\n    spawn_blocking(|| std::fs::read_to_string(\"x\")).await.unwrap();\n}\n";
        assert_eq!(hits(src).unwrap(), Vec::<(&str, u32)>::new());
    }

    #[test]
    fn a_sync_function_nested_inside_an_async_one_does_not_leave_the_async_context() {
        let src = "async fn f() {\n    fn helper() {\n        std::fs::read(\"x\").unwrap();\n    }\n    helper();\n}\n";
        assert_eq!(lines(src, "fs-in-async"), [3]);
    }

    #[test]
    fn a_call_in_a_test_async_fn_is_not_counted() {
        let src = "#[test]\nasync fn t() {\n    std::fs::read(\"x\").unwrap();\n}\n";
        assert_eq!(hits(src).unwrap(), Vec::<(&str, u32)>::new());
    }

    #[test]
    fn a_call_in_a_test_async_impl_method_is_not_counted() {
        let src = "struct S;\nimpl S {\n    #[test]\n    async fn t(&self) {\n        std::fs::read(\"x\").unwrap();\n    }\n}\n";
        assert_eq!(hits(src).unwrap(), Vec::<(&str, u32)>::new());
    }

    #[test]
    fn a_blocking_call_in_an_async_impl_method_is_caught() {
        let src = "struct S;\nimpl S {\n    async fn f(&self) {\n        std::fs::read(\"x\").unwrap();\n    }\n}\n";
        assert_eq!(lines(src, "fs-in-async"), [4]);
    }

    #[test]
    fn a_blocking_call_in_a_trait_default_method_is_caught() {
        let src = "trait T {\n    async fn f(&self) {\n        std::fs::read(\"x\").unwrap();\n    }\n}\n";
        assert_eq!(lines(src, "fs-in-async"), [3]);
    }

    #[test]
    fn a_trait_method_with_no_default_body_holds_nothing() {
        let src = "trait T {\n    async fn f(&self);\n}\n";
        assert_eq!(hits(src).unwrap(), Vec::<(&str, u32)>::new());
    }

    #[test]
    fn a_tree_with_no_blocking_call_holds_nothing() {
        let src = "async fn f() -> u8 {\n    1 + 1\n}\n";
        assert_eq!(hits(src).unwrap(), Vec::<(&str, u32)>::new());
    }
}
