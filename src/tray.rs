use crate::{autostart, hotkey, send_input};
use crate::AppState;
use anyhow::{Context, Result};
use eframe::egui;
use std::sync::atomic::Ordering;
use std::sync::{mpsc::Sender, Arc};
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, TranslateMessage, MSG, WM_HOTKEY,
};

/// Запускает поток трея: иконка, меню, горячая клавиша и цикл сообщений Win32.
pub fn spawn_tray(state: Arc<AppState>, open_tx: Sender<()>) -> Result<()> {
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || {
            if let Err(e) = run_tray(state, open_tx) {
                log::error!("tray thread error: {e}");
            }
        })?;
    Ok(())
}

fn run_tray(state: Arc<AppState>, open_tx: Sender<()>) -> Result<()> {
    // Горячая клавиша регистрируется в этом потоке, поэтому сообщения WM_HOTKEY
    // приходят в его очередь, которую обслуживает цикл ниже.
    hotkey::set_main_thread_id();

    // Меню трея. Галочка показывает текущее состояние приложения.
    let toggle_item = CheckMenuItem::new(
        "MouseCopyPasteDelete",
        true,
        state.enabled.load(Ordering::SeqCst),
        None,
    );
    let settings_item = MenuItem::new("Настройки...", true, None);
    let quit_item = MenuItem::new("Выход", true, None);
    let menu = Menu::new();
    menu.append(&toggle_item)?;
    menu.append(&settings_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit_item)?;

    let tray_icon = TrayIconBuilder::new()
        .with_icon(make_icon()?)
        .with_menu(Box::new(menu))
        .with_tooltip("MouseCopyPasteDelete")
        .build()
        .context("failed to create tray icon")?;

    update_tray_state(&toggle_item, &tray_icon, &state);

    // Первичная регистрация горячей клавиши.
    let initial_hotkey = state.settings.lock().unwrap().toggle_hotkey.clone();
    if !hotkey::register_toggle_hotkey(&initial_hotkey) {
        log::error!("hotkey registration failed (maybe conflict)");
    }

    // Слушатель событий меню. MenuItem не является Send,
    // поэтому в поток передаём только строковые идентификаторы.
    let toggle_id = toggle_item.id().0.clone();
    let settings_id = settings_item.id().0.clone();
    let quit_id = quit_item.id().0.clone();
    let menu_state = state.clone();
    let menu_tx = open_tx.clone();
    std::thread::Builder::new()
        .name("tray-menu-events".into())
        .spawn(move || {
            while let Ok(event) = MenuEvent::receiver().recv() {
                if event.id.0 == toggle_id {
                    toggle_enabled(&menu_state);
                    // Галочку и подсказку обновляет поток трея.
                    hotkey::request_tray_refresh();
                } else if event.id.0 == settings_id {
                    // Окно настроек откроет главный поток (winit требует главный поток).
                    if menu_tx.send(()).is_err() {
                        break;
                    }
                } else if event.id.0 == quit_id {
                    log::info!("quit requested from tray");
                    std::process::exit(0);
                }
            }
        })?;

    // Цикл сообщений потока трея: трей, горячая клавиша, перерегистрация.
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).into() {
            if msg.message == hotkey::WM_REFRESH_TRAY {
                update_tray_state(&toggle_item, &tray_icon, &state);
                continue;
            }

            if msg.message == hotkey::WM_REREGISTER_HOTKEY {
                let new_hotkey = state.settings.lock().unwrap().toggle_hotkey.clone();
                hotkey::unregister_hotkey();
                if !hotkey::register_toggle_hotkey(&new_hotkey) {
                    log::error!("hotkey re-register failed");
                }
                continue;
            }

            if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == hotkey::HOTKEY_ID {
                toggle_enabled(&state);
                update_tray_state(&toggle_item, &tray_icon, &state);
                continue;
            }

            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

fn update_tray_state(toggle_item: &CheckMenuItem, tray_icon: &TrayIcon, state: &AppState) {
    let enabled = state.enabled.load(Ordering::SeqCst);
    toggle_item.set_checked(enabled);

    let paste_double = state
        .settings
        .lock()
        .map(|s| s.paste_on_double_click)
        .unwrap_or(false);

    let mut tooltip = format!(
        "MouseCopyPasteDelete — авто-копирование {}",
        if enabled { "включено" } else { "выключено" }
    );
    if paste_double {
        tooltip.push_str("; двойной клик — вставка");
    }
    if let Err(e) = tray_icon.set_tooltip(Some(tooltip.as_str())) {
        log::error!("failed to update tray tooltip: {e}");
    }
}

fn toggle_enabled(state: &AppState) {
    let was_enabled = state.enabled.load(Ordering::SeqCst);
    let now_enabled = !was_enabled;
    state.enabled.store(now_enabled, Ordering::SeqCst);

    // Сохраняем состояние, чтобы оно пережило перезапуск.
    if let Ok(mut s) = state.settings.lock() {
        s.enabled = now_enabled;
        if let Err(e) = s.save() {
            log::error!("failed to save enabled state: {e}");
        }
    }

    log::info!(
        "autocopy toggled: {}",
        if now_enabled { "ON" } else { "OFF" }
    );
}

pub fn open_settings_window(state: Arc<AppState>) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MouseCopyPasteDelete — Настройки")
            .with_inner_size([524.0, 429.0])
            .with_min_inner_size([420.0, 220.0]),
        ..Default::default()
    };

    let result = eframe::run_native(
        "MouseCopyPasteDelete",
        options,
        Box::new(move |_cc| Ok(Box::new(SettingsApp::new(state)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"));
    log::info!("settings window closed");
    result
}

/// Иконка трея из SVG (`assets/copy-06.svg`), рендерится в RGBA.
fn make_icon() -> Result<Icon> {
    const SIZE: u32 = 32;
    let svg = include_str!("../assets/copy-06.svg").replace("currentColor", "#2563EB");
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_str(&svg, &opt)
        .map_err(|e| anyhow::anyhow!("failed to parse tray icon svg: {e}"))?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(SIZE, SIZE)
        .ok_or_else(|| anyhow::anyhow!("failed to create tray icon pixmap"))?;
    let svg_size = tree.size();
    let transform = resvg::tiny_skia::Transform::from_scale(
        SIZE as f32 / svg_size.width(),
        SIZE as f32 / svg_size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Icon::from_rgba(pixmap.take(), SIZE, SIZE).context("failed to build tray icon")
}

struct SettingsApp {
    state: Arc<AppState>,
    enabled: bool,
    copy_on_select: bool,
    threshold: u32,
    delay: u64,
    blacklist_text: String,
    hotkey: String,
    capturing_hotkey: bool,
    sound: bool,
    sound_name: String,
    sound_file: String,
    sound_volume: u8,
    paste_double_click: bool,
    delete_gesture: bool,
    autostart: bool,
    show_copy_params: bool,
    status: String,
    status_is_error: bool,
}

impl SettingsApp {
    fn new(state: Arc<AppState>) -> Self {
        log::info!("settings window opened");
        let s = state.settings.lock().unwrap().clone();
        Self {
            enabled: state.enabled.load(Ordering::SeqCst),
            copy_on_select: s.copy_on_select,
            state,
            threshold: s.move_threshold,
            delay: s.copy_delay_ms,
            blacklist_text: s.blacklist.join("\n"),
            hotkey: s.toggle_hotkey,
            capturing_hotkey: false,
            sound: s.sound_feedback,
            sound_name: s.sound_name,
            sound_file: s.sound_file,
            sound_volume: s.sound_volume,
            paste_double_click: s.paste_on_double_click,
            delete_gesture: s.delete_on_lmb_rmb,
            autostart: s.autostart,
            show_copy_params: false,
            status: String::new(),
            status_is_error: false,
        }
    }

    fn auto_save(&mut self, hotkey_changed: bool, autostart_changed: bool, prev_autostart: bool) {
        let new_hotkey = self.hotkey.trim().to_string();

        {
            let mut s = self.state.settings.lock().unwrap();
            s.enabled = self.enabled;
            s.copy_on_select = self.copy_on_select;
            s.move_threshold = self.threshold;
            s.copy_delay_ms = self.delay;
            s.blacklist = self
                .blacklist_text
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            s.toggle_hotkey = new_hotkey;
            s.sound_feedback = self.sound;
            s.sound_name = self.sound_name.clone();
            s.sound_file = self.sound_file.trim().to_string();
            s.sound_volume = self.sound_volume;
            s.paste_on_double_click = self.paste_double_click;
            s.delete_on_lmb_rmb = self.delete_gesture;
            s.autostart = self.autostart;

            match s.save() {
                Ok(()) => {
                    self.status.clear();
                    self.status_is_error = false;
                }
                Err(e) => {
                    log::error!("save settings failed: {e}");
                    self.status = format!("Ошибка сохранения: {e}");
                    self.status_is_error = true;
                }
            }
        }

        self.state.enabled.store(self.enabled, Ordering::SeqCst);
        hotkey::request_tray_refresh();
        if hotkey_changed {
            hotkey::request_reregister();
        }
        if autostart_changed {
            if self.autostart {
                if let Err(e) = autostart::enable() {
                    log::error!("autostart enable failed: {e}");
                    self.status = format!("Ошибка автозапуска: {e}");
                    self.status_is_error = true;
                }
            } else if let Err(e) = autostart::disable() {
                log::error!("autostart disable failed: {e}");
                self.status = format!("Ошибка автозапуска: {e}");
                self.status_is_error = true;
                let _ = prev_autostart;
            }
        }
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Подхватываем внешние изменения (горячая клавиша/трей) без автосохранения.
        let external_enabled = self.state.enabled.load(Ordering::SeqCst);
        if self.enabled != external_enabled {
            self.enabled = external_enabled;
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));

        let mut changed = false;
        let mut hotkey_changed = false;
        let prev_autostart = self.autostart;

        if self.capturing_hotkey {
            if let Some(captured) = capture_hotkey(ui.ctx()) {
                match captured {
                    HotkeyCapture::Cancelled => {
                        self.capturing_hotkey = false;
                    }
                    HotkeyCapture::Hotkey(hk) => {
                        if hk != self.hotkey {
                            self.hotkey = hk;
                            hotkey_changed = true;
                            changed = true;
                        }
                        self.capturing_hotkey = false;
                    }
                }
            }
        }

        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Шапка: название слева, тумблер вкл/выкл справа.
                    ui.horizontal(|ui| {
                        ui.heading("MouseCopyPasteDelete");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let resp = toggle_ui(ui, &mut self.enabled);
                            if resp.changed() {
                                changed = true;
                            }
                            resp.on_hover_text(if self.enabled {
                                "Выключить MouseCopyPasteDelete"
                            } else {
                                "Включить MouseCopyPasteDelete"
                            });
                        });
                    });
                    ui.separator();
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        ui.label("Горячая клавиша вкл/выкл:");
                        let hk_text = if self.capturing_hotkey {
                            "Нажмите клавиши… (Esc — отмена)".to_string()
                        } else {
                            self.hotkey.clone()
                        };
                        if ui
                            .button(hk_text)
                            .on_hover_text("Нажмите, затем нажмите нужную комбинацию")
                            .clicked()
                        {
                            self.capturing_hotkey = !self.capturing_hotkey;
                        }
                    });

                    ui.add_space(8.0);

                    // Copy + шестерёнка параметров справа.
                    ui.horizontal(|ui| {
                        changed |= ui
                            .checkbox(
                                &mut self.copy_on_select,
                                "Copy - выделить текст и он попадёт в буфер обмена",
                            )
                            .changed();
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon_button_gear(ui, self.show_copy_params)
                                .on_hover_text("Параметры копирования")
                                .clicked()
                            {
                                self.show_copy_params = !self.show_copy_params;
                            }
                        });
                    });
                    if self.show_copy_params {
                        ui.indent("copy_params_indent", |ui| {
                            egui::Grid::new("copy_params")
                                .num_columns(2)
                                .spacing([12.0, 8.0])
                                .show(ui, |ui| {
                                    ui.label("Порог движения (px):");
                                    changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut self.threshold)
                                                .range(1..=100),
                                        )
                                        .changed();
                                    ui.end_row();

                                    ui.label("Задержка перед Ctrl+C (мс):");
                                    changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut self.delay).range(0..=2000),
                                        )
                                        .changed();
                                    ui.end_row();
                                });
                        });
                    }

                    changed |= ui
                        .checkbox(
                            &mut self.paste_double_click,
                            "Paste - двойным кликом на активном поле ввода",
                        )
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut self.delete_gesture,
                            "Delete - выделить текст и не отпуская левую кнопку мыши нажать правую",
                        )
                        .changed();

                    ui.add_space(8.0);
                    changed |= ui.checkbox(&mut self.sound, "Звук после копирования").changed();
                    if self.sound {
                        let file_name = std::path::Path::new(self.sound_file.trim())
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                            .to_string();

                        ui.horizontal(|ui| {
                            let wav_label = if file_name.is_empty() {
                                "Выбрать WAV..."
                            } else {
                                file_name.as_str()
                            };
                            let mut wav_btn = ui.button(wav_label);
                            if !file_name.is_empty() {
                                wav_btn = wav_btn.on_hover_text(self.sound_file.trim());
                            }
                            if wav_btn.clicked() {
                                if let Some(path) = rfd::FileDialog::new()
                                    .add_filter("WAV audio", &["wav", "WAV"])
                                    .set_title("Выберите звук копирования")
                                    .pick_file()
                                {
                                    self.sound_file = path.display().to_string();
                                    changed = true;
                                }
                            }

                            if icon_button_play(ui).on_hover_text("Прослушать").clicked() {
                                send_input::play_copy_sound(
                                    &self.sound_name,
                                    &self.sound_file,
                                    self.sound_volume,
                                );
                            }
                            if !self.sound_file.trim().is_empty()
                                && icon_button_trash(ui)
                                    .on_hover_text("Удалить файл из настроек")
                                    .clicked()
                            {
                                self.sound_file.clear();
                                changed = true;
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label("Громкость");
                            if ui
                                .add(
                                    egui::Slider::new(&mut self.sound_volume, 0u8..=100)
                                        .show_value(true),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        });
                    }

                    ui.add_space(6.0);
                    changed |= ui
                        .checkbox(&mut self.autostart, "Автозапуск с Windows")
                        .changed();
                    ui.add_space(6.0);

                    ui.label("Чёрный список процессов (по одному на строку, имя.exe):");
                    let blacklist_resp = ui.add(
                        egui::TextEdit::multiline(&mut self.blacklist_text)
                            .desired_rows(5)
                            .desired_width(f32::INFINITY)
                            .hint_text("например:\nbank.exe\nsomegame.exe"),
                    );
                    // Автосохранение при изменении текста.
                    if blacklist_resp.changed() {
                        changed = true;
                    }

                    if !self.status.is_empty() {
                        ui.add_space(8.0);
                        let color = if self.status_is_error {
                            egui::Color32::from_rgb(200, 60, 60)
                        } else {
                            egui::Color32::from_rgb(40, 140, 60)
                        };
                        ui.colored_label(color, &self.status);
                    }
                });
        });

        if changed {
            let autostart_changed = prev_autostart != self.autostart;
            self.auto_save(hotkey_changed, autostart_changed, prev_autostart);
        }
    }
}

fn icon_button_play(ui: &mut egui::Ui) -> egui::Response {
    let size = egui::vec2(26.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let visuals = ui.style().interact(&response);
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 4.0, visuals.bg_fill);
        ui.painter().rect_stroke(rect, 4.0, visuals.bg_stroke, egui::StrokeKind::Inside);
        let c = rect.center();
        let s = 6.0;
        let points = vec![
            egui::pos2(c.x - s * 0.55, c.y - s),
            egui::pos2(c.x - s * 0.55, c.y + s),
            egui::pos2(c.x + s * 0.9, c.y),
        ];
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            visuals.fg_stroke.color,
            egui::Stroke::NONE,
        ));
    }
    response
}

fn icon_button_trash(ui: &mut egui::Ui) -> egui::Response {
    let size = egui::vec2(26.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let visuals = ui.style().interact(&response);
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 4.0, visuals.bg_fill);
        ui.painter().rect_stroke(rect, 4.0, visuals.bg_stroke, egui::StrokeKind::Inside);
        let c = rect.center();
        let color = visuals.fg_stroke.color;
        let stroke = egui::Stroke::new(1.6, color);
        // lid
        ui.painter().line_segment(
            [egui::pos2(c.x - 6.0, c.y - 5.0), egui::pos2(c.x + 6.0, c.y - 5.0)],
            stroke,
        );
        // handle
        ui.painter().line_segment(
            [egui::pos2(c.x - 2.0, c.y - 7.0), egui::pos2(c.x + 2.0, c.y - 7.0)],
            stroke,
        );
        ui.painter().line_segment(
            [egui::pos2(c.x - 2.0, c.y - 7.0), egui::pos2(c.x - 2.0, c.y - 5.0)],
            stroke,
        );
        ui.painter().line_segment(
            [egui::pos2(c.x + 2.0, c.y - 7.0), egui::pos2(c.x + 2.0, c.y - 5.0)],
            stroke,
        );
        // can
        let can = egui::Rect::from_min_max(
            egui::pos2(c.x - 5.0, c.y - 3.5),
            egui::pos2(c.x + 5.0, c.y + 7.0),
        );
        ui.painter().rect_stroke(can, 1.0, stroke, egui::StrokeKind::Inside);
        for dx in [-2.0f32, 0.0, 2.0] {
            ui.painter().line_segment(
                [egui::pos2(c.x + dx, c.y - 1.5), egui::pos2(c.x + dx, c.y + 5.0)],
                stroke,
            );
        }
    }
    response
}

fn icon_button_gear(ui: &mut egui::Ui, active: bool) -> egui::Response {
    let size = egui::vec2(26.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let visuals = ui.style().interact_selectable(&response, active);
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 4.0, visuals.bg_fill);
        ui.painter().rect_stroke(rect, 4.0, visuals.bg_stroke, egui::StrokeKind::Inside);
        let c = rect.center();
        let color = visuals.fg_stroke.color;
        let stroke = egui::Stroke::new(1.5, color);
        let r_outer = 7.0_f32;
        let r_inner = 4.2_f32;
        let r_hole = 1.8_f32;
        // teeth
        for i in 0..8 {
            let a = i as f32 * std::f32::consts::TAU / 8.0;
            let (sa, ca) = a.sin_cos();
            let (sa2, ca2) = (a + std::f32::consts::TAU / 16.0).sin_cos();
            let p1 = egui::pos2(c.x + ca * r_inner, c.y + sa * r_inner);
            let p2 = egui::pos2(c.x + ca * (r_outer + 0.8), c.y + sa * (r_outer + 0.8));
            let p3 = egui::pos2(c.x + ca2 * (r_outer + 0.8), c.y + sa2 * (r_outer + 0.8));
            let p4 = egui::pos2(c.x + ca2 * r_inner, c.y + sa2 * r_inner);
            ui.painter().add(egui::Shape::convex_polygon(
                vec![p1, p2, p3, p4],
                color,
                egui::Stroke::NONE,
            ));
        }
        ui.painter().circle_stroke(c, r_inner, stroke);
        ui.painter().circle_filled(c, r_hole, ui.visuals().panel_fill);
        ui.painter().circle_stroke(c, r_hole, stroke);
    }
    response
}

/// Простой тумблер (вкл/выкл).
fn toggle_ui(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired = egui::vec2(38.0, 20.0);
    let (rect, mut response) = ui.allocate_exact_size(desired, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }

    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let radius = rect.height() / 2.0;
        let visuals = ui.style().interact_selectable(&response, *on);
        let bg = egui::lerp(
            egui::Rgba::from(ui.visuals().widgets.inactive.bg_fill)..=egui::Rgba::from(
                egui::Color32::from_rgb(60, 160, 90),
            ),
            how_on,
        );
        ui.painter().rect_filled(rect, radius, bg);
        ui.painter().rect_stroke(
            rect,
            radius,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let knob_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        let knob_center = egui::pos2(knob_x, rect.center().y);
        ui.painter()
            .circle_filled(knob_center, radius - 2.5, visuals.fg_stroke.color);
    }

    response
}

enum HotkeyCapture {
    Cancelled,
    Hotkey(String),
}

fn capture_hotkey(ctx: &egui::Context) -> Option<HotkeyCapture> {
    ctx.input(|i| {
        for event in &i.events {
            if let egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            {
                if *key == egui::Key::Escape {
                    return Some(HotkeyCapture::Cancelled);
                }
                if is_modifier_key(*key) {
                    continue;
                }
                let mut parts = Vec::new();
                if modifiers.ctrl {
                    parts.push("Ctrl".to_string());
                }
                if modifiers.alt {
                    parts.push("Alt".to_string());
                }
                if modifiers.shift {
                    parts.push("Shift".to_string());
                }
                // egui не предоставляет Win-модификатор надёжно на всех платформах;
                // Win-комбинации можно задать ранее сохранённой строкой в settings.json.
                if modifiers.command && !modifiers.ctrl {
                    // На Windows command обычно совпадает с Ctrl; избегаем дублирования.
                }
                let has_modifier = !parts.is_empty();
                if !has_modifier {
                    continue;
                }
                if let Some(name) = key_name(*key) {
                    parts.push(name);
                    return Some(HotkeyCapture::Hotkey(parts.join("+")));
                }
            }
        }
        None
    })
}

fn is_modifier_key(_key: egui::Key) -> bool {
    // В egui модификаторы приходят отдельно в `modifiers`; здесь отсекаем
    // только случаи, которые не должны становиться основной клавишей.
    false
}

fn key_name(key: egui::Key) -> Option<String> {
    use egui::Key;
    Some(
        match key {
            Key::A => "A",
            Key::B => "B",
            Key::C => "C",
            Key::D => "D",
            Key::E => "E",
            Key::F => "F",
            Key::G => "G",
            Key::H => "H",
            Key::I => "I",
            Key::J => "J",
            Key::K => "K",
            Key::L => "L",
            Key::M => "M",
            Key::N => "N",
            Key::O => "O",
            Key::P => "P",
            Key::Q => "Q",
            Key::R => "R",
            Key::S => "S",
            Key::T => "T",
            Key::U => "U",
            Key::V => "V",
            Key::W => "W",
            Key::X => "X",
            Key::Y => "Y",
            Key::Z => "Z",
            Key::Num0 => "0",
            Key::Num1 => "1",
            Key::Num2 => "2",
            Key::Num3 => "3",
            Key::Num4 => "4",
            Key::Num5 => "5",
            Key::Num6 => "6",
            Key::Num7 => "7",
            Key::Num8 => "8",
            Key::Num9 => "9",
            Key::F1 => "F1",
            Key::F2 => "F2",
            Key::F3 => "F3",
            Key::F4 => "F4",
            Key::F5 => "F5",
            Key::F6 => "F6",
            Key::F7 => "F7",
            Key::F8 => "F8",
            Key::F9 => "F9",
            Key::F10 => "F10",
            Key::F11 => "F11",
            Key::F12 => "F12",
            Key::Space => "SPACE",
            Key::Tab => "TAB",
            Key::Enter => "ENTER",
            Key::Insert => "INSERT",
            Key::Delete => "DELETE",
            Key::Home => "HOME",
            Key::End => "END",
            Key::PageUp => "PAGEUP",
            Key::PageDown => "PAGEDOWN",
            Key::Minus => "-",
            Key::Plus | Key::Equals => "+",
            Key::Period => ".",
            Key::Comma => ",",
            _ => return None,
        }
        .to_string(),
    )
}

