use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetWindowLongW, GetWindowPlacement, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, GWL_EXSTYLE, WINDOWPLACEMENT, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowItem {
    pub hwnd: isize,
    pub title: String,
    pub rect: (i32, i32, i32, i32), // (left, top, width, height)
    pub is_minimized: bool,
}

#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmGetWindowAttribute(
        hwnd: isize,
        dw_attribute: u32,
        pv_attribute: *mut core::ffi::c_void,
        cb_attribute: u32,
    ) -> i32;
}

const DWMWA_CLOAKED: u32 = 14;

/// Returns true if the window is cloaked by DWM (e.g., suspended background UWP apps like Movies & TV)
pub fn is_window_cloaked(hwnd: HWND) -> bool {
    unsafe {
        let mut cloaked: u32 = 0;
        let hr = DwmGetWindowAttribute(
            hwnd.0 as isize,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
        hr == 0 && cloaked != 0
    }
}

pub fn get_window_title(hwnd_val: isize) -> Option<String> {
    if hwnd_val == 0 {
        return None;
    }
    unsafe {
        let hwnd = HWND(hwnd_val as *mut _);
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return None;
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let written = GetWindowTextW(hwnd, &mut buf);
        if written <= 0 {
            return None;
        }
        let title = String::from_utf16_lossy(&buf[..written as usize])
            .trim()
            .to_string();
        if title.is_empty() {
            None
        } else {
            Some(title)
        }
    }
}

unsafe extern "system" fn enum_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let list = &mut *(lparam.0 as *mut Vec<WindowItem>);

        if !IsWindowVisible(hwnd).as_bool() || is_window_cloaked(hwnd) {
            return BOOL(1);
        }

        // Skip windows belonging to ObsLite itself
        let mut win_pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut win_pid));
        if win_pid == std::process::id() {
            return BOOL(1);
        }

        // Skip floating tool windows unless explicitly marked as an app window
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if (ex_style & WS_EX_TOOLWINDOW.0) != 0 && (ex_style & WS_EX_APPWINDOW.0) == 0 {
            return BOOL(1);
        }

        let title = match get_window_title(hwnd.0 as isize) {
            Some(t) => t,
            None => return BOOL(1),
        };

        if title == "Program Manager"
            || title == "Windows Input Experience"
            || title == "Microsoft Text Input Application"
            || title == "Settings"
            || title.starts_with("ObsLite")
        {
            return BOOL(1);
        }

        let is_minimized = IsIconic(hwnd).as_bool();
        let (left, top, width, height) = if is_minimized {
            let mut wp = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            if GetWindowPlacement(hwnd, &mut wp).is_ok() {
                let r = wp.rcNormalPosition;
                (
                    r.left,
                    r.top,
                    (r.right - r.left).max(640),
                    (r.bottom - r.top).max(480),
                )
            } else {
                (0, 0, 1280, 720)
            }
        } else if let Some(client_rect) = get_client_screen_rect(hwnd) {
            client_rect
        } else {
            let mut rect = RECT::default();
            let _ = GetWindowRect(hwnd, &mut rect);
            (
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
            )
        };

        if is_minimized || (width > 80 && height > 80) {
            list.push(WindowItem {
                hwnd: hwnd.0 as isize,
                title,
                rect: (left, top, width, height),
                is_minimized,
            });
        }

        BOOL(1)
    }
}

/// Gets the client area of a window in screen coordinates (excluding title bar & invisible DWM shadow borders)
pub fn get_client_screen_rect(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
    unsafe {
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client).is_err() {
            return None;
        }
        let w = client.right - client.left;
        let h = client.bottom - client.top;
        if w <= 64 || h <= 64 {
            return None;
        }
        let mut pt = POINT {
            x: client.left,
            y: client.top,
        };
        if !ClientToScreen(hwnd, &mut pt).as_bool() {
            return None;
        }
        Some((pt.x, pt.y, w, h))
    }
}

pub fn is_window_handle_valid(hwnd_val: isize) -> bool {
    if hwnd_val == 0 {
        return false;
    }
    unsafe {
        let hwnd = HWND(hwnd_val as *mut _);
        IsWindow(Some(hwnd)).as_bool()
            && IsWindowVisible(hwnd).as_bool()
            && !is_window_cloaked(hwnd)
    }
}

pub fn is_window_minimized(hwnd_val: isize) -> bool {
    if hwnd_val == 0 {
        return false;
    }
    unsafe {
        let hwnd = HWND(hwnd_val as *mut _);
        IsIconic(hwnd).as_bool()
    }
}

pub fn get_window_dimensions(hwnd_val: isize) -> Option<(i32, i32, i32, i32)> {
    if !is_window_handle_valid(hwnd_val) || is_window_minimized(hwnd_val) {
        return None;
    }
    unsafe {
        let hwnd = HWND(hwnd_val as *mut _);
        if let Some(cr) = get_client_screen_rect(hwnd) {
            return Some(cr);
        }
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok() {
            let w = rect.right - rect.left;
            let h = rect.bottom - rect.top;
            if w > 64 && h > 64 {
                return Some((rect.left, rect.top, w, h));
            }
        }
        None
    }
}

/// Ensures the current thread is attached to the active interactive desktop station
/// so `EnumWindows`, `PrintWindow`, and `DXGI` can see all open user windows and games.
pub fn attach_to_input_desktop() {
    unsafe {
        use windows::Win32::System::StationsAndDesktops::{
            OpenInputDesktop, SetThreadDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS,
        };
        if let Ok(desktop) = OpenInputDesktop(
            DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ACCESS_FLAGS(0x01FF),
        ) {
            let _ = SetThreadDesktop(desktop);
        }
    }
}

pub fn get_available_windows() -> Vec<WindowItem> {
    attach_to_input_desktop();
    let mut list = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(enum_window_callback),
            LPARAM(&mut list as *mut _ as isize),
        );
    }
    list
}

/// Resolves a target window by checking its cached HWND (verifying its title still matches),
/// then falling back to title matching across active windows.
pub fn resolve_target_window(preferred_hwnd: isize, target_title: &str) -> Option<WindowItem> {
    attach_to_input_desktop();
    let clean_target = target_title.trim();
    let lower_target = clean_target.to_lowercase();

    if preferred_hwnd != 0 && is_window_handle_valid(preferred_hwnd) {
        if let Some(actual_title) = get_window_title(preferred_hwnd) {
            let actual_lower = actual_title.to_lowercase();
            let title_matches = clean_target.is_empty()
                || actual_lower == lower_target
                || actual_lower.contains(&lower_target)
                || lower_target.contains(&actual_lower);
            if title_matches {
                let is_min = is_window_minimized(preferred_hwnd);
                let rect = get_window_dimensions(preferred_hwnd).unwrap_or((0, 0, 1280, 720));
                return Some(WindowItem {
                    hwnd: preferred_hwnd,
                    title: actual_title,
                    rect,
                    is_minimized: is_min,
                });
            }
        }
    }

    if clean_target.is_empty() {
        return None;
    }

    let windows = get_available_windows();

    // 1. Exact title match
    if let Some(exact) = windows.iter().find(|w| w.title == clean_target) {
        return Some(exact.clone());
    }
    // 2. Case-insensitive exact match
    if let Some(ci) = windows
        .iter()
        .find(|w| w.title.to_lowercase() == lower_target)
    {
        return Some(ci.clone());
    }
    // 3. Substring match
    windows.into_iter().find(|w| {
        w.title.to_lowercase().contains(&lower_target)
            || lower_target.contains(&w.title.to_lowercase())
    })
}
