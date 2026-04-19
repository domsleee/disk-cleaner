use std::collections::HashMap;

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
    match ext.to_ascii_lowercase().as_str() {
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" => FileCategory::Video,
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "svg" | "webp" | "tiff" | "ico" | "heic" => {
            FileCategory::Image
        }
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "wma" | "m4a" | "opus" => FileCategory::Audio,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "rtf" | "csv"
        | "pages" | "numbers" | "key" => FileCategory::Document,
        "zip" | "tar" | "gz" | "rar" | "7z" | "bz2" | "xz" | "tgz" | "zst" | "dmg" | "iso" => {
            FileCategory::Archive
        }
        "rs" | "js" | "ts" | "py" | "go" | "c" | "cpp" | "h" | "hpp" | "java" | "rb" | "swift"
        | "kt" | "cs" | "jsx" | "tsx" | "vue" | "svelte" | "json" | "yaml" | "yml" | "toml"
        | "xml" | "ini" | "cfg" | "conf" | "lock" | "html" | "htm" | "css" | "scss" | "sass"
        | "less" | "md" | "mdx" => FileCategory::Code,
        _ => FileCategory::Other,
    }
}

/// Per-category statistics: (total_size, file_count).
pub struct CategoryStats {
    pub entries: Vec<(FileCategory, u64, usize)>,
}

/// Compute file category statistics from a scanned tree.
pub fn compute_stats(tree: &FileNode) -> CategoryStats {
    let mut map: HashMap<FileCategory, (u64, usize)> = HashMap::new();
    collect_stats(tree, &mut map);

    let mut entries: Vec<(FileCategory, u64, usize)> = map
        .into_iter()
        .map(|(cat, (size, count))| (cat, size, count))
        .collect();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.1));

    CategoryStats { entries }
}

fn collect_stats(node: &FileNode, map: &mut HashMap<FileCategory, (u64, usize)>) {
    if !node.is_dir() {
        let cat = categorize(node.name());
        let entry = map.entry(cat).or_insert((0, 0));
        entry.0 += node.size();
        entry.1 += 1;
    }
    for child in node.children() {
        collect_stats(child, map);
    }
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
