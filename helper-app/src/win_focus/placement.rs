//! Where the palette, the error toast and the debug window appear on screen.

use super::*;

#[cfg(windows)]
pub fn center_palette(hwnd: isize, premiere_hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        let hwnd = hwnd as HWND;
        ensure_unminimized_and_unmaximized(hwnd);
        let native_rounding = configure_palette_frame(hwnd);
        let monitor = MonitorFromWindow(premiere_hwnd as HWND, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        let mut window: RECT = std::mem::zeroed();
        if GetMonitorInfoW(monitor, &mut info) == 0 || GetWindowRect(hwnd, &mut window) == 0 {
            return;
        }
        let screen = info.rcMonitor;
        let move_to_center = |window: &RECT| {
            let width = window.right - window.left;
            let height = window.bottom - window.top;
            let x = screen.left + (screen.right - screen.left - width) / 2;
            let y = screen.top + (screen.bottom - screen.top - height) / 2;
            SetWindowPos(
                hwnd, std::ptr::null_mut(), x, y, 0, 0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        };
        move_to_center(&window);
        // Windows can resize the palette when it moves to a monitor with a different DPI.
        if GetWindowRect(hwnd, &mut window) == 0 {
            return;
        }
        move_to_center(&window);
        if native_rounding {
            return;
        }
        let width = window.right - window.left;
        let height = window.bottom - window.top;

        // Match the 10 logical pixel radius of the Slint outline.
        let dpi = GetDpiForWindow(hwnd);
        let diameter = if dpi == 0 { 20 } else { ((20 * dpi + 48) / 96) as i32 };
        let rounded = CreateRoundRectRgn(0, 0, width, height, diameter, diameter);
        if !rounded.is_null() && SetWindowRgn(hwnd, rounded, 1) == 0 {
            DeleteObject(rounded);
        }
    }
}

#[cfg(not(windows))]
pub fn center_palette(_hwnd: isize, _premiere_hwnd: isize) {}

#[cfg(windows)]
pub(super) fn monitor_work_area_and_dpi(premiere_hwnd: isize) -> Option<(RECT, u32, u32)> {
    unsafe {
        let anchor = if premiere_hwnd == 0 {
            GetForegroundWindow()
        } else {
            premiere_hwnd as HWND
        };
        let monitor = MonitorFromWindow(anchor, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        let (mut dpi_x, mut dpi_y) = (96, 96);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) < 0 {
            (dpi_x, dpi_y) = (96, 96);
        }
        Some((info.rcWork, dpi_x, dpi_y))
    }
}

#[cfg(windows)]
pub fn error_toast_position(
    premiere_hwnd: isize,
    logical_height: f32,
) -> Option<slint::PhysicalPosition> {
    let (work, dpi_x, dpi_y) = monitor_work_area_and_dpi(premiere_hwnd)?;
    let margin_x = (20.0 * dpi_x as f32 / 96.0).round() as i32;
    let bottom_offset = ((logical_height + 20.0) * dpi_y as f32 / 96.0).round() as i32;
    Some(slint::PhysicalPosition::new(
        work.left + margin_x,
        (work.bottom - bottom_offset).max(work.top),
    ))
}

#[cfg(not(windows))]
pub fn error_toast_position(
    _premiere_hwnd: isize,
    _logical_height: f32,
) -> Option<slint::PhysicalPosition> {
    None
}

#[cfg(windows)]
pub fn debug_window_position(
    premiere_hwnd: isize,
    logical_width: f32,
    logical_height: f32,
) -> Option<slint::PhysicalPosition> {
    let (work, dpi_x, dpi_y) = monitor_work_area_and_dpi(premiere_hwnd)?;
    let right_offset = ((logical_width + 20.0) * dpi_x as f32 / 96.0).round() as i32;
    let bottom_offset = ((logical_height + 20.0) * dpi_y as f32 / 96.0).round() as i32;
    Some(slint::PhysicalPosition::new(
        (work.right - right_offset).max(work.left),
        (work.bottom - bottom_offset).max(work.top),
    ))
}

#[cfg(not(windows))]
pub fn debug_window_position(
    _premiere_hwnd: isize,
    _logical_width: f32,
    _logical_height: f32,
) -> Option<slint::PhysicalPosition> {
    None
}

#[cfg(windows)]
pub fn keep_toast_inactive(window: &slint::Window) {
    let hwnd = window_hwnd(window) as HWND;
    if !hwnd.is_null() {
        unsafe {
            let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            let style = (style | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW;
            SetWindowLongW(hwnd, GWL_EXSTYLE, style as i32);
        }
    }
}

#[cfg(not(windows))]
pub fn keep_toast_inactive(_window: &slint::Window) {}

pub fn window_hwnd(_window: &slint::Window) -> isize {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let win_handle = _window.window_handle();
        if let Ok(handle) = win_handle.window_handle() {
            if let RawWindowHandle::Win32(win32) = handle.as_raw() {
                return win32.hwnd.get();
            }
        }
    }
    0
}
