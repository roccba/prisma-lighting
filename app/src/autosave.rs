//! Autosave and session recovery.
//!
//! While the app runs, a background thread snapshots the show every [`INTERVAL`] (the
//! show document with the engine's live values folded in, serialized like a `.prisma`).
//! When the snapshot differs from the last saved / opened state it is written to
//! `autosave.prisma` in the app-config dir — atomically, via a temp file + rename, so a
//! crash mid-write never leaves a torn file. When it matches again (the operator saved),
//! the autosave is removed. So **an autosave on disk means "there were unsaved changes"**,
//! and the next launch recovers it instead of the last saved show.
//!
//! The operator's own `.prisma` files are never written here. On each recovery the
//! previous autosaves rotate into `autosave.1.prisma` / `autosave.2.prisma`, keeping the
//! last three recovered sessions.

use crate::*;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often the snapshot is taken.
pub const INTERVAL: Duration = Duration::from_secs(2);

/// How many recovered sessions are kept as backups besides the live autosave.
const BACKUPS: usize = 2;

/// The autosave files in one directory.
#[derive(Debug, Clone)]
pub struct Autosave {
    dir: PathBuf,
}

impl Autosave {
    pub fn new(dir: impl Into<PathBuf>) -> Autosave {
        Autosave { dir: dir.into() }
    }

    /// The live autosave.
    pub fn path(&self) -> PathBuf {
        self.dir.join("autosave.prisma")
    }

    fn backup(&self, n: usize) -> PathBuf {
        self.dir.join(format!("autosave.{n}.prisma"))
    }

    /// The autosave's contents, if there is one (= the last session ended unsaved).
    pub fn read(&self) -> Option<String> {
        std::fs::read_to_string(self.path()).ok()
    }

    /// Write the snapshot atomically: a temp file in the same directory, then a rename
    /// over the old autosave.
    pub fn write(&self, json: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join("autosave.prisma.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, self.path())
    }

    /// No unsaved changes: remove the autosave (missing is fine).
    pub fn clear(&self) {
        let _ = std::fs::remove_file(self.path());
    }

    /// A session is being recovered: keep a copy of its autosave as backup 1, shifting
    /// older backups down and dropping the oldest. The live autosave stays in place.
    pub fn rotate_backups(&self) {
        let _ = std::fs::remove_file(self.backup(BACKUPS));
        for n in (1..BACKUPS).rev() {
            let _ = std::fs::rename(self.backup(n), self.backup(n + 1));
        }
        let _ = std::fs::copy(self.path(), self.backup(1));
    }
}

/// What the snapshot compares against: the state the operator last saved or opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Baseline {
    /// Not taken yet: the next snapshot becomes the baseline (no changes yet).
    Pending,
    /// The snapshot of the last save / open / new show.
    Clean(String),
    /// The session was recovered from an autosave: unsaved until the next save.
    Recovered,
}

/// What the autosave thread should do with a new snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing changed since the last step.
    Idle,
    /// Unsaved changes: write this snapshot.
    Write,
    /// Back to the saved state: remove the autosave.
    Clear,
}

/// Decide one autosave step. `last` is the previous snapshot this thread handled.
/// Returns the step and whether the show is now dirty. A `Pending` baseline is taken
/// from this snapshot (so it is clean).
pub fn step(baseline: &mut Baseline, last: &mut Option<String>, snapshot: String) -> (Step, bool) {
    if *baseline == Baseline::Pending {
        *baseline = Baseline::Clean(snapshot.clone());
    }
    let dirty = match baseline {
        Baseline::Clean(saved) => *saved != snapshot,
        Baseline::Recovered => true,
        Baseline::Pending => unreachable!("taken above"),
    };
    if last.as_deref() == Some(snapshot.as_str()) {
        return (Step::Idle, dirty);
    }
    *last = Some(snapshot);
    (if dirty { Step::Write } else { Step::Clear }, dirty)
}

/// The operator saved or opened `snapshot`: it is the new clean state. `last` is
/// forgotten so the next step re-evaluates (and removes a stale autosave).
pub fn mark_clean(baseline: &mut Baseline, last: &mut Option<String>, snapshot: String) {
    *baseline = Baseline::Clean(snapshot);
    *last = None;
}

/// Whether `path` names an existing file (for the "open the last saved" offer).
pub fn exists(path: &Path) -> bool {
    path.is_file()
}

// --- The app's autosave thread ---

/// One autosave step: snapshot, write or clear the autosave, and mark the window title
/// when there are unsaved changes. Run by the autosave thread and once on exit.
pub(crate) fn autosave_tick(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let Ok(snap) = show_snapshot(&state) else {
        return;
    };
    let (step, dirty) = {
        let mut g = state.autosave_state.lock().unwrap();
        let (baseline, last) = &mut *g;
        autosave::step(baseline, last, snap.clone())
    };
    match step {
        autosave::Step::Idle => {}
        autosave::Step::Write => {
            if let Err(e) = state.autosave.write(&snap) {
                eprintln!("prisma: autosave failed: {e}");
            }
        }
        autosave::Step::Clear => state.autosave.clear(),
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.set_title(if dirty { "Prisma •" } else { "Prisma" });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prisma-autosave-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_is_atomic_and_clear_removes_it() {
        let dir = temp_dir();
        let a = Autosave::new(&dir);
        assert_eq!(a.read(), None);
        a.write("one").unwrap();
        a.write("two").unwrap();
        assert_eq!(a.read().as_deref(), Some("two"));
        assert!(
            !dir.join("autosave.prisma.tmp").exists(),
            "no temp file left behind"
        );
        a.clear();
        assert_eq!(a.read(), None);
        a.clear(); // clearing nothing is fine
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recovery_keeps_the_last_sessions_as_backups() {
        let dir = temp_dir();
        let a = Autosave::new(&dir);
        for session in ["s1", "s2", "s3"] {
            a.write(session).unwrap();
            a.rotate_backups();
        }
        let read = |n: usize| std::fs::read_to_string(a.backup(n)).ok();
        assert_eq!(read(1).as_deref(), Some("s3"));
        assert_eq!(read(2).as_deref(), Some("s2"));
        assert_eq!(read(3), None, "only two backups are kept");
        assert_eq!(a.read().as_deref(), Some("s3"), "the live autosave stays");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn steps_write_only_unsaved_changes_and_clear_when_saved_again() {
        let mut base = Baseline::Pending;
        let mut last = None;
        // The first snapshot becomes the baseline: clean, nothing to keep.
        assert_eq!(step(&mut base, &mut last, "A".into()), (Step::Clear, false));
        assert_eq!(step(&mut base, &mut last, "A".into()), (Step::Idle, false));
        // A change: write it; the same snapshot again: nothing to do.
        assert_eq!(step(&mut base, &mut last, "B".into()), (Step::Write, true));
        assert_eq!(step(&mut base, &mut last, "B".into()), (Step::Idle, true));
        // Undone by hand: back to the saved state, the autosave goes.
        assert_eq!(step(&mut base, &mut last, "A".into()), (Step::Clear, false));
        // A save makes the current snapshot the baseline (and forgets `last`, so the
        // autosave written for it is removed on the next step).
        assert_eq!(step(&mut base, &mut last, "C".into()), (Step::Write, true));
        mark_clean(&mut base, &mut last, "C".into());
        assert_eq!(step(&mut base, &mut last, "C".into()), (Step::Clear, false));
    }

    #[test]
    fn a_recovered_session_stays_dirty_until_saved() {
        let mut base = Baseline::Recovered;
        let mut last = None;
        assert_eq!(step(&mut base, &mut last, "R".into()), (Step::Write, true));
        assert_eq!(step(&mut base, &mut last, "R".into()), (Step::Idle, true));
    }
}
