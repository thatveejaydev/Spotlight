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

#[cfg(windows)]
pub fn get_foreground_hwnd() -> isize {
    unsafe { GetForegroundWindow() as isize }
}

#[cfg(not(windows))]
pub fn get_foreground_hwnd() -> isize {
    0
}

#[cfg(windows)]
pub(crate) fn window_process_id(hwnd: isize) -> Option<u32> {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd as _, &mut pid) };
    (pid != 0).then_some(pid)
}

#[cfg(not(windows))]
pub(crate) fn window_process_id(_hwnd: isize) -> Option<u32> {
    None
}

#[cfg(windows)]
pub(crate) fn premiere_executable(hwnd: isize) -> Option<std::path::PathBuf> {
    if hwnd == 0 {
        return None;
    }
    unsafe {
        use std::os::windows::ffi::OsStringExt;

        let pid = window_process_id(hwnd)?;
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 || len == 0 {
            return None;
        }
        let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf[..len as usize]));
        path.to_string_lossy()
            .to_ascii_lowercase()
            .ends_with("adobe premiere pro.exe")
            .then_some(path)
    }
}

#[cfg(not(windows))]
pub(crate) fn premiere_executable(_hwnd: isize) -> Option<std::path::PathBuf> {
    None
}

#[cfg(windows)]
pub fn is_premiere_hwnd(hwnd: isize) -> bool {
    premiere_executable(hwnd).is_some()
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
fn is_vk_down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) < 0 }
}

#[cfg(windows)]
fn keyboard_input(vk: u16, up: bool) -> INPUT {
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) as u16 };
    let extended = matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0xA1 | 0xA3 | 0xA5);
    let mut flags: KEYBD_EVENT_FLAGS = 0;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Sends Premiere Pro's Effect Controls shortcut once Premiere is the active foreground window.
#[cfg(windows)]
pub fn open_effect_controls(premiere_hwnd: isize, shortcut: crate::bridge::ShortcutSpec) -> bool {
    let mut fg = get_foreground_hwnd();
    if !is_premiere_active_and_unminimized(fg) {
        if premiere_hwnd != 0 && is_premiere_active_and_unminimized(premiere_hwnd) {
            restore_foreground(premiere_hwnd);
            fg = get_foreground_hwnd();
        }
        if !is_premiere_active_and_unminimized(fg) {
            return false;
        }
    }

    send_inputs(&shortcut_inputs(shortcut))
}

#[cfg(windows)]
fn shortcut_inputs(shortcut: crate::bridge::ShortcutSpec) -> Vec<INPUT> {
    let mut inputs = Vec::with_capacity(10);

    // Clear held modifier keys that conflict with the target shortcut (e.g. Shift from Shift+Enter).
    if !shortcut.shift {
        for vk in [VK_LSHIFT, VK_RSHIFT] {
            if is_vk_down(vk) {
                inputs.push(keyboard_input(vk, true));
            }
        }
    }
    if !shortcut.ctrl {
        for vk in [VK_LCONTROL, VK_RCONTROL] {
            if is_vk_down(vk) {
                inputs.push(keyboard_input(vk, true));
            }
        }
    }
    if !shortcut.alt {
        for vk in [VK_LMENU, VK_RMENU] {
            if is_vk_down(vk) {
                inputs.push(keyboard_input(vk, true));
            }
        }
    }

    let press_ctrl = shortcut.ctrl && !is_vk_down(VK_LCONTROL) && !is_vk_down(VK_RCONTROL);
    let press_alt = shortcut.alt && !is_vk_down(VK_LMENU) && !is_vk_down(VK_RMENU);
    let press_shift = shortcut.shift && !is_vk_down(VK_LSHIFT) && !is_vk_down(VK_RSHIFT);

    if press_ctrl {
        inputs.push(keyboard_input(VK_CONTROL, false));
    }
    if press_alt {
        inputs.push(keyboard_input(VK_MENU, false));
    }
    if press_shift {
        inputs.push(keyboard_input(VK_SHIFT, false));
    }

    inputs.push(keyboard_input(shortcut.vk, false));
    inputs.push(keyboard_input(shortcut.vk, true));

    if press_shift {
        inputs.push(keyboard_input(VK_SHIFT, true));
    }
    if press_alt {
        inputs.push(keyboard_input(VK_MENU, true));
    }
    if press_ctrl {
        inputs.push(keyboard_input(VK_CONTROL, true));
    }

    inputs
}

#[cfg(windows)]
fn send_inputs(inputs: &[INPUT]) -> bool {
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    sent == inputs.len() as u32
}

#[cfg(windows)]
pub(crate) fn preset_handoff_keys_released() -> bool {
    [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN, VK_RETURN]
        .into_iter()
        .all(|vk| !is_vk_down(vk))
}

#[cfg(not(windows))]
pub(crate) fn preset_handoff_keys_released() -> bool {
    false
}

#[cfg(windows)]
fn can_send_preset_input(process_id: u32) -> bool {
    let foreground = get_foreground_hwnd();
    if window_process_id(foreground) != Some(process_id) {
        return false;
    }
    unsafe {
        let mut info: GUITHREADINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        let thread = GetWindowThreadProcessId(foreground as _, std::ptr::null_mut());
        let mut class = [0u16; 32];
        let class_len = GetClassNameW(foreground as _, class.as_mut_ptr(), class.len() as i32);
        thread != 0
            && GetGUIThreadInfo(thread, &mut info) != 0
            && info.flags & (GUI_INMENUMODE | GUI_INMOVESIZE | GUI_POPUPMENUMODE | GUI_SYSTEMMENUMODE)
                == 0
            && String::from_utf16_lossy(&class[..class_len.max(0) as usize]) != "#32770"
            && get_foreground_hwnd() == foreground
    }
}

#[cfg(windows)]
thread_local! {
    static PRESET_MODIFIERS: std::cell::Cell<Option<crate::bridge::ShortcutSpec>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(not(windows))]
pub(crate) fn begin_preset_shortcut(_process_id: u32, _shortcut: crate::bridge::ShortcutSpec) -> bool {
    false
}

#[cfg(windows)]
fn preset_modifiers(shortcut: crate::bridge::ShortcutSpec) -> impl DoubleEndedIterator<Item = u16> {
    [(VK_LCONTROL, shortcut.ctrl), (VK_LMENU, shortcut.alt), (VK_LSHIFT, shortcut.shift)]
        .into_iter()
        .filter_map(|(key, enabled)| enabled.then_some(key))
}

#[cfg(windows)]
pub(crate) fn begin_preset_shortcut(process_id: u32, shortcut: crate::bridge::ShortcutSpec) -> bool {
    if !can_send_preset_input(process_id) || !preset_handoff_keys_released() {
        return false;
    }
    PRESET_MODIFIERS.set(Some(shortcut));
    let inputs: Vec<_> = preset_modifiers(shortcut)
        .map(|key| keyboard_input(key, false))
        .collect();
    inputs.is_empty() || send_inputs(&inputs)
}

#[cfg(windows)]
pub(crate) fn press_preset_shortcut(process_id: u32, shortcut: crate::bridge::ShortcutSpec) -> bool {
    can_send_preset_input(process_id)
        && is_vk_down(VK_CONTROL) == shortcut.ctrl
        && is_vk_down(VK_MENU) == shortcut.alt
        && is_vk_down(VK_SHIFT) == shortcut.shift
        && !is_vk_down(VK_LWIN)
        && !is_vk_down(VK_RWIN)
        && send_inputs(&[keyboard_input(shortcut.vk, false), keyboard_input(shortcut.vk, true)])
}

#[cfg(not(windows))]
pub(crate) fn press_preset_shortcut(_process_id: u32, _shortcut: crate::bridge::ShortcutSpec) -> bool {
    false
}

#[cfg(windows)]
pub(crate) fn release_preset_modifiers() {
    if let Some(shortcut) = PRESET_MODIFIERS.take() {
        let inputs: Vec<_> = preset_modifiers(shortcut)
            .rev()
            .map(|key| keyboard_input(key, true))
            .collect();
        if !inputs.is_empty() {
            send_inputs(&inputs);
        }
    }
}

#[cfg(not(windows))]
pub(crate) fn release_preset_modifiers() {}

#[cfg(windows)]
pub(crate) fn type_effects_search(process_id: u32, name: &str) -> bool {
    let mut inputs = Vec::with_capacity(name.len() * 2);
    // Unicode input preserves the preset name independently of the keyboard layout and clipboard.
    for unit in name.encode_utf16() {
        for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0,
                        wScan: unit,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        }
    }
    can_send_preset_input(process_id) && preset_handoff_keys_released() && send_inputs(&inputs)
}

#[cfg(not(windows))]
pub(crate) fn type_effects_search(_process_id: u32, _name: &str) -> bool {
    false
}

#[cfg(not(windows))]
pub fn open_effect_controls(_premiere_hwnd: isize, _shortcut: crate::bridge::ShortcutSpec) -> bool {
    false
}

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

static PALETTE_VISIBLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn set_palette_visible(visible: bool) {
    PALETTE_VISIBLE.store(visible, std::sync::atomic::Ordering::Release);
}

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

    pub fn is_active(&self) -> bool {
        !self.hooks.is_empty()
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

    pub fn is_active(&self) -> bool {
        false
    }
}

#[cfg(windows)]
unsafe extern "system" fn winevent_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _event_time: u32,
) {
    if event == EVENT_SYSTEM_FOREGROUND {
        crate::presets::foreground_changed(hwnd as isize);
    }
    if !PALETTE_VISIBLE.load(std::sync::atomic::Ordering::Acquire) {
        return;
    }
    crate::debug_log::record_fg_hook_event();
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
fn monitor_work_area_and_dpi(premiere_hwnd: isize) -> Option<(RECT, u32, u32)> {
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
