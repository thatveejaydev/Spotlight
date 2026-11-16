//! Finding Premiere's processes and windows, and watching for Premiere to exit.

use super::*;

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
pub(super) struct EnumPremiereState {
    any_running: bool,
    unminimized_hwnd: isize,
}

#[cfg(windows)]
pub(super) unsafe extern "system" fn enum_premiere_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
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
pub fn find_premiere_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
            return pids;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let exe = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if exe.eq_ignore_ascii_case("adobe premiere pro.exe") && entry.th32ProcessID != 0 {
                    pids.push(entry.th32ProcessID);
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    pids
}

#[cfg(windows)]
pub fn is_premiere_running() -> bool {
    !find_premiere_pids().is_empty()
}

#[cfg(not(windows))]
pub fn is_premiere_running() -> bool {
    true
}

#[cfg(windows)]
pub fn spawn_premiere_exit_watcher<F: FnOnce() + Send + 'static>(on_exit: F) {
    let _ = std::thread::Builder::new()
        .name("premiere-exit-watcher".into())
        .spawn(move || loop {
            let pids = find_premiere_pids();
            if pids.is_empty() {
                on_exit();
                return;
            }
            let handles: Vec<_> = pids
                .into_iter()
                .take(64)
                .filter_map(|pid| {
                    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
                    (!handle.is_null()).then_some(handle)
                })
                .collect();
            if handles.is_empty() {
                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            }
            let wait_result = unsafe {
                WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE)
            };
            unsafe {
                for handle in handles {
                    CloseHandle(handle);
                }
            }
            if wait_result == u32::MAX {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        });
}

#[cfg(not(windows))]
pub fn spawn_premiere_exit_watcher<F: FnOnce() + Send + 'static>(_on_exit: F) {}

/// Opens `path` with its default app, as a double-click in Explorer would.
#[cfg(windows)]
pub fn open_with_default_app(path: &std::path::Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    let wide = |text: &std::ffi::OsStr| text.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let (verb, file) = (wide("open".as_ref()), wide(path.as_os_str()));
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW reports success with a value above 32.
    result as usize > 32
}

#[cfg(not(windows))]
pub fn open_with_default_app(_path: &std::path::Path) -> bool {
    false
}
