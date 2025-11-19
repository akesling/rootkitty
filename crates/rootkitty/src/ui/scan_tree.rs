//! Tree structure for organizing scans by path hierarchy

use crate::db::Scan;
use std::collections::HashMap;

/// Represents a node in the scan tree
#[derive(Debug, Clone)]
pub enum ScanTreeNode {
    /// A directory path that groups scans (may have children)
    PathNode {
        /// The path component (e.g., "home", "user")
        name: String,
        /// Full absolute path
        full_path: String,
        /// Child nodes (both path nodes and scan nodes)
        children: Vec<ScanTreeNode>,
        /// Whether this node is currently folded (collapsed)
        folded: bool,
    },
    /// An actual scan (leaf node)
    ScanNode {
        /// The scan data
        scan: Scan,
        /// Whether any sub-paths of this scan also have scans
        has_subscans: bool,
    },
}

impl ScanTreeNode {
    /// Get the full path for this node
    pub fn full_path(&self) -> &str {
        match self {
            ScanTreeNode::PathNode { full_path, .. } => full_path,
            ScanTreeNode::ScanNode { scan, .. } => &scan.root_path,
        }
    }

    /// Check if this node is folded
    pub fn is_folded(&self) -> bool {
        match self {
            ScanTreeNode::PathNode { folded, .. } => *folded,
            ScanTreeNode::ScanNode { .. } => false,
        }
    }

    /// Set folded state (only applies to PathNode)
    pub fn set_folded(&mut self, fold: bool) {
        if let ScanTreeNode::PathNode { folded, .. } = self {
            *folded = fold;
        }
    }

    /// Get children (empty for ScanNode)
    pub fn children(&self) -> &[ScanTreeNode] {
        match self {
            ScanTreeNode::PathNode { children, .. } => children,
            ScanTreeNode::ScanNode { .. } => &[],
        }
    }

    /// Get mutable children (empty for ScanNode)
    pub fn children_mut(&mut self) -> Option<&mut Vec<ScanTreeNode>> {
        match self {
            ScanTreeNode::PathNode { children, .. } => Some(children),
            ScanTreeNode::ScanNode { .. } => None,
        }
    }
}

/// A flattened representation of a tree node for display
#[derive(Debug, Clone)]
pub struct FlatScanNode {
    /// The tree node
    pub node: ScanTreeNode,
    /// Depth in the tree (for indentation)
    pub depth: usize,
    /// Index in the original flat list
    pub index: usize,
}

/// Build a tree structure from a list of scans
pub fn build_scan_tree(scans: &[Scan]) -> Vec<ScanTreeNode> {
    if scans.is_empty() {
        return Vec::new();
    }

    // First, group scans by their exact path
    let mut scans_by_path: HashMap<String, Vec<Scan>> = HashMap::new();
    for scan in scans {
        scans_by_path
            .entry(scan.root_path.clone())
            .or_insert_with(Vec::new)
            .push(scan.clone());
    }

    // Get all unique paths
    let mut paths: Vec<String> = scans_by_path.keys().cloned().collect();
    paths.sort();

    // Identify which paths have sub-scans
    let has_subscans_map = build_subscan_map(scans);

    // Build the tree hierarchy
    let mut root_nodes: Vec<ScanTreeNode> = Vec::new();

    for path in paths {
        let path_scans = scans_by_path.get(&path).unwrap();
        insert_path_into_tree(&mut root_nodes, &path, path_scans, &has_subscans_map);
    }

    // Collapse single-child path chains
    collapse_single_child_paths(&mut root_nodes);

    root_nodes
}

/// Build a map of which paths have sub-scans
fn build_subscan_map(scans: &[Scan]) -> HashMap<String, bool> {
    let mut map = HashMap::new();

    for scan in scans {
        // Check if any other scan is a subpath of this scan
        let has_subscans = scans.iter().any(|other| {
            other.root_path != scan.root_path && is_subpath(&other.root_path, &scan.root_path)
        });
        map.insert(scan.root_path.clone(), has_subscans);
    }

    map
}

/// Check if `path` is a subpath of `parent` (e.g., "/a/b/c" is subpath of "/a/b")
fn is_subpath(path: &str, parent: &str) -> bool {
    if path.len() <= parent.len() {
        return false;
    }

    // Normalize paths (remove trailing slashes)
    let path = path.trim_end_matches('/');
    let parent = parent.trim_end_matches('/');

    // Check if path starts with parent and has a separator
    path.starts_with(parent) && path[parent.len()..].starts_with('/')
}

/// Insert a path with its scans into the tree at the appropriate location
fn insert_path_into_tree(
    tree: &mut Vec<ScanTreeNode>,
    path: &str,
    path_scans: &[Scan],
    has_subscans_map: &HashMap<String, bool>,
) {
    let path = path.trim_end_matches('/');
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if parts.is_empty() {
        // Root path "/" - create path node with scans as children
        create_path_node_with_scans(tree, path, "/", path_scans, has_subscans_map);
        return;
    }

    insert_path_recursive(tree, &parts, 0, "", path, path_scans, has_subscans_map);
}

fn insert_path_recursive(
    nodes: &mut Vec<ScanTreeNode>,
    parts: &[&str],
    depth: usize,
    current_path: &str,
    target_path: &str,
    path_scans: &[Scan],
    has_subscans_map: &HashMap<String, bool>,
) {
    if depth >= parts.len() {
        // We've reached the end - create a path node with scans as children
        create_path_node_with_scans(
            nodes,
            target_path,
            parts.last().unwrap(),
            path_scans,
            has_subscans_map,
        );
        return;
    }

    let part = parts[depth];
    let next_path = if current_path.is_empty() {
        format!("/{}", part)
    } else if current_path == "/" {
        format!("/{}", part)
    } else {
        format!("{}/{}", current_path, part)
    };

    // Check if this is the last part (the target path itself)
    if depth == parts.len() - 1 {
        // Create the path node with scans as children
        create_path_node_with_scans(nodes, target_path, part, path_scans, has_subscans_map);
        return;
    }

    // Find or create an intermediate path node
    let node_pos = nodes.iter().position(|node| match node {
        ScanTreeNode::PathNode { full_path, .. } => full_path == &next_path,
        _ => false,
    });

    if let Some(pos) = node_pos {
        // Path node exists - recurse into it
        if let Some(children) = nodes[pos].children_mut() {
            insert_path_recursive(
                children,
                parts,
                depth + 1,
                &next_path,
                target_path,
                path_scans,
                has_subscans_map,
            );
        }
    } else {
        // Create new intermediate path node
        let mut new_node = ScanTreeNode::PathNode {
            name: part.to_string(),
            full_path: next_path.clone(),
            children: Vec::new(),
            folded: true, // Start folded by default
        };

        // Recurse to add the target path under this new node
        if let Some(children) = new_node.children_mut() {
            insert_path_recursive(
                children,
                parts,
                depth + 1,
                &next_path,
                target_path,
                path_scans,
                has_subscans_map,
            );
        }

        nodes.push(new_node);
    }
}

/// Create a PathNode for a path with scans as children
fn create_path_node_with_scans(
    nodes: &mut Vec<ScanTreeNode>,
    full_path: &str,
    name: &str,
    scans: &[Scan],
    has_subscans_map: &HashMap<String, bool>,
) {
    let mut children = Vec::new();

    // Add all scans for this path as children
    for scan in scans {
        let has_subscans = has_subscans_map
            .get(&scan.root_path)
            .copied()
            .unwrap_or(false);
        children.push(ScanTreeNode::ScanNode {
            scan: scan.clone(),
            has_subscans,
        });
    }

    // Create the path node with scans as children
    nodes.push(ScanTreeNode::PathNode {
        name: name.to_string(),
        full_path: full_path.to_string(),
        children,
        folded: true, // Start folded by default
    });
}

/// Collapse single-child path chains into a single node
/// For example, /Users → /akesling becomes /Users/akesling if akesling is the only child
fn collapse_single_child_paths(nodes: &mut Vec<ScanTreeNode>) {
    for node in nodes.iter_mut() {
        if let ScanTreeNode::PathNode {
            children,
            name,
            full_path,
            folded,
            ..
        } = node
        {
            // First, recursively collapse children
            collapse_single_child_paths(children);

            // Then check if this node should be collapsed with its child
            loop {
                if children.len() != 1 {
                    break;
                }

                // Check if the single child is a PathNode
                let should_collapse = matches!(&children[0], ScanTreeNode::PathNode { .. });

                if !should_collapse {
                    break;
                }

                // Take ownership of the child to avoid borrow issues
                let child = children.remove(0);

                if let ScanTreeNode::PathNode {
                    name: child_name,
                    full_path: child_full_path,
                    children: child_children,
                    folded: child_folded,
                } = child
                {
                    // Collapse: merge this node with its single child
                    *name = child_name;
                    *full_path = child_full_path;
                    *children = child_children;
                    *folded = child_folded;
                } else {
                    // This shouldn't happen given our check above, but restore the child just in case
                    children.push(child);
                    break;
                }
            }
        }
    }
}

/// Flatten the tree for display, respecting folded state
pub fn flatten_tree(tree: &[ScanTreeNode]) -> Vec<FlatScanNode> {
    let mut result = Vec::new();
    flatten_recursive(tree, &mut result, 0);
    result
}

fn flatten_recursive(nodes: &[ScanTreeNode], result: &mut Vec<FlatScanNode>, depth: usize) {
    for node in nodes {
        let index = result.len();
        let is_folded = node.is_folded();

        result.push(FlatScanNode {
            node: node.clone(),
            depth,
            index,
        });

        // Only add children if not folded
        if !is_folded {
            flatten_recursive(node.children(), result, depth + 1);
        }
    }
}

/// Toggle the folded state of a node in the tree
pub fn toggle_fold(tree: &mut [ScanTreeNode], path: &str) -> bool {
    toggle_fold_recursive(tree, path)
}

fn toggle_fold_recursive(nodes: &mut [ScanTreeNode], path: &str) -> bool {
    for node in nodes.iter_mut() {
        if node.full_path() == path {
            let new_state = !node.is_folded();
            node.set_folded(new_state);
            return true;
        }

        // Recurse into children
        if let Some(children) = node.children_mut() {
            if toggle_fold_recursive(children, path) {
                return true;
            }
        }
    }
    false
}

/// Unfold (expand) a node in the tree
pub fn unfold(tree: &mut [ScanTreeNode], path: &str) -> bool {
    unfold_recursive(tree, path)
}

fn unfold_recursive(nodes: &mut [ScanTreeNode], path: &str) -> bool {
    for node in nodes.iter_mut() {
        if node.full_path() == path {
            node.set_folded(false);
            return true;
        }

        // Recurse into children
        if let Some(children) = node.children_mut() {
            if unfold_recursive(children, path) {
                return true;
            }
        }
    }
    false
}

/// Unfold all nodes in the tree (recursively expand everything)
pub fn unfold_all(tree: &mut [ScanTreeNode]) {
    unfold_all_recursive(tree);
}

fn unfold_all_recursive(nodes: &mut [ScanTreeNode]) {
    for node in nodes.iter_mut() {
        node.set_folded(false);

        // Recurse into children
        if let Some(children) = node.children_mut() {
            unfold_all_recursive(children);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn create_test_scan(id: i64, path: &str) -> Scan {
        Scan {
            id,
            root_path: path.to_string(),
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            total_size: 1000,
            total_files: 10,
            total_dirs: 5,
            status: "completed".to_string(),
            entries_table: Some(format!("scan_entries_{}", id)),
        }
    }

    #[test]
    fn test_is_subpath() {
        assert!(is_subpath("/home/user/docs", "/home/user"));
        assert!(is_subpath("/home/user/docs", "/home"));
        assert!(!is_subpath("/home/user", "/home/user"));
        assert!(!is_subpath("/home", "/home/user"));
        assert!(!is_subpath("/home/user2", "/home/user"));
    }

    #[test]
    fn test_build_subscan_map() {
        let scans = vec![
            create_test_scan(1, "/home"),
            create_test_scan(2, "/home/user"),
            create_test_scan(3, "/home/user/docs"),
            create_test_scan(4, "/var"),
        ];

        let map = build_subscan_map(&scans);

        assert_eq!(map.get("/home"), Some(&true)); // has /home/user and /home/user/docs
        assert_eq!(map.get("/home/user"), Some(&true)); // has /home/user/docs
        assert_eq!(map.get("/home/user/docs"), Some(&false)); // no subscans
        assert_eq!(map.get("/var"), Some(&false)); // no subscans
    }

    #[test]
    fn test_build_scan_tree_simple() {
        let scans = vec![create_test_scan(1, "/home"), create_test_scan(2, "/var")];

        let tree = build_scan_tree(&scans);

        assert_eq!(tree.len(), 2);
        // Both should be path nodes with scan children
        for node in &tree {
            assert!(matches!(node, ScanTreeNode::PathNode { .. }));
            if let ScanTreeNode::PathNode { children, .. } = node {
                assert_eq!(children.len(), 1); // Each path has 1 scan
                assert!(matches!(children[0], ScanTreeNode::ScanNode { .. }));
            }
        }
    }

    #[test]
    fn test_build_scan_tree_nested() {
        let scans = vec![
            create_test_scan(1, "/home"),
            create_test_scan(2, "/home/user"),
        ];

        let tree = build_scan_tree(&scans);

        // Should have root nodes
        assert!(!tree.is_empty());

        // Flatten to verify structure (with nodes folded, should show path nodes only)
        let flat = flatten_tree(&tree);
        assert!(flat.len() >= 1); // At least the /home path node
    }

    #[test]
    fn test_multiple_scans_same_path() {
        let scans = vec![
            create_test_scan(1, "/home"),
            create_test_scan(2, "/home"), // Same path, different scan
        ];

        let tree = build_scan_tree(&scans);

        assert_eq!(tree.len(), 1); // One path node
        if let ScanTreeNode::PathNode { children, .. } = &tree[0] {
            assert_eq!(children.len(), 2); // Two scans for this path
            for child in children {
                assert!(matches!(child, ScanTreeNode::ScanNode { .. }));
            }
        } else {
            panic!("Expected PathNode");
        }
    }

    #[test]
    fn test_flatten_tree_respects_folding() {
        let scans = vec![
            create_test_scan(1, "/home"),
            create_test_scan(2, "/home/user"),
            create_test_scan(3, "/home/user/docs"),
        ];

        let mut tree = build_scan_tree(&scans);

        // Initially, all path nodes should be folded
        let flat = flatten_tree(&tree);
        // Should have fewer items when folded
        let folded_count = flat.len();

        // Unfold all
        unfold_all_recursive(&mut tree);
        let flat_unfolded = flatten_tree(&tree);
        let unfolded_count = flat_unfolded.len();

        // Unfolded should have more or equal items
        assert!(unfolded_count >= folded_count);
    }

    fn unfold_all_recursive(nodes: &mut [ScanTreeNode]) {
        for node in nodes.iter_mut() {
            node.set_folded(false);
            if let Some(children) = node.children_mut() {
                unfold_all_recursive(children);
            }
        }
    }

    #[test]
    fn test_toggle_fold() {
        let scans = vec![
            create_test_scan(1, "/home"),
            create_test_scan(2, "/home/user"),
        ];

        let mut tree = build_scan_tree(&scans);

        // Find a path node
        if let Some(ScanTreeNode::PathNode { full_path, .. }) = tree.first() {
            let path = full_path.clone();
            let initial_state = tree[0].is_folded();

            // Toggle it
            assert!(toggle_fold(&mut tree, &path));
            assert_eq!(tree[0].is_folded(), !initial_state);
        }
    }
}
