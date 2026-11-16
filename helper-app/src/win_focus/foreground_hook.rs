//! The WinEvent hook that reports foreground changes and Premiere minimizing.

use super::*;

/// Receives `(hwnd, event_time)` for each foreground change.
pub(super) type ForegroundListener = fn(isize, u32);

#[cfg(windows)]
thread_local! {
    static HOOK_CALLBACK: std::cell::RefCell<Option<std::rc::Rc<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
    static FOREGROUND_LISTENERS: std::cell::RefCell<Vec<ForegroundListener>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static PREMIERE_MINIMIZE_LISTENERS: std::cell::RefCell<Vec<fn()>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Calls `listener(hwnd, event_time)` on the UI thread whenever another window
/// takes the foreground, while a [`ForegroundHook`] is installed.
#[cfg(windows)]
pub fn on_foreground_change(listener: ForegroundListener) {
    FOREGROUND_LISTENERS.with(|slot| slot.borrow_mut().push(listener));
}

#[cfg(not(windows))]
pub fn on_foreground_change(_listener: ForegroundListener) {}

/// Calls `listener` on the UI thread whenever a Premiere window starts minimizing,
/// while a [`ForegroundHook`] is installed.
#[cfg(windows)]
pub fn on_premiere_minimize(listener: fn()) {
    PREMIERE_MINIMIZE_LISTENERS.with(|slot| slot.borrow_mut().push(listener));
}

#[cfg(not(windows))]
pub fn on_premiere_minimize(_listener: fn()) {}

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
pub(super) unsafe extern "system" fn winevent_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    event_time: u32,
) {
    if event == EVENT_SYSTEM_FOREGROUND {
        // Copy the list so a listener may register another without a borrow conflict.
        let listeners = FOREGROUND_LISTENERS.with(|slot| slot.borrow().clone());
        for listener in listeners {
            listener(hwnd as isize, event_time);
        }
    } else if event == EVENT_SYSTEM_MINIMIZESTART && is_premiere_hwnd(hwnd as isize) {
        let listeners = PREMIERE_MINIMIZE_LISTENERS.with(|slot| slot.borrow().clone());
        for listener in listeners {
            listener();
        }
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
