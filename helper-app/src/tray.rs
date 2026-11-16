use std::cell::RefCell;

#[cfg(windows)]
use windows_sys::{
    core::w,
    Wdk::System::SystemServices::RtlGetVersion,
    Win32::{
        Foundation::{FreeLibrary, HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::{
            LibraryLoader::{
                GetModuleHandleW, GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
            },
            SystemInformation::OSVERSIONINFOW,
        },
        UI::{
            Shell::{
                Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                NOTIFYICONDATAW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
                DestroyMenu, DestroyWindow, GetCursorPos, GetSystemMetrics, LoadImageW,
                PostMessageW, RegisterClassW, SetForegroundWindow, TrackPopupMenu, CW_USEDEFAULT,
                HICON, IMAGE_ICON, MF_GRAYED, MF_SEPARATOR, MF_STRING, SM_CXSMICON, SM_CYSMICON,
                TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, WM_CLOSE,
                WM_CONTEXTMENU, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_USER, WNDCLASSW,
            },
        },
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenSpotlight,
    OpenSettings,
    Quit,
}

thread_local! {
    static TRAY_ACTIONS: RefCell<Vec<TrayAction>> = const { RefCell::new(Vec::new()) };
    static TRAY_DISPATCH: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
}

pub fn set_action_handler<F: FnMut() + 'static>(handler: F) {
    TRAY_DISPATCH.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(handler));
    });
}

#[cfg(windows)]
fn push_tray_action(action: TrayAction) {
    TRAY_ACTIONS.with(|q| q.borrow_mut().push(action));
    let _ = slint::invoke_from_event_loop(|| {
        TRAY_DISPATCH.with(|slot| {
            if let Some(callback) = slot.borrow_mut().as_mut() {
                callback();
            }
        });
    });
}

pub fn poll_tray_action() -> Option<TrayAction> {
    TRAY_ACTIONS.with(|q| {
        let mut queue = q.borrow_mut();
        if queue.is_empty() {
            None
        } else {
            Some(queue.remove(0))
        }
    })
}

#[cfg(windows)]
const WM_TRAYICON: u32 = WM_USER + 42;
#[cfg(windows)]
const APP_ICON_RESOURCE_ID: u16 = 101;
#[cfg(windows)]
const TRAY_ICON_UID: u32 = 1;
#[cfg(windows)]
const ID_TRAY_OPEN: usize = 1001;
#[cfg(windows)]
const ID_TRAY_QUIT: usize = 1002;
#[cfg(windows)]
const ID_TRAY_SETTINGS: usize = 1003;

pub struct SystemTray {
    #[cfg(windows)]
    hwnd: HWND,
    #[cfg(windows)]
    hicon: HICON,
    #[cfg(windows)]
    removed: bool,
}

impl SystemTray {
    #[cfg(windows)]
    pub fn new() -> Option<Self> {
        unsafe {
            let hinstance = GetModuleHandleW(std::ptr::null());
            let class_name = to_wide("PremiereSpotlightTrayWndClass");

            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(tray_wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
            };
            RegisterClassW(&wc);

            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                return None;
            }

            let hicon = LoadImageW(
                hinstance,
                APP_ICON_RESOURCE_ID as usize as *const u16,
                IMAGE_ICON,
                GetSystemMetrics(SM_CXSMICON),
                GetSystemMetrics(SM_CYSMICON),
                0,
            );
            if hicon.is_null() {
                DestroyWindow(hwnd);
                return None;
            }

            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = TRAY_ICON_UID;
            nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            nid.uCallbackMessage = WM_TRAYICON;
            nid.hIcon = hicon;
            write_tip(&mut nid.szTip, "Premiere Spotlight");

            Shell_NotifyIconW(NIM_ADD, &nid);

            Some(Self {
                hwnd,
                hicon,
                removed: false,
            })
        }
    }

    #[cfg(not(windows))]
    pub fn new() -> Option<Self> {
        Some(Self {})
    }

    pub fn remove(&mut self) {
        #[cfg(windows)]
        if !self.removed {
            self.removed = true;
            unsafe {
                let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
                nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                nid.hWnd = self.hwnd;
                nid.uID = TRAY_ICON_UID;
                Shell_NotifyIconW(NIM_DELETE, &nid);
                if !self.hicon.is_null() {
                    DestroyIcon(self.hicon);
                    self.hicon = std::ptr::null_mut();
                }
                if !self.hwnd.is_null() {
                    DestroyWindow(self.hwnd);
                    self.hwnd = std::ptr::null_mut();
                }
            }
        }
    }
}

impl Drop for SystemTray {
    fn drop(&mut self) {
        self.remove();
    }
}

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

#[cfg(windows)]
fn write_tip(buf: &mut [u16; 128], text: &str) {
    buf.fill(0);
    for (dst, src) in buf.iter_mut().take(127).zip(text.encode_utf16()) {
        *dst = src;
    }
}

#[cfg(windows)]
unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CLOSE {
        push_tray_action(TrayAction::Quit);
        return 0;
    }
    if msg == WM_TRAYICON {
        let mouse_msg = (lparam as u32) & 0xFFFF;
        if mouse_msg == WM_LBUTTONUP {
            push_tray_action(TrayAction::OpenSpotlight);
            return 0;
        } else if mouse_msg == WM_RBUTTONUP || mouse_msg == WM_CONTEXTMENU {
            show_tray_context_menu(hwnd);
            return 0;
        }
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[cfg(windows)]
unsafe fn apply_native_menu_theme(hwnd: HWND) {
    let mut version: OSVERSIONINFOW = std::mem::zeroed();
    version.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
    if RtlGetVersion(&mut version) != 0 || version.dwBuildNumber < 18362 {
        return;
    }

    let theme = LoadLibraryExW(
        w!("uxtheme.dll"),
        std::ptr::null_mut(),
        LOAD_LIBRARY_SEARCH_SYSTEM32,
    );
    if theme.is_null() {
        return;
    }

    // These theme exports are ordinal-only. The signature of 135 changed in Windows 10 1903.
    if let (Some(app_mode), Some(window_mode), Some(flush_menus)) = (
        GetProcAddress(theme, 135usize as *const u8),
        GetProcAddress(theme, 133usize as *const u8),
        GetProcAddress(theme, 136usize as *const u8),
    ) {
        let set_preferred_app_mode: unsafe extern "system" fn(i32) -> i32 =
            std::mem::transmute(app_mode);
        let allow_dark_mode_for_window: unsafe extern "system" fn(HWND, bool) -> bool =
            std::mem::transmute(window_mode);
        let flush_menu_themes: unsafe extern "system" fn() = std::mem::transmute(flush_menus);

        const ALLOW_DARK: i32 = 1;
        set_preferred_app_mode(ALLOW_DARK);
        allow_dark_mode_for_window(hwnd, true);
        flush_menu_themes();
    }

    FreeLibrary(theme);
}

#[cfg(windows)]
unsafe fn show_tray_context_menu(hwnd: HWND) {
    apply_native_menu_theme(hwnd);
    let hmenu = CreatePopupMenu();
    if hmenu.is_null() {
        return;
    }

    let (_, premiere_hwnd) = crate::win_focus::query_premiere_window_state();
    let open_flags = if premiere_hwnd == 0 {
        MF_GRAYED
    } else {
        MF_STRING
    };
    AppendMenuW(hmenu, open_flags, ID_TRAY_OPEN, w!("Open Spotlight"));
    AppendMenuW(hmenu, MF_STRING, ID_TRAY_SETTINGS, w!("Settings"));
    AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(
        hmenu,
        MF_STRING,
        ID_TRAY_QUIT,
        w!("Quit Premiere Spotlight"),
    );
    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);

    let cmd = TrackPopupMenu(
        hmenu,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_BOTTOMALIGN | TPM_LEFTALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        std::ptr::null(),
    ) as usize;

    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(hmenu);

    match cmd {
        ID_TRAY_OPEN => push_tray_action(TrayAction::OpenSpotlight),
        ID_TRAY_SETTINGS => push_tray_action(TrayAction::OpenSettings),
        ID_TRAY_QUIT => push_tray_action(TrayAction::Quit),
        _ => {}
    }
}
