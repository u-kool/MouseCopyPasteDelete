use crate::clipboard;
use crate::send_input;
use crate::AppState;
use anyhow::{Context, Result};
use std::sync::{
    atomic::Ordering,
    Arc, OnceLock,
};
use std::time::Duration;
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED},
    UI::{
        Accessibility::{
            CUIAutomation, IUIAutomation, UIA_ComboBoxControlTypeId, UIA_EditControlTypeId,
        },
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
        },
        WindowsAndMessaging::{
            CallNextHookEx, DispatchMessageW, GetClassNameW, GetGUIThreadInfo, GetMessageW,
            GetParent, GetSystemMetrics, GetWindowRect, SetWindowsHookExW, TranslateMessage,
            WindowFromPoint, GUITHREADINFO, MSLLHOOKSTRUCT, MSG, SM_CXDOUBLECLK, SM_CYDOUBLECLK,
            WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
        },
    },
};
use std::cell::RefCell;

#[link(name = "user32")]
extern "system" {
    fn GetDoubleClickTime() -> u32;
}

static STATE: OnceLock<Arc<AppState>> = OnceLock::new();

thread_local! {
    static UIA: RefCell<Option<IUIAutomation>> = const { RefCell::new(None) };
}

/// Устанавливает хук мыши и запускает цикл сообщений потока.
/// Дескриптор хука живёт до завершения функции (поток живёт всё время работы приложения).
pub fn run_mouse_hook_thread(state: Arc<AppState>) -> Result<()> {
    let _ = STATE.set(state);

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        match CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
            Ok(uia) => {
                UIA.with(|cell| {
                    *cell.borrow_mut() = Some(uia);
                });
                log::info!("UI Automation initialized for text input detection");
            }
            Err(e) => {
                log::warn!("UI Automation init failed (browser/custom inputs may not be detected): {e}");
            }
        }
    }

    let _hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0) }
        .context("SetWindowsHookExW(WH_MOUSE_LL) failed")?;
    log::info!("mouse hook installed");

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let Some(state) = STATE.get() else {
        return CallNextHookEx(None, code, wparam, lparam);
    };

    let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    let message = wparam.0 as u32;

    match message {
        WM_LBUTTONDOWN => {
            // Вставка по двойному клику на активном поле ввода.
            // Работает независимо от переключателя авто-копирования.
            if try_double_click_paste(state, info) {
                state.dragging.store(false, Ordering::SeqCst);
                return LRESULT(1);
            }

            // Отслеживаем ЛКМ для жестов (авто-копирование при отпускании, удаление ЛКМ+ПКМ).
            // Выделение с зажатым модификатором не считаем перетаскиванием.
            if any_modifier_pressed() {
                state.dragging.store(false, Ordering::SeqCst);
            } else {
                *state.last_mouse_pos.lock().unwrap() = (info.pt.x, info.pt.y);
                state.drag_rbutton_delete.store(false, Ordering::SeqCst);
                state.dragging.store(true, Ordering::SeqCst);
            }
        }
        // Удаление выделенного: ЛКМ удерживается (выделение), нажата ПКМ.
        WM_RBUTTONDOWN if state.dragging.load(Ordering::SeqCst) => {
            if !state.enabled.load(Ordering::SeqCst)
                || !delete_gesture_enabled(state)
                || is_blacklisted_process(state)
            {
                return CallNextHookEx(None, code, wparam, lparam);
            }
            state.drag_rbutton_delete.store(true, Ordering::SeqCst);
            log::debug!("LMB drag + RMB: delete selection");
            let st = state.clone();
            std::thread::spawn(move || {
                // Небольшая задержка, чтобы целевое приложение обработало состояние кнопок.
                std::thread::sleep(Duration::from_millis(15));
                if is_blacklisted_process(&st) {
                    return;
                }
                match send_input::send_delete() {
                    Ok(()) => log::debug!("Delete sent (LMB drag + RMB)"),
                    Err(e) => log::error!("SendInput Delete failed: {e}"),
                }
            });
            return LRESULT(1);
        }
        WM_RBUTTONUP if state.drag_rbutton_delete.load(Ordering::SeqCst) => {
            // Флаг сбросит LBUTTONUP, чтобы не запускать авто-копирование после удаления.
            return LRESULT(1);
        }
        WM_LBUTTONUP if state.dragging.swap(false, Ordering::SeqCst) => {
            // Если во время выделения сработал жест ЛКМ+ПКМ (удаление), не копируем.
            if state.drag_rbutton_delete.swap(false, Ordering::SeqCst) {
                return CallNextHookEx(None, code, wparam, lparam);
            }

            if !state.enabled.load(Ordering::SeqCst) {
                return CallNextHookEx(None, code, wparam, lparam);
            }

            let (start_x, start_y) = *state.last_mouse_pos.lock().unwrap();
            let dx = (info.pt.x - start_x).abs();
            let dy = (info.pt.y - start_y).abs();

            let (copy_on_select, threshold, delay_ms, sound, sound_name, sound_file, sound_volume) = {
                let s = state.settings.lock().unwrap();
                (
                    s.copy_on_select,
                    s.move_threshold as i32,
                    s.copy_delay_ms,
                    s.sound_feedback,
                    s.sound_name.clone(),
                    s.sound_file.clone(),
                    s.sound_volume,
                )
            };

            if (dx > threshold || dy > threshold) && copy_on_select {
                let st = state.clone();
                // Всё «тяжёлое» — вне колбэка хука, чтобы не блокировать ввод.
                std::thread::spawn(move || {
                    if is_blacklisted_process(&st) {
                        log::debug!("foreground process is blacklisted, skip copy");
                        return;
                    }
                    if delay_ms > 0 {
                        std::thread::sleep(Duration::from_millis(delay_ms));
                    }

                    let seq_before = clipboard::sequence_number();

                    match send_input::send_ctrl_c() {
                        Ok(()) => {
                            // Даём приложению время обработать Ctrl+C.
                            std::thread::sleep(Duration::from_millis(60));

                            let seq_after = clipboard::sequence_number();
                            let changed_by_seq = seq_after != seq_before;

                            if !changed_by_seq {
                                log::debug!(
                                    "Ctrl+C sent, but clipboard did not change; skip (likely resize drag / no selection)"
                                );
                                return;
                            }

                            // Защита от повторного копирования того же текста
                            // (например, при ресайзе окна, когда выделение не менялось).
                            let text_after = clipboard::read_unicode_text().ok().flatten();
                            if let Some(ref text) = text_after {
                                let same_as_last = st
                                    .last_copied_text
                                    .lock()
                                    .map(|last| !last.is_empty() && last.as_str() == text.as_str())
                                    .unwrap_or(false);
                                if same_as_last {
                                    log::debug!(
                                        "clipboard text unchanged (duplicate copy); skip sound"
                                    );
                                    return;
                                }
                            }

                            log::debug!("Ctrl+C sent (clipboard changed)");

                            if let Some(text) = text_after {
                                if let Ok(mut last) = st.last_copied_text.lock() {
                                    *last = text;
                                }
                            }

                            if sound {
                                send_input::play_copy_sound(&sound_name, &sound_file, sound_volume);
                            }
                        }
                        Err(e) => log::error!("SendInput failed: {e}"),
                    }
                });
            }
        }
        _ => {}
    }

    CallNextHookEx(None, code, wparam, lparam)
}

fn any_modifier_pressed() -> bool {
    unsafe {
        GetAsyncKeyState(VK_CONTROL.0 as i32) < 0
            || GetAsyncKeyState(VK_MENU.0 as i32) < 0
            || GetAsyncKeyState(VK_LWIN.0 as i32) < 0
            || GetAsyncKeyState(VK_RWIN.0 as i32) < 0
    }
}

fn any_modifier_pressed_with_shift() -> bool {
    any_modifier_pressed() || unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) < 0 }
}

fn double_paste_enabled(state: &AppState) -> bool {
    state
        .settings
        .lock()
        .map(|s| s.paste_on_double_click)
        .unwrap_or(false)
}

fn delete_gesture_enabled(state: &AppState) -> bool {
    state
        .settings
        .lock()
        .map(|s| s.delete_on_lmb_rmb)
        .unwrap_or(true)
}

unsafe fn try_double_click_paste(state: &Arc<AppState>, info: &MSLLHOOKSTRUCT) -> bool {
    if !state.enabled.load(Ordering::SeqCst) {
        if let Ok(mut t) = state.last_lbutton_down.lock() {
            t.last_down_time = info.time;
            t.last_down_pt = (info.pt.x, info.pt.y);
        }
        return false;
    }
    if !double_paste_enabled(state) {
        // Фича выключена — запомним время клика.
        if let Ok(mut t) = state.last_lbutton_down.lock() {
            t.last_down_time = info.time;
            t.last_down_pt = (info.pt.x, info.pt.y);
        }
        return false;
    }

    let (last_time, last_pt) = state
        .last_lbutton_down
        .lock()
        .map(|t| (t.last_down_time, t.last_down_pt))
        .unwrap_or((0, (0, 0)));

    let dt = info.time.wrapping_sub(last_time);
    let max_dt = GetDoubleClickTime();
    let dx = (info.pt.x - last_pt.0).abs();
    let dy = (info.pt.y - last_pt.1).abs();
    let max_dx = GetSystemMetrics(SM_CXDOUBLECLK).max(4);
    let max_dy = GetSystemMetrics(SM_CYDOUBLECLK).max(4);
    let is_double = dt <= max_dt && dx <= max_dx && dy <= max_dy;

    // Обновляем трекер текущим кликом.
    if let Ok(mut t) = state.last_lbutton_down.lock() {
        t.last_down_time = info.time;
        t.last_down_pt = (info.pt.x, info.pt.y);
    }

    if !is_double {
        return false;
    }
    if any_modifier_pressed_with_shift() || is_blacklisted_process(state) {
        return false;
    }
    if !is_text_input_at(info.pt) {
        log::debug!("double-click: not on text input, skip paste");
        return false;
    }

    log::debug!("double-click on text input: intercept and paste");
    // Сбрасываем трекер, чтобы третий быстрый клик не считался двойным.
    if let Ok(mut t) = state.last_lbutton_down.lock() {
        t.last_down_time = 0;
        t.last_down_pt = (0, 0);
    }

    let st = Arc::clone(state);
    std::thread::spawn(move || {
        if is_blacklisted_process(&st) {
            log::debug!("foreground process is blacklisted, skip double-click paste");
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
        match send_input::send_ctrl_v() {
            Ok(()) => log::debug!("Ctrl+V sent (double-click)"),
            Err(e) => log::error!("SendInput Ctrl+V failed (double-click): {e}"),
        }
    });
    true
}

/// Эвристика: курсор на нативном поле ввода текста.
/// Если определить нельзя/не поле ввода — возвращает false, чтобы жест
/// вставки не перехватывал обычные действия.
unsafe fn is_text_input_at(pt: POINT) -> bool {
    let hwnd = WindowFromPoint(pt);
    if hwnd.0.is_null() {
        log::debug!("text input: WindowFromPoint returned null");
        return false;
    }

    // Проверяем само окно под курсором и ближайших родителей
    // (WindowFromPoint иногда возвращает обёртку).
    let mut cur = hwnd;
    for _ in 0..4 {
        if cur.0.is_null() {
            break;
        }
        if is_edit_class(cur) {
            log::debug!("text input: detected class at cursor");
            return true;
        }
        let Ok(parent) = GetParent(cur) else {
            break;
        };
        if parent.0.is_null() || parent == cur {
            break;
        }
        cur = parent;
    }

    // Дополнительно: активное/каретка-окно потока GUI часто является edit.
    // Это помогает, когда WindowFromPoint возвращает не тот HWND.
    let mut gui = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if GetGUIThreadInfo(0, &mut gui).is_ok() {
        for candidate in [gui.hwndCaret, gui.hwndFocus] {
            if candidate.0.is_null() || !is_edit_class(candidate) {
                continue;
            }
            // Если курсор на этом контроле (или на его области) — считаем полем ввода.
            if candidate == hwnd || is_ancestor(candidate, hwnd) || point_in_hwnd(candidate, pt) {
                log::debug!("text input: detected via GUI thread info");
                return true;
            }
        }
    }

    // Fallback: UI Automation для браузеров/Electron/кастомных контролов.
    if is_text_input_uia(pt) {
        return true;
    }

    if let Some(name) = class_name(hwnd) {
        log::debug!("text input: cursor not on input (class={name})");
    }
    false
}

unsafe fn is_text_input_uia(pt: POINT) -> bool {
    UIA.with(|cell| {
        let Some(uia) = &*cell.borrow() else {
            return false;
        };
        let element = match uia.ElementFromPoint(pt) {
            Ok(e) => e,
            Err(_) => return false,
        };
        let control_type = match element.CurrentControlType() {
            Ok(t) => t,
            Err(_) => return false,
        };
        if control_type == UIA_EditControlTypeId {
            log::debug!("text input: detected via UIA (Edit)");
            return true;
        }
        // ComboBox может быть редактируемым; лучше перехватить, чем пропустить
        // на распространённых случаях. Пользователь может отключить опцию.
        if control_type == UIA_ComboBoxControlTypeId {
            log::debug!("text input: detected via UIA (ComboBox)");
            return true;
        }
        false
    })
}

unsafe fn is_ancestor(ancestor: HWND, mut hwnd: HWND) -> bool {
    for _ in 0..8 {
        if hwnd.0.is_null() {
            return false;
        }
        if hwnd == ancestor {
            return true;
        }
        let Ok(parent) = GetParent(hwnd) else {
            return false;
        };
        if parent == hwnd {
            return false;
        }
        hwnd = parent;
    }
    false
}

unsafe fn point_in_hwnd(hwnd: HWND, pt: POINT) -> bool {
    let mut rc = RECT::default();
    if GetWindowRect(hwnd, &mut rc).is_err() {
        return false;
    }
    pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom
}

unsafe fn is_edit_class(hwnd: HWND) -> bool {
    let Some(class) = class_name(hwnd) else {
        return false;
    };
    // Стандартные Win32/WinForms/WPF-hosted edit controls.
    if class == "edit"
        || class.starts_with("richedit")
        || class == "richeditd2dpt"
        || class == "tmemo"
        || class == "tedit"
        || class == "tuniedit"
        || class == "scintilla"
    {
        return true;
    }
    // Некоторые WinForms-контролы.
    if class.starts_with("windowsforms") && class.contains("edit") {
        return true;
    }
    // Универсальные UWP/XAML edit-поверхности иногда экспонируют такие имена.
    if class.contains("text") && (class.contains("edit") || class.contains("input")) {
        return true;
    }
    false
}

unsafe fn class_name(hwnd: HWND) -> Option<String> {
    let mut buf = [0u16; 256];
    let len = GetClassNameW(hwnd, &mut buf);
    if len <= 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]).to_ascii_lowercase())
}



fn is_blacklisted_process(state: &AppState) -> bool {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
        UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
    };

    let list = state.settings.lock().unwrap().blacklist.clone();
    if list.is_empty() {
        return false;
    }

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return false;
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
        if pid == 0 {
            return false;
        }

        let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            Err(_) => return false,
        };

        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(handle);

        if !ok {
            return false;
        }

        let path = String::from_utf16_lossy(&buf[..size as usize]);
        let exe_name = std::path::Path::new(&path)
            .file_name()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        list.iter()
            .any(|b| b.trim().to_lowercase() == exe_name)
    }
}
