//! The virtual model the `ListView` paints, and the row ordering.

use std::cmp::Ordering;
use std::rc::Rc;

use xui_core::widget::ListModel;

use crate::scan::Target;

/// Columns: project, path, size, modified.
pub const COLUMN_PROJECT: usize = 0;
pub const COLUMN_PATH: usize = 1;
pub const COLUMN_SIZE: usize = 2;
pub const COLUMN_MODIFIED: usize = 3;

/// A [`ListModel`] over the scanned targets, in the app's current order.
pub struct TargetModel {
    rows: Rc<Vec<Target>>,
    order: Rc<Vec<usize>>,
}

impl TargetModel {
    /// A model over `rows`, displayed in `order` (an index into `rows`).
    pub fn new(rows: Rc<Vec<Target>>, order: Rc<Vec<usize>>) -> TargetModel {
        TargetModel { rows, order }
    }
}

impl ListModel for TargetModel {
    fn rows(&self) -> usize {
        self.order.len()
    }

    fn cell(&self, row: usize, column: usize) -> Option<&str> {
        let target = self.rows.get(*self.order.get(row)?)?;
        Some(match column {
            COLUMN_PROJECT => &*target.project,
            COLUMN_PATH => &*target.path_text,
            COLUMN_SIZE => &*target.size_text,
            COLUMN_MODIFIED => &*target.modified_text,
            _ => "",
        })
    }
}

/// The row indices of `rows`, ordered by `sort` (a column and whether it is
/// ascending). Unset sorts keep the scan order; ties break on the path.
pub fn sorted_order(rows: &[Target], sort: Option<(usize, bool)>) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| row_order(rows, a, b, sort));
    order
}

/// Merges the rows appended after `base` into an `order` that is already
/// sorted by `sort`, so streaming a batch does not re-sort every existing row.
pub fn merged_order(
    rows: &[Target],
    order: &[usize],
    base: usize,
    sort: Option<(usize, bool)>,
) -> Vec<usize> {
    let mut fresh: Vec<usize> = (base..rows.len()).collect();
    fresh.sort_by(|&a, &b| row_order(rows, a, b, sort));

    let mut merged = Vec::with_capacity(rows.len());
    let (mut left, mut right) = (0, 0);
    while left < order.len() && right < fresh.len() {
        if row_order(rows, order[left], fresh[right], sort).is_le() {
            merged.push(order[left]);
            left += 1;
        } else {
            merged.push(fresh[right]);
            right += 1;
        }
    }
    merged.extend_from_slice(&order[left..]);
    merged.extend_from_slice(&fresh[right..]);
    merged
}

/// Compares two rows by the active sort; equal rows keep the path as a stable
/// tiebreaker.
fn row_order(rows: &[Target], a: usize, b: usize, sort: Option<(usize, bool)>) -> Ordering {
    let Some((column, ascending)) = sort else {
        return a.cmp(&b);
    };
    let ordering = compare(rows, a, b, column);
    if ascending {
        ordering
    } else {
        ordering.reverse()
    }
}

fn compare(rows: &[Target], a: usize, b: usize, column: usize) -> Ordering {
    let (left, right) = (&rows[a], &rows[b]);
    let primary = match column {
        COLUMN_PROJECT => cmp_ignore_ascii_case(&left.project, &right.project),
        COLUMN_PATH => cmp_ignore_ascii_case(&left.path_text, &right.path_text),
        COLUMN_SIZE => left.size.cmp(&right.size),
        COLUMN_MODIFIED => left.modified.cmp(&right.modified),
        _ => Ordering::Equal,
    };
    primary.then_with(|| left.path.cmp(&right.path))
}

/// Case-insensitive ASCII ordering that allocates nothing (a sort must not
/// build a lowercase copy per comparison).
fn cmp_ignore_ascii_case(left: &str, right: &str) -> Ordering {
    let mut left = left.bytes();
    let mut right = right.bytes();
    loop {
        match (left.next(), right.next()) {
            (Some(a), Some(b)) => {
                let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
                if a != b {
                    return a.cmp(&b);
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    fn target(name: &str, size: u64, age_secs: u64) -> Target {
        let path = PathBuf::from(format!("C:\\{name}\\target"));
        Target {
            path_text: Arc::from(path.to_string_lossy().into_owned()),
            path: Arc::from(path),
            project: Arc::from(name),
            size,
            files: 0,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(age_secs)),
            size_text: Arc::from(""),
            modified_text: Arc::from(""),
        }
    }

    #[test]
    fn sorting_by_size_and_date_orders_the_indices() {
        let rows = vec![
            target("b", 10, 300),
            target("a", 30, 100),
            target("c", 20, 200),
        ];
        assert_eq!(
            sorted_order(&rows, Some((COLUMN_SIZE, false))),
            vec![1, 2, 0]
        );
        assert_eq!(
            sorted_order(&rows, Some((COLUMN_MODIFIED, true))),
            vec![1, 2, 0]
        );
        assert_eq!(
            sorted_order(&rows, Some((COLUMN_PROJECT, true))),
            vec![1, 0, 2]
        );
        assert_eq!(sorted_order(&rows, None), vec![0, 1, 2]);
    }

    #[test]
    fn a_merged_batch_keeps_the_order_and_places_the_new_rows() {
        let mut rows = vec![target("a", 30, 100), target("c", 10, 300)];
        let sort = Some((COLUMN_SIZE, true));
        let order = sorted_order(&rows, sort);
        assert_eq!(order, vec![1, 0]);

        // Two more targets arrive; merging must not disturb a and c.
        rows.push(target("b", 20, 200));
        rows.push(target("d", 5, 400));
        let merged = merged_order(&rows, &order, 2, sort);
        assert_eq!(merged, vec![3, 1, 2, 0], "d, c, b, a by size");

        let by_name = merged_order(&rows, &[0, 1], 2, Some((COLUMN_PROJECT, true)));
        assert_eq!(by_name, vec![0, 2, 1, 3], "a, b, c, d by project");

        // With no sort, existing order is kept and new rows append in place.
        assert_eq!(merged_order(&rows, &order, 2, None), vec![1, 0, 2, 3]);
    }
}
