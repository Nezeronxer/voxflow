//! GigaAM-v3 e2e RNNT — основной русский ASR (SberDevices, MIT). ONNX через ort,
//! CPU, int8. Выдаёт текст СРАЗУ с пунктуацией/капитализацией/ITN (e2e-чекпойнт),
//! поэтому LLM-рефайн для «нормального» текста не нужен.
//!
//! Реализация повторяет эталон onnx_asr (istupakov) бит-в-бит:
//! фронтенд — log-mel 64 (n_fft=320, hop=160, периодический Hann, HTK-мел 0–8000,
//! без нормализации, ln(clip(x,1e-9,1e9)), center=false; окно и мел-банк прогнаны
//! через bf16-округление как в prep_gigaam.py), декодер — greedy transducer
//! (blank=1024, ≤3 токенов на кадр, кэш dec_out при blank).

use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use ort::session::Session;
use ort::value::Tensor;

/// ort::Error не Send+Sync → в anyhow конвертируем строкой.
/// Error<R> генерик (builder возвращает Error<SessionBuilder>) — покрываем все R.
pub(crate) trait OrtCtx<T> {
    fn oc(self, what: &str) -> Result<T>;
}
impl<T, R> OrtCtx<T> for std::result::Result<T, ort::Error<R>> {
    fn oc(self, what: &str) -> Result<T> {
        self.map_err(|e| anyhow!("{what}: {e}"))
    }
}

/// Подкаталог в models_dir, куда складываются файлы модели.
pub const GIGAAM_DIR: &str = "gigaam";

/// Файлы модели (имя на HF, точный размер в байтах) — для скачивания/валидации.
/// vocab сверяем только на >0 байт (текст может прилететь с CRLF).
pub const GIGAAM_FILES: &[(&str, u64)] = &[
    ("v3_e2e_rnnt_encoder.int8.onnx", 224_570_477),
    ("v3_e2e_rnnt_decoder.int8.onnx", 1_159_170),
    ("v3_e2e_rnnt_joint.int8.onnx", 687_791),
    ("v3_e2e_rnnt_vocab.txt", 13_354),
];

const SAMPLE_RATE: usize = 16_000;
const N_FFT: usize = 320; // = win_length (center=false)
const HOP: usize = 160;
const N_MELS: usize = 64;
const N_BINS: usize = N_FFT / 2 + 1; // 161
const PRED_HIDDEN: usize = 320;
const ENC_DIM: usize = 768;
const VOCAB_SIZE: usize = 1025;
const BLANK_ID: i64 = 1024;
const MAX_TOKENS_PER_STEP: usize = 3;

/// Все файлы модели на месте (размер бинарей точный, vocab — просто непустой).
pub fn dir_ready(dir: &Path) -> bool {
    GIGAAM_FILES.iter().all(|(name, size)| {
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

/// Тайминги последнего transcribe — для логов латентности по этапам.
#[derive(Clone, Copy, Default)]
pub struct TranscribeStats {
    pub audio_ms: u64,
    pub frontend_ms: u64,
    pub encoder_ms: u64,
    pub decoder_ms: u64,
    pub total_ms: u64,
}

pub struct GigaAm {
    encoder: Session,
    decoder: Session,
    joint: Session,
    /// id → токен (▁ уже заменён на пробел при загрузке, как в эталоне).
    vocab: Vec<String>,
    /// Периодический Hann 320 (bf16-округлён).
    window: [f32; N_FFT],
    /// Мел-банк, транспонирован для кэша: [64][161] (bf16-округлён).
    fbanks: Vec<[f32; N_BINS]>,
    /// Таблицы DFT: cos/sin [161][320] (n_fft не степень двойки — честный DFT).
    dft_cos: Vec<[f32; N_FFT]>,
    dft_sin: Vec<[f32; N_FFT]>,
    pub last_stats: TranscribeStats,
}

impl GigaAm {
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
        // Вся тяжесть в энкодере; decoder/joint — крошечные пошаговые вызовы,
        // многопоточность там только мешает (оверхед планировщика на каждый кадр).
        let encoder = mk("v3_e2e_rnnt_encoder.int8.onnx", threads.max(1))?;
        let decoder = mk("v3_e2e_rnnt_decoder.int8.onnx", 1)?;
        let joint = mk("v3_e2e_rnnt_joint.int8.onnx", 1)?;

        let vocab_raw = std::fs::read_to_string(dir.join("v3_e2e_rnnt_vocab.txt"))
            .context("чтение vocab")?;
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
        if vocab[BLANK_ID as usize] != "<blk>" {
            return Err(anyhow!("vocab повреждён: id {BLANK_ID} ≠ <blk>"));
        }

        // Периодический Hann: hanning(321)[:-1] → w[i] = 0.5 − 0.5·cos(2πi/320).
        let mut window = [0f32; N_FFT];
        for (i, w) in window.iter_mut().enumerate() {
            *w = bf16_round((0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / N_FFT as f64).cos()) as f32);
        }

        // HTK-мел-банк 161×64 (f_min=0, f_max=8000, norm=None), bf16-округление.
        let hz_to_mel = |f: f64| 2595.0 * (1.0 + f / 700.0).log10();
        let mel_to_hz = |m: f64| 700.0 * (10f64.powf(m / 2595.0) - 1.0);
        let m_max = hz_to_mel(8000.0);
        let pts: Vec<f64> = (0..N_MELS + 2).map(|i| mel_to_hz(m_max * i as f64 / (N_MELS + 1) as f64)).collect();
        let mut fbanks = vec![[0f32; N_BINS]; N_MELS];
        for f in 0..N_BINS {
            let freq = 8000.0 * f as f64 / (N_BINS - 1) as f64;
            for m in 0..N_MELS {
                let up = (freq - pts[m]) / (pts[m + 1] - pts[m]);
                let down = (pts[m + 2] - freq) / (pts[m + 2] - pts[m + 1]);
                fbanks[m][f] = bf16_round(up.min(down).max(0.0) as f32);
            }
        }

        // DFT-таблицы (320 — не степень двойки; честный DFT по 161 бину дёшев).
        let mut dft_cos = vec![[0f32; N_FFT]; N_BINS];
        let mut dft_sin = vec![[0f32; N_FFT]; N_BINS];
        for k in 0..N_BINS {
            for n in 0..N_FFT {
                let a = std::f64::consts::TAU * k as f64 * n as f64 / N_FFT as f64;
                dft_cos[k][n] = a.cos() as f32;
                dft_sin[k][n] = a.sin() as f32;
            }
        }

        log::info!("[gigaam] загружен за {} мс ({} потоков)", t0.elapsed().as_millis(), threads);
        Ok(Self { encoder, decoder, joint, vocab, window, fbanks, dft_cos, dft_sin, last_stats: TranscribeStats::default() })
    }

    /// Распознать 16 кГц mono f32. Для буферов длиннее ~30 с качество деградирует
    /// (pos_emb) — длинное аудио режь по VAD-паузам снаружи (engine).
    pub fn transcribe(&mut self, samples_16k: &[f32]) -> Result<String> {
        let t_total = Instant::now();
        if samples_16k.len() < N_FFT {
            return Ok(String::new());
        }

        // ── Фронтенд: log-mel [1, 64, T] ──
        let t_fe = Instant::now();
        let n_frames = (samples_16k.len() - N_FFT) / HOP + 1;
        let mut features = vec![0f32; N_MELS * n_frames];
        let mut spec = [0f32; N_BINS];
        for t in 0..n_frames {
            let frame = &samples_16k[t * HOP..t * HOP + N_FFT];
            let mut wf = [0f32; N_FFT];
            for n in 0..N_FFT {
                wf[n] = frame[n] * self.window[n];
            }
            for k in 0..N_BINS {
                let (mut re, mut im) = (0f32, 0f32);
                let (ck, sk) = (&self.dft_cos[k], &self.dft_sin[k]);
                for n in 0..N_FFT {
                    re += wf[n] * ck[n];
                    im -= wf[n] * sk[n];
                }
                spec[k] = re * re + im * im;
            }
            for m in 0..N_MELS {
                let fb = &self.fbanks[m];
                let mut e = 0f32;
                for k in 0..N_BINS {
                    e += spec[k] * fb[k];
                }
                features[m * n_frames + t] = e.clamp(1e-9, 1e9).ln();
            }
        }
        let frontend_ms = t_fe.elapsed().as_millis() as u64;

        // ── Энкодер: [1,64,T] → encoded [1,768,T'], encoded_len i32 ──
        let t_enc = Instant::now();
        let feats = Tensor::from_array((vec![1i64, N_MELS as i64, n_frames as i64], features)).oc("feats")?;
        let lens = Tensor::from_array((vec![1i64], vec![n_frames as i64])).oc("lens")?;
        let enc_out = self.encoder.run(ort::inputs!["audio_signal" => feats, "length" => lens]).oc("encoder.run")?;
        let (enc_shape, enc_data) = enc_out["encoded"].try_extract_tensor::<f32>().oc("encoded")?;
        let enc_dims: Vec<i64> = enc_shape.iter().copied().collect();
        if enc_dims.len() != 3 || enc_dims[1] as usize != ENC_DIM {
            return Err(anyhow!("encoded: неожиданная форма {enc_dims:?}"));
        }
        let tp = enc_dims[2] as usize;
        let (_, len_data) = enc_out["encoded_len"].try_extract_tensor::<i32>().oc("encoded_len")?;
        let enc_len = (len_data.first().copied().unwrap_or(tp as i32) as usize).min(tp);
        // Копируем (enc_out живёт по ссылке на Session — отпускаем заём до декодера).
        let enc_data: Vec<f32> = enc_data.to_vec();
        drop(enc_out);
        let encoder_ms = t_enc.elapsed().as_millis() as u64;

        // ── Greedy transducer (как _AsrWithTransducerDecoding._decoding) ──
        let t_dec = Instant::now();
        let mut tokens: Vec<i64> = Vec::new();
        let mut h = vec![0f32; PRED_HIDDEN];
        let mut c = vec![0f32; PRED_HIDDEN];
        // Кэш dec_out при blank (decoder не перезапускаем) + (h,c) ИМЕННО того
        // прогона, что породил кэш: коммитим их при эмиссии токена (как эталон,
        // где prev_state длины 3 несёт (dec, h, c) вместе).
        let mut dec_cache: Option<Vec<f32>> = None;
        let mut pend_h = h.clone();
        let mut pend_c = c.clone();
        let mut enc_frame = vec![0f32; ENC_DIM];
        let mut t = 0usize;
        let mut emitted = 0usize;
        while t < enc_len {
            // dec_out для текущего префикса токенов (с кэшем).
            if dec_cache.is_none() {
                let last = *tokens.last().unwrap_or(&BLANK_ID);
                let x = Tensor::from_array((vec![1i64, 1], vec![last])).oc("x")?;
                let th = Tensor::from_array((vec![1i64, 1, PRED_HIDDEN as i64], h.clone())).oc("h")?;
                let tc = Tensor::from_array((vec![1i64, 1, PRED_HIDDEN as i64], c.clone())).oc("c")?;
                let out = self.decoder.run(ort::inputs!["x" => x, "h.1" => th, "c.1" => tc]).oc("decoder.run")?;
                let (_, d) = out["dec"].try_extract_tensor::<f32>().oc("dec")?;
                let (_, hh) = out["h"].try_extract_tensor::<f32>().oc("h'")?;
                let (_, cc) = out["c"].try_extract_tensor::<f32>().oc("c'")?;
                let (d, hh, cc) = (d.to_vec(), hh.to_vec(), cc.to_vec());
                dec_cache = Some(d);
                pend_h = hh;
                pend_c = cc;
            }
            let dec_out = dec_cache.clone().expect("dec_cache заполнен выше");

            // joint(enc[t] [1,768,1], dec [1,320,1]) → логиты [1025] → argmax.
            for ch in 0..ENC_DIM {
                enc_frame[ch] = enc_data[ch * tp + t];
            }
            let te = Tensor::from_array((vec![1i64, ENC_DIM as i64, 1], enc_frame.clone())).oc("enc_frame")?;
            let td = Tensor::from_array((vec![1i64, PRED_HIDDEN as i64, 1], dec_out)).oc("dec_in")?;
            let jout = self.joint.run(ort::inputs!["enc" => te, "dec" => td]).oc("joint.run")?;
            let (_, logits) = jout["joint"].try_extract_tensor::<f32>().oc("joint")?;
            let n = logits.len();
            let logits = &logits[n - VOCAB_SIZE..];
            let mut best = 0usize;
            let mut best_v = f32::NEG_INFINITY;
            for (i, &v) in logits.iter().enumerate() {
                if v > best_v {
                    best_v = v;
                    best = i;
                }
            }
            drop(jout);

            if best as i64 != BLANK_ID {
                // Эмиссия: коммитим (h,c) прогона, породившего dec_out; кэш сброс →
                // следующая итерация прогонит decoder уже с новым токеном.
                h = pend_h.clone();
                c = pend_c.clone();
                tokens.push(best as i64);
                dec_cache = None;
                emitted += 1;
                if emitted == MAX_TOKENS_PER_STEP {
                    t += 1;
                    emitted = 0;
                }
            } else {
                t += 1;
                emitted = 0;
            }
        }
        let decoder_ms = t_dec.elapsed().as_millis() as u64;

        // ── Текст: конкатенация токенов + чистка пробелов (DECODE_SPACE_PATTERN) ──
        let joined: String = tokens.iter().map(|&i| self.vocab[i as usize].as_str()).collect();
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

/// Семантика re.sub(r"\A\s|\s\B|(\s)\b", ...) из эталона: ведущие пробелы убрать;
/// внутренний пробел оставить (одним), только если дальше идёт словесный символ.
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

/// f32 → bfloat16 → f32 (round-to-nearest-even), как astype(bfloat16) в эталоне.
fn bf16_round(x: f32) -> f32 {
    let bits = x.to_bits();
    let lsb = (bits >> 16) & 1;
    f32::from_bits(bits.wrapping_add(0x7FFF + lsb) & 0xFFFF_0000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn models_dir() -> PathBuf {
        // На целевой машине модели уже скачаны (сессия 2026-06-10).
        PathBuf::from(std::env::var("LOCALAPPDATA").unwrap()).join("VoxFlow/models/gigaam")
    }

    fn read_wav_16k(p: &str) -> Vec<f32> {
        let r = hound::WavReader::open(p).expect("открыть WAV");
        let spec = r.spec();
        assert_eq!(spec.sample_rate, 16000, "тестовый WAV должен быть 16 кГц");
        match spec.sample_format {
            hound::SampleFormat::Int => {
                let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
                r.into_samples::<i32>().map(|x| x.unwrap_or(0) as f32 / max).collect()
            }
            hound::SampleFormat::Float => r.into_samples::<f32>().map(|x| x.unwrap_or(0.0)).collect(),
        }
    }

    #[test]
    fn gigaam_e2e_real_voice() {
        let dir = models_dir();
        assert!(dir_ready(&dir), "модели GigaAM не найдены в {dir:?}");
        let t0 = std::time::Instant::now();
        let mut g = GigaAm::load(&dir, 6).expect("load");
        println!("load: {} мс", t0.elapsed().as_millis());

        for wav in [
            r"C:\Users\Nezeronxer\AppData\Local\VoxFlow\dataset\20260602_124841_107.wav",
            r"C:\Users\Nezeronxer\AppData\Local\VoxFlow\dataset\20260602_125023_402.wav",
        ] {
            let samples = read_wav_16k(wav);
            let text = g.transcribe(&samples).expect("transcribe");
            let st = g.last_stats;
            println!(
                "{wav}\n  → {text:?}\n  audio={}мс frontend={}мс encoder={}мс decoder={}мс total={}мс",
                st.audio_ms, st.frontend_ms, st.encoder_ms, st.decoder_ms, st.total_ms
            );
            assert!(!text.trim().is_empty(), "пустой результат на реальном голосе");
            assert!(
                text.chars().any(|c| ('а'..='я').contains(&c.to_ascii_lowercase()) || c == 'ё' || ('А'..='Я').contains(&c)),
                "ожидалась кириллица, получено: {text:?}"
            );
            assert!(st.total_ms < 1500, "transcribe слишком медленный: {} мс", st.total_ms);
        }
    }

    #[test]
    fn clean_spaces_matches_reference() {
        // " Привет ,  мир ." → "Привет, мир."
        assert_eq!(clean_spaces(" Привет ,  мир ."), "Привет, мир.");
        assert_eq!(clean_spaces(" Включи свет"), "Включи свет");
        assert_eq!(clean_spaces(""), "");
    }
}
