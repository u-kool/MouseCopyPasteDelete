use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};

pub const HOTKEY_ID: i32 = 1;

/// Сообщение главному потоку: перечитать настройку и перерегистрировать горячую клавишу.
pub const WM_REREGISTER_HOTKEY: u32 = WM_APP + 1;

/// Сообщение потоку трея: обновить галочку и подсказку иконки.
pub const WM_REFRESH_TRAY: u32 = WM_APP + 2;

static MAIN_THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// Вызывается из потока-владельца горячей клавиши (поток трея).
pub fn set_main_thread_id() {
    MAIN_THREAD_ID.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
}

fn main_thread_id() -> u32 {
    MAIN_THREAD_ID.load(Ordering::SeqCst)
}

/// Просит главный поток перерегистрировать горячую клавишу.
pub fn request_reregister() {
    post_to_tray(WM_REREGISTER_HOTKEY);
}

/// Просит поток трея обновить отображение текущего состояния.
pub fn request_tray_refresh() {
    post_to_tray(WM_REFRESH_TRAY);
}

fn post_to_tray(message: u32) {
    let tid = main_thread_id();
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, message, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn register_toggle_hotkey(hotkey: &str) -> bool {
    let Some((modifiers, vk)) = parse_hotkey(hotkey) else {
        log::error!("invalid hotkey string: {hotkey}");
        return false;
    };

    let ok = unsafe { RegisterHotKey(None, HOTKEY_ID, modifiers, vk) }.is_ok();
    if ok {
        log::info!("hotkey registered: {hotkey}");
    } else {
        log::error!("hotkey registration failed (maybe already in use): {hotkey}");
    }
    ok
}

pub fn unregister_hotkey() {
    unsafe {
        let _ = UnregisterHotKey(None, HOTKEY_ID);
    }
}

fn parse_hotkey(s: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let mut modifiers = HOT_KEY_MODIFIERS(0);
    let mut vk = None;

    for part in s.split('+') {
        let part = part.trim().to_uppercase();
        if part.is_empty() {
            continue;
        }
        match part.as_str() {
            "CTRL" | "CONTROL" => modifiers |= MOD_CONTROL,
            "ALT" => modifiers |= MOD_ALT,
            "SHIFT" => modifiers |= MOD_SHIFT,
            "WIN" | "SUPER" | "META" => modifiers |= MOD_WIN,
            _ => {
                if vk.is_some() {
                    return None;
                }
                vk = Some(parse_key(&part)?);
            }
        }
    }

    // Требуем хотя бы один модификатор, чтобы не перехватывать обычные клавиши.
    if modifiers.0 == 0 {
        return None;
    }

    Some((modifiers | MOD_NOREPEAT, vk?))
}

fn parse_key(key: &str) -> Option<u32> {
    if key.chars().count() == 1 {
        let c = key.chars().next()?;
        if c.is_ascii_alphanumeric() {
            return Some(c as u32);
        }
        return match c {
            '+' => Some(0xBB),
            '-' => Some(0xBD),
            '.' => Some(0xBE),
            ',' => Some(0xBC),
            _ => None,
        };
    }

    let code = match key {
        "F1" => 0x70,
        "F2" => 0x71,
        "F3" => 0x72,
        "F4" => 0x73,
        "F5" => 0x74,
        "F6" => 0x75,
        "F7" => 0x76,
        "F8" => 0x77,
        "F9" => 0x78,
        "F10" => 0x79,
        "F11" => 0x7A,
        "F12" => 0x7B,
        "SPACE" => 0x20,
        "TAB" => 0x09,
        "ENTER" | "RETURN" => 0x0D,
        "ESC" | "ESCAPE" => 0x1B,
        "INSERT" => 0x2D,
        "DELETE" | "DEL" => 0x2E,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" => 0x21,
        "PAGEDOWN" => 0x22,
        _ => return None,
    };
    Some(code)
}
