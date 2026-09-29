//! Target Sweeper: find Cargo `target` directories on the fixed drives and
//! delete selected ones, with an explicit confirmation and a second safety
//! verification inside the delete worker.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod delete;
mod format;
mod icon;
mod model;
mod scan;

use std::rc::Rc;

use xui_core::Dip;
use xui_core::app::run_app;
use xui_core::backend::{Backend, PlatformSpec, Result};

fn main() -> Result<()> {
    let backend: Rc<dyn Backend> = Rc::new(xui_win32::Win32Backend::new());
    run_app(
        backend,
        PlatformSpec::new("Target Sweeper").size(Dip(1100.0), Dip(680.0)),
        |ui| {
            icon::apply(ui);
            app::build(ui).expect("the app's widgets were created")
        },
    )
}
