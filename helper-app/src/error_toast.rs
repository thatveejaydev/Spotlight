use crate::{win_focus, ErrorToastWindow};
use slint::{ComponentHandle, SharedString, Timer, TimerMode};
use std::{cell::RefCell, rc::Rc, time::Duration};

#[cfg(windows)]
thread_local! {
    pub(crate) static CREATING_TOAST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Default)]
pub(crate) struct ErrorToast {
    window: RefCell<Option<ErrorToastWindow>>,
    dismiss: Timer,
}

impl ErrorToast {
    pub(crate) fn show(self: &Rc<Self>, message: SharedString, premiere_hwnd: isize) {
        let mut slot = self.window.borrow_mut();
        if let Some(window) = slot.as_ref() {
            window.set_message(message);
            place(window, premiere_hwnd);
        } else {
            #[cfg(windows)]
            CREATING_TOAST.with(|flag| flag.set(true));
            let window = (|| {
                let window = ErrorToastWindow::new()?;
                window.set_message(message);
                place(&window, premiere_hwnd);
                window.show()?;
                win_focus::keep_toast_inactive(window.window());
                Ok::<_, slint::PlatformError>(window)
            })();
            #[cfg(windows)]
            CREATING_TOAST.with(|flag| flag.set(false));
            match window {
                Ok(window) => *slot = Some(window),
                Err(error) => {
                    eprintln!("Could not show Spotlight error: {error}");
                    return;
                }
            }
        }

        let toast = Rc::downgrade(self);
        self.dismiss
            .start(TimerMode::SingleShot, Duration::from_secs(6), move || {
                if let Some(toast) = toast.upgrade() {
                    if let Some(window) = toast.window.borrow_mut().take() {
                        // Create a new native window for each showing so Winit uses its nonactivating first show.
                        let _ = window.hide();
                    }
                }
            });
    }
}

fn place(window: &ErrorToastWindow, premiere_hwnd: isize) {
    if let Some(position) =
        win_focus::error_toast_position(premiere_hwnd, window.get_toast_height())
    {
        window.window().set_position(position);
    }
}
