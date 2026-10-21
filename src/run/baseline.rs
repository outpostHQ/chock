//! One baseline for every gate. Each gate fails on a regression, never on debt already there,
//! so a project can switch it on.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::project::document::{self, Versioned};

/// The on-disk shape. Bumped only by adding a field; a file declaring a higher version is refused
/// rather than half-read. See `document::parse`.
pub const SCHEMA: u32 = 1;

pub const FILE: &str = ".chock/baseline.json";

/// Ratchets whose count depends on the system that measured it: each system builds its own binary
/// and runs only its own `cfg` code. Off Linux, each keeps its record under `<gate>@<system>`.
const PER_SYSTEM: [&str; 4] = ["binsize", "bsize", "coverage", "mutest"];

/// The name a gate's record is kept under on `system`. Linux keeps the plain name, as every record
/// did before each system kept its own.
fn kept_as(gate: &str, system: &str) -> String {
    match PER_SYSTEM.contains(&gate) && system != "linux" {
        true => format!("{gate}@{system}"),
        false => gate.to_string(),
    }
}

/// The file a key names: the part before `#`, or the whole key.
#[must_use]
pub fn file_of(key: &str) -> &str {
    key.split_once('#').map_or(key, |(file, _)| file)
}

/// The name this system keeps the gate's record under.
#[must_use]
pub fn kept_here(gate: &str) -> String {
    kept_as(gate, std::env::consts::OS)
}

/// What a key names, which decides what an unrecognised one means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keys {
    /// Measures of an open set, counted where they fired. A key the run lacks is a count that fell
    /// to zero; a key the baseline lacks fails above zero, as an item does.
    Measures,
    /// Measures of a set the gate enumerates in full, zero rows included. A row the baseline holds
    /// and a run does not is a check that stopped happening, so it fails.
    Census,
    /// Items in the tree, such as a file or a function. A key the baseline lacks is new debt and
    /// fails, unless it measures zero.
    Items,
    /// Items whose number moves with every change, such as a binary's bytes. A key fails past the
    /// larger of `basis_points` of its record and `floor`; only a new baseline moves the record.
    Sizes { basis_points: u64, floor: u64 },
}

impl Keys {
    /// The most a key recorded at `was` may measure and still be no worse.
    #[must_use]
    pub fn allowed(self, was: u64) -> u64 {
        match self {
            Self::Sizes {
                basis_points,
                floor,
            } => was.saturating_add((was.saturating_mul(basis_points) / 10_000).max(floor)),
            Self::Measures | Self::Census | Self::Items => was,
        }
    }
}

/// One gate's numbers, lower always better. A `BTreeMap` so the file is sorted and diffs are small.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Series(pub BTreeMap<String, u64>);

fn schema_url() -> String {
    document::schema_url("baseline", SCHEMA)
}

impl Series {
    #[must_use]
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// The sum of every row.
    #[must_use]
    pub fn summed(&self) -> u64 {
        self.0.values().sum()
    }

    pub fn set(&mut self, key: &str, value: u64) {
        self.0.insert(key.to_string(), value);
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<u64> {
        self.0.get(key).copied()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// What recording this measurement over a held one writes down. Raises and adds, never lowers: a
    /// lower number is a *tighter* ratchet, and tightening a key nobody named fails elsewhere.
    #[must_use]
    pub fn kept_higher(mut self, held: &Self) -> Self {
        for (key, was) in &held.0 {
            if self.get(key).is_some_and(|now| now < *was) {
                self.set(key, *was);
            }
        }
        self
    }

    /// The record `held` lowered to this measurement wherever it went down, without the keys this
    /// run no longer finds: what a passing run locks in. `None` when nothing went down.
    #[must_use]
    pub fn tightened(&self, held: &Self, keys: Keys) -> Option<Self> {
        let mut out = held.clone();
        for (key, &was) in &held.0 {
            match self.get(key) {
                Some(now) => out.set(key, now.min(was)),
                // A key a run no longer produces is a fix to drop. A census row that stops
                // arriving is a check that stopped, which the comparison fails instead.
                None if !matches!(keys, Keys::Census) => {
                    out.0.remove(key);
                }
                None => {}
            }
        }
        // An item that moved to another file takes its record to the new key.
        out.0.extend(self.carried(held, keys));
        (out != *held).then_some(out)
    }

    /// The record each moved item takes to its new key: the lower of its number and the old key's.
    fn carried(&self, held: &Self, keys: Keys) -> Vec<(String, u64)> {
        let record = |(new, old): (String, String)| {
            let now = self.get(&new)?.min(held.get(&old)?);
            Some((new, now))
        };
        self.moved(held, keys)
            .into_iter()
            .filter_map(record)
            .collect()
    }

    /// Each item key the record lacks, with the one record key of that item this run no longer
    /// measures: a function that moved file. A name that two keys share pairs none.
    #[must_use]
    pub fn moved(&self, baseline: &Self, keys: Keys) -> BTreeMap<String, String> {
        if !matches!(keys, Keys::Items) {
            return BTreeMap::new();
        }
        let arrived = by_item(self.0.keys().filter(|key| !baseline.0.contains_key(*key)));
        let left = by_item(baseline.0.keys().filter(|key| !self.0.contains_key(*key)));
        let paired = |(item, new): (&str, Vec<&str>)| match (
            new.as_slice(),
            left.get(item).map(Vec::as_slice),
        ) {
            ([new], Some([old])) => Some((new.to_string(), old.to_string())),
            _ => None,
        };
        arrived.into_iter().filter_map(paired).collect()
    }

    /// Every key this run measured, held at zero: the record a strict gate is compared against.
    #[must_use]
    pub fn zeroed(&self) -> Self {
        Self(self.0.keys().map(|key| (key.clone(), 0)).collect())
    }

    /// Every key that got worse, plus every key the baseline never saw carrying a number above
    /// zero. No kind of key lets a count the record lacks pass.
    #[must_use]
    pub fn regressions(&self, baseline: &Self, keys: Keys) -> Vec<Change> {
        let moved = self.moved(baseline, keys);
        let held = |key: &String| baseline.get(moved.get(key).unwrap_or(key));
        self.0
            .iter()
            .filter_map(|(key, &now)| match held(key) {
                Some(was) if now > keys.allowed(was) => Some(Change::Grew {
                    key: key.clone(),
                    was,
                    now,
                }),
                Some(_) => None,
                // A new key at zero is no worse than its absence, so an upstream tool's new metric
                // fails a commit only when it counts something.
                None => (now > 0).then(|| Change::New {
                    key: key.clone(),
                    now,
                }),
            })
            .collect()
    }

    /// Keys this run measured at zero that the baseline does not carry: checking that grew and
    /// found nothing. One above zero is a regression already.
    #[must_use]
    pub fn new_at_zero(&self, baseline: &Self) -> Vec<String> {
        self.0
            .iter()
            .filter(|(key, now)| **now == 0 && !baseline.0.contains_key(*key))
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Keys the baseline carries that this run did not produce. Under [`Keys::Census`] that is a
    /// check that stopped happening, which no number in the series can report.
    #[must_use]
    pub fn stopped_measuring(&self, baseline: &Self) -> Vec<String> {
        baseline
            .0
            .keys()
            .filter(|key| !self.0.contains_key(*key))
            .cloned()
            .collect()
    }
}

/// The keys that name an item after `#`, grouped by that name.
fn by_item<'a>(keys: impl Iterator<Item = &'a String>) -> BTreeMap<&'a str, Vec<&'a str>> {
    let mut out: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for key in keys {
        if let Some((_, item)) = key.split_once('#') {
            out.entry(item).or_default().push(key);
        }
    }
    out
}

/// A key that moved the wrong way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "lowercase")]
pub enum Change {
    /// Known to the baseline and higher than it.
    Grew { key: String, was: u64, now: u64 },
    /// Not in the baseline at all, and above zero.
    New { key: String, now: u64 },
}

impl Change {
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Grew { key, .. } | Self::New { key, .. } => key,
        }
    }

    #[must_use]
    pub fn now(&self) -> u64 {
        match self {
            Self::Grew { now, .. } | Self::New { now, .. } => *now,
        }
    }
}

/// The committed file. It carries no date: git records when a baseline moved, and a self-reported
/// one is a second copy that can disagree with the commit that wrote it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    /// Recomputed on every write and never read back, so an older chock's URL never carries over.
    #[serde(rename = "$schema", skip_deserializing, default = "schema_url")]
    pub schema_url: String,
    pub version: u32,
    pub chock: String,
    pub gates: BTreeMap<String, Series>,
    /// What each gate counted when recorded. A unit change keeps the keys, so only this field shows
    /// it. Older files lack it, which is not a mismatch.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub units: BTreeMap<String, String>,
}

impl Versioned for Baseline {
    const SCHEMA: u32 = SCHEMA;
    const FILE: &'static str = FILE;

    fn version(&self) -> u32 {
        self.version
    }

    fn describe() -> &'static str {
        "a chock baseline"
    }
}

impl Baseline {
    #[must_use]
    pub fn empty(chock_version: &str) -> Self {
        Self {
            schema_url: schema_url(),
            version: SCHEMA,
            chock: chock_version.to_string(),
            gates: BTreeMap::new(),
            units: BTreeMap::new(),
        }
    }

    /// What this gate counted when recorded, or `None` for an older baseline that kept no units.
    #[must_use]
    pub fn unit(&self, name: &str) -> Option<&str> {
        self.units.get(name).map(String::as_str)
    }

    /// Write this measurement down as the gate's new record, raising and never lowering. The unit goes
    /// beside it, so a later run can tell a gate that counts something else from a tree that got worse.
    pub fn record(&mut self, name: &str, unit: &str, series: Series) {
        // A record in another unit measures a different thing, so it is replaced, not raised.
        let held = match self.unit(name).is_some_and(|was| was != unit) {
            true => Series::new(),
            false => self.gate(name),
        };
        self.set(name, series.kept_higher(&held));
        self.units.insert(name.to_string(), unit.to_string());
    }

    /// `record` for `chock baseline`, or `lower` for `chock baseline --lower`.
    pub fn keep(&mut self, name: &str, unit: &str, series: Series, lower: bool) {
        match lower {
            true => self.lower(name, &series),
            false => self.record(name, unit, series),
        }
    }

    /// What `chock baseline --keys` writes: only the `named` keys change, each to this measurement
    /// or out where it is gone, and none goes up. `Err` names each one that would.
    pub fn write_keys(
        &mut self,
        name: &str,
        unit: &str,
        series: &Series,
        named: &[&str],
    ) -> Result<(), Vec<String>> {
        if let Some(held) = self.unit(name).filter(|held| *held != unit) {
            return Err(vec![format!(
                "the record counts {held} and the gate counts {unit} now, so record the whole gate"
            )]);
        }
        let mut out = self.gate(name);
        let mut raised = Vec::new();
        for key in named {
            match (series.get(key), out.get(key)) {
                (Some(now), Some(was)) if now > was => raised.push(format!("{key}: {was} → {now}")),
                (Some(now), _) => out.set(key, now),
                (None, _) => {
                    out.0.remove(*key);
                }
            }
        }
        if !raised.is_empty() {
            return Err(raised);
        }
        self.set(name, out);
        self.units.insert(name.to_string(), unit.to_string());
        Ok(())
    }

    /// What `chock baseline --lower` writes: each held key brought down to this measurement where it
    /// went down, and dropped where it is gone. It never raises a key and never adds one.
    pub fn lower(&mut self, name: &str, series: &Series) {
        if let Some(tighter) = series.tightened(&self.gate(name), Keys::Items) {
            self.set(name, tighter);
        }
    }

    /// The series for `gate`, or an empty one. A gate with no baseline yet is not an error here:
    /// the caller decides whether that means "record me" or "cannot run".
    #[must_use]
    pub fn gate(&self, name: &str) -> Series {
        self.recorded(name).cloned().unwrap_or_default()
    }

    /// The series recorded for `gate`, or `None` where it has none yet.
    #[must_use]
    pub fn recorded(&self, name: &str) -> Option<&Series> {
        self.gates.get(&kept_here(name))
    }

    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.gates.contains_key(&kept_here(name))
    }

    pub fn set(&mut self, name: &str, series: Series) {
        self.gates.insert(kept_here(name), series);
    }

    /// Each key of file `old` renamed to `new`, in the record of each gate `names` lists (all when
    /// empty), on every system: how many moved. A key `new` already holds stops it, unchanged.
    pub fn moved_file(&mut self, names: &[&str], old: &str, new: &str) -> Result<usize, String> {
        let named = |kept: &str| {
            let gate = kept.split_once('@').map_or(kept, |(gate, _)| gate);
            names.is_empty() || names.contains(&gate)
        };
        let renamed = |key: &str| match key.strip_prefix(old) {
            Some("") => Some(new.to_string()),
            Some(rest) if rest.starts_with('#') => Some(format!("{new}{rest}")),
            _ => None,
        };
        let mut gates = self.gates.clone();
        let mut moved = 0;
        for (kept, series) in gates.iter_mut().filter(|(kept, _)| named(kept)) {
            let mut was = std::mem::take(&mut series.0);
            for (key, value) in was.clone() {
                let Some(to) = renamed(&key) else { continue };
                if was.contains_key(&to) {
                    return Err(format!("the record of {kept} already holds {to}"));
                }
                was.remove(&key);
                was.insert(to, value);
                moved += 1;
            }
            series.0 = was;
        }
        self.gates = gates;
        Ok(moved)
    }
}

/// A JSON report with every path under `root` relative to it, `/` between its parts. cargo-crap
/// reports absolute paths, and a record that holds them matches on no other machine.
#[must_use]
pub fn relativize(report: &str, root: &Path) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(report) else {
        return report.to_string();
    };
    relative_in(&mut value, root);
    format!("{value:#}")
}

/// Read as JSON and not as text, since JSON writes each `\` of a Windows path twice.
fn relative_in(value: &mut serde_json::Value, root: &Path) {
    match value {
        serde_json::Value::String(text) if Path::new(text.as_str()).starts_with(root) => {
            *text = crate::project::relative(root, Path::new(text.as_str()));
        }
        serde_json::Value::Array(items) => {
            items.iter_mut().for_each(|item| relative_in(item, root));
        }
        serde_json::Value::Object(fields) => {
            fields
                .values_mut()
                .for_each(|field| relative_in(field, root));
        }
        _ => {}
    }
}

/// What a passing ratchet holds. A pass prints no findings, so this names the items behind the
/// number; an unreadable baseline says so.
#[must_use]
pub fn held(baseline: &Result<Option<Baseline>, String>, name: &str) -> Vec<String> {
    match baseline {
        Ok(Some(baseline)) => baseline
            .gate(name)
            .0
            .iter()
            .map(|(key, count)| format!("held      {key}: {count}"))
            .collect(),
        Ok(None) => Vec::new(),
        Err(why) => vec![format!(
            "cannot read the baseline to show what it holds: {why}"
        )],
    }
}

/// The same, for the baseline recorded under `root`.
#[must_use]
pub fn held_at(root: &Path, name: &str) -> Vec<String> {
    own_held(root, name).unwrap_or_else(|| {
        let shared = document::read::<Baseline>(root).map_err(|e| e.to_string());
        held(&shared, name)
    })
}

/// What a check that keeps its own record holds there. `None` for a check on the shared baseline.
fn own_held(root: &Path, name: &str) -> Option<Vec<String>> {
    let own = match crate::gates::own_debt(root, name) {
        Ok(own) => own?,
        Err(why) => return Some(held(&Err(why), name)),
    };
    let items = own.items.iter();
    Some(
        items
            .map(|item| format!("held      {}: {}", item.key, item.count))
            .collect(),
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed unwrap in a test is the test failing"
)]
mod tests {
    use super::*;

    #[test]
    fn a_count_that_depends_on_the_system_is_kept_per_system_and_linux_keeps_the_plain_name() {
        assert_eq!(kept_as("coverage", "windows"), "coverage@windows");
        assert_eq!(kept_as("mutest", "macos"), "mutest@macos");
        assert_eq!(kept_as("coverage", "linux"), "coverage");
        assert_eq!(kept_as("slop", "windows"), "slop");
        let mut baseline = Baseline::default();
        baseline.set("coverage", series(&[("src/a.rs", 2)]));
        assert!(baseline.gates.contains_key(&kept_here("coverage")));
        assert_eq!(baseline.gate("coverage"), series(&[("src/a.rs", 2)]));
    }

    /// A lower number measured on one machine would fail a run on another that measures higher.
    #[test]
    fn a_moved_file_takes_its_keys_on_every_system_and_never_one_the_record_holds() {
        let mut held = Baseline::empty("0.1.0");
        let series = |pairs: &[(&str, u64)]| {
            Series(pairs.iter().map(|(key, n)| (key.to_string(), *n)).collect())
        };
        held.set("slop", series(&[("src/a.rs", 9), ("src/ab.rs", 2)]));
        held.gates
            .insert("nesting@windows".to_string(), series(&[("src/a.rs#f", 3)]));
        held.set("typos", series(&[("src/a.rs", 1)]));
        assert_eq!(
            held.moved_file(&["slop", "nesting"], "src/a.rs", "src/b.rs"),
            Ok(2)
        );
        let slop = series(&[("src/ab.rs", 2), ("src/b.rs", 9)]);
        assert_eq!(held.gate("slop"), slop);
        assert_eq!(held.gates["nesting@windows"], series(&[("src/b.rs#f", 3)]));
        assert_eq!(held.gate("typos"), series(&[("src/a.rs", 1)]));
        held.set("typos", series(&[("src/b.rs", 1), ("src/c.rs", 4)]));
        let was = held.clone();
        let refused = held.moved_file(&[], "src/b.rs", "src/c.rs");
        assert_eq!(
            refused,
            Err("the record of typos already holds src/c.rs".to_string())
        );
        assert_eq!(held, was);
        assert_eq!(held.moved_file(&[], "src/none.rs", "src/d.rs"), Ok(0));
        held.set("typos", series(&[("src/a.rs", 1), ("src/c.rs", 4)]));
        assert_eq!(held.moved_file(&["typos"], "src/c.rs", "src/e.rs"), Ok(1));
        let typos = series(&[("src/a.rs", 1), ("src/e.rs", 4)]);
        assert_eq!(held.gate("typos"), typos);
    }

    #[test]
    fn recording_raises_and_adds_a_key_and_never_lowers_one() {
        let held = series(&[("kept/high.rs", 20), ("gone.rs", 5)]);
        let out = series(&[("kept/high.rs", 9), ("new.rs", 3)]).kept_higher(&held);
        assert_eq!(
            out.get("kept/high.rs"),
            Some(20),
            "a record is never lowered"
        );
        assert_eq!(
            out.get("new.rs"),
            Some(3),
            "a new key is recorded as measured"
        );
        assert_eq!(
            out.get("gone.rs"),
            None,
            "an item that is gone is a fix, not debt"
        );
    }

    /// A moved file's keys: the new one comes in as measured, the old one goes, the rest stay.
    #[test]
    fn writing_named_keys_changes_only_them_and_never_raises_one() {
        let mut held = Baseline::empty("0.1.0");
        let first = [
            ("old.rs#f", 5),
            ("low.rs#g", 4),
            ("same.rs#h", 2),
            ("other.rs#i", 9),
        ];
        held.record("probe", "line(s)", series(&first));
        let now = [
            ("new.rs#f", 5),
            ("low.rs#g", 3),
            ("same.rs#h", 2),
            ("other.rs#i", 1),
            ("unnamed.rs#j", 7),
        ];
        let named = [
            "new.rs#f",
            "old.rs#f",
            "low.rs#g",
            "same.rs#h",
            "absent.rs#k",
        ];
        held.write_keys("probe", "line(s)", &series(&now), &named)
            .unwrap();
        let kept = [
            ("low.rs#g", 3),
            ("new.rs#f", 5),
            ("other.rs#i", 9),
            ("same.rs#h", 2),
        ];
        assert_eq!(held.gate("probe"), series(&kept));
        let grew = series(&[("low.rs#g", 4), ("other.rs#i", 10), ("new.rs#f", 6)]);
        let refused = held.write_keys("probe", "line(s)", &grew, &["low.rs#g", "new.rs#f"]);
        assert_eq!(refused.unwrap_err(), ["low.rs#g: 3 → 4", "new.rs#f: 5 → 6"]);
        assert_eq!(
            held.gate("probe"),
            series(&kept),
            "a refusal writes nothing"
        );
        let other = held.write_keys("probe", "file(s)", &grew, &["other.rs#i"]);
        assert!(other.unwrap_err()[0].contains("counts line(s) and the gate counts file(s)"));
        let mut fresh = Baseline::empty("0.1.0");
        fresh
            .write_keys("probe", "file(s)", &grew, &["new.rs#f"])
            .unwrap();
        assert_eq!(fresh.gate("probe"), series(&[("new.rs#f", 6)]));
        assert_eq!(fresh.unit("probe"), Some("file(s)"));
    }

    /// A gate that re-keys leaves old keys nothing measures again; recording drops them.
    #[test]
    fn a_gate_that_now_counts_by_a_different_key_records_only_the_new_keys() {
        let held = series(&[("src/a.rs", 7)]);
        let out = series(&[("src/a.rs#one_test", 4)]).kept_higher(&held);
        assert_eq!(out.get("src/a.rs#one_test"), Some(4));
        assert_eq!(out.get("src/a.rs"), None, "the old key shape is not kept");
    }

    /// One shared key means the same gate, so other held keys count as paid-off debt.
    #[test]
    fn one_key_in_common_is_the_same_gate_rather_than_a_new_key_shape() {
        let held = series(&[("src/a.rs", 7), ("src/b.rs", 2)]);
        let out = series(&[("src/a.rs", 1)]).kept_higher(&held);
        assert_eq!(out.get("src/a.rs"), Some(7));
        assert_eq!(out.get("src/b.rs"), None);
    }

    /// `boundaries` changed unit from a share of a directory to a count of its files. The keys
    /// stayed, so only the unit shows the change.
    #[test]
    fn a_gate_counting_in_another_unit_than_its_record_is_recorded_fresh() {
        let mut held = Baseline::empty("0.1.0");
        held.record("probe", "point(s) off one concern", series(&[("src", 60)]));
        assert_eq!(held.gate("probe").get("src"), Some(60));

        held.record("probe", "file(s) off one concern", series(&[("src", 21)]));
        assert_eq!(held.gate("probe").get("src"), Some(21));
        assert_eq!(held.unit("probe"), Some("file(s) off one concern"));
    }

    #[test]
    fn a_gate_recording_in_the_unit_it_already_holds_is_raised_and_not_replaced() {
        let mut held = Baseline::empty("0.1.0");
        held.record("probe", "line(s) over", series(&[("src/a.rs", 9)]));
        held.record("probe", "line(s) over", series(&[("src/a.rs", 4)]));
        assert_eq!(held.gate("probe").get("src/a.rs"), Some(9));
    }

    #[test]
    fn a_number_that_grew_is_recorded_at_what_it_grew_to() {
        let held = series(&[("src/a.rs", 4)]);
        let out = series(&[("src/a.rs", 11)]).kept_higher(&held);
        assert_eq!(out.get("src/a.rs"), Some(11));
    }

    #[test]
    fn recording_empty_or_disjoint_keys_keeps_exactly_the_measured_shape() {
        let cases = [
            (Series::new(), Series::new(), Series::new()),
            (Series::new(), series(&[("", 0)]), series(&[("", 0)])),
            (series(&[("retired", 7)]), Series::new(), Series::new()),
            (
                series(&[("old-a", 7), ("old-b", 0)]),
                series(&[("new-a", 4), ("new-b", 0)]),
                series(&[("new-a", 4), ("new-b", 0)]),
            ),
            (
                series(&[("", 7), ("retired", 9)]),
                series(&[("", 0), ("added", 3)]),
                series(&[("", 7), ("added", 3)]),
            ),
        ];
        for (held, measured, expected) in cases {
            let mut baseline = Baseline::empty("0.1.0");
            baseline.record("probe", "item(s)", held);
            baseline.record("probe", "item(s)", measured);
            assert_eq!(baseline.gate("probe"), expected);
            assert!(baseline.has("probe"), "an empty record is still a record");
            assert_eq!(baseline.unit("probe"), Some("item(s)"));
        }
    }

    fn series(pairs: &[(&str, u64)]) -> Series {
        let mut s = Series::new();
        for (k, v) in pairs {
            s.set(k, *v);
        }
        s
    }

    #[test]
    fn a_record_is_lowered_where_the_run_went_down_and_never_raised_where_it_went_up() {
        let held = series(&[("src/a.rs", 5), ("src/b.rs", 2), ("src/c.rs", 4)]);
        let now = series(&[("src/a.rs", 3), ("src/b.rs", 7), ("src/c.rs", 4)]);
        assert_eq!(
            now.tightened(&held, Keys::Items),
            Some(series(&[("src/a.rs", 3), ("src/b.rs", 2), ("src/c.rs", 4)]))
        );
        assert_eq!(
            held.tightened(&held, Keys::Items),
            None,
            "nothing went down"
        );
    }

    #[test]
    fn an_item_the_run_no_longer_finds_leaves_the_record_and_a_census_row_stays() {
        let held = series(&[("src/a.rs", 5), ("src/gone.rs", 2)]);
        let now = series(&[("src/a.rs", 5)]);
        for keys in [Keys::Items, Keys::Measures] {
            assert_eq!(now.tightened(&held, keys), Some(series(&[("src/a.rs", 5)])));
        }
        assert_eq!(
            now.tightened(&held, Keys::Census),
            None,
            "a missing row is not a fix"
        );
    }

    #[test]
    fn lowering_a_record_by_hand_never_raises_a_key_nor_adds_one() {
        let mut record = Baseline::empty("0.1.0");
        record.set(
            "coverage",
            series(&[("src/a.rs", 9), ("src/b.rs", 4), ("src/gone.rs", 1)]),
        );
        let measured = series(&[("src/a.rs", 6), ("src/b.rs", 8), ("src/new.rs", 3)]);
        record.lower("coverage", &measured);
        assert_eq!(
            record.gate("coverage"),
            series(&[("src/a.rs", 6), ("src/b.rs", 4)])
        );
        let before = record.clone();
        record.lower("coverage", &series(&[("src/a.rs", 6), ("src/b.rs", 4)]));
        assert_eq!(record, before, "nothing went down, so nothing was written");
    }

    #[test]
    fn a_zeroed_record_holds_every_key_the_run_measured_at_zero() {
        let now = series(&[("src/a.rs", 5), ("src/b.rs", 0)]);
        assert_eq!(now.zeroed(), series(&[("src/a.rs", 0), ("src/b.rs", 0)]));
        assert_eq!(
            now.regressions(&now.zeroed(), Keys::Measures),
            [Change::Grew {
                key: "src/a.rs".to_string(),
                was: 0,
                now: 5
            }]
        );
    }

    #[test]
    fn a_number_that_grew_is_a_regression_under_either_policy() {
        let now = series(&[("a", 5)]);
        let was = series(&[("a", 3)]);
        let grew = Change::Grew {
            key: "a".to_string(),
            was: 3,
            now: 5,
        };
        assert_eq!(now.regressions(&was, Keys::Items), vec![grew.clone()]);
        assert_eq!(now.regressions(&was, Keys::Measures), vec![grew]);
    }

    /// seed split `serve_zones` out of `source.rs`: its record goes along, and may not grow.
    #[test]
    fn an_item_that_moved_file_keeps_its_record_and_may_not_grow() {
        let was = series(&[("src/a.rs#serve", 52), ("src/a.rs#keep", 3)]);
        let moved = series(&[("src/a/serve.rs#serve", 50), ("src/a.rs#keep", 3)]);
        assert_eq!(moved.regressions(&was, Keys::Items), vec![]);
        assert_eq!(moved.tightened(&was, Keys::Items), Some(moved.clone()));
        let grew = series(&[("src/a/serve.rs#serve", 53), ("src/a.rs#keep", 3)]);
        let over = Change::Grew {
            key: "src/a/serve.rs#serve".to_string(),
            was: 52,
            now: 53,
        };
        assert_eq!(grew.regressions(&was, Keys::Items), vec![over]);
        let new = |key: &str, now| Change::New {
            key: key.to_string(),
            now,
        };
        // While the old key is still measured, the same name elsewhere is a new function.
        let copied = series(&[("src/b.rs#serve", 9), ("src/a.rs#serve", 52)]);
        assert_eq!(
            copied.regressions(&was, Keys::Items),
            vec![new("src/b.rs#serve", 9)]
        );
        // Two keys of one name could have come from either place, so neither takes the record.
        let split = series(&[("src/b.rs#serve", 9), ("src/c.rs#serve", 9)]);
        assert_eq!(
            split.regressions(&was, Keys::Items),
            vec![new("src/b.rs#serve", 9), new("src/c.rs#serve", 9)]
        );
        let bare = series(&[("src/b.rs", 4)]);
        let held = series(&[("src/a.rs", 4)]);
        assert_eq!(
            bare.regressions(&held, Keys::Items),
            vec![new("src/b.rs", 4)]
        );
        assert_eq!(
            moved.regressions(&was, Keys::Measures),
            vec![new("src/a/serve.rs#serve", 50)]
        );
    }

    #[test]
    fn a_number_that_held_or_shrank_is_never_a_regression() {
        let was = series(&[("a", 3), ("b", 9)]);
        let now = series(&[("a", 3), ("b", 1)]);
        assert_eq!(now.regressions(&was, Keys::Items), vec![]);
    }

    #[test]
    fn an_unknown_key_above_zero_fails_under_every_kind_of_key() {
        let now = series(&[("src/new.rs", 1200)]);
        let was = Series::new();
        let new = vec![Change::New {
            key: "src/new.rs".to_string(),
            now: 1200,
        }];
        for keys in [Keys::Items, Keys::Measures, Keys::Census] {
            assert_eq!(now.regressions(&was, keys), new);
        }
        assert_eq!(now.new_at_zero(&was), Vec::<String>::new());
    }

    #[test]
    fn only_a_key_at_zero_that_the_baseline_lacks_is_new_at_zero() {
        let was = series(&[("held", 0)]);
        let now = series(&[("counted", 2), ("held", 0), ("quiet", 0)]);
        assert_eq!(now.new_at_zero(&was), vec!["quiet".to_string()]);
        let counted = Change::New {
            key: "counted".to_string(),
            now: 2,
        };
        assert_eq!(now.regressions(&was, Keys::Census), vec![counted]);
    }

    /// An optional tool adding a metric must not fail a commit for reporting none of it.
    #[test]
    fn a_new_item_measuring_zero_is_not_new_debt() {
        let was = Series::new();
        assert_eq!(series(&[("k", 0)]).regressions(&was, Keys::Items), vec![]);
        assert_eq!(
            series(&[("k", 1)]).regressions(&was, Keys::Items),
            vec![Change::New {
                key: "k".to_string(),
                now: 1
            }]
        );
    }

    /// A size the record lacks is new debt, as an item is.
    #[test]
    fn a_size_is_worse_only_past_its_tolerance() {
        let keys = Keys::Sizes {
            basis_points: 100,
            floor: 1_000,
        };
        let was = series(&[("tool", 50_000)]);
        assert_eq!(series(&[("tool", 51_000)]).regressions(&was, keys), vec![]);
        assert_eq!(
            series(&[("tool", 51_001)]).regressions(&was, keys),
            vec![Change::Grew {
                key: "tool".to_string(),
                was: 50_000,
                now: 51_001
            }]
        );
        assert_eq!(
            keys.allowed(1_000_000),
            1_010_000,
            "the share over the floor"
        );
        assert_eq!(Keys::Items.allowed(1_000_000), 1_000_000);
        assert_eq!(
            series(&[("fresh", 900)]).regressions(&Series::new(), keys),
            vec![Change::New {
                key: "fresh".to_string(),
                now: 900
            }]
        );
    }

    #[test]
    fn a_key_the_baseline_holds_that_this_run_no_longer_produced_is_reported() {
        let was = series(&[("half-an-inverse", 0), ("interpolated-command", 3)]);
        let now = series(&[("interpolated-command", 3)]);
        assert_eq!(now.regressions(&was, Keys::Census), vec![]);
        assert_eq!(
            now.stopped_measuring(&was),
            vec!["half-an-inverse".to_string()]
        );
    }

    #[test]
    fn the_two_directions_of_a_missing_key_are_not_the_same_question() {
        let was = series(&[("kept", 1), ("retired", 2)]);
        let now = series(&[("added", 0), ("kept", 1)]);
        assert_eq!(now.new_at_zero(&was), vec!["added".to_string()]);
        assert_eq!(now.stopped_measuring(&was), vec!["retired".to_string()]);
    }

    #[test]
    fn every_key_the_run_stopped_producing_is_reported_not_only_the_first() {
        let was = series(&[("a", 1), ("b", 2), ("c", 3)]);
        let now = series(&[("b", 2)]);
        assert_eq!(
            now.stopped_measuring(&was),
            vec!["a".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn a_run_carrying_every_key_on_record_stopped_measuring_nothing() {
        let was = series(&[("a", 1)]);
        let now = series(&[("a", 1), ("b", 2)]);
        assert_eq!(now.stopped_measuring(&was), Vec::<String>::new());
    }

    #[test]
    fn every_regression_is_reported_not_only_the_first() {
        let now = series(&[("a", 5), ("b", 5), ("c", 1)]);
        let was = series(&[("a", 1), ("b", 1), ("c", 1)]);
        let found = now.regressions(&was, Keys::Measures);
        assert_eq!(
            found.iter().map(Change::key).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn a_baseline_round_trips_through_its_file_format() {
        let mut base = Baseline::empty("0.1.0");
        base.set("bigfiles", series(&[("src/init.rs", 888)]));
        let text = document::render(&base);
        assert_eq!(document::parse::<Baseline>(&text, "t").unwrap(), base);
    }

    #[test]
    fn the_rendered_file_is_sorted_and_ends_in_a_newline() {
        let mut base = Baseline::empty("0.1.0");
        base.set("g", series(&[("z", 1), ("a", 2)]));
        let text = document::render(&base);
        assert!(text.ends_with("}\n"));
        assert!(text.find("\"a\"") < text.find("\"z\""));
    }

    #[test]
    fn a_gate_with_no_baseline_reads_as_an_empty_series_not_an_error() {
        let base = Baseline::empty("0.1.0");
        assert!(base.gate("never-recorded").is_empty());
        assert!(!base.has("never-recorded"));
    }

    #[test]
    fn a_file_from_a_newer_chock_is_refused_rather_than_half_read() {
        let text = r#"{"version":99,"chock":"9.0.0","gates":{}}"#;
        assert_eq!(
            document::parse::<Baseline>(text, "b.json"),
            Err(document::Error::FromTheFuture {
                path: "b.json".to_string(),
                found: 99,
                reads: SCHEMA
            })
        );
    }

    #[test]
    fn text_that_is_not_a_baseline_names_the_file_it_came_from() {
        let err = document::parse::<Baseline>("not json", "b.json").unwrap_err();
        assert!(matches!(err, document::Error::Unparsable { path, .. } if path == "b.json"));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_missing_baseline_file_is_absence_rather_than_failure() {
        let dir = crate::testdir::make("baseline-missing");
        assert_eq!(document::read::<Baseline>(&dir), Ok(None));
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_written_baseline_reads_back_from_its_project_root() {
        let dir = crate::testdir::make("baseline-roundtrip");
        let mut base = Baseline::empty("0.1.0");
        base.set("slop", series(&[("src/a.rs", 3)]));
        std::fs::create_dir_all(dir.join(".chock")).unwrap();
        std::fs::write(dir.join(FILE), document::render(&base)).unwrap();
        assert_eq!(document::read::<Baseline>(&dir), Ok(Some(base)));
    }

    /// `relativize` read back as JSON, so no test depends on how the text is laid out.
    fn relative(report: &str, root: &Path) -> serde_json::Value {
        serde_json::from_str(&relativize(report, root)).unwrap()
    }

    #[test]
    fn a_path_under_the_root_loses_the_root() {
        assert_eq!(
            relative(r#"{"file": "/w/proj/src/a.rs"}"#, Path::new("/w/proj")),
            serde_json::json!({"file": "src/a.rs"})
        );
    }

    #[test]
    fn a_root_written_with_a_trailing_slash_gives_the_same_answer() {
        assert_eq!(
            relative(r#"{"file": "/w/proj/src/a.rs"}"#, Path::new("/w/proj/")),
            serde_json::json!({"file": "src/a.rs"})
        );
    }

    #[test]
    fn every_entry_is_rewritten_not_only_the_first() {
        assert_eq!(
            relative(
                r#"[{"file": "/w/p/a.rs"},{"at": ["/w/p/b.rs"], "line": 4}]"#,
                Path::new("/w/p")
            ),
            serde_json::json!([{"file": "a.rs"}, {"at": ["b.rs"], "line": 4}])
        );
    }

    /// `/w/projects` starts with the text `/w/proj` and is not under it. A `\` outside the root
    /// stays a `\`.
    #[test]
    fn a_path_outside_the_root_is_left_alone() {
        let outside =
            r#"{"file": "/elsewhere/a.rs", "near": "/w/projects/a.rs", "far": "D:\\x\\a.rs"}"#;
        let same: serde_json::Value = serde_json::from_str(outside).unwrap();
        assert_eq!(relative(outside, Path::new("/w/proj")), same);
    }

    /// On Windows this path holds `\`, which the report writes as `\\`.
    #[test]
    fn a_path_in_this_systems_own_form_loses_the_root() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("proj");
        let file = root.join("src").join("a.rs").display().to_string();
        let report = serde_json::json!({"file": file});
        assert_eq!(
            relative(&report.to_string(), &root),
            serde_json::json!({"file": "src/a.rs"})
        );
    }

    #[test]
    fn a_report_that_is_not_json_is_left_as_it_is() {
        let text = "error: /w/proj/src/a.rs";
        assert_eq!(relativize(text, Path::new("/w/proj")), text);
    }

    #[test]
    fn a_passing_ratchet_names_every_item_its_baseline_holds() {
        let holding = || {
            let mut baseline = Baseline::empty("0.1.0");
            baseline.record(
                "dupdeps",
                "duplicate crate build(s)",
                series(&[("syn", 2), ("proc-macro2", 2)]),
            );
            Ok(Some(baseline))
        };
        assert_eq!(
            held(&holding(), "dupdeps"),
            ["held      proc-macro2: 2", "held      syn: 2"]
        );
        assert_eq!(held(&holding(), "slop"), Vec::<String>::new());
        assert_eq!(held(&Ok(None), "dupdeps"), Vec::<String>::new());
        assert_eq!(
            held(&Err("line 1: expected value".into()), "dupdeps"),
            ["cannot read the baseline to show what it holds: line 1: expected value"]
        );
    }

    #[test]
    #[cfg_attr(all(miri, windows), ignore = "Miri cannot make a directory on Windows")]
    fn a_gate_that_keeps_its_own_record_names_what_that_record_holds() {
        let dir = crate::testdir::make("held-own-record");
        let file = dir.join(crate::gates::own_baseline("crap").unwrap());
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let record = r#"{"entries":[{"file":"src/a.rs","function":"parse","line":1,"crap":42.4}]}"#;
        std::fs::write(&file, record).unwrap();
        assert_eq!(held_at(&dir, "crap"), ["held      src/a.rs: parse: 42"]);
        assert_eq!(held_at(&dir, "dupdeps"), Vec::<String>::new());
        std::fs::write(&file, "not json").unwrap();
        let told = held_at(&dir, "crap").join("\n");
        assert!(
            told.starts_with("cannot read the baseline to show what it holds: "),
            "{told}"
        );
    }
}
