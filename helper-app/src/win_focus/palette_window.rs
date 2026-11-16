//! The palette window itself: its frameless style, caption color, z-order and visibility.

use super::*;

#[cfg(windows)]
pub(super) unsafe fn ensure_unminimized_and_unmaximized(target: HWND) {
    if IsIconic(target) != 0 || IsZoomed(target) != 0 {
        ShowWindow(target, SW_RESTORE);
    }
}

#[cfg(windows)]
pub fn is_window_visible_and_unminimized(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    unsafe {
        let target = hwnd as HWND;
        IsWindowVisible(target) != 0 && IsIconic(target) == 0
    }
}

#[cfg(not(windows))]
pub fn is_window_visible_and_unminimized(_hwnd: isize) -> bool {
    false
}

#[cfg(windows)]
pub fn is_palette_open_and_active(hwnd: isize) -> bool {
    if !is_window_visible_and_unminimized(hwnd) {
        return false;
    }
    unsafe { GetForegroundWindow() == hwnd as HWND }
}

#[cfg(not(windows))]
pub fn is_palette_open_and_active(_hwnd: isize) -> bool {
    false
}

#[cfg(windows)]
pub fn init_palette_window(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    set_palette_hwnd(hwnd);
    unsafe {
        configure_palette_frame(hwnd as HWND);
    }
}

#[cfg(not(windows))]
pub fn init_palette_window(_hwnd: isize) {}

#[cfg(windows)]
pub fn set_palette_caption_color(hwnd: isize, [red, green, blue]: [u8; 3]) {
    if hwnd == 0 {
        return;
    }
    let color = u32::from_le_bytes([red, green, blue, 0]);
    unsafe {
        DwmSetWindowAttribute(
            hwnd as HWND,
            DWMWA_CAPTION_COLOR as u32,
            (&color as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        );
    }
}

#[cfg(not(windows))]
pub fn set_palette_caption_color(_hwnd: isize, _color: [u8; 3]) {}

#[cfg(windows)]
pub fn ensure_palette_topmost(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        let target = hwnd as HWND;
        ensure_unminimized_and_unmaximized(target);
        // Include SWP_SHOWWINDOW if the OS window was hidden behind Slint's visibility cache.
        let mut flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
        if IsWindowVisible(target) == 0 {
            flags |= SWP_SHOWWINDOW;
        }
        SetWindowPos(target, HWND_TOPMOST, 0, 0, 0, 0, flags);
    }
}

#[cfg(not(windows))]
pub fn ensure_palette_topmost(_hwnd: isize) {}

#[cfg(windows)]
pub(super) const WM_NCUAHDRAWCAPTION: u32 = 0x00AE;
#[cfg(windows)]
pub(super) const WM_NCUAHDRAWFRAME: u32 = 0x00AF;
#[cfg(windows)]
pub(super) const FRAMELESS_STRIP_MASK: u32 =
    WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU;

pub(super) static PALETTE_VISIBLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
pub(super) static PALETTE_HWND: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);

pub fn set_palette_visible(visible: bool) {
    PALETTE_VISIBLE.store(visible, std::sync::atomic::Ordering::Release);
}

pub fn set_palette_hwnd(hwnd: isize) {
    if hwnd != 0 {
        PALETTE_HWND.store(hwnd, std::sync::atomic::Ordering::Release);
    }
}

pub fn palette_hwnd() -> isize {
    PALETTE_HWND.load(std::sync::atomic::Ordering::Acquire)
}

#[cfg(windows)]
thread_local! {
    static PALETTE_FRAME: std::cell::Cell<(HWND, WNDPROC, bool)> =
        const { std::cell::Cell::new((std::ptr::null_mut(), None, false)) };
}

#[cfg(windows)]
pub(super) unsafe extern "system" fn palette_frame(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let (palette, previous, _) = PALETTE_FRAME.with(|frame| frame.get());
    if hwnd != palette || previous.is_none() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    match message {
        // Keep winit from restoring WS_CAPTION/WS_SYSMENU on show, hide, or z-order updates.
        WM_STYLECHANGING if wparam as i32 == GWL_STYLE => {
            let styles = &mut *(lparam as *mut STYLESTRUCT);
            styles.styleNew &= !FRAMELESS_STRIP_MASK;
            0
        }
        // Keep the palette as a tool window so it stays out of Alt+Tab and app minimize/maximize.
        WM_STYLECHANGING if wparam as i32 == GWL_EXSTYLE => {
            let styles = &mut *(lparam as *mut STYLESTRUCT);
            styles.styleNew = (styles.styleNew | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW;
            0
        }
        WM_SYSCOMMAND
            if matches!(
                (wparam & 0xFFF0) as u32,
                SC_MINIMIZE | SC_MAXIMIZE
            ) =>
        {
            0
        }
        // Use the entire window for Slint and suppress classic Windows non-client caption painting.
        WM_NCCALCSIZE | WM_NCPAINT | WM_NCUAHDRAWCAPTION | WM_NCUAHDRAWFRAME => 0,
        WM_NCACTIVATE => CallWindowProcW(previous, hwnd, message, wparam, -1),
        WM_NCDESTROY => {
            PALETTE_FRAME.with(|frame| frame.set((std::ptr::null_mut(), None, false)));
            CallWindowProcW(previous, hwnd, message, wparam, lparam)
        }
        _ => CallWindowProcW(previous, hwnd, message, wparam, lparam),
    }
}

#[cfg(windows)]
pub(super) unsafe fn configure_palette_frame(hwnd: HWND) -> bool {
    let (palette, _, rounding) = PALETTE_FRAME.with(|frame| frame.get());
    if hwnd == palette {
        return rounding;
    }
    let previous = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, palette_frame as *const () as isize);
    if previous == 0 {
        return false;
    }
    let previous = std::mem::transmute::<isize, WNDPROC>(previous);
    PALETTE_FRAME.with(|frame| frame.set((hwnd, previous, false)));
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    let stripped = style & !FRAMELESS_STRIP_MASK;
    if stripped != style {
        SetWindowLongW(hwnd, GWL_STYLE, stripped as i32);
    }
    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    let tool_ex_style = (ex_style | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW;
    if tool_ex_style != ex_style {
        SetWindowLongW(hwnd, GWL_EXSTYLE, tool_ex_style as i32);
    }
    let set_attribute = |attribute: i32, value: u32| {
        DwmSetWindowAttribute(hwnd, attribute as u32, (&value as *const u32).cast(), 4)
    };
    set_attribute(DWMWA_BORDER_COLOR, 0xffff_fffe); // DWMWA_COLOR_NONE
    let rounded = set_attribute(DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND as u32) >= 0;
    PALETTE_FRAME.with(|frame| frame.set((hwnd, previous, rounded)));
    SetWindowPos(
        hwnd,
        std::ptr::null_mut(),
        0,
        0,
        0,
        0,
        SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
    );
    rounded
}
