use anyhow::Result;
use std::time::Duration;
use windows::core::PCWSTR;
use windows::Win32::Foundation::GetLastError;
use windows::Win32::Media::Audio::{
    waveOutClose, waveOutOpen, waveOutPrepareHeader, waveOutReset, waveOutUnprepareHeader,
    waveOutWrite, PlaySoundW, HWAVEOUT, SND_ALIAS, SND_ASYNC, SND_FILENAME, SND_NODEFAULT,
    WAVEFORMATEX, WAVEHDR, WAVE_FORMAT_PCM, WAVE_MAPPER, WHDR_DONE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_DELETE,
};

fn key_input(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_ctrl_key(vk: VIRTUAL_KEY) -> Result<()> {
    let inputs = [
        key_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        key_input(vk, KEYBD_EVENT_FLAGS(0)),
        key_input(vk, KEYEVENTF_KEYUP),
        key_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];

    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        anyhow::bail!("SendInput sent {sent} of {} events", inputs.len());
    }
    Ok(())
}

/// Отправляет сочетание Ctrl+C в активное окно.
pub fn send_ctrl_c() -> Result<()> {
    send_ctrl_key(VIRTUAL_KEY(b'C' as u16))
}

/// Отправляет сочетание Ctrl+V в активное окно.
pub fn send_ctrl_v() -> Result<()> {
    send_ctrl_key(VIRTUAL_KEY(b'V' as u16))
}

/// Отправляет клавишу Delete в активное окно.
pub fn send_delete() -> Result<()> {
    let inputs = [
        key_input(VK_DELETE, KEYBD_EVENT_FLAGS(0)),
        key_input(VK_DELETE, KEYEVENTF_KEYUP),
    ];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        anyhow::bail!("SendInput sent {sent} of {} events", inputs.len());
    }
    Ok(())
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn play_alias(alias: &str) -> bool {
    let wide = to_wide(alias);
    let ok = unsafe {
        PlaySoundW(
            PCWSTR(wide.as_ptr()),
            None,
            SND_ALIAS | SND_ASYNC | SND_NODEFAULT,
        )
        .as_bool()
    };
    if !ok {
        let err = unsafe { GetLastError() };
        log::warn!("PlaySoundW alias '{alias}' failed: {err:?}");
    }
    ok
}

fn play_wav_file_async(path: &str) -> bool {
    let p = std::path::Path::new(path);
    if !p.is_file() {
        log::warn!("sound file not found or not a file: {path}");
        return false;
    }
    let wide = to_wide(path);
    let ok = unsafe {
        PlaySoundW(
            PCWSTR(wide.as_ptr()),
            None,
            SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
        )
        .as_bool()
    };
    if !ok {
        let err = unsafe { GetLastError() };
        log::warn!("PlaySoundW file '{path}' failed: {err:?}");
    }
    ok
}

fn parse_wav_pcm(data: &[u8]) -> Option<(WAVEFORMATEX, Vec<u8>)> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let mut fmt: Option<WAVEFORMATEX> = None;
    let mut pcm: Option<Vec<u8>> = None;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().ok()?) as usize;
        pos += 8;
        let chunk = data.get(pos..pos.saturating_add(size).min(data.len()))?;
        if id == b"fmt " && chunk.len() >= 16 {
            let tag = u16::from_le_bytes(chunk[0..2].try_into().ok()?);
            if tag != WAVE_FORMAT_PCM as u16 && tag != 1 {
                return None;
            }
            fmt = Some(WAVEFORMATEX {
                wFormatTag: 1,
                nChannels: u16::from_le_bytes(chunk[2..4].try_into().ok()?),
                nSamplesPerSec: u32::from_le_bytes(chunk[4..8].try_into().ok()?),
                nAvgBytesPerSec: u32::from_le_bytes(chunk[8..12].try_into().ok()?),
                nBlockAlign: u16::from_le_bytes(chunk[12..14].try_into().ok()?),
                wBitsPerSample: u16::from_le_bytes(chunk[14..16].try_into().ok()?),
                cbSize: 0,
            });
        } else if id == b"data" {
            pcm = Some(chunk.to_vec());
        }
        pos += size + (size % 2);
        if fmt.is_some() && pcm.is_some() {
            break;
        }
    }
    Some((fmt?, pcm?))
}

fn scale_pcm(data: &mut [u8], fmt: &WAVEFORMATEX, volume: u8) {
    if volume >= 100 {
        return;
    }
    let vol = volume as f32 / 100.0;
    match fmt.wBitsPerSample {
        16 => {
            for chunk in data.chunks_exact_mut(2) {
                let s = i16::from_le_bytes([chunk[0], chunk[1]]) as f32;
                let out = (s * vol).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
                chunk.copy_from_slice(&out.to_le_bytes());
            }
        }
        8 => {
            for b in data.iter_mut() {
                let s = (*b as i32 - 128) as f32;
                let out = (s * vol).round().clamp(-128.0, 127.0) as i32 + 128;
                *b = out as u8;
            }
        }
        _ => {}
    }
}

fn play_wav_with_volume(path: &str, volume: u8) -> bool {
    if volume >= 100 {
        return play_wav_file_async(path);
    }
    if volume == 0 {
        return true;
    }
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("read sound file failed: {path}: {e}");
            return false;
        }
    };
    let Some((fmt, mut pcm)) = parse_wav_pcm(&data) else {
        log::warn!("unsupported WAV format, fallback to PlaySoundW: {path}");
        return play_wav_file_async(path);
    };
    scale_pcm(&mut pcm, &fmt, volume);
    let path_owned = path.to_string();
    std::thread::spawn(move || unsafe {
        let mut hwave = HWAVEOUT::default();
        let mm = waveOutOpen(
            Some(&mut hwave),
            WAVE_MAPPER,
            &fmt,
            None,
            None,
            windows::Win32::Media::Audio::MIDI_WAVE_OPEN_TYPE(0),
        );
        if mm != 0 {
            log::warn!("waveOutOpen failed: {mm}");
            return;
        }
        let mut hdr = WAVEHDR {
            lpData: windows::core::PSTR(pcm.as_mut_ptr()),
            dwBufferLength: pcm.len() as u32,
            dwBytesRecorded: 0,
            dwUser: 0,
            dwFlags: 0,
            dwLoops: 0,
            lpNext: std::ptr::null_mut(),
            reserved: 0,
        };
        let prep = waveOutPrepareHeader(hwave, &mut hdr, std::mem::size_of::<WAVEHDR>() as u32);
        if prep != 0 {
            log::warn!("waveOutPrepareHeader failed: {prep}");
            let _ = waveOutClose(hwave);
            return;
        }
        let wr = waveOutWrite(hwave, &mut hdr, std::mem::size_of::<WAVEHDR>() as u32);
        if wr != 0 {
            log::warn!("waveOutWrite failed: {wr}");
            let _ = waveOutUnprepareHeader(hwave, &mut hdr, std::mem::size_of::<WAVEHDR>() as u32);
            let _ = waveOutClose(hwave);
            return;
        }
        // Ждём завершения воспроизведения, затем освобождаем ресурсы.
        for _ in 0..400 {
            if (hdr.dwFlags & WHDR_DONE) != 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        let _ = waveOutReset(hwave);
        let _ = waveOutUnprepareHeader(hwave, &mut hdr, std::mem::size_of::<WAVEHDR>() as u32);
        let _ = waveOutClose(hwave);
        let _ = path_owned;
    });
    true
}

/// Проигрывает выбранный звук копирования.
/// Если задан существующий WAV-файл, он имеет приоритет над системным звуком.
/// `volume`: 0..=100 (для системных звуков/Beep громкость не регулируется).
pub fn play_copy_sound(name: &str, file: &str, volume: u8) {
    if volume == 0 {
        return;
    }
    let file = file.trim();
    if !file.is_empty() {
        if play_wav_with_volume(file, volume) {
            log::debug!("copy sound played: file {file} volume={volume}");
            return;
        }
        log::warn!("cannot play sound file {file}, falling back to preset");
    }

    if name == "Beep" {
        beep();
        log::debug!("copy sound played: beep");
        return;
    }

    if play_alias(name) {
        log::debug!("copy sound played: alias {name}");
    } else {
        log::warn!("cannot play sound alias {name}, using beep");
        beep();
    }
}

pub fn beep() {
    unsafe {
        let _ = windows::Win32::System::Diagnostics::Debug::Beep(800, 100);
    }
}
