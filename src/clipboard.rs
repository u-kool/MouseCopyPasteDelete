use anyhow::{anyhow, Result};
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::DataExchange::{
    CloseClipboard, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable,
    OpenClipboard,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

const CF_UNICODETEXT: u32 = 13;

pub fn sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// Читает текущий текст из буфера обмена (CF_UNICODETEXT).
/// Возвращает `Ok(None)`, если текстового формата нет или буфер недоступен.
pub fn read_unicode_text() -> Result<Option<String>> {
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return Ok(None);
        }

        OpenClipboard(None).map_err(|e| anyhow!("OpenClipboard failed: {e}"))?;

        let result = (|| {
            let handle = match GetClipboardData(CF_UNICODETEXT) {
                Ok(h) => h,
                Err(_) => return Ok(None),
            };
            if handle.is_invalid() {
                return Ok(None);
            }

            // HGLOBAL и HANDLE — совместимые непрозрачные дескрипторы.
            let hglobal = HGLOBAL(handle.0);
            let ptr = GlobalLock(hglobal);
            if ptr.is_null() {
                return Ok(None);
            }

            let size = GlobalSize(hglobal);
            let text = if size == 0 {
                String::new()
            } else {
                // size в байтах; UTF-16 — по 2 байта. Ограничиваем разумным объёмом.
                let max_bytes = size.min(2 * 1024 * 1024);
                let len_u16 = max_bytes / 2;
                let slice = std::slice::from_raw_parts(ptr as *const u16, len_u16);
                let end = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
                String::from_utf16_lossy(&slice[..end])
            };

            let _ = GlobalUnlock(hglobal);
            Ok(Some(text))
        })();

        let _ = CloseClipboard();
        result
    }
}
