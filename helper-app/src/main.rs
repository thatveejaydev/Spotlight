#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod action_hotkeys;
mod actions;
mod app;
mod audio_preview;
mod availability;
mod bridge;
mod catalog;
mod cursor_lock;
mod debug_log;
mod error_toast;
mod installer;
mod premiere_prefs;
mod presets;
mod protocol;
mod request;
mod safe_file;
mod settings;
mod tray;
mod win_focus;

slint::include_modules!();

#[cfg(windows)]
struct InstanceMutexGuard(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for InstanceMutexGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
fn acquire_instance_mutex() -> Option<InstanceMutexGuard> {
    // installer/premiere-spotlight.iss waits on this name (MyAppMutex) before upgrading.
    let name: Vec<u16> = "PremiereSpotlightInstanceMutex\0".encode_utf16().collect();
    let handle = unsafe {
        windows_sys::Win32::System::Threading::CreateMutexW(
            std::ptr::null(),
            0,
            name.as_ptr(),
        )
    };
    if handle.is_null() {
        return None;
    }
    let already_exists = unsafe {
        windows_sys::Win32::Foundation::GetLastError()
            == windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS
    };
    if already_exists {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        None
    } else {
        Some(InstanceMutexGuard(handle))
    }
}

fn main() -> Result<(), slint::PlatformError> {
    debug_log::install_panic_hook();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has_flag = |flag: &str| args.iter().any(|arg| arg == flag);

    // Older installers also pass --no-startup; Spotlight never registers a startup entry.
    if has_flag("--install") {
        installer::install_for_setup();
        return Ok(());
    }
    if has_flag("--uninstall") {
        installer::uninstall_for_setup(has_flag("--purge-config"));
        return Ok(());
    }

    // 1. Auto-install / enable the UXP bridge plugin in Premiere Pro and clean up legacy Startup entries
    installer::ensure_installed();

    // 2. Spotlight only runs while Adobe Premiere Pro is open
    if !win_focus::is_premiere_running() {
        return Ok(());
    }

    // 3. Single-instance check: if Premiere Spotlight is already running in the System Tray, wake it (unless --watch) and exit
    if bridge::check_or_wake_existing_instance(!has_flag("--watch")) {
        return Ok(());
    }

    #[cfg(windows)]
    let Some(_instance_mutex) = acquire_instance_mutex() else {
        return Ok(());
    };

    app::run()
}

