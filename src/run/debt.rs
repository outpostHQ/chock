//! The debt a project's baseline holds, largest first, with what to do about each kind.

use serde::Serialize;

use crate::run::baseline::Baseline;

/// How many of a gate's largest records are listed; the rest are counted.
const SHOWN: usize = 5;

/// One gate's recorded debt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Debt {
    pub gate: String,
    pub unit: String,
    pub total: u64,
    /// Every record above zero, largest first.
    pub items: Vec<Held>,
    pub fix: Option<&'static str>,
}

/// One record: a file, or a file and the function or lint inside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Held {
    pub key: String,
    pub count: u64,
}

/// Every gate holding debt, in the order a run takes them.
#[must_use]
pub fn owed(baseline: &Baseline) -> Vec<Debt> {
    crate::gates::registry()
        .iter()
        .filter_map(|gate| {
            let mut items: Vec<Held> = baseline
                .gate(gate.name)
                .0
                .into_iter()
                .filter(|(_, count)| *count > 0)
                .map(|(key, count)| Held { key, count })
                .collect();
            items.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));
            let unit = baseline.unit(gate.name).or(gate.counts_in())?.to_string();
            (!items.is_empty()).then(|| Debt {
                gate: gate.name.to_string(),
                total: items.iter().map(|held| held.count).sum(),
                unit,
                items,
                fix: crate::gates::fixes::fix(gate.name),
            })
        })
        .collect()
}

/// The debt for a person: each gate's total, its largest records, and the fix.
#[must_use]
pub fn render(owed: &[Debt]) -> String {
    if owed.is_empty() {
        return "The baseline holds no debt.\n".to_string();
    }
    let mut out =
        "Debt on record. A run that measures less lowers the record and keeps it lower.\n"
            .to_string();
    for debt in owed {
        out.push_str(&format!(
            "\n{} — {} {} in {} place(s)\n",
            debt.gate,
            debt.total,
            debt.unit,
            debt.items.len()
        ));
        for held in debt.items.iter().take(SHOWN) {
            out.push_str(&format!("  {}: {}\n", held.key, held.count));
        }
        let more = debt.items.len().saturating_sub(SHOWN);
        if more > 0 {
            out.push_str(&format!("  and {more} more\n"));
        }
        if let Some(fix) = debt.fix {
            out.push_str(&format!("  fix: {fix}\n"));
        }
    }
    out
}

/// The debt recorded under `root`, rendered for a person or, with `json`, for an agent.
pub fn shown(root: &std::path::Path, json: bool) -> Result<String, String> {
    let held = crate::project::document::read::<Baseline>(root).map_err(|e| e.to_string())?;
    let owed = owed(&held.unwrap_or_else(|| Baseline::empty(env!("CARGO_PKG_VERSION"))));
    Ok(match json {
        true => format!("{}\n", render_json(&owed)),
        false => render(&owed),
    })
}

/// The debt for an agent: every record, not only the largest.
#[must_use]
pub fn render_json(owed: &[Debt]) -> String {
    #[derive(Serialize)]
    struct Owed<'a> {
        debt: &'a [Debt],
    }
    serde_json::to_string(&Owed { debt: owed }).unwrap_or_default()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;
    use crate::run::baseline::Series;

    fn holding(gate: &str, pairs: &[(&str, u64)]) -> Baseline {
        let mut baseline = Baseline::empty("0.1.0");
        let mut series = Series::new();
        for (key, count) in pairs {
            series.set(key, *count);
        }
        baseline.record(gate, "over-long comment block(s)", series);
        baseline
    }

    #[test]
    fn debt_is_listed_largest_first_with_zeroes_left_out_and_the_fix_beneath() {
        let held = holding("slop", &[("src/a.rs", 2), ("src/b.rs", 7), ("src/c.rs", 0)]);
        let owed = owed(&held);
        assert_eq!(
            owed,
            [Debt {
                gate: "slop".to_string(),
                unit: "over-long comment block(s)".to_string(),
                total: 9,
                items: vec![
                    Held {
                        key: "src/b.rs".to_string(),
                        count: 7
                    },
                    Held {
                        key: "src/a.rs".to_string(),
                        count: 2
                    },
                ],
                fix: crate::gates::fixes::fix("slop"),
            }]
        );
        assert_eq!(
            render(&owed),
            format!(
                "Debt on record. A run that measures less lowers the record and keeps it lower.\n\nslop — 9 over-long comment block(s) in 2 place(s)\n  src/b.rs: 7\n  src/a.rs: 2\n  fix: {}\n",
                crate::gates::fixes::fix("slop").unwrap()
            )
        );
        assert!(render_json(&owed).starts_with(
            r#"{"debt":[{"gate":"slop","unit":"over-long comment block(s)","total":9"#
        ));
    }

    #[test]
    fn the_record_on_disk_is_shown_to_a_person_or_an_agent_and_an_unreadable_one_is_refused() {
        let dir = crate::testdir::make("debt-shown");
        assert_eq!(shown(&dir, false).unwrap(), "The baseline holds no debt.\n");
        assert_eq!(shown(&dir, true).unwrap(), "{\"debt\":[]}\n");
        let file = dir.join(crate::run::baseline::FILE);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let held = holding("slop", &[("src/a.rs", 2)]);
        crate::project::document::write(&file, &held.render()).unwrap();
        let told = shown(&dir, false).unwrap();
        assert!(
            told.contains("slop — 2 over-long comment block(s) in 1 place(s)"),
            "{told}"
        );
        assert!(
            shown(&dir, true)
                .unwrap()
                .starts_with(r#"{"debt":[{"gate":"slop""#)
        );
        std::fs::write(&file, "not json").unwrap();
        assert!(shown(&dir, false).is_err());
    }

    #[test]
    fn past_five_records_the_rest_are_counted_and_none_held_says_so() {
        let pairs: Vec<(String, u64)> = (1..=7).map(|n| (format!("src/f{n}.rs"), n)).collect();
        let pairs: Vec<(&str, u64)> = pairs.iter().map(|(k, n)| (k.as_str(), *n)).collect();
        let shown = render(&owed(&holding("slop", &pairs)));
        assert!(shown.contains("  src/f3.rs: 3\n  and 2 more\n"), "{shown}");
        assert!(!shown.contains("src/f2.rs"), "{shown}");
        assert_eq!(
            render(&owed(&holding("slop", &[("src/a.rs", 0)]))),
            "The baseline holds no debt.\n"
        );
    }
}
