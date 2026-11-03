//! What one pipe's reader keeps of a process's output while it reads to the end.

use super::Keep;

/// What one pipe's reader holds so far: every byte up to the cap, or with a `Keep`, each distinct
/// kept line, the line still arriving held apart until its newline.
pub(super) struct Holding {
    cap: usize,
    keep: Option<Keep>,
    held: Vec<u8>,
    truncated: bool,
    arriving: Vec<u8>,
    seen: std::collections::HashSet<String>,
}

impl Holding {
    pub(super) fn new(cap: usize, keep: Option<Keep>) -> Self {
        Self {
            cap,
            keep,
            held: Vec::new(),
            truncated: false,
            arriving: Vec::new(),
            seen: std::collections::HashSet::new(),
        }
    }

    pub(super) fn take(&mut self, read: &[u8]) {
        let Some(keep) = self.keep else {
            self.hold(read);
            return;
        };
        for piece in read.split_inclusive(|byte| *byte == b'\n') {
            self.arriving.extend_from_slice(piece);
            if piece.ends_with(b"\n") {
                self.end_line(keep);
            }
        }
    }

    fn end_line(&mut self, keep: Keep) {
        let line = String::from_utf8_lossy(&self.arriving).into_owned();
        self.arriving.clear();
        let Some(kept) = keep(line.trim_end_matches(['\n', '\r'])) else {
            return;
        };
        if self.seen.insert(kept.clone()) {
            self.hold(kept.as_bytes());
            self.hold(b"\n");
        }
    }

    fn hold(&mut self, bytes: &[u8]) {
        let room = self.cap.saturating_sub(self.held.len());
        self.held.extend(bytes.iter().take(room));
        self.truncated |= bytes.len() > room;
    }

    /// What was held, and whether the cap cut it; a last line with no newline is still a line.
    pub(super) fn finish(mut self) -> (String, bool) {
        if let Some(keep) = self.keep
            && !self.arriving.is_empty()
        {
            self.end_line(keep);
        }
        (String::from_utf8_lossy_owned(self.held), self.truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upper(line: &str) -> Option<String> {
        line.starts_with('k').then(|| line.to_uppercase())
    }

    #[test]
    fn a_kept_line_split_across_reads_is_held_once_and_a_last_line_needs_no_newline() {
        let keep: Keep = upper;
        let mut holding = Holding::new(64, Some(keep));
        for read in [&b"kee"[..], b"p\ndrop\nke", b"ep\nkept"] {
            holding.take(read);
        }
        assert_eq!(holding.finish(), ("KEEP\nKEPT\n".to_string(), false));
    }
}
