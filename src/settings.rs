use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Включено ли приложение/авто-копирование (мастер-переключатель).
    pub enabled: bool,
    /// Авто-копирование выделенного текста по отпусканию ЛКМ.
    pub copy_on_select: bool,
    pub move_threshold: u32,
    pub copy_delay_ms: u64,
    pub blacklist: Vec<String>,
    pub toggle_hotkey: String,
    pub sound_feedback: bool,
    /// Имя системного звука (SystemAsterisk и т. п.) или "Beep".
    pub sound_name: String,
    /// Необязательный путь к собственному WAV-файлу.
    pub sound_file: String,
    /// Громкость звука, 0..=100.
    pub sound_volume: u8,
    /// Вставка по двойному клику на активном поле ввода.
    #[serde(alias = "paste_on_middle_click")]
    pub paste_on_double_click: bool,
    /// Удаление выделения жестом: удерживать ЛКМ + нажать ПКМ.
    pub delete_on_lmb_rmb: bool,
    pub autostart: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            copy_on_select: true,
            move_threshold: 5,
            copy_delay_ms: 80,
            blacklist: default_blacklist(),
            toggle_hotkey: "Ctrl+Alt+C".into(),
            sound_feedback: false,
            sound_name: "SystemAsterisk".into(),
            sound_file: String::new(),
            sound_volume: 100,
            paste_on_double_click: true,
            delete_on_lmb_rmb: true,
            autostart: false,
        }
    }
}

fn default_blacklist() -> Vec<String> {
    [
        "SnippingTool.exe",
        "ScreenClippingHost.exe",
        "Skrinshoter.exe",
        "ShareX.exe",
        "Greenshot.exe",
        "Lightshot.exe",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

impl Settings {
    pub fn config_path() -> PathBuf {
        ProjectDirs::from("com", "autoclipx", "autoclipx-rs")
            .map(|dirs| dirs.config_dir().join("settings.json"))
            .unwrap_or_else(|| PathBuf::from("settings.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("read settings from {path:?}"))?;
        let mut s: Self = serde_json::from_str(&data)
            .with_context(|| format!("parse settings from {path:?}"))?;
        s.ensure_default_blacklist();
        Ok(s)
    }

    fn ensure_default_blacklist(&mut self) {
        for item in default_blacklist() {
            let present = self
                .blacklist
                .iter()
                .any(|b| b.trim().eq_ignore_ascii_case(&item));
            if !present {
                self.blacklist.push(item);
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)?;
        fs::write(&path, data).with_context(|| format!("write settings to {path:?}"))?;
        log::info!("settings saved to {path:?}");
        Ok(())
    }
}
