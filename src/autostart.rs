use anyhow::{Context, Result};
use std::env;
use windows::core::PCWSTR;
use windows::Win32::{
    Foundation::ERROR_SUCCESS,
    System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_SET_VALUE, REG_SZ,
    },
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "MouseCopyPasteDelete";
const LEGACY_VALUE_NAMES: &[&str] = &["AutoCopyPasteDelete", "AutoCopyPaste", "AutoClipX-RS"];

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn delete_legacy_values(hkey: HKEY) {
    for legacy in LEGACY_VALUE_NAMES {
        let w = wide(legacy);
        let _ = RegDeleteValueW(hkey, PCWSTR(w.as_ptr()));
    }
}

pub fn enable() -> Result<()> {
    let exe = env::current_exe().context("cannot get exe path")?;
    let exe_str = format!("\"{}\"", exe.to_string_lossy());
    let exe_wide: Vec<u16> = exe_str.encode_utf16().chain(std::iter::once(0)).collect();
    let name_wide = wide(VALUE_NAME);
    let key_wide = wide(RUN_KEY);

    unsafe {
        let mut hkey = HKEY::default();
        let res = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_wide.as_ptr()),
            Some(0),
            KEY_SET_VALUE,
            &mut hkey,
        );
        if res != ERROR_SUCCESS {
            anyhow::bail!("RegOpenKeyExW failed: {res:?}");
        }

        let bytes = std::slice::from_raw_parts(
            exe_wide.as_ptr() as *const u8,
            exe_wide.len() * std::mem::size_of::<u16>(),
        );
        let res = RegSetValueExW(hkey, PCWSTR(name_wide.as_ptr()), Some(0), REG_SZ, Some(bytes));
        // Миграция со старых имён.
        delete_legacy_values(hkey);
        let _ = RegCloseKey(hkey);

        if res != ERROR_SUCCESS {
            anyhow::bail!("RegSetValueExW failed: {res:?}");
        }
    }

    log::info!("autostart enabled");
    Ok(())
}

pub fn disable() -> Result<()> {
    let name_wide = wide(VALUE_NAME);
    let key_wide = wide(RUN_KEY);

    unsafe {
        let mut hkey = HKEY::default();
        let res = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_wide.as_ptr()),
            Some(0),
            KEY_SET_VALUE,
            &mut hkey,
        );
        if res != ERROR_SUCCESS {
            // Ключа нет — значит, автозапуск и так выключен.
            return Ok(());
        }
        let _ = RegDeleteValueW(hkey, PCWSTR(name_wide.as_ptr()));
        delete_legacy_values(hkey);
        let _ = RegCloseKey(hkey);
    }

    log::info!("autostart disabled");
    Ok(())
}
