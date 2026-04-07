//! Background file deletion — runs on a worker thread with progress tracking.
//!
//! Extracted from App so the logic is independently testable without egui.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

/// Result of background deletion: list of (path, optional error message).
pub type DeleteResults = Vec<(PathBuf, Option<String>)>;

/// Outcome from [`BackgroundDeleter::poll`].
pub enum PollResult {
    /// Deletion still running (or nothing was started).
    Pending,
    /// Deletion finished — contains per-path results.
    Done(DeleteResults),
}

/// Manages a single background deletion job at a time.
pub struct BackgroundDeleter {
    active: bool,
    progress: Arc<AtomicUsize>,
    total: usize,
    receiver: Option<mpsc::Receiver<DeleteResults>>,
}

impl Default for BackgroundDeleter {
    fn default() -> Self {
        Self {
            active: false,
            progress: Arc::new(AtomicUsize::new(0)),
            total: 0,
            receiver: None,
        }
    }
}

impl BackgroundDeleter {
    /// Whether a deletion job is currently running.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Number of items processed so far (atomically updated by worker thread).
    pub fn done_count(&self) -> usize {
        self.progress.load(Ordering::Relaxed)
    }

    /// Total number of items in the current job.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Start a background deletion job.
    ///
    /// `use_trash` — when true, move to OS trash instead of permanent delete.
    /// No-op if `paths` is empty or a job is already running.
    pub fn start(&mut self, paths: Vec<PathBuf>, use_trash: bool) {
        if paths.is_empty() || self.active {
            return;
        }
        let total = paths.len();
        let progress = Arc::new(AtomicUsize::new(0));
        let progress_clone = Arc::clone(&progress);
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let mut results: Vec<(PathBuf, Option<String>)> = Vec::with_capacity(total);
            for path in paths {
                let result = if use_trash {
                    trash::delete(&path).map_err(|e| e.to_string())
                } else if path.is_dir() {
                    std::fs::remove_dir_all(&path).map_err(|e| e.to_string())
                } else {
                    std::fs::remove_file(&path).map_err(|e| e.to_string())
                };
                let err = result.err();
                results.push((path, err));
                progress_clone.fetch_add(1, Ordering::Relaxed);
            }
            let _ = tx.send(results);
        });

        self.active = true;
        self.progress = progress;
        self.total = total;
        self.receiver = Some(rx);
    }

    /// Non-blocking poll. Returns [`PollResult::Done`] exactly once per job.
    pub fn poll(&mut self) -> PollResult {
        if !self.active {
            return PollResult::Pending;
        }
        if let Some(ref rx) = self.receiver {
            if let Ok(results) = rx.try_recv() {
                self.active = false;
                self.receiver = None;
                return PollResult::Done(results);
            }
        }
        PollResult::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: create a temp file and return its path.
    fn make_file(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
        let p = dir.path().join(name);
        fs::write(&p, contents).unwrap();
        p
    }

    /// Helper: create a temp subdirectory with a file inside.
    fn make_dir_with_file(dir: &TempDir, dirname: &str, filename: &str) -> PathBuf {
        let d = dir.path().join(dirname);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(filename), "x").unwrap();
        d
    }

    /// Helper: spin-poll until deletion finishes (with timeout).
    fn poll_until_done(deleter: &mut BackgroundDeleter) -> DeleteResults {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match deleter.poll() {
                PollResult::Done(results) => return results,
                PollResult::Pending => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "deletion timed out after 5s"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }

    #[test]
    fn delete_single_file() {
        let tmp = TempDir::new().unwrap();
        let f = make_file(&tmp, "delete_me.txt", "gone");

        let mut d = BackgroundDeleter::default();
        d.start(vec![f.clone()], false);

        assert!(d.is_active());
        let results = poll_until_done(&mut d);
        assert!(!d.is_active());

        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_none(), "expected no error");
        assert!(!f.exists(), "file should be deleted");
    }

    #[test]
    fn delete_directory_recursively() {
        let tmp = TempDir::new().unwrap();
        let dir = make_dir_with_file(&tmp, "subdir", "inner.txt");

        let mut d = BackgroundDeleter::default();
        d.start(vec![dir.clone()], false);

        let results = poll_until_done(&mut d);
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_none());
        assert!(!dir.exists(), "directory should be removed");
    }

    #[test]
    fn delete_multiple_files_batch() {
        let tmp = TempDir::new().unwrap();
        let f1 = make_file(&tmp, "a.txt", "a");
        let f2 = make_file(&tmp, "b.txt", "b");
        let f3 = make_file(&tmp, "c.txt", "c");

        let mut d = BackgroundDeleter::default();
        d.start(vec![f1.clone(), f2.clone(), f3.clone()], false);

        assert_eq!(d.total(), 3);
        let results = poll_until_done(&mut d);

        assert_eq!(results.len(), 3);
        for (_, err) in &results {
            assert!(err.is_none());
        }
        assert!(!f1.exists());
        assert!(!f2.exists());
        assert!(!f3.exists());
    }

    #[test]
    fn progress_counter_increments() {
        let tmp = TempDir::new().unwrap();
        let f1 = make_file(&tmp, "a.txt", "a");
        let f2 = make_file(&tmp, "b.txt", "b");

        let mut d = BackgroundDeleter::default();
        d.start(vec![f1, f2], false);

        let results = poll_until_done(&mut d);
        // After completion, progress should equal total
        assert_eq!(d.done_count(), 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn nonexistent_path_reports_error() {
        let tmp = TempDir::new().unwrap();
        let ghost = tmp.path().join("does_not_exist.txt");

        let mut d = BackgroundDeleter::default();
        d.start(vec![ghost.clone()], false);

        let results = poll_until_done(&mut d);
        assert_eq!(results.len(), 1);
        assert!(
            results[0].1.is_some(),
            "should report error for missing file"
        );
    }

    #[test]
    fn mixed_success_and_failure() {
        let tmp = TempDir::new().unwrap();
        let good = make_file(&tmp, "real.txt", "data");
        let bad = tmp.path().join("phantom.txt");

        let mut d = BackgroundDeleter::default();
        d.start(vec![good.clone(), bad.clone()], false);

        let results = poll_until_done(&mut d);
        assert_eq!(results.len(), 2);

        // First path (real file) should succeed
        assert_eq!(results[0].0, good);
        assert!(results[0].1.is_none());

        // Second path (missing) should fail
        assert_eq!(results[1].0, bad);
        assert!(results[1].1.is_some());
    }

    #[test]
    fn empty_paths_is_noop() {
        let mut d = BackgroundDeleter::default();
        d.start(vec![], false);

        // Should not start
        assert!(!d.is_active());
        assert_eq!(d.total(), 0);
    }

    #[test]
    fn second_start_while_active_is_noop() {
        let tmp = TempDir::new().unwrap();
        let f1 = make_file(&tmp, "first.txt", "1");
        let f2 = make_file(&tmp, "second.txt", "2");

        let mut d = BackgroundDeleter::default();
        d.start(vec![f1.clone()], false);
        assert!(d.is_active());
        assert_eq!(d.total(), 1);

        // Second start should be ignored
        d.start(vec![f2.clone()], false);
        assert_eq!(d.total(), 1, "should still be first job");

        let results = poll_until_done(&mut d);
        assert_eq!(results.len(), 1);
        assert!(!f1.exists(), "first file deleted");
        assert!(f2.exists(), "second file untouched");
    }

    #[test]
    fn can_start_new_job_after_completion() {
        let tmp = TempDir::new().unwrap();
        let f1 = make_file(&tmp, "first.txt", "1");
        let f2 = make_file(&tmp, "second.txt", "2");

        let mut d = BackgroundDeleter::default();

        // First job
        d.start(vec![f1.clone()], false);
        let r1 = poll_until_done(&mut d);
        assert_eq!(r1.len(), 1);
        assert!(!f1.exists());

        // Second job should work fine
        d.start(vec![f2.clone()], false);
        assert!(d.is_active());
        let r2 = poll_until_done(&mut d);
        assert_eq!(r2.len(), 1);
        assert!(!f2.exists());
    }

    #[test]
    fn poll_returns_pending_when_idle() {
        let mut d = BackgroundDeleter::default();
        assert!(matches!(d.poll(), PollResult::Pending));
    }

    #[test]
    fn poll_returns_done_exactly_once() {
        let tmp = TempDir::new().unwrap();
        let f = make_file(&tmp, "once.txt", "x");

        let mut d = BackgroundDeleter::default();
        d.start(vec![f], false);

        let results = poll_until_done(&mut d);
        assert_eq!(results.len(), 1);

        // Second poll should be Pending (results already consumed)
        assert!(matches!(d.poll(), PollResult::Pending));
    }
}
