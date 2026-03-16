use std::path::PathBuf;

pub struct FileNode {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub is_dir: bool,
    pub children: Vec<FileNode>,
    pub expanded: bool,
    pub selected: bool,
}

/// Auto-expand directories that represent >25% of their parent's size,
/// up to `max_depth` levels deep from the given node.
pub fn auto_expand(node: &mut FileNode, depth: usize, max_depth: usize) {
    if depth >= max_depth || !node.is_dir || node.size == 0 {
        return;
    }
    for child in &mut node.children {
        if child.is_dir && child.size * 4 >= node.size {
            child.expanded = true;
            auto_expand(child, depth + 1, max_depth);
        }
    }
}

#[cfg(test)]
pub fn leaf(name: &str, size: u64) -> FileNode {
    FileNode {
        name: name.to_string(),
        path: PathBuf::from(name),
        size,
        is_dir: false,
        children: Vec::new(),
        expanded: false,
        selected: false,
    }
}

#[cfg(test)]
pub fn dir(name: &str, children: Vec<FileNode>) -> FileNode {
    let size = children.iter().map(|c| c.size).sum();
    FileNode {
        name: name.to_string(),
        path: PathBuf::from(name),
        size,
        is_dir: true,
        children,
        expanded: false,
        selected: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_expand_expands_large_children() {
        // root (400): big_dir (300, 75%), small_dir (100, 25%)
        let mut root = dir("root", vec![
            dir("big_dir", vec![leaf("a.txt", 300)]),
            dir("small_dir", vec![leaf("b.txt", 100)]),
        ]);
        root.expanded = true;

        auto_expand(&mut root, 0, 2);

        assert!(root.children[0].expanded, "big_dir should be expanded (75%)");
        assert!(root.children[1].expanded, "small_dir should be expanded (25%)");
    }

    #[test]
    fn auto_expand_skips_small_children() {
        // root (400): big_dir (300), tiny_dir (10), rest is a file
        let mut root = dir("root", vec![
            dir("big_dir", vec![leaf("a.txt", 300)]),
            dir("tiny_dir", vec![leaf("b.txt", 10)]),
            leaf("c.txt", 90),
        ]);
        root.expanded = true;

        auto_expand(&mut root, 0, 2);

        assert!(root.children[0].expanded, "big_dir should be expanded");
        assert!(!root.children[1].expanded, "tiny_dir should NOT be expanded (2.5%)");
    }

    #[test]
    fn auto_expand_respects_max_depth() {
        // 3-level deep tree, but max_depth=1 should only expand first level
        let mut root = dir("root", vec![
            dir("lvl1", vec![
                dir("lvl2", vec![leaf("deep.txt", 100)]),
            ]),
        ]);
        root.expanded = true;

        auto_expand(&mut root, 0, 1);

        assert!(root.children[0].expanded, "lvl1 should be expanded");
        assert!(!root.children[0].children[0].expanded, "lvl2 should NOT be expanded (depth limit)");
    }

    #[test]
    fn auto_expand_recurses_into_expanded_children() {
        let mut root = dir("root", vec![
            dir("lvl1", vec![
                dir("lvl2", vec![leaf("big.txt", 800)]),
                leaf("small.txt", 10),
            ]),
        ]);
        root.expanded = true;

        auto_expand(&mut root, 0, 2);

        assert!(root.children[0].expanded, "lvl1 expanded");
        assert!(root.children[0].children[0].expanded, "lvl2 expanded (98% of parent)");
    }
}
