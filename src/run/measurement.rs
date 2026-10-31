//! What a ratchet read: its series, its notes, what it knows of each key, and the files it could
//! not read.

use std::fmt::Write as _;

use crate::run::baseline::Series;
use crate::run::report::{Detail, Finding};

/// A measured series with scope notes that must survive comparison and baseline recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measurement {
    /// The number the gate measured for each key.
    pub series: Series,
    /// Scope notes that baseline output shows but never records as debt.
    pub findings: Vec<Finding>,
    /// What the gate knows about a key beyond its number, by key.
    pub details: Details,
    /// Each file the gate could not read, as `path: reason`. A read with any rules on nothing.
    pub unmeasured: Vec<String>,
    /// What trips the gate whatever its record holds, such as a proof that fails. A read with any
    /// is never recorded, so a fault cannot become debt.
    pub faults: Vec<Finding>,
}

/// A gate's details, by the key they are about.
pub type Details = std::collections::BTreeMap<String, Detail>;

impl Measurement {
    /// A series and its notes, with no details for any key.
    #[must_use]
    pub fn of(series: Series, findings: Vec<Finding>) -> Self {
        Self {
            series,
            findings,
            details: Details::new(),
            unmeasured: Vec::new(),
            faults: Vec::new(),
        }
    }

    /// Baseline output keeps scope notes visible without recording them as measured debt.
    #[must_use]
    pub fn notes(&self, gate: &str) -> String {
        self.findings
            .iter()
            .fold(String::new(), |mut notes, finding| {
                let _ = writeln!(notes, "  note      {gate:<12} {}", finding.render());
                notes
            })
    }

    /// A read of every file, with no fault. Recording or comparing a read that left a file out
    /// would take that file's debt as paid, and recording a fault would make it debt.
    /// # Errors
    /// The text says which files went unread, or which faults the read found.
    pub fn whole(self) -> Result<Self, String> {
        match self.why_unrecorded() {
            Some(why) => Err(why),
            None => Ok(self),
        }
    }

    /// Why this read is not recorded, if it is not.
    fn why_unrecorded(&self) -> Option<String> {
        if !self.unmeasured.is_empty() {
            return Some(self.unread());
        }
        (!self.faults.is_empty()).then(|| self.unrecorded())
    }

    /// Why a read with a fault is not recorded: each fault, as a report line.
    #[must_use]
    pub fn unrecorded(&self) -> String {
        let faults: Vec<String> = self.faults.iter().map(Finding::render).collect();
        format!(
            "{} fault(s) trip this gate whatever its record holds, so nothing is recorded: {}",
            faults.len(),
            faults.join("; ")
        )
    }

    /// Why a read that left files out rules on nothing: each file it could not read, and why.
    #[must_use]
    pub fn unread(&self) -> String {
        unread_of(&self.unmeasured)
    }
}

/// Why a read that left `files` out rules on nothing, each as `path: reason`.
#[must_use]
pub fn unread_of(files: &[String]) -> String {
    format!(
        "{} file(s) could not be read, so nothing is ruled on or recorded: {}",
        files.len(),
        files.join("; ")
    )
}
