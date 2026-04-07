//! Compact tree node — stores only the filename (not the full path) to
//! reduce per-node memory.  Full paths are reconstructed during traversal
//! by joining ancestor names.  The root node's name is the absolute scan
//! path so that reconstruction produces correct absolute paths.

/// Bit 63 of the size field stores the hidden flag.
/// Max representable size: 2^63 − 1 ≈ 9.2 EB (more than enough).
const HIDDEN_BIT: u64 = 1 << 63;

#[derive(Clone)]
pub struct FileLeaf {
    pub name: Box<str>,
    /// Lower 63 bits: file size in bytes. Bit 63: hidden flag.
    size_hidden: u64,
}

impl FileLeaf {
    #[inline]
    pub fn new(name: Box<str>, size: u64, hidden: bool) -> Self {
        Self {
            name,
            size_hidden: size | if hidden { HIDDEN_BIT } else { 0 },
        }
    }

    #[inline]
    pub fn size(&self) -> u64 {
        self.size_hidden & !HIDDEN_BIT
    }

    #[inline]
    pub fn is_hidden(&self) -> bool {
        self.size_hidden & HIDDEN_BIT != 0
    }
}

pub struct DirNode {
    pub name: Box<str>,
    pub size: u64,
    pub children: Vec<FileNode>,
    pub expanded: bool,
    /// True when the directory is hidden (dotfile or OS-level UF_HIDDEN flag).
    pub hidden: bool,
}

pub enum FileNode {
    File(FileLeaf),
    Dir(Box<DirNode>),
}

impl FileNode {
    pub fn name(&self) -> &str {
        match self {
            FileNode::File(f) => &f.name,
            FileNode::Dir(d) => &d.name,
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            FileNode::File(f) => f.size(),
            FileNode::Dir(d) => d.size,
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self, FileNode::Dir(_))
    }

    pub fn is_hidden(&self) -> bool {
        match self {
            FileNode::File(f) => f.is_hidden(),
            FileNode::Dir(d) => d.hidden,
        }
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

    pub fn set_expanded(&mut self, val: bool) {
        if let FileNode::Dir(d) = self {
            d.expanded = val;
        }
    }

    pub fn as_dir_mut(&mut self) -> Option<&mut DirNode> {
        match self {
            FileNode::Dir(d) => Some(d),
            FileNode::File(_) => None,
        }
    }
}

/// Sort children of every directory by descending size. Called once after
/// the full tree is built so the hot `walk_dir` path does zero sorting.
pub fn sort_children_recursive(node: &mut FileNode) {
    if let FileNode::Dir(d) = node {
        d.children
            .sort_by_key(|c| std::cmp::Reverse(c.size()));
        for child in &mut d.children {
            sort_children_recursive(child);
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
    FileNode::File(FileLeaf::new(
        name.into(),
        size,
        name.starts_with('.'),
    ))
}

#[cfg(test)]
pub fn dir(name: &str, children: Vec<FileNode>) -> FileNode {
    let size = children.iter().map(|c| c.size()).sum();
    FileNode::Dir(Box::new(DirNode {
        name: name.into(),
        size,
        children,
        expanded: false,
        hidden: name.starts_with('.'),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_expand_expands_large_children() {
        let mut root = dir(
            "root",
            vec![
                dir("big_dir", vec![leaf("a.txt", 300)]),
                dir("small_dir", vec![leaf("b.txt", 100)]),
            ],
        );
        root.set_expanded(true);

        auto_expand(&mut root, 0, 2);

        assert!(
            root.children()[0].expanded(),
            "big_dir should be expanded (75%)"
        );
        assert!(
            root.children()[1].expanded(),
            "small_dir should be expanded (25%)"
        );
    }

    #[test]
    fn auto_expand_skips_small_children() {
        let mut root = dir(
            "root",
            vec![
                dir("big_dir", vec![leaf("a.txt", 300)]),
                dir("tiny_dir", vec![leaf("b.txt", 10)]),
                leaf("c.txt", 90),
            ],
        );
        root.set_expanded(true);

        auto_expand(&mut root, 0, 2);

        assert!(root.children()[0].expanded(), "big_dir should be expanded");
        assert!(
            !root.children()[1].expanded(),
            "tiny_dir should NOT be expanded (2.5%)"
        );
    }

    #[test]
    fn auto_expand_respects_max_depth() {
        let mut root = dir(
            "root",
            vec![dir("lvl1", vec![dir("lvl2", vec![leaf("deep.txt", 100)])])],
        );
        root.set_expanded(true);

        auto_expand(&mut root, 0, 1);

        assert!(root.children()[0].expanded(), "lvl1 should be expanded");
        assert!(
            !root.children()[0].children()[0].expanded(),
            "lvl2 should NOT be expanded (depth limit)"
        );
    }

    #[test]
    fn auto_expand_recurses_into_expanded_children() {
        let mut root = dir(
            "root",
            vec![dir(
                "lvl1",
                vec![
                    dir("lvl2", vec![leaf("big.txt", 800)]),
                    leaf("small.txt", 10),
                ],
            )],
        );
        root.set_expanded(true);

        auto_expand(&mut root, 0, 2);

        assert!(root.children()[0].expanded(), "lvl1 expanded");
        assert!(
            root.children()[0].children()[0].expanded(),
            "lvl2 expanded (98% of parent)"
        );
    }
}
