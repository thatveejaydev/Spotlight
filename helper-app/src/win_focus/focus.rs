//! Moving the foreground between the palette and Premiere.

use super::*;

#[cfg(windows)]
pub fn allow_current_process_foreground() {
    unsafe {
        AllowSetForegroundWindow(GetCurrentProcessId());
    }
}

#[cfg(not(windows))]
pub fn allow_current_process_foreground() {}

#[cfg(windows)]
pub(super) unsafe fn activate_window_without_synthetic_keys(target: HWND) {
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
