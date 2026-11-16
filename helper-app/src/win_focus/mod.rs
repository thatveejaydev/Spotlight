//! Windows integration: Premiere's processes and windows, foreground control,
//! synthetic input, the palette window's style and placement, and the WinEvent
//! hook. Every submodule's public functions are re-exported here, so callers use
//! `win_focus::name`. Non-Windows builds get inert stubs for tests.

mod focus;
mod foreground_hook;
mod input;
mod palette_window;
mod placement;
mod process;

pub(crate) use focus::*;
pub(crate) use foreground_hook::*;
pub(crate) use input::*;
pub(crate) use palette_window::*;
pub(crate) use placement::*;
pub(crate) use process::*;

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, BOOL, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR,
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    },
    Graphics::Gdi::{
        CreateRoundRectRgn, DeleteObject, GetMonitorInfoW, MonitorFromWindow, SetWindowRgn,
        MONITORINFO, MONITOR_DEFAULTTONEAREST,
    },
    System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    },
    System::Threading::{
        AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId, OpenProcess,
        QueryFullProcessImageNameW, WaitForMultipleObjects, INFINITE,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    },
    UI::{
        Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
        HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI},
        Input::KeyboardAndMouse::{
            keybd_event, GetAsyncKeyState, MapVirtualKeyW, SendInput, INPUT, INPUT_0,
            INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
            KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT,
            VK_LWIN, VK_MENU, VK_RCONTROL, VK_RETURN, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
        },
        WindowsAndMessaging::{
            AllowSetForegroundWindow, BringWindowToTop, CallWindowProcW, DefWindowProcW,
            EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo,
            GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic,
            IsWindowVisible, IsZoomed, GUI_INMENUMODE, GUI_INMOVESIZE, GUI_POPUPMENUMODE,
            GUI_SYSTEMMENUMODE, GUITHREADINFO,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_hwnd_ignores_zero_and_stores_nonzero() {
        PALETTE_HWND.store(0, std::sync::atomic::Ordering::Release);
        set_palette_hwnd(0);
        assert_eq!(palette_hwnd(), 0);
        set_palette_hwnd(0x1234);
        assert_eq!(palette_hwnd(), 0x1234);
        set_palette_hwnd(0);
        assert_eq!(palette_hwnd(), 0x1234);
        PALETTE_HWND.store(0, std::sync::atomic::Ordering::Release);
    }

    #[cfg(windows)]
    #[test]
    fn find_premiere_pids_excludes_spotlight_process() {
        let self_pid = unsafe { GetCurrentProcessId() };
        assert!(!find_premiere_pids().contains(&self_pid));
    }
}
