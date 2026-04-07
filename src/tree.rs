//! Index-arena file tree — all nodes in a flat `Vec<NodeData>`, children
//! referenced via contiguous ranges in a separate index `Vec<NodeId>`.
//! Eliminates per-directory `Box<DirNode>` and per-node `Vec<FileNode>`.

/// Bit 63 of the size field stores the hidden flag.
/// Max representable size: 2^63 - 1 ~ 9.2 EB.
const HIDDEN_BIT: u64 = 1 << 63;
const DIR_FLAG: u8 = 1;
const EXPANDED_FLAG: u8 = 2;

/// Opaque handle into the arena. u32 supports ~4 billion nodes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(pub u32);

/// Per-node data stored in the flat arena.
pub struct NodeData {
    pub name: Box<str>,
    /// Lower 63 bits: file size in bytes. Bit 63: hidden flag.
    size_hidden: u64,
    /// Start index into `FileTree::child_indices`.
    children_start: u32,
    /// Number of direct children.
    children_count: u32,
    /// Bit 0: is_dir. Bit 1: expanded.
    flags: u8,
}

impl NodeData {
    fn new(name: Box<str>, size: u64, hidden: bool, is_dir: bool) -> Self {
        Self {
            name,
            size_hidden: size | if hidden { HIDDEN_BIT } else { 0 },
            children_start: 0,
            children_count: 0,
            flags: if is_dir { DIR_FLAG } else { 0 },
        }
    }
}

/// Flat arena holding the entire file tree. All nodes live in a single `Vec`,
/// and children are referenced via contiguous ranges in a separate index `Vec`.
pub struct FileTree {
    nodes: Vec<NodeData>,
    child_indices: Vec<NodeId>,
    root: NodeId,
}

impl FileTree {
    pub fn root(&self) -> NodeId {
        self.root
    }

    #[inline]
    pub fn name(&self, id: NodeId) -> &str {
        &self.nodes[id.0 as usize].name
    }

    #[inline]
    pub fn size(&self, id: NodeId) -> u64 {
        self.nodes[id.0 as usize].size_hidden & !HIDDEN_BIT
    }

    #[inline]
    pub fn is_dir(&self, id: NodeId) -> bool {
        self.nodes[id.0 as usize].flags & DIR_FLAG != 0
    }

    #[inline]
    pub fn is_hidden(&self, id: NodeId) -> bool {
        self.nodes[id.0 as usize].size_hidden & HIDDEN_BIT != 0
    }

    #[inline]
    pub fn expanded(&self, id: NodeId) -> bool {
        self.nodes[id.0 as usize].flags & EXPANDED_FLAG != 0
    }

    #[inline]
    pub fn set_expanded(&mut self, id: NodeId, val: bool) {
        let n = &mut self.nodes[id.0 as usize];
        if val {
            n.flags |= EXPANDED_FLAG;
        } else {
            n.flags &= !EXPANDED_FLAG;
        }
    }

    #[inline]
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        let n = &self.nodes[id.0 as usize];
        let start = n.children_start as usize;
        let end = start + n.children_count as usize;
        &self.child_indices[start..end]
    }

    #[inline]
    pub fn children_count(&self, id: NodeId) -> usize {
        self.nodes[id.0 as usize].children_count as usize
    }

    #[allow(dead_code)]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    fn set_size(&mut self, id: NodeId, new_size: u64) {
        let n = &mut self.nodes[id.0 as usize];
        let hidden = n.size_hidden & HIDDEN_BIT;
        n.size_hidden = new_size | hidden;
    }

    pub(crate) fn sub_size(&mut self, id: NodeId, delta: u64) {
        let current = self.size(id);
        self.set_size(id, current.saturating_sub(delta));
    }

    /// Remove a child by swapping with the last child in the parent's range.
    /// Returns the removed child's size. Order is NOT preserved (swap-remove).
    pub fn remove_child(&mut self, parent: NodeId, child_pos: usize) -> u64 {
        let pn = &self.nodes[parent.0 as usize];
        let start = pn.children_start as usize;
        let count = pn.children_count as usize;

        let removed_id = self.child_indices[start + child_pos];
        let removed_size = self.size(removed_id);

        // Swap-remove: move last child into the removed slot
        self.child_indices.swap(start + child_pos, start + count - 1);
        self.nodes[parent.0 as usize].children_count -= 1;

        // Update parent size
        self.sub_size(parent, removed_size);

        removed_size
    }
}

// ─── Building from old FileNode (used by scanner) ──────────────

/// Internal types used only during scanning. Not part of the public API.
pub(crate) struct ScanDirNode {
    pub name: Box<str>,
    pub size: u64,
    pub children: Vec<ScanFileNode>,
    pub hidden: bool,
}

pub(crate) struct ScanFileLeaf {
    pub name: Box<str>,
    size_hidden: u64,
}

impl ScanFileLeaf {
    pub(crate) fn new(name: Box<str>, size: u64, hidden: bool) -> Self {
        Self {
            name,
            size_hidden: size | if hidden { HIDDEN_BIT } else { 0 },
        }
    }

    #[inline]
    pub(crate) fn size(&self) -> u64 {
        self.size_hidden & !HIDDEN_BIT
    }

    #[inline]
    pub(crate) fn is_hidden(&self) -> bool {
        self.size_hidden & HIDDEN_BIT != 0
    }
}

pub(crate) enum ScanFileNode {
    File(ScanFileLeaf),
    Dir(Box<ScanDirNode>),
}

impl ScanFileNode {
    pub(crate) fn size(&self) -> u64 {
        match self {
            ScanFileNode::File(f) => f.size(),
            ScanFileNode::Dir(d) => d.size,
        }
    }
}

/// Sort children of every directory by descending size in a scan tree.
pub(crate) fn sort_scan_children(node: &mut ScanFileNode) {
    if let ScanFileNode::Dir(d) = node {
        d.children
            .sort_by_key(|c| std::cmp::Reverse(c.size()));
        for child in &mut d.children {
            sort_scan_children(child);
        }
    }
}

/// Convert a scan-time `ScanFileNode` tree into a `FileTree` arena.
pub(crate) fn from_scan_tree(root: ScanFileNode) -> FileTree {
    let node_count = count_scan_nodes(&root);
    let mut tree = FileTree {
        nodes: Vec::with_capacity(node_count),
        child_indices: Vec::with_capacity(node_count),
        root: NodeId(0),
    };
    tree.root = flatten_scan_node(&root, &mut tree);
    tree
}

fn count_scan_nodes(node: &ScanFileNode) -> usize {
    match node {
        ScanFileNode::File(_) => 1,
        ScanFileNode::Dir(d) => 1 + d.children.iter().map(count_scan_nodes).sum::<usize>(),
    }
}

fn flatten_scan_node(node: &ScanFileNode, tree: &mut FileTree) -> NodeId {
    let id = NodeId(tree.nodes.len() as u32);

    match node {
        ScanFileNode::File(f) => {
            tree.nodes
                .push(NodeData::new(f.name.clone(), f.size(), f.is_hidden(), false));
        }
        ScanFileNode::Dir(d) => {
            tree.nodes
                .push(NodeData::new(d.name.clone(), d.size, d.hidden, true));

            let children_start = tree.child_indices.len() as u32;
            let count = d.children.len() as u32;

            // Reserve child index slots
            for _ in 0..count {
                tree.child_indices.push(NodeId(0));
            }

            // Recursively flatten each child
            for (i, child) in d.children.iter().enumerate() {
                let child_id = flatten_scan_node(child, tree);
                tree.child_indices[children_start as usize + i] = child_id;
            }

            tree.nodes[id.0 as usize].children_start = children_start;
            tree.nodes[id.0 as usize].children_count = count;
        }
    }

    id
}

// ─── Tree operations ───────────────────────────────────────────

/// Sort children of every directory by descending size.
#[allow(dead_code)]
pub fn sort_children_recursive(tree: &mut FileTree, id: NodeId) {
    if !tree.is_dir(id) {
        return;
    }
    let n = &tree.nodes[id.0 as usize];
    let start = n.children_start as usize;
    let count = n.children_count as usize;

    // Sort this node's children by descending size
    // We need a temporary copy of sizes because we can't borrow tree mutably
    // while reading sizes through it.
    let mut child_with_size: Vec<(NodeId, u64)> = tree.child_indices[start..start + count]
        .iter()
        .map(|&cid| (cid, tree.size(cid)))
        .collect();
    child_with_size.sort_by_key(|&(_, size)| std::cmp::Reverse(size));
    for (i, &(cid, _)) in child_with_size.iter().enumerate() {
        tree.child_indices[start + i] = cid;
    }

    // Recurse into directory children
    let child_ids: Vec<NodeId> = tree.child_indices[start..start + count].to_vec();
    for child_id in child_ids {
        sort_children_recursive(tree, child_id);
    }
}

/// Auto-expand directories that represent >25% of their parent's size,
/// up to `max_depth` levels deep.
pub fn auto_expand(tree: &mut FileTree, id: NodeId, depth: usize, max_depth: usize) {
    if depth >= max_depth || !tree.is_dir(id) || tree.size(id) == 0 {
        return;
    }
    let parent_size = tree.size(id);
    let child_ids: Vec<NodeId> = tree.children(id).to_vec();
    for child_id in child_ids {
        if tree.is_dir(child_id) && tree.size(child_id) * 4 >= parent_size {
            tree.set_expanded(child_id, true);
            auto_expand(tree, child_id, depth + 1, max_depth);
        }
    }
}

// ─── Test helpers ──────────────────────────────────────────────

#[cfg(test)]
pub enum TestNode {
    File {
        name: Box<str>,
        size: u64,
        hidden: bool,
    },
    Dir {
        name: Box<str>,
        children: Vec<TestNode>,
        hidden: bool,
    },
}

#[cfg(test)]
pub fn leaf(name: &str, size: u64) -> TestNode {
    TestNode::File {
        name: name.into(),
        size,
        hidden: name.starts_with('.'),
    }
}

#[cfg(test)]
pub fn dir(name: &str, children: Vec<TestNode>) -> TestNode {
    TestNode::Dir {
        name: name.into(),
        children,
        hidden: name.starts_with('.'),
    }
}

#[cfg(test)]
pub fn build_test_tree(root: TestNode) -> FileTree {
    let mut tree = FileTree {
        nodes: Vec::new(),
        child_indices: Vec::new(),
        root: NodeId(0),
    };
    tree.root = flatten_test_node(&root, &mut tree);
    tree
}

#[cfg(test)]
fn test_node_size(node: &TestNode) -> u64 {
    match node {
        TestNode::File { size, .. } => *size,
        TestNode::Dir { children, .. } => children.iter().map(test_node_size).sum(),
    }
}

#[cfg(test)]
fn flatten_test_node(node: &TestNode, tree: &mut FileTree) -> NodeId {
    let id = NodeId(tree.nodes.len() as u32);

    match node {
        TestNode::File {
            name,
            size,
            hidden,
        } => {
            tree.nodes
                .push(NodeData::new(name.clone(), *size, *hidden, false));
        }
        TestNode::Dir {
            name,
            children,
            hidden,
        } => {
            let size = test_node_size(node);
            tree.nodes
                .push(NodeData::new(name.clone(), size, *hidden, true));

            let children_start = tree.child_indices.len() as u32;
            let count = children.len() as u32;

            for _ in 0..count {
                tree.child_indices.push(NodeId(0));
            }

            for (i, child) in children.iter().enumerate() {
                let child_id = flatten_test_node(child, tree);
                tree.child_indices[children_start as usize + i] = child_id;
            }

            tree.nodes[id.0 as usize].children_start = children_start;
            tree.nodes[id.0 as usize].children_count = count;
        }
    }

    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_expand_expands_large_children() {
        let mut tree = build_test_tree(dir(
            "root",
            vec![
                dir("big_dir", vec![leaf("a.txt", 300)]),
                dir("small_dir", vec![leaf("b.txt", 100)]),
            ],
        ));
        let root = tree.root();
        tree.set_expanded(root, true);

        auto_expand(&mut tree, root, 0, 2);

        let children = tree.children(root);
        assert!(
            tree.expanded(children[0]),
            "big_dir should be expanded (75%)"
        );
        assert!(
            tree.expanded(children[1]),
            "small_dir should be expanded (25%)"
        );
    }

    #[test]
    fn auto_expand_skips_small_children() {
        let mut tree = build_test_tree(dir(
            "root",
            vec![
                dir("big_dir", vec![leaf("a.txt", 300)]),
                dir("tiny_dir", vec![leaf("b.txt", 10)]),
                leaf("c.txt", 90),
            ],
        ));
        let root = tree.root();
        tree.set_expanded(root, true);

        auto_expand(&mut tree, root, 0, 2);

        let children = tree.children(root);
        assert!(tree.expanded(children[0]), "big_dir should be expanded");
        assert!(
            !tree.expanded(children[1]),
            "tiny_dir should NOT be expanded (2.5%)"
        );
    }

    #[test]
    fn auto_expand_respects_max_depth() {
        let mut tree = build_test_tree(dir(
            "root",
            vec![dir("lvl1", vec![dir("lvl2", vec![leaf("deep.txt", 100)])])],
        ));
        let root = tree.root();
        tree.set_expanded(root, true);

        auto_expand(&mut tree, root, 0, 1);

        let lvl1 = tree.children(root)[0];
        assert!(tree.expanded(lvl1), "lvl1 should be expanded");
        let lvl2 = tree.children(lvl1)[0];
        assert!(
            !tree.expanded(lvl2),
            "lvl2 should NOT be expanded (depth limit)"
        );
    }

    #[test]
    fn auto_expand_recurses_into_expanded_children() {
        let mut tree = build_test_tree(dir(
            "root",
            vec![dir(
                "lvl1",
                vec![
                    dir("lvl2", vec![leaf("big.txt", 800)]),
                    leaf("small.txt", 10),
                ],
            )],
        ));
        let root = tree.root();
        tree.set_expanded(root, true);

        auto_expand(&mut tree, root, 0, 2);

        let lvl1 = tree.children(root)[0];
        assert!(tree.expanded(lvl1), "lvl1 expanded");
        let lvl2 = tree.children(lvl1)[0];
        assert!(
            tree.expanded(lvl2),
            "lvl2 expanded (98% of parent)"
        );
    }

    #[test]
    fn sort_children_by_size() {
        let mut tree = build_test_tree(dir(
            "root",
            vec![
                leaf("small.txt", 10),
                leaf("big.txt", 1000),
                leaf("medium.txt", 100),
            ],
        ));
        let root = tree.root();
        sort_children_recursive(&mut tree, root);

        let children = tree.children(root);
        assert_eq!(tree.name(children[0]), "big.txt");
        assert_eq!(tree.name(children[1]), "medium.txt");
        assert_eq!(tree.name(children[2]), "small.txt");
    }

    #[test]
    fn remove_child_updates_size() {
        let mut tree = build_test_tree(dir(
            "root",
            vec![leaf("a.txt", 300), leaf("b.txt", 200)],
        ));
        let root = tree.root();
        assert_eq!(tree.size(root), 500);

        let removed = tree.remove_child(root, 0);
        assert_eq!(removed, 300);
        assert_eq!(tree.size(root), 200);
        assert_eq!(tree.children_count(root), 1);
    }

    #[test]
    fn node_count_correct() {
        let tree = build_test_tree(dir(
            "root",
            vec![
                dir("sub", vec![leaf("a.txt", 10), leaf("b.txt", 20)]),
                leaf("c.txt", 30),
            ],
        ));
        // root + sub + a + b + c = 5
        assert_eq!(tree.node_count(), 5);
    }
}
