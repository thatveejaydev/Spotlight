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
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    },
    UI::{
        HiDpi::GetDpiForWindow,
        Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP, VK_MENU},
        WindowsAndMessaging::{
            BringWindowToTop, CallWindowProcW, DefWindowProcW, EnumWindows, GetForegroundWindow, GetWindowRect,
            GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
            SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, GWLP_WNDPROC, SWP_FRAMECHANGED,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WM_NCACTIVATE, WM_NCCALCSIZE,
            WM_NCDESTROY, WM_NCPAINT, WNDPROC,
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
    }
    is_premiere_hwnd(hwnd)
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
pub fn force_foreground(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        // Synthesize a harmless Alt key tap to satisfy Windows ForegroundLockTimeout rules
        keybd_event(VK_MENU as u8, 0, 0, 0);
        keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
        BringWindowToTop(hwnd as _);
        SetForegroundWindow(hwnd as _);
    }
}

#[cfg(not(windows))]
pub fn force_foreground(_hwnd: isize) {}

#[cfg(windows)]
thread_local! {
    static PALETTE_FRAME: std::cell::Cell<(HWND, WNDPROC, bool)> =
        const { std::cell::Cell::new((std::ptr::null_mut(), None, false)) };
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
        // Use the entire window for Slint, including during focus and DPI changes.
        WM_NCCALCSIZE | WM_NCPAINT => 0,
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
