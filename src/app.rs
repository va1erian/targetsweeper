//! The application: the scan/delete state mapped onto the xui widgets.
//!
//! Scanning and deletion run on worker threads and report back through a
//! [`Proxy`], so the UI thread never blocks. Nothing is deleted without an
//! explicit confirmation, and every path is verified again inside the delete
//! worker before it is touched.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use xui_core::app::{App, Proxy, Ui};
use xui_core::arrange::{LayoutExt, Mounted, column, row, spacer};
use xui_core::backend::Result;
use xui_core::widget::{
    Button, Dialog, DialogAction, Fill, HasText, ListView, SortDirection, StatusBar,
};
use xui_core::{Dip, Insets, dip};

use crate::delete::{self, DeleteReport};
use crate::format;
use crate::model::{COLUMN_SIZE, TargetModel, merged_order, sorted_order};
use crate::scan::{self, Target};

/// The messages the UI thread handles.
pub enum Msg {
    /// Starts a scan, or cancels the running one.
    Scan,
    /// A scan worker's progress report.
    ScanProgress {
        run: u64,
        dirs: u64,
        found: usize,
        current: PathBuf,
    },
    /// Targets the running scan found, streamed while it walks.
    TargetsFound { run: u64, targets: Vec<Target> },
    /// A scan finished (or was cancelled), with everything found.
    ScanDone {
        run: u64,
        targets: Vec<Target>,
        dirs: u64,
        errors: u64,
        cancelled: bool,
        elapsed: Duration,
    },
    /// Selects every row.
    SelectAll,
    /// Clears the selection.
    SelectNone,
    /// The list's selection changed.
    Selection(Vec<usize>),
    /// A header was clicked: sort by that column.
    Sort(usize),
    /// The delete button: ask for confirmation.
    RequestDelete,
    /// The confirmation was accepted: start deleting.
    DeleteConfirmed,
    /// A dialog was dismissed without an action.
    DialogClosed,
    /// A delete worker's progress report.
    DeleteProgress { done: usize, total: usize },
    /// A delete run finished.
    DeleteDone(DeleteReport),
}

/// The application state. The mounted layout owns the widget tree; the app
/// keeps handles only for the widgets it updates.
pub struct Sweeper {
    scan: Rc<Button<Msg>>,
    select_all: Rc<Button<Msg>>,
    select_none: Rc<Button<Msg>>,
    delete: Rc<Button<Msg>>,
    list: Rc<ListView<Msg>>,
    status: Rc<StatusBar<Msg>>,
    dialog: Option<Dialog<Msg>>,
    rows: Rc<Vec<Target>>,
    order: Rc<Vec<usize>>,
    selection: Vec<usize>,
    sort: Option<(usize, bool)>,
    scanning: bool,
    deleting: bool,
    cancel: Arc<AtomicBool>,
    /// Incremented per scan; worker messages from older runs are ignored.
    run: u64,
    proxy: Proxy<Msg>,
    state_text: String,
    _mounted: Mounted<Msg>,
}

/// Builds the window's widgets and starts the first scan.
pub fn build(ui: &Ui<Msg>) -> Result<Sweeper> {
    let proxy = ui.proxy();

    let scan = Rc::new(Button::auto(ui, "Scan")?.on_click(|| Some(Msg::Scan)));
    let select_all = Rc::new(Button::auto(ui, "Select all")?.on_click(|| Some(Msg::SelectAll)));
    let select_none = Rc::new(Button::auto(ui, "Select none")?.on_click(|| Some(Msg::SelectNone)));
    let delete =
        Rc::new(Button::auto(ui, "Delete selected")?.on_click(|| Some(Msg::RequestDelete)));
    ui.set_enabled(delete.id(), false);

    let list = Rc::new(
        ListView::auto(
            ui,
            TargetModel::new(Rc::new(Vec::new()), Rc::new(Vec::new())),
        )?
        .column("Project", Dip(180.0))
        .column("Path", Fill)
        .column_right("Size", Dip(90.0))
        .column_right("Modified", Dip(140.0))
        .multi_select(true)
        .on_selection(|rows| Some(Msg::Selection(rows.to_vec())))
        .on_sort(|column| Some(Msg::Sort(column))),
    );
    list.set_sort_indicator(COLUMN_SIZE, SortDirection::Descending);
    let status = Rc::new(StatusBar::auto(
        ui,
        &["Starting scan…", "0 targets", "0 selected"],
    )?);

    let toolbar = row()
        .spacing(dip(8.0))
        .child(&scan)
        .child(&select_all)
        .child(&select_none)
        .child(spacer())
        .child(&delete)
        .height(dip(28.0));
    let root = column()
        .margins(Insets::all(dip(10.0)))
        .spacing(dip(8.0))
        .child(toolbar)
        .child(list.fill(1))
        .child(status.fixed(dip(24.0)));
    let mounted = ui.mount(root)?;

    let mut app = Sweeper {
        scan,
        select_all,
        select_none,
        delete,
        list,
        status,
        dialog: None,
        rows: Rc::new(Vec::new()),
        order: Rc::new(Vec::new()),
        selection: Vec::new(),
        sort: Some((COLUMN_SIZE, false)),
        scanning: false,
        deleting: false,
        cancel: Arc::new(AtomicBool::new(false)),
        run: 0,
        proxy,
        state_text: "Starting scan…".into(),
        _mounted: mounted,
    };
    app.begin_scan(ui);
    Ok(app)
}

impl Sweeper {
    /// Starts a scan on a worker thread; the UI keeps running. Results stream
    /// in through [`Msg::TargetsFound`] while the scan walks.
    fn begin_scan(&mut self, ui: &Ui<Msg>) {
        if self.scanning || self.deleting {
            return;
        }
        self.scanning = true;
        self.run += 1;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.rows = Rc::new(Vec::new());
        self.order = Rc::new(Vec::new());
        self.selection.clear();
        self.list.set_model(self.model());
        self.list.set_selection(&[]);
        self.state_text = "Scanning fixed drives…".into();
        self.scan.set_text("Cancel scan");
        self.refresh_status();
        self.refresh_actions(ui);
        start_scan(
            self.proxy.clone(),
            Arc::clone(&self.cancel),
            scan::fixed_drives(),
            self.run,
        );
    }

    /// The model for the list, in the current order.
    fn model(&self) -> TargetModel {
        TargetModel::new(Rc::clone(&self.rows), Rc::clone(&self.order))
    }

    /// Recomputes the display order for the current sort.
    fn reorder(&mut self) {
        self.order = Rc::new(sorted_order(&self.rows, self.sort));
    }

    /// The selected rows' total size.
    fn selected_size(&self) -> u64 {
        self.selection
            .iter()
            .filter_map(|&row| self.order.get(row).and_then(|&index| self.rows.get(index)))
            .map(|target| target.size)
            .sum()
    }

    /// Updates the status bar from the current state.
    fn refresh_status(&self) {
        let total: u64 = self.rows.iter().map(|target| target.size).sum();
        let files: u64 = self.rows.iter().map(|target| target.files).sum();
        self.status.set_text(0, &self.state_text);
        self.status.set_text(
            1,
            &format!(
                "{} targets, {files} files, {}",
                self.rows.len(),
                format::human_size(total)
            ),
        );
        let selected = self.selection.len();
        if selected == 0 {
            self.status.set_text(2, "0 selected");
        } else {
            self.status.set_text(
                2,
                &format!(
                    "{selected} selected ({})",
                    format::human_size(self.selected_size())
                ),
            );
        }
    }

    /// Enables the buttons and the list for the current state.
    fn refresh_actions(&self, ui: &Ui<Msg>) {
        let idle = !self.scanning && !self.deleting;
        ui.set_enabled(self.scan.id(), !self.deleting);
        ui.set_enabled(self.select_all.id(), idle && !self.rows.is_empty());
        ui.set_enabled(self.select_none.id(), idle && !self.selection.is_empty());
        ui.set_enabled(self.delete.id(), idle && !self.selection.is_empty());
        ui.set_enabled(self.list.id(), !self.deleting);
    }

    /// Opens the confirmation dialog for the current selection.
    fn confirm_delete(&mut self, ui: &Ui<Msg>) {
        if self.selection.is_empty() || self.scanning || self.deleting {
            return;
        }
        let count = self.selection.len();
        let plural = if count == 1 { "y" } else { "ies" };
        let message = format!(
            "Permanently delete {count} Cargo target director{plural} ({} total)? Only the target/ build-artifact trees are removed; sources, Cargo.toml and Cargo.lock are untouched. Nothing is deleted until you confirm.",
            format::human_size(self.selected_size())
        );
        let Ok(dialog) = Dialog::confirm(ui, "Delete target directories?", &message) else {
            return;
        };
        let dialog = dialog
            .accept_label("Delete")
            .on_action(|action| match action {
                DialogAction::Accept(_) => Some(Msg::DeleteConfirmed),
                DialogAction::Cancel => Some(Msg::DialogClosed),
            });
        dialog.open();
        self.dialog = Some(dialog);
    }

    /// Starts the confirmed deletion on a worker thread.
    fn begin_delete(&mut self, ui: &Ui<Msg>) {
        self.dialog = None;
        if self.selection.is_empty() || self.deleting {
            return;
        }
        let paths: Vec<PathBuf> = self
            .selection
            .iter()
            .filter_map(|&row| self.order.get(row).and_then(|&index| self.rows.get(index)))
            .map(|target| target.path.as_ref().to_path_buf())
            .collect();
        if paths.is_empty() {
            return;
        }
        self.deleting = true;
        self.state_text = format!("Deleting {} directories…", paths.len());
        self.refresh_status();
        self.refresh_actions(ui);
        start_delete(self.proxy.clone(), paths);
    }

    /// Applies a finished deletion report: drops the removed rows, reports the
    /// outcome.
    fn finish_delete(&mut self, report: DeleteReport, ui: &Ui<Msg>) {
        self.deleting = false;
        let removed: HashSet<PathBuf> = report.removed.iter().cloned().collect();
        if !removed.is_empty() {
            let remaining: Vec<Target> = self
                .rows
                .iter()
                .filter(|target| !removed.contains(target.path.as_ref()))
                .cloned()
                .collect();
            self.rows = Rc::new(remaining);
            self.selection.clear();
            self.reorder();
            self.list.set_model(self.model());
            self.list.set_selection(&[]);
        }
        self.state_text = if report.failures.is_empty() {
            format!(
                "Deleted {} directories — freed {}",
                report.removed.len(),
                report.freed_text()
            )
        } else {
            format!(
                "Deleted {} directories — freed {}; {} could not be fully removed",
                report.removed.len(),
                report.freed_text(),
                report.failures.len()
            )
        };

        let title = if report.failures.is_empty() {
            "Deletion complete"
        } else {
            "Deletion finished with errors"
        };
        let mut message = format!(
            "Removed {} target directories, freeing {}.",
            report.removed.len(),
            report.freed_text()
        );
        if !report.failures.is_empty() {
            message.push_str(" These could not be fully removed:");
            for (path, error) in report.failures.iter().take(4) {
                message.push_str(&format!(" {} ({error});", path.display()));
            }
            if report.failures.len() > 4 {
                message.push_str(&format!(" and {} more.", report.failures.len() - 4));
            }
        }
        if let Ok(dialog) = Dialog::message(ui, title, &message) {
            let dialog = dialog.on_action(|_| Some(Msg::DialogClosed));
            dialog.open();
            self.dialog = Some(dialog);
        }
        self.refresh_status();
    }
}

impl App for Sweeper {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Scan => {
                if self.scanning {
                    self.cancel.store(true, Ordering::Relaxed);
                    self.state_text = "Cancelling scan…".into();
                    self.refresh_status();
                } else {
                    self.begin_scan(ui);
                }
            }
            Msg::ScanProgress {
                run,
                dirs,
                found,
                current,
            } => {
                if run != self.run {
                    return;
                }
                self.state_text = format!(
                    "Scanning {} — {dirs} directories, {found} targets",
                    current.display()
                );
                self.refresh_status();
            }
            Msg::TargetsFound { run, targets } => {
                if run != self.run {
                    return;
                }
                let old_order = Rc::clone(&self.order);
                let base = self.rows.len();
                let mut rows = self.rows.as_ref().clone();
                rows.extend(targets);
                self.rows = Rc::new(rows);
                // Merge the new rows into the existing order, so a batch does
                // not re-sort every row that was already placed.
                self.order = Rc::new(merged_order(&self.rows, &old_order, base, self.sort));
                // As with a sort, the selection is a set of display rows:
                // remap it by target so streaming rows in cannot make a
                // selected row point at a different directory.
                self.selection =
                    remap_selection(&self.selection, &old_order, &self.order, &self.rows);
                self.list.set_model(self.model());
                self.list.set_selection(&self.selection);
                self.refresh_status();
            }
            Msg::ScanDone {
                run,
                targets,
                dirs,
                errors,
                cancelled,
                elapsed,
            } => {
                if run != self.run {
                    return;
                }
                self.scanning = false;
                let old_order = Rc::clone(&self.order);
                self.rows = Rc::new(targets);
                self.reorder();
                self.selection =
                    remap_selection(&self.selection, &old_order, &self.order, &self.rows);
                self.list.set_model(self.model());
                self.list.set_selection(&self.selection);
                self.scan.set_text("Rescan");
                self.state_text = if cancelled {
                    format!(
                        "Scan cancelled — {dirs} directories visited, {errors} unreadable, {} targets",
                        self.rows.len()
                    )
                } else {
                    format!(
                        "Scanned {dirs} directories in {:.1}s — {} targets, {errors} unreadable",
                        elapsed.as_secs_f64(),
                        self.rows.len()
                    )
                };
                self.refresh_status();
            }
            Msg::SelectAll => {
                let all: Vec<usize> = (0..self.rows.len()).collect();
                self.list.set_selection(&all);
                self.selection = all;
                self.refresh_status();
            }
            Msg::SelectNone => {
                self.list.set_selection(&[]);
                self.selection.clear();
                self.refresh_status();
            }
            Msg::Selection(rows) => {
                self.selection = rows;
                self.refresh_status();
            }
            Msg::Sort(column) => {
                let old_order = Rc::clone(&self.order);
                let ascending = match self.sort {
                    Some((sorted, was)) if sorted == column => !was,
                    _ => true,
                };
                self.sort = Some((column, ascending));
                self.reorder();
                // The selection is stored as display-row indices; remap it by
                // target so a sort cannot leave a row pointing at another
                // directory (and a later delete deleting something else).
                self.selection =
                    remap_selection(&self.selection, &old_order, &self.order, &self.rows);
                self.list.set_sort_indicator(
                    column,
                    if ascending {
                        SortDirection::Ascending
                    } else {
                        SortDirection::Descending
                    },
                );
                self.list.set_model(self.model());
                self.list.set_selection(&self.selection);
                self.refresh_status();
            }
            Msg::RequestDelete => self.confirm_delete(ui),
            Msg::DeleteConfirmed => self.begin_delete(ui),
            Msg::DialogClosed => self.dialog = None,
            Msg::DeleteProgress { done, total } => {
                self.state_text = format!("Deleting {done}/{total}…");
                self.refresh_status();
            }
            Msg::DeleteDone(report) => self.finish_delete(report, ui),
        }
        self.refresh_actions(ui);
    }
}

/// Scans on a worker thread and reports through the proxy: progress while
/// walking, found targets in small batches, then the final outcome.
fn start_scan(proxy: Proxy<Msg>, cancel: Arc<AtomicBool>, roots: Vec<PathBuf>, run: u64) {
    thread::spawn(move || {
        let started = Instant::now();
        let outcome = {
            let mut last_progress = Instant::now();
            let batch = RefCell::new(FoundBatch::new(proxy.clone(), run));
            let mut progress = |dir: &Path, dirs: u64, found: usize| {
                if last_progress.elapsed() >= PROGRESS_REPORT_EVERY {
                    last_progress = Instant::now();
                    let _ = proxy.send(Msg::ScanProgress {
                        run,
                        dirs,
                        found,
                        current: dir.to_path_buf(),
                    });
                    // A lone target caught between progress ticks still shows
                    // up promptly.
                    batch.borrow_mut().flush();
                }
            };
            let mut found = |target: &Target| batch.borrow_mut().push(target);
            let outcome = scan::scan(&roots, &cancel, &mut progress, &mut found);
            batch.borrow_mut().flush();
            outcome
        };
        let _ = proxy.send(Msg::ScanDone {
            run,
            targets: outcome.targets,
            dirs: outcome.dirs,
            errors: outcome.errors,
            cancelled: outcome.cancelled,
            elapsed: started.elapsed(),
        });
    });
}

/// Batches streamed targets so a burst is one message, but never holds them
/// longer than [`FOUND_REPORT_EVERY`].
struct FoundBatch {
    proxy: Proxy<Msg>,
    run: u64,
    pending: Vec<Target>,
    last: Instant,
}

impl FoundBatch {
    fn new(proxy: Proxy<Msg>, run: u64) -> FoundBatch {
        FoundBatch {
            proxy,
            run,
            pending: Vec::new(),
            last: Instant::now(),
        }
    }

    fn push(&mut self, target: &Target) {
        self.pending.push(target.clone());
        if self.pending.len() >= FOUND_BATCH || self.last.elapsed() >= FOUND_REPORT_EVERY {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        self.last = Instant::now();
        let _ = self.proxy.send(Msg::TargetsFound {
            run: self.run,
            targets: std::mem::take(&mut self.pending),
        });
    }
}

/// How often the scan worker reports progress.
const PROGRESS_REPORT_EVERY: Duration = Duration::from_millis(150);
/// How many found targets are batched before a [`Msg::TargetsFound`] send.
const FOUND_BATCH: usize = 16;
/// How long found targets may wait for a batch, so slow scans still stream.
const FOUND_REPORT_EVERY: Duration = Duration::from_millis(250);

/// Deletes on a worker thread, reporting per-directory progress and a final
/// report.
fn start_delete(proxy: Proxy<Msg>, paths: Vec<PathBuf>) {
    thread::spawn(move || {
        let total = paths.len();
        let mut report = DeleteReport::default();
        for (index, path) in paths.iter().enumerate() {
            let _ = proxy.send(Msg::DeleteProgress {
                done: index + 1,
                total,
            });
            match delete::delete_target(path) {
                Ok(freed) => {
                    report.freed += freed;
                    report.removed.push(path.clone());
                }
                Err(error) => report.failures.push((path.clone(), error)),
            }
        }
        let _ = proxy.send(Msg::DeleteDone(report));
    });
}

/// Remaps display-row indices to the same targets after `order` changed, so a
/// reorder (a sort, or newly streamed rows) can never leave a selected row
/// pointing at a different directory.
fn remap_selection(
    selection: &[usize],
    old_order: &[usize],
    new_order: &[usize],
    rows: &[Target],
) -> Vec<usize> {
    let selected: HashSet<&Path> = selection
        .iter()
        .filter_map(|&row| old_order.get(row).and_then(|&index| rows.get(index)))
        .map(|target| target.path.as_ref())
        .collect();
    new_order
        .iter()
        .enumerate()
        .filter_map(|(row, &index)| {
            rows.get(index)
                .is_some_and(|target| selected.contains(target.path.as_ref()))
                .then_some(row)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(path: &str) -> Target {
        Target {
            path: Arc::from(PathBuf::from(path)),
            project: Arc::from(path),
            size: 0,
            files: 0,
            modified: None,
            path_text: Arc::from(path),
            size_text: Arc::from(""),
            modified_text: Arc::from(""),
        }
    }

    #[test]
    fn sorting_remaps_the_selection_by_target() {
        let rows = vec![
            target("C:\\a\\target"),
            target("C:\\b\\target"),
            target("C:\\c\\target"),
        ];
        let old_order = vec![0, 1, 2];
        // Reversed: c, b, a. Rows 0 (a) and 2 (c) become rows 2 and 0.
        assert_eq!(
            remap_selection(&[0, 2], &old_order, &[2, 1, 0], &rows),
            vec![0, 2]
        );
        // A selection whose target is no longer listed maps to nothing.
        assert!(remap_selection(&[1], &old_order, &[2, 0], &rows[0..1]).is_empty());
    }
}
