use eframe::egui;

use crate::tree::FileNode;

/// High-level file category for grouping by type.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FileCategory {
    Video,
    Image,
    Audio,
    Document,
    Archive,
    Code,
    Other,
}

impl FileCategory {
    const ALL: [Self; 7] = [
        Self::Video,
        Self::Image,
        Self::Audio,
        Self::Document,
        Self::Archive,
        Self::Code,
        Self::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Video => "Videos",
            Self::Image => "Images",
            Self::Audio => "Audio",
            Self::Document => "Documents",
            Self::Archive => "Archives",
            Self::Code => "Code",
            Self::Other => "Other",
        }
    }

    pub fn color(self) -> egui::Color32 {
        match self {
            Self::Video => egui::Color32::from_rgb(192, 57, 43),
            Self::Image => egui::Color32::from_rgb(39, 174, 96),
            Self::Audio => egui::Color32::from_rgb(142, 68, 173),
            Self::Document => egui::Color32::from_rgb(41, 128, 185),
            Self::Archive => egui::Color32::from_rgb(211, 84, 0),
            Self::Code => egui::Color32::from_rgb(22, 160, 133),
            Self::Other => egui::Color32::from_rgb(93, 109, 126),
        }
    }
}

/// Categorize a file by its name/extension.
pub fn categorize(name: &str) -> FileCategory {
    let ext = name.rsplit('.').next().unwrap_or("");
    // All recognized extensions fit in seven ASCII bytes ("numbers").
    let mut lowercase = [0u8; 7];
    if ext.len() > lowercase.len() {
        return FileCategory::Other;
    }
    for (out, byte) in lowercase.iter_mut().zip(ext.bytes()) {
        *out = byte.to_ascii_lowercase();
    }
    match &lowercase[..ext.len()] {
        b"mp4" | b"mkv" | b"avi" | b"mov" | b"wmv" | b"flv" | b"webm" | b"m4v" => {
            FileCategory::Video
        }
        b"jpg" | b"jpeg" | b"png" | b"gif" | b"bmp" | b"svg" | b"webp" | b"tiff" | b"ico"
        | b"heic" => FileCategory::Image,
        b"mp3" | b"wav" | b"flac" | b"aac" | b"ogg" | b"wma" | b"m4a" | b"opus" => {
            FileCategory::Audio
        }
        b"pdf" | b"doc" | b"docx" | b"xls" | b"xlsx" | b"ppt" | b"pptx" | b"txt" | b"rtf"
        | b"csv" | b"pages" | b"numbers" | b"key" => FileCategory::Document,
        b"zip" | b"tar" | b"gz" | b"rar" | b"7z" | b"bz2" | b"xz" | b"tgz" | b"zst" | b"dmg"
        | b"iso" => FileCategory::Archive,
        b"rs" | b"js" | b"ts" | b"py" | b"go" | b"c" | b"cpp" | b"h" | b"hpp" | b"java" | b"rb"
        | b"swift" | b"kt" | b"cs" | b"jsx" | b"tsx" | b"vue" | b"svelte" | b"json" | b"yaml"
        | b"yml" | b"toml" | b"xml" | b"ini" | b"cfg" | b"conf" | b"lock" | b"html" | b"htm"
        | b"css" | b"scss" | b"sass" | b"less" | b"md" | b"mdx" => FileCategory::Code,
        _ => FileCategory::Other,
    }
}

/// Per-category statistics: (total_size, file_count).
pub struct CategoryStats {
    pub entries: Vec<(FileCategory, u64, usize)>,
}

/// Compute file category statistics from a scanned tree.
#[allow(dead_code)] // Library/benchmark entry point; the GUI uses the cancellable worker.
pub fn compute_stats(tree: &FileNode) -> CategoryStats {
    compute_stats_inner(tree, &|| false).expect("uncancelled category count")
}

pub(crate) fn compute_stats_cancellable(
    tree: &FileNode,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Option<CategoryStats> {
    compute_stats_inner(tree, &|| {
        cancelled.load(std::sync::atomic::Ordering::Relaxed)
    })
}

fn compute_stats_inner(tree: &FileNode, cancelled: &impl Fn() -> bool) -> Option<CategoryStats> {
    let mut totals = [(0, 0); FileCategory::ALL.len()];
    if !collect_stats(tree, &mut totals, cancelled) {
        return None;
    }

    let mut entries: Vec<(FileCategory, u64, usize)> = FileCategory::ALL
        .into_iter()
        .zip(totals)
        .filter(|(_, (_, count))| *count != 0)
        .map(|(cat, (size, count))| (cat, size, count))
        .collect();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.1));

    Some(CategoryStats { entries })
}

fn collect_stats(
    node: &FileNode,
    totals: &mut [(u64, usize); FileCategory::ALL.len()],
    cancelled: &impl Fn() -> bool,
) -> bool {
    if cancelled() {
        return false;
    }
    if !node.is_dir() {
        let cat = categorize(node.name());
        let entry = &mut totals[cat as usize];
        entry.0 += node.size();
        entry.1 += 1;
    }
    for child in node.children() {
        if !collect_stats(child, totals, cancelled) {
            return false;
        }
    }
    true
}

/// Returns true if this node (or any descendant) matches the given category.
pub fn node_matches_category(node: &FileNode, cat: FileCategory) -> bool {
    if !node.is_dir() {
        return categorize(node.name()) == cat;
    }
    node.children()
        .iter()
        .any(|c| node_matches_category(c, cat))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{dir, leaf};

    #[test]
    fn categorize_video() {
        assert_eq!(categorize("movie.mp4"), FileCategory::Video);
        assert_eq!(categorize("clip.MKV"), FileCategory::Video);
    }

    #[test]
    fn categorize_image() {
        assert_eq!(categorize("photo.jpg"), FileCategory::Image);
        assert_eq!(categorize("icon.PNG"), FileCategory::Image);
    }

    #[test]
    fn categorize_code() {
        assert_eq!(categorize("main.rs"), FileCategory::Code);
        assert_eq!(categorize("config.json"), FileCategory::Code);
        assert_eq!(categorize("style.css"), FileCategory::Code);
    }

    #[test]
    fn categorize_unknown() {
        assert_eq!(categorize("mystery"), FileCategory::Other);
        assert_eq!(categorize("data.xyz"), FileCategory::Other);
    }

    #[test]
    fn categorize_preserves_case_and_extension_edge_cases() {
        for (name, expected) in [
            ("budget.NuMbErS", FileCategory::Document),
            (".MP4", FileCategory::Video),
            ("archive.tar.GZ", FileCategory::Archive),
            ("rs", FileCategory::Code),
            ("", FileCategory::Other),
            ("name.", FileCategory::Other),
            ("name.numbersx", FileCategory::Other),
            ("name.İso", FileCategory::Other),
            ("name.🎥", FileCategory::Other),
        ] {
            assert_eq!(categorize(name), expected, "{name}");
        }
    }

    #[test]
    fn stats_keep_zero_byte_files_but_omit_absent_categories() {
        let empty = dir("empty", vec![]);
        assert!(compute_stats(&empty).entries.is_empty());
        let tree = dir("root", vec![dir("nested", vec![leaf("empty.zip", 0)])]);
        assert_eq!(
            compute_stats(&tree).entries,
            [(FileCategory::Archive, 0, 1)]
        );
    }

    #[test]
    fn stats_counts_files() {
        let tree = dir(
            "root",
            vec![
                leaf("movie.mp4", 1000),
                leaf("song.mp3", 500),
                leaf("photo.jpg", 200),
                leaf("readme.md", 50),
            ],
        );
        let stats = compute_stats(&tree);
        assert!(!stats.entries.is_empty());
        // First entry should be largest (video = 1000)
        assert_eq!(stats.entries[0].0, FileCategory::Video);
        assert_eq!(stats.entries[0].1, 1000);
        assert_eq!(stats.entries[0].2, 1);
    }

    #[test]
    fn stats_aggregates_category() {
        let tree = dir(
            "root",
            vec![leaf("a.rs", 100), leaf("b.py", 200), leaf("c.toml", 50)],
        );
        let stats = compute_stats(&tree);
        // All are Code category
        assert_eq!(stats.entries.len(), 1);
        assert_eq!(stats.entries[0].0, FileCategory::Code);
        assert_eq!(stats.entries[0].1, 350);
        assert_eq!(stats.entries[0].2, 3);
    }

    #[test]
    fn stats_sorted_by_size() {
        let tree = dir(
            "root",
            vec![
                leaf("small.txt", 10),
                leaf("big.mp4", 9999),
                leaf("medium.zip", 500),
            ],
        );
        let stats = compute_stats(&tree);
        // Should be sorted descending by size
        for i in 1..stats.entries.len() {
            assert!(stats.entries[i - 1].1 >= stats.entries[i].1);
        }
    }

    #[test]
    fn node_matches_category_file() {
        let node = leaf("video.mp4", 100);
        assert!(node_matches_category(&node, FileCategory::Video));
        assert!(!node_matches_category(&node, FileCategory::Audio));
    }

    #[test]
    fn node_matches_category_dir() {
        let tree = dir("root", vec![leaf("song.mp3", 50), leaf("readme.md", 10)]);
        assert!(node_matches_category(&tree, FileCategory::Audio));
        assert!(node_matches_category(&tree, FileCategory::Code));
        assert!(!node_matches_category(&tree, FileCategory::Video));
    }
}
