//! Tree operations for file listing: sorting, filtering, and folding

use crate::db::StoredFileEntry;
use std::collections::{HashMap, HashSet};

pub use super::types::SortMode;

/// Pure function to compute visible entries based on folded state and optional search query
///
/// This function:
/// 1. Filters out entries whose parent directories are folded
/// 2. Filters by search query if provided (case-insensitive match on name or path)
///    - When searching, includes parent directories of matching items for context
/// 3. Sorts the remaining entries based on the sort mode
pub fn compute_visible_entries<'a>(
    all_entries: &'a [StoredFileEntry],
    folded_dirs: &HashSet<String>,
    sort_mode: SortMode,
    search_query: Option<&str>,
) -> Vec<&'a StoredFileEntry> {
    // Step 1: Filter to only visible entries (not under folded directories)
    let unfolded: Vec<&StoredFileEntry> = all_entries
        .iter()
        .filter(|entry| !is_entry_hidden(entry, folded_dirs))
        .collect();

    // Step 2: Apply search filter if query is provided
    let visible = if let Some(query) = search_query {
        if query.is_empty() {
            unfolded
        } else {
            apply_search_filter(&unfolded, query)
        }
    } else {
        unfolded
    };

    // Step 3: Sort based on mode
    sort_entries(visible, sort_mode)
}

/// Apply search filter and include parent directories of matching items
/// This ensures the full path from root to each match is visible
fn apply_search_filter<'a>(
    entries: &[&'a StoredFileEntry],
    query: &str,
) -> Vec<&'a StoredFileEntry> {
    let query_lower = query.to_lowercase();

    // Find all entries that match the search query
    let matching_entries: HashSet<&str> = entries
        .iter()
        .filter(|entry| {
            entry.name.to_lowercase().contains(&query_lower)
                || entry.path.to_lowercase().contains(&query_lower)
        })
        .map(|e| e.path.as_str())
        .collect();

    // Build set of all paths to include (matches + their ancestors)
    let mut paths_to_include = HashSet::new();

    for matching_path in &matching_entries {
        // Add the matching entry itself
        paths_to_include.insert(*matching_path);

        // Add all parent directories
        let mut current_path = *matching_path;
        while let Some(parent_path) = get_parent_path(current_path) {
            if !paths_to_include.insert(parent_path) {
                // Already added this parent (and thus all its ancestors)
                break;
            }
            current_path = parent_path;
        }
    }

    // Filter entries to only those in our include set
    entries
        .iter()
        .filter(|entry| paths_to_include.contains(entry.path.as_str()))
        .copied()
        .collect()
}

/// Get the parent path from a path string
fn get_parent_path(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}

/// Check if an entry is hidden because a parent directory is folded
fn is_entry_hidden(entry: &StoredFileEntry, folded_dirs: &HashSet<String>) -> bool {
    // Check each potential parent path
    let path_parts: Vec<&str> = entry.path.split('/').collect();
    let mut current_path = String::new();

    for (i, part) in path_parts.iter().enumerate() {
        if i == path_parts.len() - 1 {
            // This is the entry itself, not a parent
            break;
        }

        if i == 0 {
            current_path = part.to_string();
        } else {
            current_path = format!("{}/{}", current_path, part);
        }

        if folded_dirs.contains(&current_path) {
            return true;
        }
    }

    false
}

/// Sort entries based on the sort mode, maintaining tree structure
fn sort_entries(mut entries: Vec<&StoredFileEntry>, sort_mode: SortMode) -> Vec<&StoredFileEntry> {
    match sort_mode {
        SortMode::ByPath => {
            // Sort by path to ensure hierarchical order (parents before children)
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            entries
        }
        SortMode::BySize => {
            // Tree-based hierarchical sort: sort children by size within each parent,
            // but keep all descendants with their parent
            sort_hierarchically_by_size(&mut entries);
            entries
        }
    }
}

/// Sort entries hierarchically by size, maintaining tree structure
/// This ensures that:
/// 1. All children of a directory appear immediately after that directory
/// 2. Within each level, siblings are sorted by size (largest first)
fn sort_hierarchically_by_size(entries: &mut Vec<&StoredFileEntry>) {
    // Build a mapping from path to index for quick lookups
    let path_to_idx: HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.path.as_str(), i))
        .collect();

    // Build a parent→children mapping for O(1) child lookups
    let mut parent_to_children: HashMap<&str, Vec<usize>> = HashMap::new();
    for (idx, entry) in entries.iter().enumerate() {
        if let Some(parent_path) = &entry.parent_path {
            parent_to_children
                .entry(parent_path.as_str())
                .or_insert_with(Vec::new)
                .push(idx);
        }
    }

    // Sort each parent's children by size (descending)
    for children in parent_to_children.values_mut() {
        children.sort_by(|&a, &b| entries[b].size.cmp(&entries[a].size));
    }

    // Recursively sort entries starting from roots (entries whose parents aren't in the list)
    let mut sorted = Vec::new();
    let mut processed = HashSet::new();

    // Find root entries: either depth 0, no parent_path, OR parent not in the entry list
    // This handles the case where we have a filtered list (e.g., top 1000 entries)
    for (idx, entry) in entries.iter().enumerate() {
        let is_root = entry.depth == 0
            || entry.parent_path.is_none()
            || entry
                .parent_path
                .as_ref()
                .is_none_or(|parent| !path_to_idx.contains_key(parent.as_str()));

        if is_root {
            sort_subtree(
                idx,
                entries,
                &parent_to_children,
                &mut sorted,
                &mut processed,
            );
        }
    }

    // Copy sorted entries back
    for (i, entry) in sorted.iter().enumerate() {
        entries[i] = entry;
    }
}

/// Recursively sort a subtree by size
fn sort_subtree<'a>(
    idx: usize,
    all_entries: &[&'a StoredFileEntry],
    parent_to_children: &HashMap<&str, Vec<usize>>,
    sorted: &mut Vec<&'a StoredFileEntry>,
    processed: &mut HashSet<usize>,
) {
    if processed.contains(&idx) {
        return;
    }

    let entry = all_entries[idx];
    sorted.push(entry);
    processed.insert(idx);

    // Look up pre-sorted children from the index (O(1) lookup)
    if let Some(children_indices) = parent_to_children.get(entry.path.as_str()) {
        // Children are already sorted by size in the map
        for &child_idx in children_indices {
            if !processed.contains(&child_idx) {
                sort_subtree(
                    child_idx,
                    all_entries,
                    parent_to_children,
                    sorted,
                    processed,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::StoredFileEntry;
    use chrono::Utc;

    fn create_test_entry_with_size(
        id: i64,
        path: &str,
        name: &str,
        depth: i64,
        is_dir: bool,
        size: i64,
    ) -> StoredFileEntry {
        let parent_path = if depth > 0 {
            path.rsplit_once('/').map(|(parent, _)| parent.to_string())
        } else {
            None
        };

        StoredFileEntry {
            id,
            scan_id: 1,
            path: path.to_string(),
            name: name.to_string(),
            parent_path,
            size,
            is_dir,
            modified_at: Some(Utc::now()),
            depth,
        }
    }

    /// Create a realistic test fixture representing a typical directory structure
    fn create_test_fixture() -> Vec<StoredFileEntry> {
        vec![
            create_test_entry_with_size(1, "/project", "project", 0, true, 5000),
            create_test_entry_with_size(2, "/project/src", "src", 1, true, 3000),
            create_test_entry_with_size(3, "/project/src/main.rs", "main.rs", 2, false, 1000),
            create_test_entry_with_size(4, "/project/src/lib.rs", "lib.rs", 2, false, 500),
            create_test_entry_with_size(5, "/project/src/utils", "utils", 2, true, 1500),
            create_test_entry_with_size(
                6,
                "/project/src/utils/helper.rs",
                "helper.rs",
                3,
                false,
                800,
            ),
            create_test_entry_with_size(
                7,
                "/project/src/utils/config.rs",
                "config.rs",
                3,
                false,
                700,
            ),
            create_test_entry_with_size(8, "/project/tests", "tests", 1, true, 1500),
            create_test_entry_with_size(
                9,
                "/project/tests/integration.rs",
                "integration.rs",
                2,
                false,
                1500,
            ),
            create_test_entry_with_size(10, "/project/docs", "docs", 1, true, 300),
            create_test_entry_with_size(11, "/project/docs/README.md", "README.md", 2, false, 300),
            create_test_entry_with_size(12, "/project/Cargo.toml", "Cargo.toml", 1, false, 200),
        ]
    }

    #[test]
    fn test_sort_by_path_all_unfolded() {
        let entries = create_test_fixture();
        let folded = HashSet::new();

        let visible = compute_visible_entries(&entries, &folded, SortMode::ByPath, None);
        let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

        assert_eq!(visible.len(), 12);
        assert_eq!(paths[0], "/project");
        assert_eq!(paths[1], "/project/Cargo.toml");
        assert_eq!(paths[4], "/project/src");
    }

    #[test]
    fn test_sort_by_size_all_unfolded() {
        let entries = create_test_fixture();
        let folded = HashSet::new();

        let visible = compute_visible_entries(&entries, &folded, SortMode::BySize, None);
        let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

        assert_eq!(visible.len(), 12);
        assert_eq!(paths[0], "/project");
        assert_eq!(
            paths[1], "/project/src",
            "src should be first child (largest)"
        );

        // All of src's descendants should come before tests
        let tests_idx = paths.iter().position(|&p| p == "/project/tests").unwrap();
        let utils_idx = paths
            .iter()
            .position(|&p| p == "/project/src/utils")
            .unwrap();

        assert!(utils_idx < tests_idx, "utils should come before tests");
    }

    #[test]
    fn test_sort_by_size_with_folded_dirs() {
        let entries = create_test_fixture();
        let mut folded = HashSet::new();
        folded.insert("/project/src/utils".to_string());

        let visible = compute_visible_entries(&entries, &folded, SortMode::BySize, None);
        let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

        assert!(
            !paths.contains(&"/project/src/utils/helper.rs"),
            "helper.rs should be hidden"
        );
        assert!(
            paths.contains(&"/project/src/utils"),
            "utils itself should be visible"
        );
        assert_eq!(visible.len(), 10);
    }

    #[test]
    fn test_parents_always_before_children() {
        // This test verifies that in ANY sorting mode, parents always appear before their children
        let entries = create_test_fixture();
        let folded = HashSet::new();

        // Test both sort modes
        for sort_mode in [SortMode::ByPath, SortMode::BySize] {
            let visible = compute_visible_entries(&entries, &folded, sort_mode, None);
            let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

            // For each entry, verify its parent appears before it
            for (i, entry) in visible.iter().enumerate() {
                if let Some(parent_path) = &entry.parent_path {
                    let parent_idx = paths.iter().position(|&p| p == parent_path.as_str());
                    assert!(
                        parent_idx.is_some(),
                        "Parent '{}' not found for '{}'",
                        parent_path,
                        entry.path
                    );
                    let parent_idx = parent_idx.unwrap();
                    assert!(
                        parent_idx < i,
                        "Parent '{}' at index {} should appear before child '{}' at index {} (sort mode: {:?})",
                        parent_path,
                        parent_idx,
                        entry.path,
                        i,
                        sort_mode
                    );
                }
            }
        }
    }

    #[test]
    fn test_sort_with_missing_parents() {
        // Simulate the case where we have top N entries by size, but not all parents
        // This is what happens when using get_largest_entries with a limit
        let entries = vec![
            // Parent is NOT included (too small)
            create_test_entry_with_size(
                1,
                "/root/large_child.txt",
                "large_child.txt",
                1,
                false,
                1000,
            ),
            create_test_entry_with_size(
                2,
                "/root/medium_child.txt",
                "medium_child.txt",
                1,
                false,
                500,
            ),
        ];

        let folded = HashSet::new();
        let visible = compute_visible_entries(&entries, &folded, SortMode::BySize, None);

        // Even without parent, children should be sorted by size
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].size, 1000);
        assert_eq!(visible[1].size, 500);
    }

    #[test]
    fn test_unfold_with_size_sort() {
        // Test unfolding a directory when sorted by size
        let entries = create_test_fixture();

        // Start with /project/src folded
        let mut folded = HashSet::new();
        folded.insert("/project/src".to_string());

        let visible = compute_visible_entries(&entries, &folded, SortMode::BySize, None);
        let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

        // src should be visible but its children hidden
        assert!(paths.contains(&"/project/src"));
        assert!(!paths.contains(&"/project/src/main.rs"));

        // Now unfold src
        folded.remove("/project/src");
        let visible = compute_visible_entries(&entries, &folded, SortMode::BySize, None);
        let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

        // src should still come before its children
        let src_idx = paths.iter().position(|&p| p == "/project/src").unwrap();
        let main_idx = paths
            .iter()
            .position(|&p| p == "/project/src/main.rs")
            .unwrap();
        let utils_idx = paths
            .iter()
            .position(|&p| p == "/project/src/utils")
            .unwrap();

        assert!(
            src_idx < main_idx,
            "src at {} should come before main.rs at {}",
            src_idx,
            main_idx
        );
        assert!(
            src_idx < utils_idx,
            "src at {} should come before utils at {}",
            src_idx,
            utils_idx
        );
    }
}
