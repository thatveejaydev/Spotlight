use crate::{actions::Action, settings::AppSettings};
use global_hotkey::hotkey::Code;
#[cfg(windows)]
use global_hotkey::hotkey::{HotKey, Modifiers};

#[cfg(any(windows, test))]
#[derive(Clone, Copy)]
enum KeyPress {
    Up,
    Passed,
    Consumed,
}

#[cfg(any(windows, test))]
impl KeyPress {
    fn handle(&mut self, injected: bool, up: bool, dispatch: impl FnOnce() -> bool) -> bool {
        if injected {
            return false;
        }
        if up {
            return matches!(std::mem::replace(self, Self::Up), Self::Consumed);
        }
        if matches!(self, Self::Up) {
            *self = if dispatch() {
                Self::Consumed
            } else {
                Self::Passed
            };
        }
        matches!(self, Self::Consumed)
    }
}

pub(crate) fn virtual_key(code: Code) -> Option<u16> {
    let name = code.to_string();
    let letter_or_digit = name
        .strip_prefix("Key")
        .or_else(|| name.strip_prefix("Digit"));
    if let Some([key]) = letter_or_digit.map(str::as_bytes) {
        return Some(u16::from(*key));
    }
    if let Some(number) = name
        .strip_prefix('F')
        .and_then(|key| key.parse::<u16>().ok())
    {
        return Some(0x6f + number); // VK_F1 through VK_F24
    }
    // Windows virtual key codes for the remaining keys supported by the recorder.
    Some(match code {
        Code::Backspace => 0x08,
        Code::Tab => 0x09,
        Code::Enter => 0x0d,
        Code::Pause => 0x13,
        Code::Space => 0x20,
        Code::PageUp => 0x21,
        Code::PageDown => 0x22,
        Code::End => 0x23,
        Code::Home => 0x24,
        Code::ArrowLeft => 0x25,
        Code::ArrowUp => 0x26,
        Code::ArrowRight => 0x27,
        Code::ArrowDown => 0x28,
        Code::Insert => 0x2d,
        Code::Delete => 0x2e,
        Code::ScrollLock => 0x91,
        Code::Semicolon => 0xba,
        Code::Equal => 0xbb,
        Code::Comma => 0xbc,
        Code::Minus => 0xbd,
        Code::Period => 0xbe,
        Code::Slash => 0xbf,
        Code::Backquote => 0xc0,
        Code::BracketLeft => 0xdb,
        Code::Backslash => 0xdc,
        Code::BracketRight => 0xdd,
        Code::Quote => 0xde,
        _ => return None,
    })
}

#[cfg(windows)]
#[derive(Clone, Copy)]
pub(crate) struct Binding {
    pub action: Action,
    hotkey: HotKey,
    key: u16,
}

#[cfg(windows)]
impl Binding {
    pub fn is_current(self, settings: &AppSettings) -> bool {
        settings
            .action_hotkeys
            .get(self.action.id())
            .is_some_and(|spec| {
                crate::settings::parse_palette_hotkey_spec(spec)
                    .is_ok_and(|(hotkey, _)| hotkey == self.hotkey)
            })
    }
}

#[cfg(not(windows))]
#[derive(Clone, Copy)]
pub(crate) struct Binding {
    pub action: Action,
}

#[cfg(not(windows))]
impl Binding {
    pub fn is_current(self, _settings: &AppSettings) -> bool {
        false
    }
}

#[cfg(windows)]
pub(crate) use windows::{foreground_changed, release_active_hold, update_bindings, ActionHotkeyHook};

#[cfg(not(windows))]
pub(crate) fn update_bindings(_settings: &AppSettings) {}

#[cfg(not(windows))]
pub(crate) fn foreground_changed(_hwnd: isize) {}

#[cfg(not(windows))]
pub(crate) fn release_active_hold() {}

#[cfg(not(windows))]
pub(crate) struct ActionHotkeyHook;

#[cfg(not(windows))]
impl ActionHotkeyHook {
    pub fn install(
        _dispatch: impl Fn(Binding, isize) -> bool + Send + 'static,
    ) -> Result<Self, String> {
        Err("Action hotkeys outside Spotlight require Windows.".into())
    }
}

#[cfg(any(windows, test))]
fn hold_releases_on_key_up(
    hold_key: u16,
    hold_mods: global_hotkey::hotkey::Modifiers,
    released_vk: u16,
) -> bool {
    use global_hotkey::hotkey::Modifiers;
    if released_vk == hold_key {
        return true;
    }
    if hold_mods.contains(Modifiers::CONTROL)
        && matches!(released_vk, 0x11 | 0xA2 | 0xA3)
    {
        return true;
    }
    if hold_mods.contains(Modifiers::ALT)
        && matches!(released_vk, 0x12 | 0xA4 | 0xA5)
    {
        return true;
    }
    if hold_mods.contains(Modifiers::SHIFT)
        && matches!(released_vk, 0x10 | 0xA0 | 0xA1)
    {
        return true;
    }
    hold_mods.intersects(Modifiers::SUPER | Modifiers::META) && matches!(released_vk, 0x5B | 0x5C)
}

#[cfg(windows)]
mod windows {
    use super::{
        hold_releases_on_key_up, virtual_key, Action, AppSettings, Binding, KeyPress, Modifiers,
    };
    use crate::{
        cursor_lock,
        debug_log::{self, LogKind},
        settings, win_focus,
    };
    use std::{
        cell::{Cell, RefCell},
        ptr::null_mut,
        sync::{mpsc, Mutex, RwLock},
        thread::{self, JoinHandle},
    };
    use windows_sys::Win32::{
        Foundation::{LPARAM, LRESULT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
            },
            WindowsAndMessaging::{
                CallNextHookEx, GetMessageW, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
                UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_UP,
                LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL,
                WM_LBUTTONUP, WM_MOUSEMOVE, WM_QUIT,
            },
        },
    };

    static BINDINGS: RwLock<Vec<Binding>> = RwLock::new(Vec::new());
    static ACTIVE_HOLD: Mutex<Option<Binding>> = Mutex::new(None);
    type Dispatch = Box<dyn Fn(Binding, isize) -> bool + Send>;

    thread_local! {
        static DISPATCH: RefCell<Option<Dispatch>> = const { RefCell::new(None) };
        static KEYS: [Cell<KeyPress>; 256] = const { [const { Cell::new(KeyPress::Up) }; 256] };
    }

    pub(crate) fn release_active_hold() {
        let mut guard = match ACTIVE_HOLD.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.take().is_some() {
            cursor_lock::release();
        }
    }

    pub(crate) fn foreground_changed(hwnd: isize) {
        if hwnd != 0 && !win_focus::is_premiere_action_window(hwnd) {
            release_active_hold();
        }
    }

    pub(crate) fn update_bindings(settings: &AppSettings) {
        release_active_hold();
        let bindings = Action::ALL.into_iter().filter_map(|action| {
            let spec = settings.action_hotkeys.get(action.id())?;
            let (hotkey, _) = settings::parse_palette_hotkey_spec(spec).ok()?;
            Some(Binding {
                action,
                hotkey,
                key: virtual_key(hotkey.key)?,
            })
        });
        if let Ok(mut active) = BINDINGS.write() {
            *active = bindings.collect();
            debug_log::log_event(
                LogKind::Info,
                "ACTION_HOTKEY",
                format!("Loaded {} action hotkey binding(s)", active.len()),
            );
        }
    }

    pub(crate) struct ActionHotkeyHook {
        thread_id: u32,
        worker: Option<JoinHandle<()>>,
    }

    impl ActionHotkeyHook {
        pub fn install(
            dispatch: impl Fn(Binding, isize) -> bool + Send + 'static,
        ) -> Result<Self, String> {
            let (ready, started) = mpsc::sync_channel(1);
            let worker = thread::Builder::new()
                .name("spotlight-action-hotkeys".into())
                .spawn(move || unsafe {
                    DISPATCH.with(|slot| *slot.borrow_mut() = Some(Box::new(dispatch)));
                    let mut message: MSG = std::mem::zeroed();
                    PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
                    let hook = SetWindowsHookExW(
                        WH_KEYBOARD_LL,
                        Some(keyboard_event),
                        GetModuleHandleW(std::ptr::null()),
                        0,
                    );
                    if hook.is_null() {
                        let _ = ready.send(Err(std::io::Error::last_os_error()));
                        return;
                    }
                    let mouse_hook = SetWindowsHookExW(
                        WH_MOUSE_LL,
                        Some(mouse_event),
                        GetModuleHandleW(std::ptr::null()),
                        0,
                    );
                    if mouse_hook.is_null() {
                        let err = std::io::Error::last_os_error();
                        UnhookWindowsHookEx(hook);
                        let _ = ready.send(Err(err));
                        return;
                    }
                    if ready.send(Ok(GetCurrentThreadId())).is_ok() {
                        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {}
                    }
                    UnhookWindowsHookEx(mouse_hook);
                    UnhookWindowsHookEx(hook);
                })
                .map_err(|err| format!("Could not start action hotkeys: {err}"))?;
            let thread_id = started
                .recv()
                .map_err(|err| format!("Could not start action hotkeys: {err}"))?
                .map_err(|err| format!("Could not install action hotkeys: {err}"))?;
            debug_log::log_event(
                LogKind::Info,
                "ACTION_HOTKEY",
                "Installed keyboard & mouse action hooks",
            );
            Ok(Self {
                thread_id,
                worker: Some(worker),
            })
        }
    }

    impl Drop for ActionHotkeyHook {
        fn drop(&mut self) {
            release_active_hold();
            if unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) } != 0 {
                if let Some(worker) = self.worker.take() {
                    let _ = worker.join();
                }
            }
        }
    }

    fn held_modifiers() -> Modifiers {
        // The event's primary key has not updated yet. Only sample modifier keys.
        let pressed = |key| unsafe { GetAsyncKeyState(i32::from(key)) < 0 };
        let mut mods = Modifiers::empty();
        mods.set(Modifiers::CONTROL, pressed(VK_CONTROL));
        mods.set(Modifiers::ALT, pressed(VK_MENU));
        mods.set(Modifiers::SHIFT, pressed(VK_SHIFT));
        mods.set(Modifiers::SUPER, pressed(VK_LWIN) || pressed(VK_RWIN));
        mods
    }

    unsafe extern "system" fn keyboard_event(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let event = &*(lparam as *const KBDLLHOOKSTRUCT);
            if event.vkCode < 256 {
                let key = event.vkCode as u16;
                let injected = event.flags & LLKHF_INJECTED != 0;
                let up = event.flags & LLKHF_UP != 0;
                if up && !injected {
                    let should_release = ACTIVE_HOLD
                        .try_lock()
                        .ok()
                        .and_then(|guard| *guard)
                        .is_some_and(|hold| hold_releases_on_key_up(hold.key, hold.hotkey.mods, key));
                    if should_release {
                        release_active_hold();
                    }
                }
                if KEYS.with(|keys| {
                    let state = &keys[usize::from(key)];
                    let mut press = state.get();
                    let handled = press.handle(injected, up, || dispatch_key(key));
                    state.set(press);
                    handled
                }) {
                    return 1;
                }
            }
        }
        CallNextHookEx(null_mut(), code, wparam, lparam)
    }

    unsafe extern "system" fn mouse_event(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let event = &*(lparam as *const MSLLHOOKSTRUCT);
            let injected = event.flags & LLMHF_INJECTED != 0;
            if wparam == WM_LBUTTONUP as WPARAM {
                cursor_lock::handle_left_button_up(injected);
            } else if wparam == WM_MOUSEMOVE as WPARAM
                && cursor_lock::handle_mouse_move(event.pt.x, event.pt.y, injected)
            {
                return 1;
            }
        }
        CallNextHookEx(null_mut(), code, wparam, lparam)
    }

    fn dispatch_key(key: u16) -> bool {
        let binding = BINDINGS.try_read().ok().and_then(|bindings| {
            if !bindings.iter().any(|binding| binding.key == key) {
                return None;
            }
            let mods = held_modifiers();
            bindings
                .iter()
                .copied()
                .find(|binding| binding.key == key && binding.hotkey.mods == mods)
        });
        let Some(binding) = binding else { return false };
        let foreground = win_focus::get_foreground_hwnd();
        if !win_focus::is_premiere_action_window(foreground) {
            debug_log::log_event(
                LogKind::Focus,
                "ACTION_HOTKEY",
                format!(
                    "Ignored {} (fg_hwnd={foreground:#x}, premiere_active={})",
                    binding.action.name(),
                    win_focus::is_premiere_active_and_unminimized(foreground)
                ),
            );
            return false;
        }
        if binding.action.is_hold() {
            let mut guard = match ACTIVE_HOLD.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if cursor_lock::engage(binding.key, binding.hotkey.mods) {
                *guard = Some(binding);
                return true;
            }
            debug_log::log_event(
                LogKind::Error,
                "ACTION_HOTKEY",
                format!("Failed to engage cursor lock for {}", binding.action.name()),
            );
            return false;
        }
        win_focus::allow_current_process_foreground();
        DISPATCH.with(|slot| {
            slot.borrow()
                .as_ref()
                .is_some_and(|dispatch| dispatch(binding, foreground))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatches_once_and_passes_injected_keys_through() {
        let mut press = KeyPress::Up;
        assert!(!press.handle(true, false, || panic!("injected Shortcut")));
        assert!(press.handle(false, false, || true));
        assert!(!press.handle(true, true, || panic!("injected release")));
        assert!(press.handle(false, false, || panic!("repeat")));
        assert!(press.handle(false, true, || panic!("release")));
        assert!(!press.handle(false, false, || false)); // A press outside Premiere.
        assert!(!press.handle(false, false, || panic!("repeat after a focus change")));
        assert!(!press.handle(false, true, || panic!("unconsumed release")));
        assert!(press.handle(false, false, || true));
    }

    #[test]
    fn hold_releases_on_primary_or_required_modifier_key_up() {
        use global_hotkey::hotkey::Modifiers;

        let f8 = virtual_key(Code::F8).unwrap();
        assert!(hold_releases_on_key_up(f8, Modifiers::empty(), f8));
        assert!(!hold_releases_on_key_up(f8, Modifiers::empty(), 0xA0)); // unrelated Shift up

        let key_h = virtual_key(Code::KeyH).unwrap();
        let mods = Modifiers::ALT | Modifiers::SHIFT;
        assert!(hold_releases_on_key_up(key_h, mods, key_h));
        assert!(hold_releases_on_key_up(key_h, mods, 0xA4)); // VK_LMENU
        assert!(hold_releases_on_key_up(key_h, mods, 0xA1)); // VK_RSHIFT
        assert!(!hold_releases_on_key_up(key_h, mods, 0xA2)); // unrelated Ctrl up
    }
}
