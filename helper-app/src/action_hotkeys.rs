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
pub(crate) use windows::{update_bindings, ActionHotkeyHook};

#[cfg(not(windows))]
pub(crate) fn update_bindings(_settings: &AppSettings) {}

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

#[cfg(windows)]
mod windows {
    use super::{virtual_key, Action, AppSettings, Binding, KeyPress, Modifiers};
    use crate::{settings, win_focus};
    use std::{
        cell::{Cell, RefCell},
        ptr::null_mut,
        sync::{mpsc, RwLock},
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
                UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_UP, MSG,
                PM_NOREMOVE, WH_KEYBOARD_LL, WM_QUIT,
            },
        },
    };

    static BINDINGS: RwLock<Vec<Binding>> = RwLock::new(Vec::new());
    type Dispatch = Box<dyn Fn(Binding, isize) -> bool + Send>;

    thread_local! {
        static DISPATCH: RefCell<Option<Dispatch>> = const { RefCell::new(None) };
        static KEYS: [Cell<KeyPress>; 256] = const { [const { Cell::new(KeyPress::Up) }; 256] };
    }

    pub(crate) fn update_bindings(settings: &AppSettings) {
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
                    if ready.send(Ok(GetCurrentThreadId())).is_ok() {
                        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {}
                    }
                    UnhookWindowsHookEx(hook);
                })
                .map_err(|err| format!("Could not start action hotkeys: {err}"))?;
            let thread_id = started
                .recv()
                .map_err(|err| format!("Could not start action hotkeys: {err}"))?
                .map_err(|err| format!("Could not install action hotkeys: {err}"))?;
            Ok(Self {
                thread_id,
                worker: Some(worker),
            })
        }
    }

    impl Drop for ActionHotkeyHook {
        fn drop(&mut self) {
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
                if KEYS.with(|keys| {
                    let state = &keys[usize::from(key)];
                    let mut press = state.get();
                    let handled = press.handle(
                        event.flags & LLKHF_INJECTED != 0,
                        event.flags & LLKHF_UP != 0,
                        || dispatch_key(key),
                    );
                    state.set(press);
                    handled
                }) {
                    return 1;
                }
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
}
