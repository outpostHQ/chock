//! What a ratchet read: its series, its notes, what it knows of each key, and the files it could
//! not read.

use crate::run::baseline::Series;
use crate::run::report::{Detail, Finding};

/// A measured series with scope notes that must survive comparison and baseline recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measurement {
    pub series: Series,
    pub findings: Vec<Finding>,
    /// What the gate knows about a key beyond its number, by key.
    pub details: Details,
    /// Each file the gate could not read, as `path: reason`. A read with any rules on nothing.
    pub unmeasured: Vec<String>,
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
        }
    }

    /// Baseline output keeps scope notes visible without recording them as measured debt.
    #[must_use]
    pub fn notes(&self, gate: &str) -> String {
        self.findings
            .iter()
            .map(|finding| format!("  note      {gate:<12} {}\n", finding.render()))
            .collect()
    }

    /// A read of every file. Recording or comparing a read that left a file out would take that
    /// file's debt as paid.
    pub fn whole(self) -> Result<Self, String> {
        match self.unmeasured.is_empty() {
            true => Ok(self),
            false => Err(self.unread()),
        }
    }

    /// Why a read that left files out rules on nothing: each file it could not read, and why.
    #[must_use]
    pub fn unread(&self) -> String {
        format!(
            "{} file(s) could not be read, so nothing is ruled on or recorded: {}",
            self.unmeasured.len(),
            self.unmeasured.join("; ")
        )
    }
}
