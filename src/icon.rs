//! Installs the icon embedded by `build.rs` as the window icon.
//!
//! The resource gives Explorer and pinned shortcuts the icon; `WM_SETICON`
//! gives the running window (title bar and taskbar) the same image. The
//! statically hosted windows are created by `Win32Backend`, so this reads the
//! window handle from the portable layer and talks to the resource directly.

use core::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, HICON, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTSIZE, LR_SHARED,
    LoadImageW, SM_CXSMICON, SM_CYSMICON, SendMessageW, WM_SETICON,
};
use windows::core::PCWSTR;
use xui_core::app::Ui;

use crate::app::Msg;

/// The icon resource id declared in `app.rc`.
const ICON_RESOURCE_ID: u16 = 1;

/// Sets the running window's large and small icons from the embedded resource.
/// Does nothing when the backend exposes no native window.
pub fn apply(ui: &Ui<Msg>) {
    let Some(native) = ui.native_window() else {
        return;
    };
    if native.is_null() {
        return;
    }
    let hwnd = HWND(native.raw() as *mut c_void);

    // SAFETY: the module handle belongs to the running executable; `LoadImageW`
    // reads the icon resource from it and, with `LR_SHARED`, returns a
    // system-owned icon that must not be destroyed. `SendMessageW` only reads
    // the icon handle from `lparam`.
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        let name = PCWSTR(ICON_RESOURCE_ID as usize as *const u16);
        let Ok(big) = LoadImageW(
            Some(module.into()),
            name,
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE | LR_SHARED,
        ) else {
            return;
        };
        let big = HICON(big.0);
        let _ = SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(LPARAM(big.0 as isize)),
        );

        // A small icon loaded at the small system size stays crisp; fall back
        // to the large one when the resource cannot provide one.
        let small = LoadImageW(
            Some(module.into()),
            name,
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_SHARED,
        )
        .map(|handle| HICON(handle.0))
        .unwrap_or(big);
        let _ = SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(LPARAM(small.0 as isize)),
        );
    }
}
