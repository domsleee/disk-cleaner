//! Category totals computed against a shared scan tree without blocking rendering.

use crate::{
    categories::{self, CategoryStats},
    tree::FileNode,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

#[derive(Default)]
pub struct CategoryWorker {
    receiver: Option<mpsc::Receiver<Option<CategoryStats>>>,
    cancelled: Arc<AtomicBool>,
}

pub enum Poll {
    Pending,
    Complete(Option<CategoryStats>),
    Failed,
}

impl CategoryWorker {
    pub fn is_active(&self) -> bool {
        self.receiver.is_some()
    }

    pub fn start(&mut self, tree: Arc<FileNode>) {
        assert!(!self.is_active());
        let (tx, rx) = mpsc::channel();
        self.cancelled = Arc::new(AtomicBool::new(false));
        let cancelled = self.cancelled.clone();
        std::thread::spawn(move || {
            let stats = categories::compute_stats_cancellable(&tree, &cancelled);
            // Release the read-only tree before notifying the UI, so edits can resume.
            drop(tree);
            let _ = tx.send(stats);
        });
        self.receiver = Some(rx);
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn poll(&mut self) -> Poll {
        let Some(rx) = &self.receiver else {
            return Poll::Pending;
        };
        match rx.try_recv() {
            Ok(stats) => {
                self.receiver = None;
                // Also reject a result that finished just before cancellation.
                Poll::Complete(if self.cancelled.load(Ordering::Relaxed) {
                    None
                } else {
                    stats
                })
            }
            Err(mpsc::TryRecvError::Empty) => Poll::Pending,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.receiver = None;
                if self.cancelled.load(Ordering::Relaxed) {
                    Poll::Complete(None)
                } else {
                    Poll::Failed
                }
            }
        }
    }
}

impl Drop for CategoryWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{dir, leaf};

    fn finish(worker: &mut CategoryWorker) -> Option<CategoryStats> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match worker.poll() {
                Poll::Complete(stats) => return stats,
                Poll::Failed => panic!("category worker disconnected"),
                Poll::Pending => {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }
    }

    #[test]
    fn returns_totals_and_releases_tree_for_editing() {
        let mut tree = Arc::new(dir(
            "root",
            vec![leaf("movie.mp4", 200), leaf("empty.zip", 0)],
        ));
        let mut worker = CategoryWorker::default();
        worker.start(tree.clone());
        let stats = finish(&mut worker).unwrap();
        assert_eq!(stats.entries, categories::compute_stats(&tree).entries);
        assert!(Arc::get_mut(&mut tree).is_some());
        assert!(!worker.is_active());
    }

    #[test]
    fn cancelled_result_is_discarded_and_next_job_succeeds() {
        let mut worker = CategoryWorker::default();
        let old = Arc::new(dir("old", vec![leaf("old.zip", 200)]));
        worker.start(old.clone());
        worker.cancel();
        assert!(finish(&mut worker).is_none());
        assert_eq!(Arc::strong_count(&old), 1);
        worker.start(Arc::new(dir("new", vec![leaf("new.txt", 10)])));
        let stats = finish(&mut worker).unwrap();
        assert_eq!(stats.entries, [(categories::FileCategory::Document, 10, 1)]);
    }
}
