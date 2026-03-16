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
