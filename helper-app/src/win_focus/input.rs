//! Synthetic key input sent to Premiere: panel shortcuts, markers and preset search.

use super::*;

#[cfg(windows)]
pub(super) fn is_vk_down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) < 0 }
}

#[cfg(windows)]
pub(super) fn keyboard_input(vk: u16, up: bool) -> INPUT {
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
pub fn open_effect_controls(premiere_hwnd: isize, shortcut: crate::premiere_prefs::ShortcutSpec) -> bool {
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
pub(crate) fn send_marker_shortcut(process_id: u32, shortcut: crate::premiere_prefs::ShortcutSpec) -> Option<isize> {
    let foreground = get_foreground_hwnd();
    (can_send_preset_input(process_id)
        && preset_handoff_keys_released()
        && get_foreground_hwnd() == foreground
        && send_inputs(&shortcut_inputs(shortcut)))
        .then_some(foreground)
}

#[cfg(not(windows))]
pub(crate) fn send_marker_shortcut(_process_id: u32, _shortcut: crate::premiere_prefs::ShortcutSpec) -> Option<isize> {
    None
}

#[cfg(windows)]
pub(crate) fn foreground_event_clock() -> u32 {
    unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount() }
}

#[cfg(not(windows))]
pub(crate) fn foreground_event_clock() -> u32 {
    0
}

#[cfg(windows)]
pub(crate) fn has_owned_modal(target: isize) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
    let foreground = get_foreground_hwnd();
    foreground != 0 && foreground != target && unsafe {
        let owner = GetAncestor(target as _, GA_ROOTOWNER);
        !owner.is_null()
            && GetAncestor(foreground as _, GA_ROOTOWNER) == owner
            && (IsWindowEnabled(target as _) == 0 || IsWindowEnabled(owner) == 0)
    }
}

#[cfg(not(windows))]
pub(crate) fn has_owned_modal(_target: isize) -> bool {
    false
}

#[cfg(windows)]
pub(super) fn shortcut_inputs(shortcut: crate::premiere_prefs::ShortcutSpec) -> Vec<INPUT> {
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
pub(super) fn send_inputs(inputs: &[INPUT]) -> bool {
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
pub(super) fn can_send_preset_input(process_id: u32) -> bool {
    let foreground = get_foreground_hwnd();
    window_process_id(foreground) == Some(process_id)
        && accepts_premiere_keys(foreground)
        && get_foreground_hwnd() == foreground
}

#[cfg(windows)]
pub(super) fn accepts_premiere_keys(hwnd: isize) -> bool {
    unsafe {
        let mut info: GUITHREADINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        let thread = GetWindowThreadProcessId(hwnd as _, std::ptr::null_mut());
        let mut class = [0u16; 32];
        let class_len = GetClassNameW(hwnd as _, class.as_mut_ptr(), class.len() as i32);
        thread != 0
            && GetGUIThreadInfo(thread, &mut info) != 0
            && info.flags & (GUI_INMENUMODE | GUI_INMOVESIZE | GUI_POPUPMENUMODE | GUI_SYSTEMMENUMODE)
                == 0
            && class[..class_len.max(0) as usize].iter().copied().ne("#32770".encode_utf16())
    }
}

#[cfg(windows)]
pub(crate) fn is_premiere_action_window(hwnd: isize) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
    is_premiere_active_and_unminimized(hwnd) && accepts_premiere_keys(hwnd) && unsafe {
        let owner = GetAncestor(hwnd as _, GA_ROOTOWNER);
        IsWindowEnabled(hwnd as _) != 0 && !owner.is_null() && IsWindowEnabled(owner) != 0
    }
}

#[cfg(not(windows))]
pub(crate) fn is_premiere_action_window(_hwnd: isize) -> bool {
    false
}

#[cfg(windows)]
thread_local! {
    static PRESET_MODIFIERS: std::cell::Cell<Option<crate::premiere_prefs::ShortcutSpec>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(not(windows))]
pub(crate) fn begin_preset_shortcut(_process_id: u32, _shortcut: crate::premiere_prefs::ShortcutSpec) -> bool {
    false
}

#[cfg(windows)]
pub(super) fn preset_modifiers(shortcut: crate::premiere_prefs::ShortcutSpec) -> impl DoubleEndedIterator<Item = u16> {
    [(VK_LCONTROL, shortcut.ctrl), (VK_LMENU, shortcut.alt), (VK_LSHIFT, shortcut.shift)]
        .into_iter()
        .filter_map(|(key, enabled)| enabled.then_some(key))
}

#[cfg(windows)]
pub(crate) fn begin_preset_shortcut(process_id: u32, shortcut: crate::premiere_prefs::ShortcutSpec) -> bool {
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
pub(crate) fn press_preset_shortcut(process_id: u32, shortcut: crate::premiere_prefs::ShortcutSpec) -> bool {
    can_send_preset_input(process_id)
        && is_vk_down(VK_CONTROL) == shortcut.ctrl
        && is_vk_down(VK_MENU) == shortcut.alt
        && is_vk_down(VK_SHIFT) == shortcut.shift
        && !is_vk_down(VK_LWIN)
        && !is_vk_down(VK_RWIN)
        && send_inputs(&[keyboard_input(shortcut.vk, false), keyboard_input(shortcut.vk, true)])
}

#[cfg(not(windows))]
pub(crate) fn press_preset_shortcut(_process_id: u32, _shortcut: crate::premiere_prefs::ShortcutSpec) -> bool {
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
pub fn open_effect_controls(_premiere_hwnd: isize, _shortcut: crate::premiere_prefs::ShortcutSpec) -> bool {
    false
}
