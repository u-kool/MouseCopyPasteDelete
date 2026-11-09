use anyhow::Result;
use directories::ProjectDirs;
use simplelog::{Config, LevelFilter, WriteLogger};
use std::fs::{File, OpenOptions};
use std::path::PathBuf;

const MAX_LOG_SIZE: u64 = 2 * 1024 * 1024;

pub fn init() -> Result<()> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("app.log"));
        }
    }
    if let Some(dirs) = ProjectDirs::from("com", "autoclipx", "autoclipx-rs") {
        candidates.push(dirs.data_local_dir().join("app.log"));
    }
    candidates.push(std::env::temp_dir().join("autoclipx-rs.log"));

    let mut last_err = None;
    for path in candidates {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let file = match open_log_file(&path) {
            Ok(f) => f,
            Err(e) => {
                last_err = Some(anyhow::Error::from(e).context(format!("open {path:?}")));
                continue;
            }
        };

        WriteLogger::init(LevelFilter::Debug, Config::default(), file)?;
        log::info!("log file: {path:?}");
        return Ok(());
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no writable log path found")))
}

/// Дописывает лог, но пересоздаёт файл, если он превысил 2 МБ.
fn open_log_file(path: &PathBuf) -> std::io::Result<File> {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > MAX_LOG_SIZE {
            return File::create(path);
        }
    }
    OpenOptions::new().create(true).append(true).open(path)
}
