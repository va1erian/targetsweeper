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
            COLUMN_PROJECT => target.project.as_str(),
            COLUMN_PATH => target.path.to_str()?,
            COLUMN_SIZE => target.size_text.as_str(),
            COLUMN_MODIFIED => target.modified_text.as_str(),
            _ => "",
        })
    }
}

/// The row indices of `rows`, ordered by `sort` (a column and whether it is
/// ascending). Unset sorts keep the scan order; ties break on the path.
pub fn sorted_order(rows: &[Target], sort: Option<(usize, bool)>) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    if let Some((column, ascending)) = sort {
        order.sort_by(|&a, &b| {
            let ordering = compare(rows, a, b, column);
            if ascending {
                ordering
            } else {
                ordering.reverse()
            }
        });
    }
    order
}

fn compare(rows: &[Target], a: usize, b: usize, column: usize) -> Ordering {
    let (left, right) = (&rows[a], &rows[b]);
    let primary = match column {
        COLUMN_PROJECT => left
            .project
            .to_ascii_lowercase()
            .cmp(&right.project.to_ascii_lowercase()),
        COLUMN_PATH => left
            .path
            .to_string_lossy()
            .to_ascii_lowercase()
            .cmp(&right.path.to_string_lossy().to_ascii_lowercase()),
        COLUMN_SIZE => left.size.cmp(&right.size),
        COLUMN_MODIFIED => left.modified.cmp(&right.modified),
        _ => Ordering::Equal,
    };
    primary.then_with(|| left.path.cmp(&right.path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn target(name: &str, size: u64, age_secs: u64) -> Target {
        Target {
            path: std::path::PathBuf::from(format!("C:\\{name}\\target")),
            project: name.to_string(),
            size,
            files: 0,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(age_secs)),
            size_text: String::new(),
            modified_text: String::new(),
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
}
