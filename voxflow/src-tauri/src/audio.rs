//! Захват и обработка аудио для VoxFlow.
//!
//! Модуль отвечает за:
//! - перечисление входных устройств (`list_input_devices`);
//! - запуск захвата с микрофона в моно f32 (`start_capture` + [`Capture`]);
//! - ресэмплинг в 16 кГц (`resample_to_16k`);
//! - обрезку тишины по краям (`trim_silence`);
//! - запись WAV 16 кГц / моно / 16 бит (`write_wav_16k_mono`).
//!
//! Важно: cpal `Stream` — `!Send`. Поэтому [`Capture`] создаётся и
//! потребляется на одном и том же рабочем потоке, без `Send`/`Sync`-границ
//! и без перемещения между потоками.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

/// Возвращает имена всех входных устройств (без дубликатов).
///
/// Best-effort: любые ошибки проглатываются, возвращается то, что удалось
/// собрать.
#[allow(deprecated)] // cpal 0.17: name() помечен deprecated, но имя нужно для выбора устройства
pub fn list_input_devices() -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let host = cpal::default_host();
    if let Ok(devices) = host.input_devices() {
        for device in devices {
            if let Ok(name) = device.name() {
                if seen.insert(name.clone()) {
                    names.push(name);
                }
            }
        }
    }

    names
}

/// Активный захват с микрофона.
///
/// Хранит живой cpal-поток (`!Send`), общий буфер моно-сэмплов и нативную
/// частоту дискретизации. Поток останавливается при `Drop`.
pub struct Capture {
    /// Живой входной поток. Держим его, пока идёт запись.
    stream: cpal::Stream,
    /// Общий буфер моно f32, в который пишет data-callback.
    buffer: Arc<Mutex<Vec<f32>>>,
    /// Нативная частота дискретизации устройства, Гц.
    sample_rate: u32,
}

impl Capture {
    /// Нативная частота дискретизации устройства, Гц.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Клон Arc на общий буфер сэмплов — чтобы фоновый поток мог СНИМАТЬ
    /// накопленные сэмплы (read-only) БЕЗ доступа к !Send cpal-потоку.
    /// Буфер НЕ сливается (`finish()` на потоке движка нужен полный буфер).
    pub fn buffer_handle(&self) -> Arc<Mutex<Vec<f32>>> {
        Arc::clone(&self.buffer)
    }

    /// Останавливает захват (дропает поток) и возвращает накопленные
    /// моно-сэмплы.
    pub fn finish(self) -> Vec<f32> {
        // Явный drop останавливает аудиопоток до того, как мы заберём буфер.
        drop(self.stream);
        let mut guard = self.buffer.lock().expect("audio buffer mutex poisoned");
        std::mem::take(&mut *guard)
    }
}

/// Запускает захват с указанного устройства.
///
/// Пустая строка `device_name` => устройство по умолчанию хоста.
/// Data-callback сводит каждый кадр в один моно f32 (среднее по каналам)
/// и дописывает его в общий буфер.
#[allow(deprecated)] // cpal 0.17: name() помечен deprecated, но выбор устройства идёт по имени
pub fn start_capture(device_name: &str) -> Result<Capture> {
    let host = cpal::default_host();

    let device = if device_name.is_empty() {
        host.default_input_device()
            .ok_or_else(|| anyhow!("нет входного устройства по умолчанию"))?
    } else {
        host.input_devices()
            .context("не удалось перечислить входные устройства")?
            .find(|d| d.name().map(|n| n == device_name).unwrap_or(false))
            .ok_or_else(|| anyhow!("входное устройство не найдено: {device_name}"))?
    };

    let supported = device
        .default_input_config()
        .context("не удалось получить конфигурацию входа по умолчанию")?;

    let sample_format = supported.sample_format();
    let sample_rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let config = supported.config();

    let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));

    let err_fn = |err| log::error!("ошибка аудиопотока: {err}");

    // Строим поток под конкретный формат сэмплов; каждый кадр (group из
    // `channels` сэмплов) усредняем в один моно f32.
    let stream = match sample_format {
        SampleFormat::F32 => {
            let buf = Arc::clone(&buffer);
            device.build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    push_mono(&buf, data, channels, |s| s);
                },
                err_fn,
                None,
            )
        }
        SampleFormat::I16 => {
            let buf = Arc::clone(&buffer);
            device.build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    push_mono(&buf, data, channels, |s| s as f32 / 32768.0);
                },
                err_fn,
                None,
            )
        }
        SampleFormat::U16 => {
            let buf = Arc::clone(&buffer);
            device.build_input_stream(
                &config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    push_mono(&buf, data, channels, |s| (s as f32 - 32768.0) / 32768.0);
                },
                err_fn,
                None,
            )
        }
        other => return Err(anyhow!("неподдерживаемый формат сэмплов: {other:?}")),
    }
    .context("не удалось построить входной поток")?;

    stream.play().context("не удалось запустить аудиопоток")?;

    Ok(Capture {
        stream,
        buffer,
        sample_rate,
    })
}

/// Сводит чередующиеся (interleaved) сэмплы в моно и дописывает в буфер.
///
/// `to_f32` приводит исходный тип сэмпла к f32 в диапазоне -1.0..=1.0;
/// затем кадр из `channels` сэмплов усредняется в один моно-сэмпл.
fn push_mono<T: Copy>(
    buffer: &Arc<Mutex<Vec<f32>>>,
    data: &[T],
    channels: usize,
    to_f32: impl Fn(T) -> f32,
) {
    let channels = channels.max(1);
    let mut guard = match buffer.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    guard.reserve(data.len() / channels + 1);
    for frame in data.chunks(channels) {
        let sum: f32 = frame.iter().map(|&s| to_f32(s)).sum();
        guard.push(sum / frame.len() as f32);
    }
}

/// Ресэмплит моно f32 в 16000 Гц.
///
/// Если вход уже 16 кГц — копия без изменений. Иначе: лёгкий антиалиасинговый
/// низкочастотный фильтр (скользящее среднее с окном
/// `max(1, round(in_rate/16000))`), затем линейная интерполяция на сетку
/// 16000 Гц. Чистый Rust, без новых крейтов.
pub fn resample_to_16k(samples: &[f32], in_rate: u32) -> Vec<f32> {
    const OUT_RATE: u32 = 16000;

    if in_rate == OUT_RATE {
        return samples.to_vec();
    }
    if samples.is_empty() || in_rate == 0 {
        return Vec::new();
    }

    // 1. Лёгкий low-pass: скользящее среднее.
    let window = ((in_rate as f32 / OUT_RATE as f32).round() as usize).max(1);
    let filtered = moving_average(samples, window);

    // 2. Линейная интерполяция на сетку 16 кГц.
    let ratio = in_rate as f64 / OUT_RATE as f64;
    let out_len = ((filtered.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);

    let last = filtered.len() - 1;
    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos.floor() as usize;
        if idx >= last {
            out.push(filtered[last]);
        } else {
            let frac = (src_pos - idx as f64) as f32;
            let a = filtered[idx];
            let b = filtered[idx + 1];
            out.push(a + (b - a) * frac);
        }
    }

    out
}

/// Скользящее среднее с окном `window` (центрированное, по краям усекается).
fn moving_average(samples: &[f32], window: usize) -> Vec<f32> {
    if window <= 1 {
        return samples.to_vec();
    }
    let n = samples.len();
    let half = window / 2;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let start = i.saturating_sub(half);
        let end = (i + half + 1).min(n);
        let slice = &samples[start..end];
        let sum: f32 = slice.iter().copied().sum();
        out.push(sum / slice.len() as f32);
    }
    out
}

/// Обрезает тишину по краям по RMS-энергии (простой VAD).
///
/// Кадр ~20 мс (`rate/50` сэмплов). Находим первый и последний кадр с
/// RMS >= 0.01, оставляем поля ~150 мс с каждой стороны. Если ни один кадр
/// не прошёл порог — возвращаем вход без изменений.
pub fn trim_silence(samples: &[f32], rate: u32) -> Vec<f32> {
    const THRESHOLD: f32 = 0.01;

    if samples.is_empty() || rate == 0 {
        return samples.to_vec();
    }

    let frame = (rate as usize / 50).max(1);
    let num_frames = samples.len() / frame;
    if num_frames == 0 {
        return samples.to_vec();
    }

    let mut first: Option<usize> = None;
    let mut last: Option<usize> = None;

    for f in 0..num_frames {
        let start = f * frame;
        let slice = &samples[start..start + frame];
        let sum_sq: f32 = slice.iter().map(|&s| s * s).sum();
        let rms = (sum_sq / frame as f32).sqrt();
        if rms >= THRESHOLD {
            if first.is_none() {
                first = Some(f);
            }
            last = Some(f);
        }
    }

    let (first, last) = match (first, last) {
        (Some(a), Some(b)) => (a, b),
        _ => return samples.to_vec(),
    };

    // Поля ~150 мс.
    let margin = (rate as usize * 150 / 1000).max(0);

    let start = (first * frame).saturating_sub(margin);
    let end = ((last + 1) * frame + margin).min(samples.len());

    samples[start..end].to_vec()
}

/// Пишет моно f32 в WAV 16 кГц / 16 бит / моно.
///
/// Каждый сэмпл клампится в -1.0..=1.0 и масштабируется в i16.
pub fn write_wav_16k_mono(path: &std::path::Path, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer =
        hound::WavWriter::create(path, spec).context("не удалось создать WAV-файл")?;

    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        writer.write_sample(v).context("не удалось записать сэмпл")?;
    }

    writer.finalize().context("не удалось финализировать WAV")?;
    Ok(())
}
