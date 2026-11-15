#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, BOOL, HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR,
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    },
    Graphics::Gdi::{
        CreateRoundRectRgn, DeleteObject, GetMonitorInfoW, MonitorFromWindow, SetWindowRgn,
        MONITORINFO, MONITOR_DEFAULTTONEAREST,
    },
    System::Threading::{
        AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId, OpenProcess,
        QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    },
    UI::{
        Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
        HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI},
        Input::KeyboardAndMouse::{
            keybd_event, GetAsyncKeyState, KEYEVENTF_KEYUP, VK_CONTROL, VK_MENU,
        },
        WindowsAndMessaging::{
            AllowSetForegroundWindow, BringWindowToTop, CallWindowProcW, DefWindowProcW,
            EnumWindows, GetAncestor, GetForegroundWindow, GetWindowLongW, GetWindowRect,
            GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed,
            SetForegroundWindow, SetWindowLongPtrW, SetWindowLongW, SetWindowPos, ShowWindow,
            EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART,
            GA_ROOTOWNER, GWLP_WNDPROC, GWL_EXSTYLE, GWL_STYLE, HWND_TOPMOST, SC_MAXIMIZE,
            SC_MINIMIZE, STYLESTRUCT, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SWP_NOZORDER, SWP_SHOWWINDOW, SW_RESTORE, WINEVENT_OUTOFCONTEXT, WM_NCACTIVATE,
            WM_NCCALCSIZE, WM_NCDESTROY, WM_NCPAINT, WM_STYLECHANGING, WM_SYSCOMMAND, WNDPROC,
            WS_CAPTION, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_MAXIMIZEBOX,
            WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME,
        },
    },
};

#[cfg(windows)]
pub fn get_foreground_hwnd() -> isize {
    unsafe { GetForegroundWindow() as isize }
}

#[cfg(not(windows))]
pub fn get_foreground_hwnd() -> isize {
    0
}

#[cfg(windows)]
pub fn is_premiere_hwnd(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    unsafe {
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd as _, &mut pid);
        if pid == 0 {
            return false;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 || len == 0 {
            return false;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
        path.ends_with("adobe premiere pro.exe")
    }
}

#[cfg(not(windows))]
pub fn is_premiere_hwnd(_hwnd: isize) -> bool {
    true
}

#[cfg(windows)]
pub fn is_premiere_active_and_unminimized(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    unsafe {
        if IsIconic(hwnd as _) != 0 {
            return false;
        }
        if is_premiere_hwnd(hwnd) {
            return true;
        }
        let root = GetAncestor(hwnd as _, GA_ROOTOWNER);
        !root.is_null()
            && root as isize != hwnd
            && IsIconic(root) == 0
            && is_premiere_hwnd(root as isize)
    }
}

#[cfg(not(windows))]
pub fn is_premiere_active_and_unminimized(_hwnd: isize) -> bool {
    true
}

#[cfg(windows)]
struct EnumPremiereState {
    any_running: bool,
    unminimized_hwnd: isize,
}

#[cfg(windows)]
unsafe extern "system" fn enum_premiere_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = &mut *(lparam as *mut EnumPremiereState);
    if is_premiere_hwnd(hwnd as isize) {
        state.any_running = true;
        if IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 && GetWindowTextLengthW(hwnd) > 0 {
            state.unminimized_hwnd = hwnd as isize;
            return 0; // Stop enumeration once we found an active unminimized Premiere window
        }
    }
    1
}

/// Returns `(is_premiere_running, unminimized_premiere_hwnd)`
#[cfg(windows)]
pub fn query_premiere_window_state() -> (bool, isize) {
    unsafe {
        let mut state = EnumPremiereState {
            any_running: false,
            unminimized_hwnd: 0,
        };
        EnumWindows(
            Some(enum_premiere_proc),
            (&mut state as *mut EnumPremiereState) as LPARAM,
        );
        (state.any_running, state.unminimized_hwnd)
    }
}

#[cfg(not(windows))]
pub fn query_premiere_window_state() -> (bool, isize) {
    (true, 1)
}

#[cfg(windows)]
pub fn allow_current_process_foreground() {
    unsafe {
        AllowSetForegroundWindow(GetCurrentProcessId());
    }
}

#[cfg(not(windows))]
pub fn allow_current_process_foreground() {}

#[cfg(windows)]
unsafe fn activate_window_without_synthetic_keys(target: HWND) {
    if IsIconic(target) != 0 {
        ShowWindow(target, SW_RESTORE);
    }
    let fg = GetForegroundWindow();
    if fg == target {
        BringWindowToTop(target);
        return;
    }

    // Try a direct foreground transfer first so valid WM_HOTKEY / active-window rights
    // succeed without attaching input queues to another app.
    BringWindowToTop(target);
    SetForegroundWindow(target);
    if GetForegroundWindow() == target {
        return;
    }

    let cur_tid = GetCurrentThreadId();
    let fg_tid = if fg.is_null() {
        0
    } else {
        GetWindowThreadProcessId(fg, std::ptr::null_mut())
    };
    let target_tid = GetWindowThreadProcessId(target, std::ptr::null_mut());
    let attach_cur = fg_tid != 0 && fg_tid != cur_tid && AttachThreadInput(cur_tid, fg_tid, 1) != 0;
    let attach_target = target_tid != 0
        && target_tid != cur_tid
        && target_tid != fg_tid
        && AttachThreadInput(target_tid, cur_tid, 1) != 0;

    BringWindowToTop(target);
    SetForegroundWindow(target);

    if attach_target {
        AttachThreadInput(target_tid, cur_tid, 0);
    }
    if attach_cur {
        AttachThreadInput(cur_tid, fg_tid, 0);
    }
}

#[cfg(windows)]
pub fn force_foreground(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        let target = hwnd as HWND;
        activate_window_without_synthetic_keys(target);

        // Only use an Alt tap when Ctrl is not held so we never inject Ctrl+Alt on hotkey presses.
        if GetForegroundWindow() != target && GetAsyncKeyState(VK_CONTROL as i32) >= 0 {
            keybd_event(VK_MENU as u8, 0, 0, 0);
            keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
            BringWindowToTop(target);
            SetForegroundWindow(target);
        }
    }
}

#[cfg(not(windows))]
pub fn force_foreground(_hwnd: isize) {}

#[cfg(windows)]
pub fn restore_foreground(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        // Never inject synthetic Alt when returning focus to Premiere, or its menu bar activates.
        activate_window_without_synthetic_keys(hwnd as HWND);
    }
}

#[cfg(not(windows))]
pub fn restore_foreground(_hwnd: isize) {}

#[cfg(windows)]
unsafe fn ensure_unminimized_and_unmaximized(target: HWND) {
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
    unsafe {
        configure_palette_frame(hwnd as HWND);
    }
}

#[cfg(not(windows))]
pub fn init_palette_window(_hwnd: isize) {}

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
const WM_NCUAHDRAWCAPTION: u32 = 0x00AE;
#[cfg(windows)]
const WM_NCUAHDRAWFRAME: u32 = 0x00AF;
#[cfg(windows)]
const FRAMELESS_STRIP_MASK: u32 =
    WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU;

#[cfg(windows)]
thread_local! {
    static PALETTE_FRAME: std::cell::Cell<(HWND, WNDPROC, bool)> =
        const { std::cell::Cell::new((std::ptr::null_mut(), None, false)) };
    static HOOK_CALLBACK: std::cell::RefCell<Option<std::rc::Rc<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(windows)]
pub struct ForegroundHook {
    hooks: Vec<HWINEVENTHOOK>,
}

#[cfg(windows)]
impl ForegroundHook {
    /// Installs out-of-context WinEvent hooks for foreground and minimize/restore transitions.
    pub fn install<F: Fn() + 'static>(on_change: F) -> Self {
        HOOK_CALLBACK.with(|slot| {
            *slot.borrow_mut() = Some(std::rc::Rc::new(on_change));
        });
        let mut hooks = Vec::with_capacity(2);
        unsafe {
            for (min_ev, max_ev) in [
                (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
                (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            ] {
                let hook = SetWinEventHook(
                    min_ev,
                    max_ev,
                    std::ptr::null_mut(),
                    Some(winevent_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                );
                if !hook.is_null() {
                    hooks.push(hook);
                }
            }
        }
        Self { hooks }
    }
}

#[cfg(windows)]
impl Drop for ForegroundHook {
    fn drop(&mut self) {
        unsafe {
            for hook in self.hooks.drain(..) {
                UnhookWinEvent(hook);
            }
        }
        HOOK_CALLBACK.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }
}

#[cfg(not(windows))]
pub struct ForegroundHook;

#[cfg(not(windows))]
impl ForegroundHook {
    pub fn install<F: Fn() + 'static>(_on_change: F) -> Self {
        Self
    }
}

#[cfg(windows)]
unsafe extern "system" fn winevent_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _event_time: u32,
) {
    let cb = HOOK_CALLBACK.with(|slot| slot.borrow().clone());
    if let Some(cb) = cb {
        cb();
    }
}

#[cfg(windows)]
unsafe extern "system" fn palette_frame(
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
unsafe fn configure_palette_frame(hwnd: HWND) -> bool {
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
    // Match the search header if the compositor draws any part of the caption.
    set_attribute(DWMWA_CAPTION_COLOR, 0x0022_1817);
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
pub fn error_toast_position(
    premiere_hwnd: isize,
    logical_height: f32,
) -> Option<slint::PhysicalPosition> {
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
        let margin_x = (20.0 * dpi_x as f32 / 96.0).round() as i32;
        let bottom_offset = ((logical_height + 20.0) * dpi_y as f32 / 96.0).round() as i32;
        Some(slint::PhysicalPosition::new(
            info.rcWork.left + margin_x,
            (info.rcWork.bottom - bottom_offset).max(info.rcWork.top),
        ))
    }
}

#[cfg(not(windows))]
pub fn error_toast_position(
    _premiere_hwnd: isize,
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
