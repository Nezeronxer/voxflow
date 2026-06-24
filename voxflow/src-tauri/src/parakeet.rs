//! NVIDIA Parakeet TDT 0.6B v3 — мультиязычный ASR (EN + 24 языка, включая RU).
//! ONNX через ort (CPU, int8), модель istupakov/parakeet-tdt-0.6b-v3-onnx (CC-BY-4.0).
//! Используется для language="en" и автодетекта языка (language="auto").
//!
//! Структура повторяет gigaam.rs, но фронтенд — готовый ONNX-граф nemo128.onnx
//! (log-mel 128), а LSTM-предиктор и joint слиты в один граф decoder_joint.
//! Декодер — TDT greedy (blank=8192, 5 duration-голов = прыжок 0..4 кадра,
//! ≤10 токенов на кадр), дословно по эталону onnx_asr (istupakov).

use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use ort::session::Session;
use ort::value::Tensor;

use crate::gigaam::{OrtCtx, TranscribeStats};

/// Подкаталог в models_dir с файлами модели.
pub const PARAKEET_DIR: &str = "parakeet";

/// Файлы модели (имя на HF, точный размер в байтах) — для скачивания/валидации.
/// Размеры сверены с HF tree API 2026-06-10. vocab сверяем только на >0 байт.
pub const PARAKEET_FILES: &[(&str, u64)] = &[
    ("encoder-model.int8.onnx", 652_183_999),
    ("decoder_joint-model.int8.onnx", 18_202_004),
    ("nemo128.onnx", 139_764),
    ("vocab.txt", 93_939),
];

const SAMPLE_RATE: usize = 16_000;
const N_MELS: usize = 128;
const ENC_DIM: usize = 1024;
const PRED_STATE: usize = 640; // h/c LSTM-предиктора: [2,1,640]
const VOCAB_SIZE: usize = 8193; // id 0..8192, 8192 = <blk>
const BLANK_ID: usize = 8192;
const NUM_DURATIONS: usize = 5; // TDT-головы: прыжок 0..4 кадра
const NUM_LOGITS: usize = VOCAB_SIZE + NUM_DURATIONS; // 8198
const MAX_TOKENS_PER_STEP: usize = 10;

/// Все файлы модели на месте (размер бинарей точный, vocab — просто непустой).
pub fn dir_ready(dir: &Path) -> bool {
    PARAKEET_FILES.iter().all(|(name, size)| {
        let p = dir.join(name);
        match std::fs::metadata(&p) {
            Ok(m) => {
                if name.ends_with(".txt") {
                    m.len() > 0
                } else {
                    m.len() == *size
                }
            }
            Err(_) => false,
        }
    })
}

/// Доля кириллических букв среди всех букв > 0.5 — основа LID-роутера
/// (Parakeet знает русский, поэтому его выдачу можно классифицировать по алфавиту).
pub fn is_mostly_cyrillic(text: &str) -> bool {
    let mut cyr = 0usize;
    let mut total = 0usize;
    for ch in text.chars() {
        if ch.is_alphabetic() {
            total += 1;
            if ('\u{0400}'..='\u{04FF}').contains(&ch) {
                cyr += 1;
            }
        }
    }
    total > 0 && cyr * 2 > total
}

pub struct Parakeet {
    /// nemo128.onnx: waveforms → log-mel 128 (mel-фронтенд не пишем руками).
    preprocessor: Session,
    encoder: Session,
    /// LSTM-предиктор + joint в одном графе (пошаговые вызовы, 1 поток).
    decoder_joint: Session,
    /// id → токен (▁ уже заменён на пробел при загрузке, как в эталоне).
    vocab: Vec<String>,
    pub last_stats: TranscribeStats,
}

impl Parakeet {
    pub fn load(dir: &Path, threads: usize) -> Result<Self> {
        let t0 = Instant::now();
        let mk = |file: &str, th: usize| -> Result<Session> {
            Session::builder()
                .oc("builder")?
                .with_intra_threads(th)
                .oc("intra_threads")?
                .commit_from_file(dir.join(file))
                .oc(&format!("загрузка {file}"))
        };
        // Тяжесть в препроцессоре (STFT всего сигнала) и энкодере; decoder_joint —
        // крошечные пошаговые вызовы, многопоточность там только мешает.
        let preprocessor = mk("nemo128.onnx", threads.max(1))?;
        let encoder = mk("encoder-model.int8.onnx", threads.max(1))?;
        let decoder_joint = mk("decoder_joint-model.int8.onnx", 1)?;

        let vocab_raw = std::fs::read_to_string(dir.join("vocab.txt")).context("чтение vocab")?;
        let mut vocab = vec![String::new(); VOCAB_SIZE];
        for line in vocab_raw.lines() {
            let line = line.trim_end_matches('\r');
            if let Some((tok, id)) = line.rsplit_once(' ') {
                if let Ok(i) = id.parse::<usize>() {
                    if i < VOCAB_SIZE {
                        vocab[i] = tok.replace('\u{2581}', " ");
                    }
                }
            }
        }
        if vocab[BLANK_ID] != "<blk>" {
            return Err(anyhow!("vocab повреждён: id {BLANK_ID} ≠ <blk>"));
        }

        log::info!(
            "[parakeet] загружен за {} мс ({} потоков)",
            t0.elapsed().as_millis(),
            threads
        );
        Ok(Self {
            preprocessor,
            encoder,
            decoder_joint,
            vocab,
            last_stats: TranscribeStats::default(),
        })
    }

    /// Распознать 16 кГц mono f32. Сегментацию длинного аудио делает engine
    /// снаружи (по VAD-паузам), как и с gigaam.
    pub fn transcribe(&mut self, samples_16k: &[f32]) -> Result<String> {
        let t_total = Instant::now();
        if samples_16k.len() < 512 {
            // Меньше одного окна фронтенда (win=400) — распознавать нечего.
            return Ok(String::new());
        }

        // ── Препроцессор nemo128: waveforms [1,N] → features [1,128,T] ──
        let t_fe = Instant::now();
        let n = samples_16k.len();
        let wf =
            Tensor::from_array((vec![1i64, n as i64], samples_16k.to_vec())).oc("waveforms")?;
        let wl = Tensor::from_array((vec![1i64], vec![n as i64])).oc("waveforms_lens")?;
        let pre_out = self
            .preprocessor
            .run(ort::inputs!["waveforms" => wf, "waveforms_lens" => wl])
            .oc("preprocessor.run")?;
        let (f_shape, f_data) = pre_out["features"]
            .try_extract_tensor::<f32>()
            .oc("features")?;
        let f_dims: Vec<i64> = f_shape.iter().copied().collect();
        if f_dims.len() != 3 || f_dims[1] as usize != N_MELS {
            return Err(anyhow!("features: неожиданная форма {f_dims:?}"));
        }
        let n_frames = f_dims[2];
        let (_, fl_data) = pre_out["features_lens"]
            .try_extract_tensor::<i64>()
            .oc("features_lens")?;
        let feat_len = fl_data.first().copied().unwrap_or(n_frames).min(n_frames);
        // Копируем (выход живёт по ссылке на Session — отпускаем заём до энкодера).
        let features: Vec<f32> = f_data.to_vec();
        drop(pre_out);
        let frontend_ms = t_fe.elapsed().as_millis() as u64;

        // ── Энкодер: [1,128,T] → outputs [1,1024,T'] (T'≈T/8), encoded_lengths ──
        let t_enc = Instant::now();
        let feats = Tensor::from_array((vec![1i64, N_MELS as i64, n_frames], features))
            .oc("audio_signal")?;
        let lens = Tensor::from_array((vec![1i64], vec![feat_len])).oc("length")?;
        let enc_out = self
            .encoder
            .run(ort::inputs!["audio_signal" => feats, "length" => lens])
            .oc("encoder.run")?;
        let (e_shape, e_data) = enc_out["outputs"]
            .try_extract_tensor::<f32>()
            .oc("outputs")?;
        let e_dims: Vec<i64> = e_shape.iter().copied().collect();
        if e_dims.len() != 3 || e_dims[1] as usize != ENC_DIM {
            return Err(anyhow!("outputs: неожиданная форма {e_dims:?}"));
        }
        let tp = e_dims[2] as usize;
        let (_, el_data) = enc_out["encoded_lengths"]
            .try_extract_tensor::<i64>()
            .oc("encoded_lengths")?;
        let enc_len = (el_data.first().copied().unwrap_or(tp as i64) as usize).min(tp);
        let enc_data: Vec<f32> = e_data.to_vec();
        drop(enc_out);
        let encoder_ms = t_enc.elapsed().as_millis() as u64;

        // ── TDT greedy (дословно по onnx_asr): token + duration на каждом шаге ──
        let t_dec = Instant::now();
        let mut tokens: Vec<usize> = Vec::new();
        let mut h = vec![0f32; 2 * PRED_STATE];
        let mut c = vec![0f32; 2 * PRED_STATE];
        let mut enc_frame = vec![0f32; ENC_DIM];
        let mut t = 0usize;
        let mut emitted = 0usize;
        while t < enc_len {
            for ch in 0..ENC_DIM {
                enc_frame[ch] = enc_data[ch * tp + t];
            }
            // ВНИМАНИЕ: targets/target_length у этого графа — i32, не i64.
            let last = tokens.last().map(|&i| i as i32).unwrap_or(BLANK_ID as i32);
            let te = Tensor::from_array((vec![1i64, ENC_DIM as i64, 1], enc_frame.clone()))
                .oc("enc_frame")?;
            let tg = Tensor::from_array((vec![1i64, 1], vec![last])).oc("targets")?;
            let tl = Tensor::from_array((vec![1i64], vec![1i32])).oc("target_length")?;
            let th =
                Tensor::from_array((vec![2i64, 1, PRED_STATE as i64], h.clone())).oc("states_1")?;
            let tc =
                Tensor::from_array((vec![2i64, 1, PRED_STATE as i64], c.clone())).oc("states_2")?;
            let out = self
                .decoder_joint
                .run(ort::inputs![
                    "encoder_outputs" => te,
                    "targets" => tg,
                    "target_length" => tl,
                    "input_states_1" => th,
                    "input_states_2" => tc,
                ])
                .oc("decoder_joint.run")?;
            let (_, logits) = out["outputs"]
                .try_extract_tensor::<f32>()
                .oc("dj outputs")?;
            let nl = logits.len();
            if nl < NUM_LOGITS {
                return Err(anyhow!(
                    "decoder_joint: ожидалось ≥{NUM_LOGITS} логитов, получено {nl}"
                ));
            }
            let logits = &logits[nl - NUM_LOGITS..]; // [1,1,1,8198] → последние 8198
            let token = argmax(&logits[..VOCAB_SIZE]);
            let step = argmax(&logits[VOCAB_SIZE..]); // duration-голова: прыжок 0..4
            if token != BLANK_ID {
                // Состояния предиктора двигаются только при эмиссии (blank — SOS).
                let (_, hh) = out["output_states_1"]
                    .try_extract_tensor::<f32>()
                    .oc("output_states_1")?;
                let (_, cc) = out["output_states_2"]
                    .try_extract_tensor::<f32>()
                    .oc("output_states_2")?;
                h = hh.to_vec();
                c = cc.to_vec();
                tokens.push(token);
                emitted += 1;
            }
            if step > 0 {
                t += step;
                emitted = 0;
            } else if token == BLANK_ID || emitted == MAX_TOKENS_PER_STEP {
                t += 1;
                emitted = 0;
            }
            // token != blank и step == 0 → остаёмся на кадре t (ещё токены отсюда).
        }
        let decoder_ms = t_dec.elapsed().as_millis() as u64;

        // ── Текст: конкатенация токенов + чистка пробелов ──
        let joined: String = tokens.iter().map(|&i| self.vocab[i].as_str()).collect();
        let text = clean_spaces(&joined);

        self.last_stats = TranscribeStats {
            audio_ms: (samples_16k.len() * 1000 / SAMPLE_RATE) as u64,
            frontend_ms,
            encoder_ms,
            decoder_ms,
            total_ms: t_total.elapsed().as_millis() as u64,
        };
        Ok(text)
    }
}

fn argmax(xs: &[f32]) -> usize {
    let mut best = 0usize;
    let mut best_v = f32::NEG_INFINITY;
    for (i, &v) in xs.iter().enumerate() {
        if v > best_v {
            best_v = v;
            best = i;
        }
    }
    best
}

/// Копия gigaam::clean_spaces (там private; не выносим в общий модуль — файл
/// правит параллельный агент). Семантика re.sub(r"\A\s|\s\B|(\s)\b", ...).
fn clean_spaces(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &ch) in chars.iter().enumerate() {
        if ch.is_whitespace() {
            if out.is_empty() {
                continue; // \A\s
            }
            match chars.get(i + 1) {
                Some(&n) if n.is_alphanumeric() || n == '_' => out.push(' '), // (\s)\b → " "
                _ => {} // \s\B → убрать (перед пунктуацией/пробелом/в конце)
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn models_dir() -> PathBuf {
        // На целевой машине модель уже скачана (сессия 2026-06-10).
        PathBuf::from(std::env::var("LOCALAPPDATA").unwrap()).join("VoxFlow/models/parakeet")
    }

    fn read_wav_16k(p: &str) -> Vec<f32> {
        let r = hound::WavReader::open(p).expect("открыть WAV");
        let spec = r.spec();
        assert_eq!(spec.sample_rate, 16000, "тестовый WAV должен быть 16 кГц");
        match spec.sample_format {
            hound::SampleFormat::Int => {
                let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
                r.into_samples::<i32>()
                    .map(|x| x.unwrap_or(0) as f32 / max)
                    .collect()
            }
            hound::SampleFormat::Float => {
                r.into_samples::<f32>().map(|x| x.unwrap_or(0.0)).collect()
            }
        }
    }

    #[test]
    #[ignore = "requires local Parakeet models and private evaluation WAV files"]
    fn parakeet_e2e_en() {
        let dir = models_dir();
        assert!(dir_ready(&dir), "модели Parakeet не найдены в {dir:?}");
        let t0 = std::time::Instant::now();
        let mut p = Parakeet::load(&dir, 6).expect("load");
        println!("load: {} мс", t0.elapsed().as_millis());

        // Синтез фразы "Hello, please schedule a meeting for tomorrow at 3 in the afternoon."
        let samples = read_wav_16k(r"C:\Users\Nezeronxer\AppData\Local\VoxFlow\eval\en_01.wav");
        let text = p.transcribe(&samples).expect("transcribe");
        let st = p.last_stats;
        println!(
            "en_01 → {text:?}\n  audio={}мс frontend={}мс encoder={}мс decoder={}мс total={}мс",
            st.audio_ms, st.frontend_ms, st.encoder_ms, st.decoder_ms, st.total_ms
        );
        assert!(!text.trim().is_empty(), "пустой результат на EN-голосе");
        assert!(
            text.chars().any(|ch| ch.is_ascii_alphabetic()),
            "ожидалась латиница: {text:?}"
        );
        assert!(
            !is_mostly_cyrillic(&text),
            "EN-фраза не должна детектиться как кириллица: {text:?}"
        );
    }

    #[test]
    #[ignore = "requires local Parakeet models and private evaluation WAV files"]
    fn parakeet_e2e_ru() {
        let dir = models_dir();
        assert!(dir_ready(&dir), "модели Parakeet не найдены в {dir:?}");
        let mut p = Parakeet::load(&dir, 6).expect("load");

        // Parakeet знает русский — это путь автодетекта (language="auto").
        let samples = read_wav_16k(r"C:\Users\Nezeronxer\AppData\Local\VoxFlow\eval\ru_03.wav");
        let text = p.transcribe(&samples).expect("transcribe");
        let st = p.last_stats;
        println!(
            "ru_03 → {text:?}\n  audio={}мс frontend={}мс encoder={}мс decoder={}мс total={}мс",
            st.audio_ms, st.frontend_ms, st.encoder_ms, st.decoder_ms, st.total_ms
        );
        assert!(!text.trim().is_empty(), "пустой результат на RU-голосе");
        assert!(
            text.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c)),
            "ожидалась кириллица, получено: {text:?}"
        );
        assert!(
            is_mostly_cyrillic(&text),
            "RU-фраза должна детектиться как кириллица: {text:?}"
        );
    }

    #[test]
    fn mostly_cyrillic_classifier() {
        assert!(!is_mostly_cyrillic(""));
        assert!(!is_mostly_cyrillic("12345, 67!")); // нет букв → false
        assert!(!is_mostly_cyrillic("Hello, world!"));
        assert!(is_mostly_cyrillic("Привет, мир!"));
        assert!(is_mostly_cyrillic("Привет world и ещё слова тут")); // 18 кир. из 23 букв
        assert!(!is_mostly_cyrillic("Hello world и word")); // латиницы больше
        assert!(is_mostly_cyrillic("Ёжик ел йогурт")); // ё/й внутри U+0400..04FF
    }
}
