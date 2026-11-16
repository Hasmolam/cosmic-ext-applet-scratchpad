// SPDX-License-Identifier: GPL-3.0-only

//! Per-note save bookkeeping.
//!
//! Each note allows at most one write in flight. Edits bump `revision`; a write
//! snapshots the current revision and `saved_revision` only advances when that
//! write reports success. Save requests that arrive while a write is running are
//! coalesced into a single follow-up write of the latest content, so an older
//! snapshot can never land on disk after a newer one.

/// Stable identity of a note for the lifetime of the applet process.
///
/// Messages produced by async tasks carry this id instead of a `Vec` index,
/// because deleting or restoring notes shifts indices while a task is running.
pub type NoteId = u64;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SaveState {
    revision: u64,
    saved_revision: u64,
    in_flight: Option<u64>,
    pending: bool,
    failed: bool,
}

impl SaveState {
    /// Records an in-memory change that is not on disk yet.
    pub fn mark_edited(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// True when the latest in-memory revision is confirmed on disk.
    pub fn is_saved(&self) -> bool {
        self.saved_revision == self.revision
    }

    /// True when the most recent write attempt failed and nothing has been
    /// written successfully since.
    pub fn has_failed(&self) -> bool {
        self.failed
    }

    pub fn is_in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Requests a write of the current revision.
    ///
    /// Returns the revision the caller must write now, or `None` when the note
    /// is already saved or a write is running. In the latter case a follow-up
    /// write is scheduled and reported by [`SaveState::finish`].
    pub fn begin(&mut self) -> Option<u64> {
        if self.is_saved() {
            return None;
        }
        if self.in_flight.is_some() {
            self.pending = true;
            return None;
        }
        self.pending = false;
        self.in_flight = Some(self.revision);
        Some(self.revision)
    }

    /// Records the outcome of the write started for `revision`.
    ///
    /// Returns true when a follow-up write was requested while this one ran and
    /// the note still has unsaved changes; the caller should call
    /// [`SaveState::begin`] again.
    pub fn finish(&mut self, revision: u64, ok: bool) -> bool {
        if self.in_flight != Some(revision) {
            return false;
        }
        self.in_flight = None;
        if ok {
            self.saved_revision = revision;
            self.failed = false;
        } else {
            self.failed = true;
        }
        let follow_up = self.pending && !self.is_saved();
        self.pending = false;
        follow_up
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_is_saved_and_idle() {
        let mut state = SaveState::default();
        assert!(state.is_saved());
        assert!(!state.has_failed());
        assert_eq!(state.begin(), None);
    }

    #[test]
    fn successful_write_marks_saved() {
        let mut state = SaveState::default();
        state.mark_edited();
        assert!(!state.is_saved());

        let rev = state.begin().unwrap();
        assert!(state.is_in_flight());
        assert!(!state.is_saved());
        assert!(!state.finish(rev, true));
        assert!(state.is_saved());
        assert!(!state.is_in_flight());
    }

    #[test]
    fn only_one_write_in_flight_and_requests_coalesce() {
        let mut state = SaveState::default();
        state.mark_edited();
        let first = state.begin().unwrap();

        state.mark_edited();
        assert_eq!(state.begin(), None);
        state.mark_edited();
        assert_eq!(state.begin(), None);

        // First write lands; the note is still dirty, so one follow-up is due.
        assert!(state.finish(first, true));
        assert!(!state.is_saved());

        let second = state.begin().unwrap();
        assert!(second > first);
        assert!(!state.finish(second, true));
        assert!(state.is_saved());
    }

    #[test]
    fn overlapping_saves_never_report_older_snapshot_as_latest() {
        // Two save requests for the same note while the first is still writing.
        // The completed state must correspond to the newest revision, and the
        // older write must not be able to mark the newer edit as saved.
        let mut state = SaveState::default();
        state.mark_edited();
        let old = state.begin().unwrap();
        state.mark_edited();
        let newest = state.revision;
        assert_eq!(state.begin(), None);

        assert!(state.finish(old, true));
        assert!(!state.is_saved());

        let follow = state.begin().unwrap();
        assert_eq!(follow, newest);
        assert!(!state.finish(follow, true));
        assert!(state.is_saved());
    }

    #[test]
    fn failed_write_stays_unsaved_and_retries_on_next_request() {
        let mut state = SaveState::default();
        state.mark_edited();
        let rev = state.begin().unwrap();
        assert!(!state.finish(rev, false));
        assert!(!state.is_saved());
        assert!(state.has_failed());

        // Closing the popup or the next debounce asks again.
        let retry = state.begin().unwrap();
        assert_eq!(retry, rev);
        assert!(!state.finish(retry, true));
        assert!(state.is_saved());
        assert!(!state.has_failed());
    }

    #[test]
    fn failed_write_with_pending_request_retries_once() {
        let mut state = SaveState::default();
        state.mark_edited();
        let rev = state.begin().unwrap();
        state.mark_edited();
        assert_eq!(state.begin(), None);

        assert!(state.finish(rev, false));
        let retry = state.begin().unwrap();
        // A second failure with no new request must not schedule another write.
        assert!(!state.finish(retry, false));
        assert!(state.has_failed());
        assert!(!state.is_in_flight());
    }

    #[test]
    fn stale_or_unknown_completion_is_ignored() {
        let mut state = SaveState::default();
        state.mark_edited();
        let rev = state.begin().unwrap();
        let before = state.clone();
        assert!(!state.finish(rev + 7, true));
        assert_eq!(state, before);
    }
}
