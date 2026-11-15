use std::cell::RefCell;

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{ HWND, LPARAM, LRESULT, POINT, WPARAM},
    Graphics::Gdi::{CreateBitmap, DeleteObject},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
            NOTIFYICONDATAW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
            DestroyIcon, DestroyMenu, DestroyWindow, GetCursorPos, PostMessageW, RegisterClassW,
            SetForegroundWindow, TrackPopupMenu, CW_USEDEFAULT, HICON, ICONINFO, MF_CHECKED,
            MF_GRAYED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
            TPM_NONOTIFY, TPM_RETURNCMD, WM_CONTEXTMENU, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
            WM_USER, WNDCLASSW,
        },
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenSpotlight,
    OpenSettings,
    ToggleDefaultEffectControls,
    OpenConfigFile,
    ReloadConfig,
    Quit,
}

thread_local! {
    static TRAY_ACTIONS: RefCell<Vec<TrayAction>> = const { RefCell::new(Vec::new()) };
    static TRAY_CONNECTED: RefCell<bool> = const { RefCell::new(false) };
    static HOTKEY_LABEL: RefCell<String> = RefCell::new(String::from("Ctrl+Space"));
    static HOTKEY_AVAILABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    static DEFAULT_EFFECT_CONTROLS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TRAY_DISPATCH: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
}

pub fn set_hotkey_status(label: &str, available: bool) {
    HOTKEY_LABEL.with(|slot| *slot.borrow_mut() = label.to_string());
    HOTKEY_AVAILABLE.set(available);
}

pub fn set_default_effect_controls(enabled: bool) {
    DEFAULT_EFFECT_CONTROLS.set(enabled);
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
const TRAY_ICON_UID: u32 = 1;
#[cfg(windows)]
const ID_TRAY_OPEN: usize = 1001;
#[cfg(windows)]
const ID_TRAY_QUIT: usize = 1002;
#[cfg(windows)]
const ID_TRAY_SETTINGS: usize = 1003;
#[cfg(windows)]
const ID_TRAY_TOGGLE_EC: usize = 1004;
#[cfg(windows)]
const ID_TRAY_OPEN_CONFIG: usize = 1005;
#[cfg(windows)]
const ID_TRAY_RELOAD_CONFIG: usize = 1006;

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

            let hicon = create_spotlight_hicon();

            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = TRAY_ICON_UID;
            nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            nid.uCallbackMessage = WM_TRAYICON;
            nid.hIcon = hicon;
            let hotkey = HOTKEY_LABEL.with(|s| s.borrow().clone());
            write_tip(&mut nid.szTip, &format!("Premiere Spotlight · {hotkey} in Premiere Pro"));

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

    pub fn refresh_tooltip(&self) {
        let connected = TRAY_CONNECTED.with(|c| *c.borrow());
        self.set_connected(connected);
    }

    pub fn set_connected(&self, connected: bool) {
        TRAY_CONNECTED.with(|c| *c.borrow_mut() = connected);
        #[cfg(windows)]
        unsafe {
            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = self.hwnd;
            nid.uID = TRAY_ICON_UID;
            nid.uFlags = NIF_TIP;
            let hotkey = HOTKEY_LABEL.with(|s| s.borrow().clone());
            let tip = if connected {
                format!("Premiere Spotlight — Connected ({hotkey})")
            } else {
                format!("Premiere Spotlight — Waiting for Premiere Pro ({hotkey})...")
            };
            write_tip(&mut nid.szTip, &tip);
            Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
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
unsafe fn show_tray_context_menu(hwnd: HWND) {
    let hmenu = CreatePopupMenu();
    if hmenu.is_null() {
        return;
    }

    let connected = TRAY_CONNECTED.with(|c| *c.borrow());
    let (premiere_running, unminimized_premiere_hwnd) =
        crate::win_focus::query_premiere_window_state();

    let status_label = if !premiere_running {
        to_wide("Premiere Spotlight · Waiting for Premiere Pro")
    } else if unminimized_premiere_hwnd == 0 {
        to_wide("Premiere Spotlight · Premiere Minimized")
    } else if connected {
        to_wide("Premiere Spotlight · Connected")
    } else {
        to_wide("Premiere Spotlight · Premiere Active")
    };
    let hotkey_label = HOTKEY_LABEL.with(|s| s.borrow().clone());
    let hotkey_available = HOTKEY_AVAILABLE.get();
    let default_ec = DEFAULT_EFFECT_CONTROLS.get();
    let open_label = to_wide(&if hotkey_available {
        format!("Open Spotlight ({hotkey_label})")
    } else {
        "Open Spotlight".to_string()
    });
    let settings_label = to_wide("Settings…");
    let ec_toggle_label = to_wide("Always Open Effect Controls on Enter");
    let open_config_label = to_wide("Open config.json");
    let reload_config_label = to_wide("Reload Config");
    let quit_label = to_wide("Quit Premiere Spotlight");

    AppendMenuW(hmenu, MF_STRING | MF_GRAYED, 0, status_label.as_ptr());
    if !hotkey_available {
        let unavailable = to_wide(&format!("{hotkey_label} unavailable"));
        AppendMenuW(hmenu, MF_STRING | MF_GRAYED, 0, unavailable.as_ptr());
    }
    AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null());

    let open_flags = if unminimized_premiere_hwnd != 0 {
        MF_STRING
    } else {
        MF_STRING | MF_GRAYED
    };
    AppendMenuW(hmenu, open_flags, ID_TRAY_OPEN, open_label.as_ptr());
    AppendMenuW(hmenu, MF_STRING, ID_TRAY_SETTINGS, settings_label.as_ptr());
    AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null());
    let ec_flags = MF_STRING | if default_ec { MF_CHECKED } else { MF_UNCHECKED };
    AppendMenuW(hmenu, ec_flags, ID_TRAY_TOGGLE_EC, ec_toggle_label.as_ptr());
    AppendMenuW(hmenu, MF_STRING, ID_TRAY_OPEN_CONFIG, open_config_label.as_ptr());
    AppendMenuW(hmenu, MF_STRING, ID_TRAY_RELOAD_CONFIG, reload_config_label.as_ptr());
    AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(hmenu, MF_STRING, ID_TRAY_QUIT, quit_label.as_ptr());

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
        ID_TRAY_TOGGLE_EC => push_tray_action(TrayAction::ToggleDefaultEffectControls),
        ID_TRAY_OPEN_CONFIG => push_tray_action(TrayAction::OpenConfigFile),
        ID_TRAY_RELOAD_CONFIG => push_tray_action(TrayAction::ReloadConfig),
        ID_TRAY_QUIT => push_tray_action(TrayAction::Quit),
        _ => {}
    }
}

/// Generates a 32x32 rounded indigo ("FX") system tray icon in memory.
#[cfg(windows)]
unsafe fn create_spotlight_hicon() -> HICON {
    const W: usize = 32;
    const H: usize = 32;
    let mut pixels = [0u32; W * H];

    // 1. Draw rounded square background (#6366F1 indigo with #818CF8 border)
    for y in 0..H {
        for x in 0..W {
            let dx = if x < 6 {
                6 - x
            } else if x > 25 {
                x - 25
            } else {
                0
            };
            let dy = if y < 6 {
                6 - y
            } else if y > 25 {
                y - 25
            } else {
                0
            };
            let dist_sq = dx * dx + dy * dy;
            if dist_sq <= 25 {
                let is_border = dist_sq >= 16 || x == 1 || x == 30 || y == 1 || y == 30;
                // BGRA in little-endian u32 = 0xAARRGGBB
                pixels[y * W + x] = if is_border {
                    0xFF818CF8
                } else {
                    0xFF4F46E5
                };
            }
        }
    }

    // 2. Draw crisp white "FX" glyph (11 rows high, centered around y=10..21)
    const GLYPH_F: [u8; 11] = [
        0b1111111,
        0b1111111,
        0b1100000,
        0b1100000,
        0b1111110,
        0b1111110,
        0b1100000,
        0b1100000,
        0b1100000,
        0b1100000,
        0b1100000,
    ];
    const GLYPH_X: [u8; 11] = [
        0b1100011,
        0b1100011,
        0b0110110,
        0b0110110,
        0b0011100,
        0b0011100,
        0b0011100,
        0b0110110,
        0b0110110,
        0b1100011,
        0b1100011,
    ];

    for (row_idx, &bits) in GLYPH_F.iter().enumerate() {
        let y = 10 + row_idx;
        for col in 0..7 {
            if (bits >> (6 - col)) & 1 == 1 {
                pixels[y * W + (7 + col)] = 0xFFFFFFFF;
            }
        }
    }
    for (row_idx, &bits) in GLYPH_X.iter().enumerate() {
        let y = 10 + row_idx;
        for col in 0..7 {
            if (bits >> (6 - col)) & 1 == 1 {
                pixels[y * W + (17 + col)] = 0xFFFFFFFF;
            }
        }
    }

    let color_bmp = CreateBitmap(
        W as i32,
        H as i32,
        1,
        32,
        pixels.as_ptr() as *const std::ffi::c_void,
    );
    let mask_bytes = [0u8; (W * H) / 8];
    let mask_bmp = CreateBitmap(
        W as i32,
        H as i32,
        1,
        1,
        mask_bytes.as_ptr() as *const std::ffi::c_void,
    );

    let icon_info = ICONINFO {
        fIcon: 1,
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask_bmp,
        hbmColor: color_bmp,
    };

    let hicon = CreateIconIndirect(&icon_info);
    DeleteObject(color_bmp as _);
    DeleteObject(mask_bmp as _);
    hicon
}
