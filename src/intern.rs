use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

/// A reference-counted interned path string. Multiple caches and UI structs
/// share the same `Arc<str>` for identical paths, eliminating redundant
/// `PathBuf` allocations.
pub type InternedPath = Arc<str>;

/// Deduplicates path strings so that identical paths share one `Arc<str>`
/// allocation. Call [`intern`] with any `&Path`; the interner converts to
/// a UTF-8 string (lossy) and returns a cheap-to-clone `Arc<str>`.
///
/// Typical usage: create one `PathInterner` on the `App` struct and pass it
/// to `collect_cached_rows`, `build_text_match_cache`, etc.
#[derive(Default)]
pub struct PathInterner {
    pool: HashSet<Arc<str>>,
}

impl PathInterner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern a `&Path`, returning a shared `Arc<str>`.
    pub fn intern(&mut self, path: &Path) -> Arc<str> {
        self.intern_str(&path.to_string_lossy())
    }

    /// Intern a raw string slice.
    pub fn intern_str(&mut self, s: &str) -> Arc<str> {
        if let Some(existing) = self.pool.get(s) {
            return Arc::clone(existing);
        }
        let arc: Arc<str> = Arc::from(s);
        self.pool.insert(Arc::clone(&arc));
        arc
    }

    /// Drop all interned strings. Call when starting a new scan so stale
    /// paths from the previous tree don't linger.
    pub fn clear(&mut self) {
        self.pool.clear();
    }
}
