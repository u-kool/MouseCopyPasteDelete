#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod clipboard;
mod hooks;
mod hotkey;
mod logger;
mod send_input;
mod settings;
mod tray;

use anyhow::Result;
use single_instance::SingleInstance;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

#[derive(Default, Clone, Copy)]
pub struct ClickTracker {
    pub last_down_time: u32,
    pub last_down_pt: (i32, i32),
}

/// Глобальное состояние приложения.
pub struct AppState {
    pub enabled: AtomicBool,
    pub settings: Mutex<settings::Settings>,
    pub last_mouse_pos: Mutex<(i32, i32)>,
    pub dragging: AtomicBool,
    pub drag_rbutton_delete: AtomicBool,
    pub last_lbutton_down: Mutex<ClickTracker>,
    pub settings_window_open: AtomicBool,
    /// Последний текст, успешно скопированный утилитой (для защиты от повторного копирования).
    pub last_copied_text: Mutex<String>,
}

fn main() -> Result<()> {
    if let Err(e) = logger::init() {
        eprintln!("failed to init logger: {e}");
    }
    log::info!("mousecopypastedelete starting");

    // Один экземпляр приложения.
    let instance = SingleInstance::new("mousecopypastedelete-mutex")?;
    if !instance.is_single() {
        log::warn!("another instance is already running");
        return Ok(());
    }

    // Загрузка настроек.
    let settings = settings::Settings::load().unwrap_or_else(|e| {
        log::error!("failed to load settings: {e}; using defaults");
        settings::Settings::default()
    });
    let enabled = settings.enabled;
    let autostart_enabled = settings.autostart;

    let state = Arc::new(AppState {
        enabled: AtomicBool::new(enabled),
        settings: Mutex::new(settings),
        last_mouse_pos: Mutex::new((0, 0)),
        dragging: AtomicBool::new(false),
        drag_rbutton_delete: AtomicBool::new(false),
        last_lbutton_down: Mutex::new(ClickTracker::default()),
        settings_window_open: AtomicBool::new(false),
        last_copied_text: Mutex::new(String::new()),
    });

    // Синхронизация автозапуска с сохранённой настройкой.
    if autostart_enabled {
        if let Err(e) = autostart::enable() {
            log::error!("autostart enable failed: {e}");
        }
    } else if let Err(e) = autostart::disable() {
        log::error!("autostart disable failed: {e}");
    }

    // Поток низкоуровневого хука мыши (со своим циклом сообщений).
    let hook_state = state.clone();
    std::thread::Builder::new()
        .name("mouse-hook".into())
        .spawn(move || {
            if let Err(e) = hooks::run_mouse_hook_thread(hook_state) {
                log::error!("mouse hook thread error: {e}");
            }
        })?;

    // Трей, горячая клавиша и их цикл сообщений — в отдельном потоке.
    let (open_tx, open_rx) = mpsc::channel::<()>();
    tray::spawn_tray(state.clone(), open_tx.clone())?;

    // Тестовая возможность: сразу открыть окно настроек (для отладки).
    if std::env::var_os("MOUSECOPYPASTEDELETE_OPEN_SETTINGS").is_some()
        || std::env::var_os("AUTOCOPYPASTEDELETE_OPEN_SETTINGS").is_some()
        || std::env::var_os("AUTOCOPYPASTE_OPEN_SETTINGS").is_some()
        || std::env::var_os("AUTOCLIPX_OPEN_SETTINGS").is_some()
    {
        let _ = open_tx.send(());
    }

    // Главный поток обслуживает окно настроек (winit требует главный поток).
    while open_rx.recv().is_ok() {
        if state.settings_window_open.swap(true, Ordering::SeqCst) {
            continue;
        }

        log::info!("opening settings window");
        if let Err(e) = tray::open_settings_window(state.clone()) {
            log::error!("settings window error: {e}");
        }
        state.settings_window_open.store(false, Ordering::SeqCst);

        // Запросы, накопившиеся пока окно было открыто, не открываем повторно.
        while open_rx.try_recv().is_ok() {}
    }

    log::info!("mousecopypastedelete exiting");
    drop(instance);
    Ok(())
}
