use std::path::PathBuf;

use crate::tree::FileNode;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SafetyLevel {
    Safe,
    Caution,
}

impl SafetyLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Safe => "Safe to delete",
            Self::Caution => "Review first",
        }
    }

    pub fn color(self) -> eframe::egui::Color32 {
        match self {
            Self::Safe => eframe::egui::Color32::from_rgb(39, 174, 96),
            Self::Caution => eframe::egui::Color32::from_rgb(220, 150, 50),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SuggestionCategory {
    BuildArtifacts,
    PackageCaches,
    SystemCaches,
    IdeArtifacts,
    TempFiles,
    OldInstallers,
}

impl SuggestionCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::BuildArtifacts => "Build Artifacts",
            Self::PackageCaches => "Package Caches",
            Self::SystemCaches => "System Caches",
            Self::IdeArtifacts => "IDE Artifacts",
            Self::TempFiles => "Temp & Log Files",
            Self::OldInstallers => "Old Installers",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::BuildArtifacts => "Compiler output, build directories that can be regenerated",
            Self::PackageCaches => "Downloaded dependencies that can be re-fetched",
            Self::SystemCaches => "Application and system caches",
            Self::IdeArtifacts => "IDE indexes, settings caches, and debug symbols",
            Self::TempFiles => "Temporary files, logs, crash reports, and backups",
            Self::OldInstallers => "Disk images and installer packages",
        }
    }

    pub fn safety(self) -> SafetyLevel {
        match self {
            Self::BuildArtifacts => SafetyLevel::Safe,
            Self::PackageCaches => SafetyLevel::Safe,
            Self::SystemCaches => SafetyLevel::Caution,
            Self::IdeArtifacts => SafetyLevel::Caution,
            Self::TempFiles => SafetyLevel::Safe,
            Self::OldInstallers => SafetyLevel::Caution,
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::BuildArtifacts => "\u{1F3D7}", // construction
            Self::PackageCaches => "\u{1F4E6}",  // package
            Self::SystemCaches => "\u{1F5C4}",   // file cabinet
            Self::IdeArtifacts => "\u{1F4BB}",   // laptop
            Self::TempFiles => "\u{1F5D1}",      // wastebasket
            Self::OldInstallers => "\u{1F4BF}",  // disc
        }
    }
}

/// A single detected item (directory or file) that can be cleaned.
pub struct SuggestionItem {
    pub path: PathBuf,
    pub size: u64,
}

/// A group of suggestions under one category.
pub struct SuggestionGroup {
    pub category: SuggestionCategory,
    pub items: Vec<SuggestionItem>,
    pub total_size: u64,
    pub expanded: bool,
}

/// All detected suggestions after analyzing a scan.
pub struct SuggestionReport {
    pub groups: Vec<SuggestionGroup>,
    pub total_reclaimable: u64,
}

/// Directory names that indicate build artifacts.
const BUILD_DIRS: &[&str] = &[
    "target",
    "build",
    "dist",
    "__pycache__",
    ".gradle",
    "DerivedData",
    ".next",
    ".nuxt",
    "out",
    ".build",
    "cmake-build-debug",
    "cmake-build-release",
];

/// Directory names that indicate package caches.
const PACKAGE_DIRS: &[&str] = &[
    "node_modules",
    ".venv",
    "venv",
    "vendor",
    "Pods",
    ".tox",
    "bower_components",
    "__pypackages__",
];

/// Directory names that indicate system caches.
const CACHE_DIRS: &[&str] = &["Caches", ".cache", "Cache", "CacheStorage", "GPUCache"];

/// Directory names that indicate IDE artifacts.
const IDE_DIRS: &[&str] = &[".idea", ".vs", ".vscode", ".fleet"];

/// File extensions for temp/log files.
const TEMP_EXTENSIONS: &[&str] = &["tmp", "temp", "log", "bak", "swp", "swo"];

/// File extensions for old installers.
const INSTALLER_EXTENSIONS: &[&str] = &["dmg", "pkg", "iso", "msi", "exe"];

fn matches_dir_pattern(name: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| name.eq_ignore_ascii_case(p))
}

fn has_extension(name: &str, extensions: &[&str]) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("");
    extensions.iter().any(|e| ext.eq_ignore_ascii_case(e))
}

fn is_dsym(name: &str) -> bool {
    name.ends_with(".dSYM")
}

/// Walk the scanned tree and detect reclaimable items.
pub fn analyze(tree: &FileNode) -> SuggestionReport {
    let mut build_items = Vec::new();
    let mut package_items = Vec::new();
    let mut cache_items = Vec::new();
    let mut ide_items = Vec::new();
    let mut temp_items = Vec::new();
    let mut installer_items = Vec::new();

    let mut path_buf = PathBuf::from(tree.name());
    walk_for_suggestions(
        tree,
        &mut path_buf,
        &mut build_items,
        &mut package_items,
        &mut cache_items,
        &mut ide_items,
        &mut temp_items,
        &mut installer_items,
    );

    let mut groups = Vec::new();

    for (category, items) in [
        (SuggestionCategory::BuildArtifacts, build_items),
        (SuggestionCategory::PackageCaches, package_items),
        (SuggestionCategory::SystemCaches, cache_items),
        (SuggestionCategory::IdeArtifacts, ide_items),
        (SuggestionCategory::TempFiles, temp_items),
        (SuggestionCategory::OldInstallers, installer_items),
    ] {
        if !items.is_empty() {
            let total_size = items.iter().map(|i| i.size).sum();
            groups.push(SuggestionGroup {
                category,
                items,
                total_size,
                expanded: false,
            });
        }
    }

    groups.sort_by(|a, b| b.total_size.cmp(&a.total_size));
    let total_reclaimable = groups.iter().map(|g| g.total_size).sum();

    SuggestionReport {
        groups,
        total_reclaimable,
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_for_suggestions(
    node: &FileNode,
    current_path: &mut PathBuf,
    build_items: &mut Vec<SuggestionItem>,
    package_items: &mut Vec<SuggestionItem>,
    cache_items: &mut Vec<SuggestionItem>,
    ide_items: &mut Vec<SuggestionItem>,
    temp_items: &mut Vec<SuggestionItem>,
    installer_items: &mut Vec<SuggestionItem>,
) {
    let name = node.name();

    if node.is_dir() {
        // Check if this directory matches a known pattern.
        // If it does, add it as a single suggestion item and DON'T recurse
        // into it (to avoid double-counting children).
        if matches_dir_pattern(name, BUILD_DIRS) && node.size() > 0 {
            build_items.push(SuggestionItem {
                path: current_path.clone(),
                size: node.size(),
            });
            return;
        }
        if matches_dir_pattern(name, PACKAGE_DIRS) && node.size() > 0 {
            package_items.push(SuggestionItem {
                path: current_path.clone(),
                size: node.size(),
            });
            return;
        }
        if matches_dir_pattern(name, CACHE_DIRS) && node.size() > 0 {
            cache_items.push(SuggestionItem {
                path: current_path.clone(),
                size: node.size(),
            });
            return;
        }
        if matches_dir_pattern(name, IDE_DIRS) && node.size() > 0 {
            ide_items.push(SuggestionItem {
                path: current_path.clone(),
                size: node.size(),
            });
            return;
        }
        if is_dsym(name) && node.size() > 0 {
            ide_items.push(SuggestionItem {
                path: current_path.clone(),
                size: node.size(),
            });
            return;
        }

        // Not a known directory pattern — recurse into children
        for child in node.children() {
            current_path.push(child.name());
            walk_for_suggestions(
                child,
                current_path,
                build_items,
                package_items,
                cache_items,
                ide_items,
                temp_items,
                installer_items,
            );
            current_path.pop();
        }
    } else {
        // File-level checks
        if has_extension(name, TEMP_EXTENSIONS) && node.size() > 0 {
            temp_items.push(SuggestionItem {
                path: current_path.clone(),
                size: node.size(),
            });
        } else if has_extension(name, INSTALLER_EXTENSIONS) && node.size() > 0 {
            installer_items.push(SuggestionItem {
                path: current_path.clone(),
                size: node.size(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{dir, leaf};

    #[test]
    fn detect_build_artifacts() {
        let tree = dir(
            "/test",
            vec![
                dir("target", vec![leaf("debug.o", 1000)]),
                dir("src", vec![leaf("main.rs", 50)]),
            ],
        );
        let report = analyze(&tree);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(
            report.groups[0].category,
            SuggestionCategory::BuildArtifacts
        );
        assert_eq!(report.groups[0].total_size, 1000);
        assert_eq!(report.total_reclaimable, 1000);
    }

    #[test]
    fn detect_node_modules() {
        let tree = dir(
            "/test",
            vec![
                dir("node_modules", vec![leaf("react.js", 5000)]),
                leaf("index.js", 100),
            ],
        );
        let report = analyze(&tree);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].category, SuggestionCategory::PackageCaches);
        assert_eq!(report.groups[0].total_size, 5000);
    }

    #[test]
    fn detect_temp_files() {
        let tree = dir(
            "/test",
            vec![
                leaf("debug.log", 200),
                leaf("data.tmp", 100),
                leaf("main.rs", 50),
            ],
        );
        let report = analyze(&tree);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].category, SuggestionCategory::TempFiles);
        assert_eq!(report.groups[0].total_size, 300);
    }

    #[test]
    fn detect_installers() {
        let tree = dir(
            "/test",
            vec![leaf("Xcode.dmg", 8_000_000), leaf("notes.txt", 50)],
        );
        let report = analyze(&tree);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].category, SuggestionCategory::OldInstallers);
        assert_eq!(report.groups[0].total_size, 8_000_000);
    }

    #[test]
    fn no_double_count_nested_targets() {
        let tree = dir(
            "/test",
            vec![dir(
                "project",
                vec![dir("target", vec![dir("debug", vec![leaf("bin", 500)])])],
            )],
        );
        let report = analyze(&tree);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].items.len(), 1);
        assert_eq!(report.groups[0].total_size, 500);
    }

    #[test]
    fn empty_tree_no_suggestions() {
        let tree = dir("/test", vec![]);
        let report = analyze(&tree);
        assert!(report.groups.is_empty());
        assert_eq!(report.total_reclaimable, 0);
    }

    #[test]
    fn multiple_categories_sorted_by_size() {
        let tree = dir(
            "/test",
            vec![
                dir("node_modules", vec![leaf("pkg.js", 10_000)]),
                dir("target", vec![leaf("debug.o", 500)]),
                leaf("crash.log", 50),
            ],
        );
        let report = analyze(&tree);
        assert_eq!(report.groups.len(), 3);
        // Sorted by size descending
        assert!(report.groups[0].total_size >= report.groups[1].total_size);
        assert!(report.groups[1].total_size >= report.groups[2].total_size);
        assert_eq!(report.total_reclaimable, 10_550);
    }
}
