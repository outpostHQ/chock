//! The modules a call to one of a file's own `macro_rules!` declares: a `mod $m;` in the first arm
//! that matches declares one module for each identifier the call binds to `$m`.

use proc_macro2::{TokenStream, TokenTree};

use matcher::{Miss, matched};

mod matcher;

/// One arm of a `macro_rules!`: the shape a call must have, and each variable that a `mod $v;` in
/// the body declares as a module.
#[derive(Debug)]
pub(super) struct Arm {
    matcher: Vec<TokenTree>,
    modules: Vec<String>,
}

/// The arms of a `macro_rules!` body, `(matcher) => { body }` each, in order.
pub(super) fn arms(tokens: &TokenStream) -> Vec<Arm> {
    trees(tokens)
        .split(|tree| is_punct(tree, ';'))
        .filter_map(|arm| match arm {
            [
                TokenTree::Group(matcher),
                arrow,
                head,
                TokenTree::Group(body),
            ] if is_punct(arrow, '=') && is_punct(head, '>') => Some(Arm {
                matcher: trees(&matcher.stream()),
                modules: declared(&body.stream()),
            }),
            _ => None,
        })
        .collect()
}

/// The module names a call with `tokens` declares, from the first arm that matches all of them.
pub(super) fn modules(arms: &[Arm], tokens: &TokenStream) -> Vec<String> {
    let input = trees(tokens);
    let mut bound = Vec::new();
    let chosen = arms
        .iter()
        .map(|arm| {
            bound.clear();
            (arm, matched(&arm.matcher, &input, &mut bound))
        })
        .take_while(|(_, outcome)| !matches!(outcome, Err(Miss::Unreadable)))
        .find(|(_, outcome)| matches!(outcome, Ok(used) if *used == input.len()));
    let Some((arm, _)) = chosen else {
        return Vec::new();
    };
    bound
        .into_iter()
        .filter(|(variable, _)| arm.modules.contains(variable))
        .map(|(_, name)| name)
        .collect()
}

/// Every variable a `mod $v;` names in a body, at any depth.
fn declared(tokens: &TokenStream) -> Vec<String> {
    let variable = |window: &[TokenTree]| match window {
        [
            TokenTree::Ident(keyword),
            dollar,
            TokenTree::Ident(variable),
            end,
        ] if *keyword == "mod" && is_punct(dollar, '$') && is_punct(end, ';') => {
            Some(variable.to_string())
        }
        _ => None,
    };
    super::scanned(tokens, 4, &variable, &declared)
}

fn is_punct(tree: &TokenTree, char: char) -> bool {
    matches!(tree, TokenTree::Punct(punct) if punct.as_char() == char)
}

fn trees(tokens: &TokenStream) -> Vec<TokenTree> {
    tokens.clone().into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The modules a call with `call` declares, given the body of a `macro_rules!`.
    fn names(rules: &str, call: &str) -> Vec<String> {
        modules(&arms(&rules.parse().unwrap()), &call.parse().unwrap())
    }

    #[test]
    fn each_fragment_kind_the_matcher_reads_takes_its_own_tokens() {
        let rules = "($a:literal $b:literal $c:literal $l:lifetime $t:tt $m:ident) => { mod $m; };";
        assert_eq!(names(rules, "-1 true \"s\" 'a { any } name"), ["name"]);
    }

    #[test]
    fn a_separator_sits_between_repetitions_and_never_after_the_last() {
        let rules = "($($m:ident),*) => { $(mod $m;)* };";
        assert_eq!(names(rules, "a, b"), ["a", "b"]);
        assert_eq!(names(rules, "a, b,"), Vec::<String>::new());
    }

    #[test]
    fn a_question_mark_repeats_at_most_once_and_a_plus_at_least_once() {
        let once = "($($m:ident)?) => { $(mod $m;)? };";
        assert_eq!(names(once, "a"), ["a"]);
        assert_eq!(names(once, "a b"), Vec::<String>::new());
        let some = "($($m:ident)+) => { $(mod $m;)+ };";
        assert_eq!(names(some, ""), Vec::<String>::new());
    }

    #[test]
    fn a_group_matches_only_a_group_with_the_same_delimiter() {
        let rules = "([$m:ident]) => { mod $m; };";
        assert_eq!(names(rules, "[a]"), ["a"]);
        assert_eq!(names(rules, "(a)"), Vec::<String>::new());
    }

    #[test]
    fn an_arm_the_matcher_cannot_read_stops_the_search() {
        let stopped =
            |arm: &str| names(&format!("{arm} ($($m:ident)*) => {{ $(mod $m;)* }};"), "x");
        assert_eq!(stopped("($e:expr) => {};"), Vec::<String>::new());
        assert_eq!(stopped("($($e:expr),*) => {};"), Vec::<String>::new());
        assert_eq!(stopped("($($m:ident)) => {};"), Vec::<String>::new());
    }

    #[test]
    fn a_literal_in_a_matcher_matches_only_the_same_literal() {
        let rules = "(\"kind\" $m:ident) => { mod $m; };";
        assert_eq!(names(rules, "\"kind\" a"), ["a"]);
        assert_eq!(names(rules, "\"other\" a"), Vec::<String>::new());
        assert_eq!(names(rules, "kind a"), Vec::<String>::new());
    }

    #[test]
    fn a_raw_identifier_names_the_module_without_its_prefix() {
        assert_eq!(names("($m:ident) => { mod $m; };", "r#async"), ["async"]);
    }
}
