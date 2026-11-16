#[cfg(windows)]
pub(crate) use windows::{engage, handle_left_button_up, handle_mouse_move, release};

#[cfg(not(windows))]
pub(crate) fn engage(_key: u16, _mods: global_hotkey::hotkey::Modifiers) -> bool {
    false
}

#[cfg(not(windows))]
pub(crate) fn release() {}

#[cfg(not(windows))]
pub(crate) fn handle_mouse_move(_pt_x: i32, _pt_y: i32, _injected: bool) -> bool {
    false
}

#[cfg(not(windows))]
pub(crate) fn handle_left_button_up(_injected: bool) {}

#[cfg(any(windows, test))]
pub(crate) fn clamp_horizontal_move(
    locked_y: i32,
    pt_x: i32,
    pt_y: i32,
    injected: bool,
) -> Option<(i32, i32)> {
    if injected || pt_y == locked_y {
        None
    } else {
        Some((pt_x, locked_y))
    }
}

#[cfg(any(windows, test))]
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
struct SyntheticClickState {
    held: bool,
}

#[cfg(any(windows, test))]
impl SyntheticClickState {
    fn on_engage(&mut self, left_already_down: bool) -> bool {
        self.held = !left_already_down;
        self.held
    }

    fn on_physical_left_up(&mut self, injected: bool) {
        if !injected {
            self.held = false;
        }
    }

    fn take_release(&mut self) -> bool {
        std::mem::take(&mut self.held)
    }
}

#[cfg(windows)]
mod windows {
    use super::{clamp_horizontal_move, SyntheticClickState};
    use crate::{
        debug_log::{self, LogKind},
        win_focus,
    };
    use global_hotkey::hotkey::Modifiers;
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicI32, Ordering},
            Condvar, Mutex, OnceLock,
        },
        thread,
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::POINT,
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN,
                MOUSEEVENTF_LEFTUP, MOUSEINPUT, VK_CONTROL, VK_LBUTTON, VK_LWIN, VK_MENU,
                VK_RWIN, VK_SHIFT,
            },
            WindowsAndMessaging::{GetCursorPos, SetCursorPos},
        },
    };

    const WATCHDOG_INTERVAL: Duration = Duration::from_millis(25);

    static LOCKED: AtomicBool = AtomicBool::new(false);
    static LOCKED_Y: AtomicI32 = AtomicI32::new(0);
    static SYNTHETIC_CLICK: Mutex<SyntheticClickState> = Mutex::new(SyntheticClickState {
        held: false,
    });

    #[derive(Clone, Copy)]
    struct ActiveLock {
        mods: Modifiers,
    }

    struct LockControl {
        state: Mutex<Option<ActiveLock>>,
        wake: Condvar,
        watchdog_started: OnceLock<()>,
    }

    fn control() -> &'static LockControl {
        static CONTROL: OnceLock<LockControl> = OnceLock::new();
        CONTROL.get_or_init(|| LockControl {
            state: Mutex::new(None),
            wake: Condvar::new(),
            watchdog_started: OnceLock::new(),
        })
    }

    fn is_vk_down(vk: u16) -> bool {
        unsafe { GetAsyncKeyState(i32::from(vk)) < 0 }
    }

    fn send_left_button(down: bool) -> bool {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: if down {
                        MOUSEEVENTF_LEFTDOWN
                    } else {
                        MOUSEEVENTF_LEFTUP
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) == 1 }
    }

    fn hold_modifiers_still_pressed(mods: Modifiers) -> bool {
        // The primary key is swallowed by WH_KEYBOARD_LL so GetAsyncKeyState never sees it as down;
        // primary key release arrives via LLKHF_UP, while unswallowed modifiers can be polled here.
        if mods.contains(Modifiers::CONTROL) && !is_vk_down(VK_CONTROL) {
            return false;
        }
        if mods.contains(Modifiers::ALT) && !is_vk_down(VK_MENU) {
            return false;
        }
        if mods.contains(Modifiers::SHIFT) && !is_vk_down(VK_SHIFT) {
            return false;
        }
        if mods.intersects(Modifiers::SUPER | Modifiers::META)
            && !(is_vk_down(VK_LWIN) || is_vk_down(VK_RWIN))
        {
            return false;
        }
        true
    }

    fn ensure_watchdog_started(ctrl: &LockControl) {
        ctrl.watchdog_started.get_or_init(|| {
            let _ = thread::Builder::new()
                .name("spotlight-cursor-lock".into())
                .spawn(watchdog_loop);
        });
    }

    fn clear_lock(state: &mut Option<ActiveLock>) {
        if state.take().is_some() {
            let should_release_click = {
                let mut click = match SYNTHETIC_CLICK.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                click.take_release()
            };
            if should_release_click && !send_left_button(false) {
                debug_log::log_event(
                    LogKind::Error,
                    "CURSOR_LOCK",
                    format!("SendInput(LEFTUP) failed: {}", std::io::Error::last_os_error()),
                );
            }
            LOCKED.store(false, Ordering::Release);
            debug_log::log_event(LogKind::Info, "CURSOR_LOCK", "Released horizontal cursor lock");
        }
    }

    fn watchdog_loop() {
        let ctrl = control();
        let Ok(mut guard) = ctrl.state.lock() else {
            return;
        };
        loop {
            while guard.is_none() {
                guard = match ctrl.wake.wait(guard) {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
            }
            let (result_guard, _) = match ctrl.wake.wait_timeout(guard, WATCHDOG_INTERVAL) {
                Ok(pair) => pair,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard = result_guard;
            let Some(active) = *guard else {
                continue;
            };
            let mods_ok = hold_modifiers_still_pressed(active.mods);
            let fg_hwnd = win_focus::get_foreground_hwnd();
            let fg_ok = win_focus::is_premiere_action_window(fg_hwnd);
            if !mods_ok || !fg_ok {
                debug_log::log_event(
                    LogKind::Focus,
                    "CURSOR_LOCK",
                    format!(
                        "Watchdog releasing lock (mods_ok={mods_ok}, fg_ok={fg_ok}, fg_hwnd={fg_hwnd:#x})"
                    ),
                );
                clear_lock(&mut guard);
            }
        }
    }

    /// Clamps off-axis hardware mouse moves to the locked Y coordinate while active.
    pub(crate) fn handle_mouse_move(pt_x: i32, pt_y: i32, injected: bool) -> bool {
        if !LOCKED.load(Ordering::Acquire) {
            return false;
        }
        let locked_y = LOCKED_Y.load(Ordering::Acquire);
        if let Some((target_x, target_y)) = clamp_horizontal_move(locked_y, pt_x, pt_y, injected) {
            if unsafe { SetCursorPos(target_x, target_y) } == 0 {
                debug_log::log_event(
                    LogKind::Error,
                    "CURSOR_LOCK",
                    format!(
                        "SetCursorPos({target_x}, {target_y}) failed: {}",
                        std::io::Error::last_os_error()
                    ),
                );
            }
            return true;
        }
        false
    }

    /// Clears synthetic click state if the user manually releases the left mouse button.
    pub(crate) fn handle_left_button_up(injected: bool) {
        if !LOCKED.load(Ordering::Acquire) || injected {
            return;
        }
        if let Ok(mut click) = SYNTHETIC_CLICK.try_lock() {
            click.on_physical_left_up(injected);
        }
    }

    /// Locks the cursor to its current Y coordinate and starts a left-drag if hovering.
    pub(crate) fn engage(_key: u16, mods: Modifiers) -> bool {
        let mut pt = POINT { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut pt) } == 0 {
            return false;
        }

        let ctrl = control();
        ensure_watchdog_started(ctrl);
        let mut guard = match ctrl.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        LOCKED_Y.store(pt.y, Ordering::Release);
        LOCKED.store(true, Ordering::Release);
        *guard = Some(ActiveLock { mods });

        let left_already_down = is_vk_down(VK_LBUTTON);
        let should_click = {
            let mut click = match SYNTHETIC_CLICK.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            click.on_engage(left_already_down)
        };
        let mut clicked = false;
        if should_click {
            if send_left_button(true) {
                clicked = true;
            } else {
                let mut click = match SYNTHETIC_CLICK.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                click.take_release();
                debug_log::log_event(
                    LogKind::Error,
                    "CURSOR_LOCK",
                    format!("SendInput(LEFTDOWN) failed: {}", std::io::Error::last_os_error()),
                );
            }
        }

        ctrl.wake.notify_one();
        debug_log::log_event(
            LogKind::Info,
            "CURSOR_LOCK",
            format!("Locked cursor horizontally at Y={} (clicked={clicked})", pt.y),
        );
        true
    }

    /// Releases any active synthetic drag and horizontal cursor lock.
    pub(crate) fn release() {
        let ctrl = control();
        let mut guard = match ctrl.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        clear_lock(&mut guard);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_only_non_injected_off_axis_moves() {
        assert_eq!(clamp_horizontal_move(540, 1200, 548, false), Some((1200, 540)));
        assert_eq!(clamp_horizontal_move(-120, -400, -115, false), Some((-400, -120)));
        assert_eq!(clamp_horizontal_move(540, 1200, 540, false), None);
        assert_eq!(clamp_horizontal_move(540, 1200, 548, true), None);
    }

    #[test]
    fn synthetic_click_holds_only_when_hovering_and_clears_on_physical_up() {
        let mut click = SyntheticClickState::default();
        assert!(click.on_engage(false));
        click.on_physical_left_up(true);
        assert!(click.take_release());
        assert!(!click.take_release());

        assert!(click.on_engage(false));
        click.on_physical_left_up(false);
        assert!(!click.take_release());

        assert!(!click.on_engage(true));
        assert!(!click.take_release());
    }
}
