//! Treemap visualization for file sizes

use crate::db::StoredFileEntry;
use ratatui::layout::Rect;

/// A rectangle in the treemap with associated file data
#[derive(Debug, Clone)]
pub struct TreemapRect {
    pub entry: StoredFileEntry,
    pub rect: Rect,
    pub color_index: usize,
}

/// Build treemap rectangles using squarified treemap algorithm
pub fn build_treemap(entries: &[StoredFileEntry], area: Rect, depth: usize) -> Vec<TreemapRect> {
    let mut result = Vec::new();

    if entries.is_empty() || area.width == 0 || area.height == 0 {
        return result;
    }

    // Filter to only directories and files at this level, sorted by size
    let mut sorted_entries: Vec<StoredFileEntry> = entries.to_vec();
    sorted_entries.sort_by(|a, b| b.size.cmp(&a.size));

    // Calculate total size
    let total_size: i64 = sorted_entries.iter().map(|e| e.size).sum();

    if total_size == 0 {
        return result;
    }

    // Use squarified treemap layout
    squarify(&sorted_entries, area, total_size, &mut result, depth);

    result
}

/// Squarified treemap algorithm
fn squarify(
    entries: &[StoredFileEntry],
    area: Rect,
    total_size: i64,
    result: &mut Vec<TreemapRect>,
    depth: usize,
) {
    if entries.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }

    let mut remaining = entries.to_vec();
    let mut current_x = area.x;
    let mut current_y = area.y;
    let mut current_width = area.width;
    let mut current_height = area.height;

    while !remaining.is_empty() {
        let is_horizontal = current_width >= current_height;
        let length = if is_horizontal {
            current_width
        } else {
            current_height
        };

        // Take items that fit well together
        let (row, rest) = take_row(&remaining, total_size, length as i64, is_horizontal);

        if row.is_empty() {
            break;
        }

        // Calculate dimensions for this row
        let row_size: i64 = row.iter().map(|e| e.size).sum();
        let row_thickness = if total_size > 0 {
            ((row_size as f64 / total_size as f64) * length as f64) as u16
        } else {
            1
        };

        // Layout items in this row
        let mut offset = 0u16;
        for entry in &row {
            let item_size = ((entry.size as f64 / row_size as f64)
                * (if is_horizontal {
                    current_height
                } else {
                    current_width
                }) as f64) as u16;

            let rect = if is_horizontal {
                Rect {
                    x: current_x,
                    y: current_y + offset,
                    width: row_thickness.min(current_width),
                    height: item_size.min(current_height - offset),
                }
            } else {
                Rect {
                    x: current_x + offset,
                    y: current_y,
                    width: item_size.min(current_width - offset),
                    height: row_thickness.min(current_height),
                }
            };

            // Only add if rect has area
            if rect.width > 0 && rect.height > 0 {
                result.push(TreemapRect {
                    entry: entry.clone(),
                    rect,
                    color_index: depth % 8, // Rotate through 8 colors
                });
            }

            offset += item_size;
        }

        // Update remaining area
        if is_horizontal {
            current_x += row_thickness;
            current_width = current_width.saturating_sub(row_thickness);
        } else {
            current_y += row_thickness;
            current_height = current_height.saturating_sub(row_thickness);
        }

        remaining = rest;
    }
}

/// Take a row of items that should be laid out together
fn take_row(
    entries: &[StoredFileEntry],
    total_size: i64,
    length: i64,
    _is_horizontal: bool,
) -> (Vec<StoredFileEntry>, Vec<StoredFileEntry>) {
    if entries.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let mut row = Vec::new();
    let mut best_ratio = f64::MAX;

    for i in 0..entries.len() {
        row.push(entries[i].clone());
        let ratio = calculate_aspect_ratio(&row, total_size, length);

        if ratio < best_ratio {
            best_ratio = ratio;
        } else {
            // Ratio got worse, don't include this item
            row.pop();
            break;
        }

        // Don't take too many items in one row
        if i >= 10 {
            break;
        }
    }

    if row.is_empty() {
        row.push(entries[0].clone());
    }

    let rest = entries[row.len()..].to_vec();
    (row, rest)
}

/// Calculate worst aspect ratio for a row of items
fn calculate_aspect_ratio(items: &[StoredFileEntry], total_size: i64, length: i64) -> f64 {
    if items.is_empty() || total_size == 0 || length == 0 {
        return f64::MAX;
    }

    let row_size: i64 = items.iter().map(|e| e.size).sum();
    let row_width = (row_size as f64 / total_size as f64) * length as f64;

    let mut worst_ratio = 0.0f64;
    for item in items {
        let item_height = (item.size as f64 / row_size as f64) * length as f64;
        let ratio = if row_width > item_height {
            row_width / item_height
        } else {
            item_height / row_width
        };
        worst_ratio = worst_ratio.max(ratio);
    }

    worst_ratio
}
