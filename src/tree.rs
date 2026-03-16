use std::path::{Path, PathBuf};

pub struct FileLeaf {
    pub path: PathBuf,
    pub size: u64,
    pub selected: bool,
}

pub struct DirNode {
    pub path: PathBuf,
    pub size: u64,
    pub children: Vec<FileNode>,
    pub expanded: bool,
    pub selected: bool,
}

pub enum FileNode {
    File(FileLeaf),
    Dir(DirNode),
}

impl FileNode {
    /// Derive display name from the path's final component.
    /// Falls back to the full path string for root paths like "/".
    pub fn name(&self) -> &str {
        let p = self.path();
        p.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| p.to_str().unwrap_or(""))
    }

    pub fn path(&self) -> &Path {
        match self {
            FileNode::File(f) => &f.path,
            FileNode::Dir(d) => &d.path,
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            FileNode::File(f) => f.size,
            FileNode::Dir(d) => d.size,
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self, FileNode::Dir(_))
    }

    pub fn children(&self) -> &[FileNode] {
        match self {
            FileNode::File(_) => &[],
            FileNode::Dir(d) => &d.children,
        }
    }

    pub fn expanded(&self) -> bool {
        match self {
            FileNode::File(_) => false,
            FileNode::Dir(d) => d.expanded,
        }
    }

    pub fn selected(&self) -> bool {
        match self {
            FileNode::File(f) => f.selected,
            FileNode::Dir(d) => d.selected,
        }
    }

    pub fn set_expanded(&mut self, val: bool) {
        if let FileNode::Dir(d) = self {
            d.expanded = val;
        }
    }

    pub fn set_selected(&mut self, val: bool) {
        match self {
            FileNode::File(f) => f.selected = val,
            FileNode::Dir(d) => d.selected = val,
        }
    }

    pub fn as_dir_mut(&mut self) -> Option<&mut DirNode> {
        match self {
            FileNode::Dir(d) => Some(d),
            FileNode::File(_) => None,
        }
    }
}

/// Auto-expand directories that represent >25% of their parent's size,
/// up to `max_depth` levels deep from the given node.
pub fn auto_expand(node: &mut FileNode, depth: usize, max_depth: usize) {
    if depth >= max_depth || !node.is_dir() || node.size() == 0 {
        return;
    }
    let parent_size = node.size();
    if let FileNode::Dir(d) = node {
        for child in &mut d.children {
            if child.is_dir() && child.size() * 4 >= parent_size {
                child.set_expanded(true);
                auto_expand(child, depth + 1, max_depth);
            }
        }
    }
}

#[cfg(test)]
pub fn leaf(name: &str, size: u64) -> FileNode {
    FileNode::File(FileLeaf {
        path: PathBuf::from(name),
        size,
        selected: false,
    })
}

#[cfg(test)]
pub fn dir(name: &str, children: Vec<FileNode>) -> FileNode {
    let size = children.iter().map(|c| c.size()).sum();
    FileNode::Dir(DirNode {
        path: PathBuf::from(name),
        size,
        children,
        expanded: false,
        selected: false,
    })
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
        root.set_expanded(true);

        auto_expand(&mut root, 0, 2);

        assert!(root.children()[0].expanded(), "big_dir should be expanded (75%)");
        assert!(root.children()[1].expanded(), "small_dir should be expanded (25%)");
    }

    #[test]
    fn auto_expand_skips_small_children() {
        // root (400): big_dir (300), tiny_dir (10), rest is a file
        let mut root = dir("root", vec![
            dir("big_dir", vec![leaf("a.txt", 300)]),
            dir("tiny_dir", vec![leaf("b.txt", 10)]),
            leaf("c.txt", 90),
        ]);
        root.set_expanded(true);

        auto_expand(&mut root, 0, 2);

        assert!(root.children()[0].expanded(), "big_dir should be expanded");
        assert!(!root.children()[1].expanded(), "tiny_dir should NOT be expanded (2.5%)");
    }

    #[test]
    fn auto_expand_respects_max_depth() {
        // 3-level deep tree, but max_depth=1 should only expand first level
        let mut root = dir("root", vec![
            dir("lvl1", vec![
                dir("lvl2", vec![leaf("deep.txt", 100)]),
            ]),
        ]);
        root.set_expanded(true);

        auto_expand(&mut root, 0, 1);

        assert!(root.children()[0].expanded(), "lvl1 should be expanded");
        assert!(!root.children()[0].children()[0].expanded(), "lvl2 should NOT be expanded (depth limit)");
    }

    #[test]
    fn auto_expand_recurses_into_expanded_children() {
        let mut root = dir("root", vec![
            dir("lvl1", vec![
                dir("lvl2", vec![leaf("big.txt", 800)]),
                leaf("small.txt", 10),
            ]),
        ]);
        root.set_expanded(true);

        auto_expand(&mut root, 0, 2);

        assert!(root.children()[0].expanded(), "lvl1 expanded");
        assert!(root.children()[0].children()[0].expanded(), "lvl2 expanded (98% of parent)");
    }
}
