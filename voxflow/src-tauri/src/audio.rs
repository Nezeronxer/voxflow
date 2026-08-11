//! Захват и обработка аудио для VoxFlow.
//!
//! Модуль отвечает за:
//! - перечисление входных устройств (`list_input_devices`);
//! - запуск захвата с микрофона в моно f32 (`start_capture` + [`Capture`]);
//! - ресэмплинг в 16 кГц (`resample_to_16k`, инкрементальный [`Resampler16k`]
//!   для партиал-петель);
//! - нормализацию уровня перед ASR (`normalize_for_asr`);
//! - обрезку тишины по краям (`trim_silence`);
//! - запись WAV 16 кГц / моно / 16 бит (`write_wav_16k_mono`).
//!
//! Важно: cpal `Stream` — `!Send`. Поэтому [`Capture`] создаётся и
//! потребляется на одном и том же рабочем потоке, без `Send`/`Sync`-границ
//! и без перемещения между потоками.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

/// Жёсткий предел длительности одной записи (защита от забытой защёлки: без него
/// буфер растёт ~11 МБ/мин при 48 кГц до исчерпания памяти). 30 минут — заведомо
/// больше любой реальной диктовки; по достижении новые сэмплы отбрасываются и
/// выставляется флаг overflow (UI может предупредить, см. Capture::overflowed).
const MAX_CAPTURE_SECS: usize = 30 * 60;

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
    /// Взводится data-callback'ом, когда буфер упёрся в [`MAX_CAPTURE_SECS`]
    /// и новые сэмплы начали отбрасываться.
    overflow: Arc<AtomicBool>,
}

impl Capture {
    /// Нативная частота дискретизации устройства, Гц.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Клон Arc на общий буфер сэмплов — чтобы фоновый поток мог СНИМАТЬ
    /// накопленные сэмплы (read-only) БЕЗ доступа к !Send cpal-потоку.
    /// Буфер НЕ сливается (`finish()` на потоке движка нужен полный буфер).
    /// Для периодического съёма в петлях используй [`Capture::tail_since`] —
    /// он не клонирует весь буфер под локом.
    pub fn buffer_handle(&self) -> Arc<Mutex<Vec<f32>>> {
        Arc::clone(&self.buffer)
    }

    /// Останавливает захват (дропает поток) и возвращает накопленные
    /// моно-сэмплы.
    pub fn finish(self) -> Vec<f32> {
        // Явный drop останавливает аудиопоток до того, как мы заберём буфер.
        drop(self.stream);
        // Poison-recovery: если поток, читавший буфер, упал с паникой под локом,
        // сами данные всё равно согласованы (Vec только дописывается) — забираем
        // их вместо того, чтобы ронять поток движка и убивать диктовку.
        let mut guard = self.buffer.lock().unwrap_or_else(|p| p.into_inner());
        std::mem::take(&mut *guard)
    }

    /// true, если запись достигла [`MAX_CAPTURE_SECS`] и новые сэмплы
    /// отбрасывались — хвост диктовки потерян, UI может предупредить.
    pub fn overflowed(&self) -> bool {
        self.overflow.load(Ordering::Relaxed)
    }

    /// Удобная обёртка над свободной [`tail_since`] для кода, у которого есть
    /// сам `Capture`. Петли партиалов движка работают через `buffer_handle()`
    /// (Capture — `!Send`) и зовут свободную функцию напрямую, поэтому метод
    /// сейчас без вызывающих — оставлен как API захвата.
    #[allow(dead_code)]
    pub fn tail_since(&self, cursor: usize) -> (Vec<f32>, usize) {
        tail_since(&self.buffer, cursor)
    }
}

/// Дёшево снимает хвост буфера начиная с `cursor`: под локом копируется
/// ТОЛЬКО `buffer[cursor..]`, а не весь буфер. Петли партиалов дергают это
/// каждые ~350 мс — полный clone многоминутной записи держал бы лок
/// десятки мс и блокировал data-callback (дропы сэмплов, см. P1-1).
/// Возвращает (хвост, новый курсор для следующего вызова).
///
/// Свободная функция над голым `Mutex`: петлям движка достаточно Arc на буфер
/// (см. [`Capture::buffer_handle`]), а тесты гоняют её без cpal-устройства.
pub(crate) fn tail_since(buffer: &Mutex<Vec<f32>>, cursor: usize) -> (Vec<f32>, usize) {
    let guard = buffer.lock().unwrap_or_else(|p| p.into_inner());
    let len = guard.len();
    if cursor >= len {
        // Буфер только растёт, так что cursor > len — лишь теоретический случай;
        // нормализуем курсор, чтобы вызывающий не ушёл в панику среза.
        return (Vec::new(), len);
    }
    (guard[cursor..].to_vec(), len)
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
    let overflow = Arc::new(AtomicBool::new(false));
    // Кап буфера в МОНО-сэмплах по фактической частоте устройства.
    let max_samples = MAX_CAPTURE_SECS.saturating_mul(sample_rate as usize);

    let err_fn = |err| log::error!("ошибка аудиопотока: {err}");

    // Строим поток под конкретный формат сэмплов; каждый кадр (group из
    // `channels` сэмплов) усредняем в один моно f32.
    let stream = match sample_format {
        SampleFormat::F32 => {
            let buf = Arc::clone(&buffer);
            let ovf = Arc::clone(&overflow);
            device.build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    push_mono(&buf, &ovf, max_samples, data, channels, |s| s);
                },
                err_fn,
                None,
            )
        }
        SampleFormat::I16 => {
            let buf = Arc::clone(&buffer);
            let ovf = Arc::clone(&overflow);
            device.build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    push_mono(&buf, &ovf, max_samples, data, channels, |s| {
                        s as f32 / 32768.0
                    });
                },
                err_fn,
                None,
            )
        }
        SampleFormat::U16 => {
            let buf = Arc::clone(&buffer);
            let ovf = Arc::clone(&overflow);
            device.build_input_stream(
                &config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    push_mono(&buf, &ovf, max_samples, data, channels, |s| {
                        (s as f32 - 32768.0) / 32768.0
                    });
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
        overflow,
    })
}

/// Сводит чередующиеся (interleaved) сэмплы в моно и дописывает в буфер.
///
/// `to_f32` приводит исходный тип сэмпла к f32 в диапазоне -1.0..=1.0;
/// затем кадр из `channels` сэмплов усредняется в один моно-сэмпл.
///
/// Буфер не растёт выше `max_samples` (защита от забытой защёлки, P2-1):
/// лишние кадры отбрасываются, при первом же отброшенном кадре взводится
/// `overflow`. Уже накопленные сэмплы при этом сохраняются.
fn push_mono<T: Copy>(
    buffer: &Mutex<Vec<f32>>,
    overflow: &AtomicBool,
    max_samples: usize,
    data: &[T],
    channels: usize,
    to_f32: impl Fn(T) -> f32,
) {
    let channels = channels.max(1);
    // Poison-recovery: паника чужого потока под этим локом не должна
    // останавливать запись — буфер остаётся согласованным (Vec дописывается
    // только здесь), продолжаем писать.
    let mut guard = buffer.lock().unwrap_or_else(|p| p.into_inner());
    let remaining = max_samples.saturating_sub(guard.len());
    guard.reserve((data.len() / channels + 1).min(remaining));
    for (pushed, frame) in data.chunks(channels).enumerate() {
        if pushed == remaining {
            // Упёрлись в кап — кадр отброшен, сигналим о переполнении.
            overflow.store(true, Ordering::Relaxed);
            return;
        }
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

/// Инкрементальный ресэмплер в 16 кГц для партиал-петель (фикс P1-1: вместо
/// полного ре-ресемпла ВСЕГО буфера каждый тик — O(n²) за диктовку — петля
/// скармливает только новый хвост захвата, O(n) суммарно).
///
/// Повторяет алгоритм [`resample_to_16k`] (то же скользящее среднее + линейная
/// интерполяция, формулы один в один): конкатенация результатов [`feed`]
/// по кускам бит-точно совпадает с пакетным `resample_to_16k` того же массива
/// на всей общей длине, КАК БЫ ни был порезан вход. Единственное расхождение —
/// на конце потока: последние ~`window/2` входных сэмплов не эмитятся, пока не
/// придут следующие куски (центрированному окну фильтра нужно «будущее»),
/// поэтому инкрементальный выход короче пакетного на несколько хвостовых
/// сэмплов (доли миллисекунды). Для финального текста движок по-прежнему
/// гонит пакетный ресэмпл всего буфера — там хвост не теряется.
///
/// [`feed`]: Resampler16k::feed
pub struct Resampler16k {
    /// Входная частота, Гц. 16000 => `feed` работает как passthrough.
    in_rate: u32,
    /// in_rate / 16000 — шаг по входу на один выходной сэмпл.
    ratio: f64,
    /// Полуширина центрированного окна скользящего среднего (window/2).
    half: usize,
    /// Хвост входных сэмплов, ещё нужный окнам фильтра будущих выходов.
    pending: Vec<f32>,
    /// Абсолютный (с начала потока) индекс первого сэмпла в `pending`.
    buf_start: usize,
    /// Сколько всего входных сэмплов принято.
    total_in: usize,
    /// Абсолютный индекс следующего выходного сэмпла.
    next_out: usize,
}

impl Resampler16k {
    pub fn new(in_rate: u32) -> Self {
        const OUT_RATE: u32 = 16000;
        // Окно — как в resample_to_16k, иначе бит-точность недостижима.
        let window = ((in_rate as f32 / OUT_RATE as f32).round() as usize).max(1);
        Resampler16k {
            in_rate,
            ratio: in_rate as f64 / OUT_RATE as f64,
            half: window / 2,
            pending: Vec::new(),
            buf_start: 0,
            total_in: 0,
            next_out: 0,
        }
    }

    /// Скармливает очередной кусок входа, возвращает готовые 16 кГц-сэмплы.
    pub fn feed(&mut self, chunk: &[f32]) -> Vec<f32> {
        if self.in_rate == 0 {
            // Вырожденный случай — как у resample_to_16k.
            return Vec::new();
        }
        if self.in_rate == 16000 {
            return chunk.to_vec();
        }

        self.pending.extend_from_slice(chunk);
        self.total_in += chunk.len();

        // Максимальный индекс фильтра, чьё окно [i-half, i+half] целиком лежит
        // в уже принятых данных; правее значения зависят от будущих сэмплов.
        let max_final = match self.total_in.checked_sub(self.half + 1) {
            Some(i) => i,
            None => return Vec::new(),
        };

        let mut out = Vec::new();
        loop {
            // Формулы — один в один как в resample_to_16k (бит-точность).
            let src_pos = self.next_out as f64 * self.ratio;
            let idx = src_pos.floor() as usize;
            if idx + 1 > max_final {
                break;
            }
            let a = self.filtered_at(idx);
            let b = self.filtered_at(idx + 1);
            let frac = (src_pos - idx as f64) as f32;
            out.push(a + (b - a) * frac);
            self.next_out += 1;
        }

        // Вход левее самого раннего ещё нужного окна больше не понадобится.
        let needed_from =
            ((self.next_out as f64 * self.ratio).floor() as usize).saturating_sub(self.half);
        if needed_from > self.buf_start {
            self.pending.drain(..needed_from - self.buf_start);
            self.buf_start = needed_from;
        }

        out
    }

    /// Значение скользящего среднего в абсолютном индексе `i` — как в
    /// `moving_average`: окно центрированное, слева усекается нулевым краем.
    /// Вызывается только для финализированных i (окно целиком в `pending`).
    fn filtered_at(&self, i: usize) -> f32 {
        let start = i.saturating_sub(self.half);
        let end = i + self.half + 1;
        let slice = &self.pending[start - self.buf_start..end - self.buf_start];
        let sum: f32 = slice.iter().copied().sum();
        sum / slice.len() as f32
    }
}

/// Целевой уровень речи (95-й перцентиль покадрового RMS), на котором штатно
/// работают и VAD, и ASR. ~ -24 dBFS: заметно выше шумовой полки, но далеко от
/// клиппинга.
const ASR_TARGET_LEVEL: f32 = 0.06;
/// Ниже этого уровня усиливать нечего: это мёртвый вход или чистая тишина, и
/// любое усиление лишь раскачает шумовую полку до «речи».
const ASR_SILENCE_LEVEL: f32 = 5e-4;
/// Потолок усиления. Выше него мы поднимаем уже не речь, а шум тракта.
const ASR_MAX_GAIN: f32 = 20.0;
/// Пик после усиления не должен подходить к 1.0 вплотную.
const ASR_PEAK_CEILING: f32 = 0.95;

/// Уровень речи на входе и усиление, применённое перед ASR.
#[derive(Clone, Copy, Debug)]
pub struct InputGain {
    pub speech_level: f32, // 95-й перцентиль покадрового RMS до усиления
    pub gain: f32,         // применённый множитель (1.0 — не трогали)
}

/// Приводит тихую запись к уровню, на котором штатно работают VAD и ASR.
///
/// Работает с буфером 16 кГц моно (кадры по 20 мс). Уровень речи меряем
/// 95-м перцентилем покадрового RMS: бóльшая часть буфера — паузы, поэтому
/// среднее и медиана занизили бы речь в разы (у тихого микрофона средний RMS
/// 0.002 при речевых кадрах 0.005–0.018).
///
/// Функция чистая: возвращает новый `Vec`, вход не мутирует. Это важно — в
/// live-петлях она зовётся на накопительном буфере каждый тик, и повторное
/// усиление уже усиленного было бы лавиной.
pub fn normalize_for_asr(samples: &[f32]) -> (Vec<f32>, InputGain) {
    // Функция работает только с 16 кГц моно, частоту не принимает.
    const RATE: usize = 16000;

    if samples.is_empty() {
        return (
            Vec::new(),
            InputGain {
                speech_level: 0.0,
                gain: 1.0,
            },
        );
    }

    let frame = RATE / 50; // 20 мс
    let mut levels = frame_rms(samples, frame);
    let speech_level = if levels.is_empty() {
        // Буфер короче одного кадра — меряем RMS целиком.
        rms(samples)
    } else {
        levels.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        percentile_sorted(&levels, 0.95)
    };

    // Уже достаточно громко — или практически тишина, где усиливать нечего.
    let already_loud = speech_level >= ASR_TARGET_LEVEL;
    let nothing_to_lift = speech_level < ASR_SILENCE_LEVEL;
    if already_loud || nothing_to_lift {
        return (
            samples.to_vec(),
            InputGain {
                speech_level,
                gain: 1.0,
            },
        );
    }

    // Усиление ограничено и абсолютным потолком, и запасом до клиппинга.
    // Пик считаем робастно — 99-й перцентиль покадровых пиков, а не абсолютный
    // максимум: один щелчок мыши или стук по клавише иначе диктует усиление на
    // всю диктовку (в замере на реальной записи ×14 схлопывалось до ×3.8, и
    // речь оставалась тихой). Сам щелчок срежет кламп ниже — потеря нулевая.
    let mut peaks = frame_peaks(samples, frame);
    let robust_peak = if peaks.is_empty() {
        samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
    } else {
        peaks.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        // Отбрасываем верхний процент кадров, но не меньше одного: на коротком
        // буфере перцентиль сам по себе вернул бы тот самый выброс.
        let dropped = (peaks.len() / 100).max(1);
        peaks[peaks.len().saturating_sub(dropped + 1)]
    };
    let peak_cap = if robust_peak > 0.0 {
        ASR_PEAK_CEILING / robust_peak
    } else {
        ASR_MAX_GAIN
    };
    let gain = (ASR_TARGET_LEVEL / speech_level)
        .min(ASR_MAX_GAIN)
        .min(peak_cap)
        .max(1.0);

    // Кламп — страховка от редких транзиентов выше потолка пика.
    let out = samples
        .iter()
        .map(|&s| (s * gain).clamp(-1.0, 1.0))
        .collect();

    (out, InputGain { speech_level, gain })
}

/// RMS по всему срезу; пустой срез — 0.0.
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

/// Покадровый RMS: кадры ровно по `frame` сэмплов, хвост короче кадра
/// отбрасывается. Пустой результат означает «буфер короче одного кадра».
fn frame_rms(samples: &[f32], frame: usize) -> Vec<f32> {
    let frame = frame.max(1);
    let num_frames = samples.len() / frame;
    (0..num_frames)
        .map(|f| rms(&samples[f * frame..(f + 1) * frame]))
        .collect()
}

/// Покадровый пик (максимум модуля). Кадры те же, что у [`frame_rms`].
fn frame_peaks(samples: &[f32], frame: usize) -> Vec<f32> {
    let frame = frame.max(1);
    let num_frames = samples.len() / frame;
    (0..num_frames)
        .map(|f| {
            samples[f * frame..(f + 1) * frame]
                .iter()
                .fold(0.0f32, |m, &s| m.max(s.abs()))
        })
        .collect()
}

/// Перцентиль `p` (0.0..=1.0) по УЖЕ отсортированному по возрастанию срезу.
/// Пустой срез — 0.0.
fn percentile_sorted(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f32 * p.clamp(0.0, 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Обрезает тишину по краям по RMS-энергии (грубый VAD перед точным Silero).
///
/// Кадр ~20 мс (`rate/50` сэмплов). Порог считается от статистики самой
/// записи — `clamp(медиана * 2.5, ABS_FLOOR, p95 * 0.3)`, — а не константой:
/// на тихом микрофоне (покадровый RMS речи 0.005–0.018) прежний жёсткий порог
/// 0.01 срезал 32 секунды диктовки до 0.35 с. Находим первый и последний
/// кадр выше порога, оставляем поля ~150 мс с каждой стороны.
///
/// Fail-open: если от записи длиннее секунды осталось меньше четверти —
/// возвращаем буфер целиком. Точную сегментацию делает Silero VAD выше по
/// стеку, а грубая обрезка не имеет права выбрасывать диктовку. Если ни один
/// кадр не прошёл порог — вход тоже возвращается без изменений.
pub fn trim_silence(samples: &[f32], rate: u32) -> Vec<f32> {
    // Маленький абсолютный пол: защита от полностью нулевого буфера, где вся
    // статистика вырождается в ноль и порогом стал бы сам ноль.
    const ABS_FLOOR: f32 = 0.0015;

    if samples.is_empty() || rate == 0 {
        return samples.to_vec();
    }

    let frame = (rate as usize / 50).max(1);
    let levels = frame_rms(samples, frame);
    if levels.is_empty() {
        return samples.to_vec();
    }

    let mut sorted = levels.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // Медиана — оценка шумовой полки, p95 — оценка уровня речи. Порог держим
    // над полкой, но обязательно НИЖЕ речи: в записи почти без пауз медиана
    // сама попадает в речь, и `median * 2.5` срезал бы саму диктовку.
    let threshold = (percentile_sorted(&sorted, 0.5) * 2.5)
        .min(percentile_sorted(&sorted, 0.95) * 0.3)
        .max(ABS_FLOOR);

    let mut first: Option<usize> = None;
    let mut last: Option<usize> = None;

    for (f, &level) in levels.iter().enumerate() {
        if level >= threshold {
            if first.is_none() {
                first = Some(f);
            }
            last = Some(f);
        }
    }

    let (first, last) = match (first, last) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            log::warn!(
                "trim_silence: ни один кадр не прошёл адаптивный порог RMS {threshold:.5} — буфер оставлен целиком ({} сэмплов)",
                samples.len()
            );
            return samples.to_vec();
        }
    };

    // Поля ~150 мс.
    let margin = rate as usize * 150 / 1000;

    let start = (first * frame).saturating_sub(margin);
    let end = ((last + 1) * frame + margin).min(samples.len());

    // Fail-open: грубая обрезка не имеет права съесть почти всю диктовку.
    let kept = end - start;
    if samples.len() > rate as usize && kept * 4 < samples.len() {
        log::warn!(
            "trim_silence: обрезка оставила {kept} из {} сэмплов при пороге RMS {threshold:.5} — буфер оставлен целиком",
            samples.len()
        );
        return samples.to_vec();
    }

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

    let file = crate::paths::create_private_file(path).context("не удалось создать WAV-файл")?;
    let mut writer = hound::WavWriter::new(file, spec).context("не удалось создать WAV-файл")?;

    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        writer
            .write_sample(v)
            .context("не удалось записать сэмпл")?;
    }

    writer.finalize().context("не удалось финализировать WAV")?;
    Ok(())
}

/// Duration of a PCM WAV rounded up to whole seconds.
///
/// ASR transports use this to derive bounded request deadlines from the actual
/// utterance length instead of applying one large timeout to every short tap.
/// A malformed/non-WAV file returns `None`; callers keep their conservative
/// fallback deadline in that case.
pub(crate) fn wav_duration_secs_ceil(path: &Path) -> Option<u64> {
    let reader = hound::WavReader::open(path).ok()?;
    let sample_rate = u64::from(reader.spec().sample_rate.max(1));
    Some(u64::from(reader.duration()).div_ceil(sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Детерминированный «речеподобный» сигнал из пары синусоид.
    fn test_signal(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * 0.013).sin() * 0.6 + (i as f32 * 0.071).cos() * 0.3)
            .collect()
    }

    /// Синус ~509 Гц амплитудой `amp` на сетке 16 кГц. Покадровый RMS такого
    /// сигнала — `amp / sqrt(2)` с точностью до 1 %.
    fn sine(n: usize, amp: f32) -> Vec<f32> {
        (0..n).map(|i| (i as f32 * 0.2).sin() * amp).collect()
    }

    /// Уровень речи (95-й перцентиль покадрового RMS) буфера 16 кГц — тем же
    /// способом, каким его меряет normalize_for_asr.
    fn speech_level_16k(samples: &[f32]) -> f32 {
        let mut levels = frame_rms(samples, 320);
        levels.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        percentile_sorted(&levels, 0.95)
    }

    /// Инкрементальный ресэмпл по рваным кускам == пакетный на общей длине;
    /// недоэмиченный хвост — не больше нескольких сэмплов (см. док Resampler16k).
    fn assert_incremental_matches_batch(rate: u32) {
        let signal = test_signal(rate as usize); // ~1 секунда
        let batch = resample_to_16k(&signal, rate);

        let mut rs = Resampler16k::new(rate);
        let mut inc: Vec<f32> = Vec::new();
        // Нарочно рваные размеры кусков — гоняем границы чанков.
        let sizes = [480usize, 1, 1024, 7, 333, 4800, 2];
        let mut pos = 0;
        let mut k = 0;
        while pos < signal.len() {
            let len = sizes[k % sizes.len()].min(signal.len() - pos);
            inc.extend(rs.feed(&signal[pos..pos + len]));
            pos += len;
            k += 1;
        }

        assert!(inc.len() <= batch.len());
        assert!(
            batch.len() - inc.len() <= 8,
            "недоэмиченный хвост слишком длинный: batch={} inc={}",
            batch.len(),
            inc.len()
        );
        for (i, (&a, &b)) in inc.iter().zip(batch.iter()).enumerate() {
            assert!(
                (a - b).abs() <= 1e-6,
                "расхождение на сэмпле {i}: inc={a} batch={b}"
            );
        }
    }

    #[test]
    fn resampler16k_matches_batch_44100() {
        assert_incremental_matches_batch(44100);
    }

    #[cfg(unix)]
    #[test]
    fn wav_files_are_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "voxflow-private-wav-{}-{nanos}.wav",
            std::process::id()
        ));

        write_wav_16k_mono(&path, &[0.0, 0.25, -0.25]).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn wav_duration_rounds_up_for_request_deadlines() {
        let path = std::env::temp_dir().join(format!(
            "voxflow-duration-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        write_wav_16k_mono(&path, &vec![0.0; 16001]).unwrap();
        assert_eq!(wav_duration_secs_ceil(&path), Some(2));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn resampler16k_matches_batch_48000() {
        assert_incremental_matches_batch(48000);
    }

    #[test]
    fn resampler16k_passthrough_16000() {
        let signal = test_signal(1600);
        let mut rs = Resampler16k::new(16000);
        assert_eq!(rs.feed(&signal), signal);
    }

    #[test]
    fn push_mono_caps_buffer_and_sets_overflow() {
        let buf: Mutex<Vec<f32>> = Mutex::new(Vec::new());
        let ovf = AtomicBool::new(false);
        // Кап 10 моно-сэмплов; 4 стерео-кадра (8 сэмплов) влезают целиком.
        push_mono(&buf, &ovf, 10, &[0.5f32; 8], 2, |s| s);
        assert_eq!(buf.lock().unwrap().len(), 4);
        assert!(!ovf.load(Ordering::Relaxed), "флаг до переполнения");
        // Ещё 8 кадров: влезают 6, два отброшены, флаг взведён.
        push_mono(&buf, &ovf, 10, &[0.5f32; 16], 2, |s| s);
        assert_eq!(buf.lock().unwrap().len(), 10);
        assert!(ovf.load(Ordering::Relaxed), "флаг после переполнения");
        // Дальше всё отбрасывается, длина не растёт.
        push_mono(&buf, &ovf, 10, &[0.5f32; 4], 2, |s| s);
        assert_eq!(buf.lock().unwrap().len(), 10);
    }

    #[test]
    fn tail_since_returns_only_new_samples() {
        let buf = Mutex::new(vec![0.0f32, 1.0, 2.0, 3.0, 4.0]);
        let (tail, cur) = tail_since(&buf, 0);
        assert_eq!(tail, vec![0.0, 1.0, 2.0, 3.0, 4.0]);
        assert_eq!(cur, 5);
        // Повторный съём без новых данных — пусто, курсор на месте.
        let (tail, cur) = tail_since(&buf, cur);
        assert!(tail.is_empty());
        assert_eq!(cur, 5);
        // Дописали — снимается только хвост.
        buf.lock().unwrap().extend_from_slice(&[5.0, 6.0]);
        let (tail, cur) = tail_since(&buf, cur);
        assert_eq!(tail, vec![5.0, 6.0]);
        assert_eq!(cur, 7);
        // Курсор за пределами буфера нормализуется, а не паникует.
        let (tail, cur) = tail_since(&buf, 100);
        assert!(tail.is_empty());
        assert_eq!(cur, 7);
    }

    #[test]
    fn poisoned_mutex_does_not_panic() {
        let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(vec![1.0, 2.0]));
        // Травим мьютекс паникой чужого потока под локом.
        let buf2 = Arc::clone(&buf);
        let _ = std::thread::spawn(move || {
            let _g = buf2.lock().unwrap();
            panic!("намеренная паника для отравления мьютекса");
        })
        .join();
        assert!(buf.is_poisoned());

        // push_mono продолжает писать, tail_since продолжает читать.
        let ovf = AtomicBool::new(false);
        push_mono(&buf, &ovf, 100, &[3.0f32], 1, |s| s);
        let (tail, cur) = tail_since(&buf, 0);
        assert_eq!(tail, vec![1.0, 2.0, 3.0]);
        assert_eq!(cur, 3);
        assert!(!ovf.load(Ordering::Relaxed));
    }

    #[test]
    fn normalize_for_asr_survives_a_single_loud_transient() {
        // Тихая речь + один щелчок (клавиша/мышь) громче речи в 25 раз.
        // По абсолютному пику усиление схлопнулось бы почти до 1.0, и диктовка
        // осталась бы тихой; робастный пик (p99 покадровых) это игнорирует.
        let mut buf = sine(16000, 0.01);
        for (i, sample) in buf.iter_mut().enumerate().skip(8000).take(320) {
            *sample = if i % 2 == 0 { 0.25 } else { -0.25 };
        }

        let (out, gain) = normalize_for_asr(&buf);

        assert!(
            gain.gain > 4.0,
            "один щелчок не должен диктовать усиление всей диктовки: {gain:?}"
        );
        let after = speech_level_16k(&out);
        assert!(
            after > ASR_TARGET_LEVEL / 2.0,
            "после усиления речь всё ещё тихая: {after}"
        );
        assert!(
            out.iter().all(|s| s.abs() <= 1.0),
            "щелчок должен срезаться клампом, а не выходить за пределы"
        );
    }

    #[test]
    fn normalize_for_asr_lifts_quiet_speech_to_target() {
        // 1 с: 0.2 с тишины, 0.6 с очень тихой «речи», 0.2 с тишины.
        let mut buf = vec![0.0f32; 3200];
        buf.extend(sine(9600, 0.01));
        buf.extend(vec![0.0f32; 3200]);

        let (out, gain) = normalize_for_asr(&buf);

        assert_eq!(out.len(), buf.len());
        assert!(gain.gain > 1.0, "тихий вход должен усиливаться: {gain:?}");
        assert!(
            (gain.speech_level - 0.00707).abs() < 0.001,
            "уровень речи померен неверно: {gain:?}"
        );
        // Уровень доехал до целевого, клиппинга при этом нет.
        let after = speech_level_16k(&out);
        assert!(
            (after - ASR_TARGET_LEVEL).abs() < 0.012,
            "после усиления уровень {after}, ждали ~{ASR_TARGET_LEVEL}"
        );
        assert!(
            out.iter().all(|s| s.abs() <= 1.0),
            "усиление вышло за пределы -1.0..=1.0"
        );

        // Функция чистая: вход не тронут, повторный вызов не усиливает снова
        // (в live-петлях она зовётся на накопительном буфере каждый тик).
        assert!((speech_level_16k(&buf) - gain.speech_level).abs() < 1e-6);
        let (_, gain2) = normalize_for_asr(&out);
        assert!(
            gain2.gain < 1.001,
            "повторное усиление уже усиленного буфера: {gain2:?}"
        );
    }

    #[test]
    fn normalize_for_asr_leaves_loud_input_untouched() {
        let buf = sine(16000, 0.3);
        let (out, gain) = normalize_for_asr(&buf);

        assert_eq!(gain.gain, 1.0, "громкий вход трогать нельзя: {gain:?}");
        assert!(gain.speech_level > ASR_TARGET_LEVEL);
        assert_eq!(out, buf);
    }

    #[test]
    fn normalize_for_asr_does_not_amplify_silence() {
        // Полная тишина.
        let quiet = vec![0.0f32; 16000];
        let (out, gain) = normalize_for_asr(&quiet);
        assert_eq!(gain.gain, 1.0, "тишину усиливать нечем: {gain:?}");
        assert_eq!(out, quiet);

        // Мёртвый вход: шумовая полка ниже порога тишины — не раскачиваем её.
        let noise = sine(16000, 1e-5);
        let (out, gain) = normalize_for_asr(&noise);
        assert!(gain.speech_level < ASR_SILENCE_LEVEL);
        assert_eq!(gain.gain, 1.0, "шумовая полка не должна расти: {gain:?}");
        assert_eq!(out, noise);
    }

    #[test]
    fn trim_silence_keeps_quiet_dictation() {
        // Регресс на баг тихого микрофона: 3 с записи, посередине 0.8 с речи
        // амплитудой 0.008 (покадровый RMS ~0.0057), по краям тишина.
        let mut buf = vec![0.0f32; 17600];
        buf.extend(sine(12800, 0.008));
        buf.extend(vec![0.0f32; 17600]);
        // Пара громких слогов внутри тихой речи: прежний жёсткий порог 0.01
        // цеплялся только за них и схлопывал диктовку до ~0.36 с.
        for s in &mut buf[23680..24640] {
            *s *= 3.75;
        }

        let trimmed = trim_silence(&buf, 16000);

        assert!(
            trimmed.len() >= 12800,
            "тихая диктовка схлопнута: {} из {} сэмплов",
            trimmed.len(),
            buf.len()
        );
        assert!(
            trimmed.len() < buf.len(),
            "края тишины всё же должны срезаться: {}",
            trimmed.len()
        );
    }

    #[test]
    fn trim_silence_still_cuts_silent_edges() {
        // 3 с: секунда тишины, секунда нормальной по громкости речи, секунда
        // тишины. Остаться должна речь плюс поля по 150 мс.
        let mut buf = vec![0.0f32; 16000];
        buf.extend(sine(16000, 0.3));
        buf.extend(vec![0.0f32; 16000]);

        let trimmed = trim_silence(&buf, 16000);

        assert!(
            trimmed.len() >= 16000,
            "речь обрезана: {} сэмплов",
            trimmed.len()
        );
        // 1 с речи + поля по 150 мс + запас на кадр 20 мс.
        assert!(
            trimmed.len() <= 16000 + 2 * 2400 + 640,
            "тишина по краям не срезана: {} сэмплов",
            trimmed.len()
        );
    }

    #[test]
    fn trim_silence_fails_open_when_trim_would_eat_dictation() {
        // 3 с почти тишины с одним коротким щелчком: обрезка оставила бы ~11 %
        // буфера. Грубый VAD не имеет права выбрасывать диктовку — точную
        // сегментацию делает Silero выше по стеку.
        let mut buf = vec![0.0f32; 48000];
        buf[24000..24320].copy_from_slice(&sine(320, 0.5));

        let trimmed = trim_silence(&buf, 16000);

        assert_eq!(
            trimmed.len(),
            buf.len(),
            "fail-open не сработал: {} из {} сэмплов",
            trimmed.len(),
            buf.len()
        );
    }

    #[test]
    fn empty_and_degenerate_input_does_not_panic() {
        let (out, gain) = normalize_for_asr(&[]);
        assert!(out.is_empty());
        assert_eq!(gain.gain, 1.0);
        assert_eq!(gain.speech_level, 0.0);
        assert!(trim_silence(&[], 16000).is_empty());

        // Буфер короче одного кадра 20 мс.
        let (short, _) = normalize_for_asr(&[0.1, -0.1]);
        assert_eq!(short.len(), 2);
        assert_eq!(trim_silence(&[0.1, -0.1], 16000).len(), 2);

        // Полностью нулевой буфер: ни один кадр не проходит абсолютный пол
        // порога — возвращаем всё как есть.
        let silence = vec![0.0f32; 16000];
        assert_eq!(trim_silence(&silence, 16000).len(), silence.len());

        // Нулевая частота — тоже без паники.
        assert_eq!(trim_silence(&silence, 0).len(), silence.len());
    }
}
