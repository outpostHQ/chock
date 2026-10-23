//! The laws of a TLA+ module: each theorem, as its statement and the definitions it reaches.

use std::collections::BTreeMap;

use super::Law;
use crate::gates::proofs::tla::text::{self, Unit};

/// The words that state what a TLA+ proof must prove.
const STATED: [&str; 4] = ["THEOREM", "LEMMA", "PROPOSITION", "COROLLARY"];

/// The words that start a TLA+ proof; a step label such as `<1>` starts one too.
const PROOF: [&str; 4] = ["PROOF", "BY", "OBVIOUS", "OMITTED"];

/// Each theorem at column 0, as its statement and each definition of its module that the
/// statement reaches, through other definitions too.
pub(super) fn theorems(tla: &str) -> Vec<Law> {
    let units = text::units(&text::uncommented(tla));
    let defined: BTreeMap<&str, &Unit> = units
        .iter()
        .filter_map(|unit| Some((defines(unit)?, unit)))
        .collect();
    let stated = units.iter().filter(|unit| STATED.contains(&unit.first()));
    let law = |unit: &Unit| {
        let statement = statement(&unit.text);
        let reached = reached(text::words(statement).collect(), &defined);
        let texts = [statement]
            .into_iter()
            .chain(reached.iter().map(|it| it.text.as_str()));
        Law {
            name: theorem_name(statement),
            line: unit.line,
            text: texts.map(normal).collect::<Vec<_>>().join("\n"),
        }
    };
    stated.map(law).collect()
}

/// The name a definition at column 0 defines: `Name == …`, `Name(a, b) == …` or `Name[x] == …`.
fn defines(unit: &Unit) -> Option<&str> {
    let (head, _) = unit.text.split_once("==")?;
    let name = unit.first();
    let rest = head.trim_start().strip_prefix(name)?.trim();
    let wrapped = |open: char, close: char| rest.starts_with(open) && rest.ends_with(close);
    let shaped = rest.is_empty() || wrapped('(', ')') || wrapped('[', ']');
    (shaped && !name.is_empty()).then_some(name)
}

/// A theorem's text before the first word or step label that starts its proof.
fn statement(text: &str) -> &str {
    let mut at = 0;
    while let Some(c) = text.get(at..).and_then(|rest| rest.chars().next()) {
        let rest = &text[at..];
        let len = rest
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        let starts = match c {
            '<' => !text[..at].ends_with('<') && is_label(&rest[1..]),
            _ => PROOF.contains(&&rest[..len]),
        };
        if starts {
            return &text[..at];
        }
        at += len.max(c.len_utf8());
    }
    text
}

/// Whether the text after a `<` makes a step label: `<1>`, `<*>` or `<+>`.
fn is_label(rest: &str) -> bool {
    let level = rest.split_once('>').map(|(level, _)| level);
    let numbered = |it: &str| !it.is_empty() && it.bytes().all(|b| b.is_ascii_digit());
    level.is_some_and(|it| it == "*" || it == "+" || numbered(it))
}

/// `THEOREM Name` for a named theorem, and the bare word for one with no name.
fn theorem_name(statement: &str) -> String {
    let mut words = text::words(statement);
    let kind = words.next().unwrap_or_default();
    match statement.split_once("==") {
        Some((head, _)) if text::words(head).count() == 2 => {
            format!("{kind} {}", words.next().unwrap_or_default())
        }
        _ => kind.to_string(),
    }
}

/// The definitions that `names` reach, through each other, in the order the module writes them.
fn reached<'a>(mut names: Vec<&'a str>, defined: &BTreeMap<&str, &'a Unit>) -> Vec<&'a Unit> {
    let mut found = BTreeMap::new();
    while let Some(name) = names.pop() {
        if let Some(unit) = defined.get(name)
            && found.insert(unit.line, *unit).is_none()
        {
            names.extend(text::words(&unit.text));
        }
    }
    found.into_values().collect()
}

/// TLA+ text with blank lines dropped and each run of spaces in a line made one. A line keeps its
/// indent, since a list of `/\` or `\/` reads by its column.
fn normal(text: &str) -> String {
    let line = |line: &str| {
        let body = line.trim_start();
        let indent = &line[..line.len() - body.len()];
        format!(
            "{indent}{}",
            body.split_whitespace().collect::<Vec<_>>().join(" ")
        )
    };
    let lines = text.lines().filter(|it| !it.trim().is_empty()).map(line);
    lines.collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gates::proofs::laws::tests::law;

    #[test]
    fn a_theorem_is_its_statement_and_each_definition_it_reaches() {
        let tla = "---- MODULE M ----\nVARIABLE x\nInit == x = 0\n\
                   Next == x' = x + 1\nSpec == Init /\\ [][Next]_x\nTypeOK(v) == v \\in Nat\n\
                   THEOREM Safe == Spec => [](TypeOK(x)  /\\ x >= 0) \\* why\n\
                   <1>1. Init => TypeOK(x)\n  BY DEF Init\n  <1>. QED\n\
                   LEMMA Spec => <<1>>[1] = 1 BY DEF Spec\nCOROLLARY TRUE\nPROOF OBVIOUS\n====\n";
        let found = theorems(tla);
        let spec = "Init == x = 0\nNext == x' = x + 1\nSpec == Init /\\ [][Next]_x\n\
                    TypeOK(v) == v \\in Nat";
        let safe = format!("THEOREM Safe == Spec => [](TypeOK(x) /\\ x >= 0)\n{spec}");
        let lemma = "LEMMA Spec => <<1>>[1] = 1\nInit == x = 0\nNext == x' = x + 1\n\
                     Spec == Init /\\ [][Next]_x";
        assert_eq!(
            found,
            [
                law("THEOREM Safe", 7, &safe),
                law("LEMMA", 11, lemma),
                law("COROLLARY", 12, "COROLLARY TRUE")
            ]
        );
        let commented = tla.replace("Init == x = 0", "Init == (* start *) x =  0 \\* at zero");
        assert_eq!(theorems(&commented), found);
        let weaker = tla.replace("x >= 0)", "TRUE)");
        assert_ne!(theorems(&weaker)[0].text, found[0].text);
        let unreached = tla.replace("====", "Other == 1\n====");
        assert_eq!(theorems(&unreached), found);
    }

    #[test]
    fn only_a_name_alone_or_with_its_arguments_before_the_equals_is_defined() {
        let unit = |text: &str| Unit {
            line: 1,
            text: text.to_string(),
        };
        assert_eq!(defines(&unit("A == 1")), Some("A"));
        assert_eq!(defines(&unit("F(a, b) == a")), Some("F"));
        assert_eq!(defines(&unit("f[n \\in Nat] == n")), Some("f"));
        for text in [
            "THEOREM T == TRUE",
            "====",
            "a \\prec b == TRUE",
            "VARIABLE x",
        ] {
            assert_eq!(defines(&unit(text)), None, "{text}");
        }
        assert_eq!(
            statement("THEOREM a < 1 /\\ b > 2 OMITTED"),
            "THEOREM a < 1 /\\ b > 2 "
        );
        assert_eq!(statement("THEOREM TRUE\n<*> x"), "THEOREM TRUE\n");
        assert_eq!(statement("THEOREM BYE"), "THEOREM BYE");
        assert_eq!(theorem_name("THEOREM x == y"), "THEOREM x");
        assert_eq!(theorem_name("THEOREM x = y"), "THEOREM");
        assert_eq!(theorem_name("THEOREM LET y == 1 IN y = 1"), "THEOREM");
    }
}
