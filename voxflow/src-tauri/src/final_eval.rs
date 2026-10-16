//! Замер точности ФИНАЛЬНОГО локального распознавания (D-036).
//!
//! Эталона нет, поэтому меряем расхождение с псевдоэталоном (Whisper large-v3,
//! файлы `<имя>.txt` в `VOXFLOW_EVAL_REF`) на выборке из `VOXFLOW_EVAL_LIST`.
//! Записи в `dataset/` уже прошли normalize + trim + компакт пауз — это ровно
//! тот буфер, который боевой финал отдаёт `local_transcribe_long`.
//!
//! `VOXFLOW_TEST_DATA_DIR=... VOXFLOW_EVAL_LIST=... VOXFLOW_EVAL_REF=... \
//!  cargo test --lib final_accuracy_eval -- --ignored --nocapture`

use std::path::Path;

use crate::vad::{SileroVad, CHUNK};

#[derive(Clone, Copy, Debug)]
pub enum Cut {
    /// Боевое поведение: последний нечевой чанк перед лимитом.
    LastNonSpeech,
    /// Середина самой длинной паузы в окне (с) перед лимитом.
    LongestPauseMid(usize),
}

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub thr: f32,
    pub pad: usize,
    pub max_seg: usize,
    pub cut: Cut,
    pub sil_split: usize,
    pub dc: bool,
    /// Привести p95 покадрового RMS к этому уровню (и вверх, и вниз).
    pub rms_target: Option<f32>,
}

impl Params {
    pub const BASE: Params = Params {
        thr: 0.35,
        pad: 4800,
        max_seg: 25 * 16000,
        cut: Cut::LastNonSpeech,
        sil_split: 600 * 16,
        dc: false,
        rms_target: None,
    };
}

pub fn vad_probs(vad: &mut SileroVad, samples: &[f32]) -> Vec<f32> {
    vad.reset();
    let p = samples
        .chunks(CHUNK)
        .map(|c| vad.process_chunk(c).unwrap_or(1.0))
        .collect();
    vad.reset();
    p
}

fn frame_rms_p95(samples: &[f32]) -> f32 {
    let mut v: Vec<f32> = samples
        .chunks(320)
        .map(|f| (f.iter().map(|x| x * x).sum::<f32>() / f.len() as f32).sqrt())
        .collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f32 * 0.95) as usize]
}

pub fn preprocess(samples: &[f32], p: &Params) -> Vec<f32> {
    let mut s = samples.to_vec();
    if p.dc && !s.is_empty() {
        let m = s.iter().sum::<f32>() / s.len() as f32;
        s.iter_mut().for_each(|x| *x -= m);
    }
    if let Some(t) = p.rms_target {
        let l = frame_rms_p95(&s);
        if l > 1e-4 {
            let peak = s.iter().fold(0f32, |m, x| m.max(x.abs())).max(1e-6);
            let g = (t / l).min(0.98 / peak);
            s.iter_mut().for_each(|x| *x *= g);
        }
    }
    s
}

/// Копия логики `local_transcribe_long` с параметрами; без постобработки сегментов.
pub fn long_variant<F>(
    probs: &[f32],
    samples: &[f32],
    p: &Params,
    transcribe: &mut F,
) -> anyhow::Result<String>
where
    F: FnMut(&[f32]) -> anyhow::Result<String>,
{
    let chunk = CHUNK;
    let speech: Vec<bool> = probs.iter().map(|&x| x >= p.thr).collect();
    if !speech.iter().any(|&x| x) {
        return if samples.len() <= p.max_seg {
            transcribe(samples)
        } else {
            Ok(String::new())
        };
    }
    let n = speech.len();
    let mut units: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < n {
        if !speech[i] {
            i += 1;
            continue;
        }
        let s0 = i;
        let mut last = i;
        let mut j = i + 1;
        while j < n {
            if speech[j] {
                last = j;
                j += 1;
                continue;
            }
            let mut k = j;
            while k < n && !speech[k] {
                k += 1;
            }
            if (k - j) * chunk >= p.sil_split {
                break;
            }
            j = k;
        }
        units.push((s0 * chunk, ((last + 1) * chunk).min(samples.len())));
        i = j.max(last + 1);
    }
    let mut out: Vec<String> = Vec::new();
    for &(us, ue) in &units {
        let s0 = us.saturating_sub(p.pad);
        let e0 = (ue + p.pad).min(samples.len());
        let mut start = s0;
        while start < e0 {
            let limit = (start + p.max_seg).min(e0);
            let mut cut = limit;
            if limit < e0 {
                let from = start / chunk + 1;
                let to = (limit / chunk).min(speech.len());
                match p.cut {
                    Cut::LastNonSpeech => {
                        for c in (from..to).rev() {
                            if !speech[c] {
                                cut = c * chunk;
                                break;
                            }
                        }
                    }
                    Cut::LongestPauseMid(win_s) => {
                        let lo = from.max(to.saturating_sub(win_s * 16000 / chunk));
                        // (длина, конец) самой длинной паузы; при равенстве позже.
                        let mut best: Option<(usize, usize)> = None;
                        let mut c = lo;
                        while c < to {
                            if speech[c] {
                                c += 1;
                                continue;
                            }
                            let a = c;
                            while c < to && !speech[c] {
                                c += 1;
                            }
                            if best.is_none_or(|(l, _)| c - a >= l) {
                                best = Some((c - a, c));
                            }
                        }
                        if let Some((l, e)) = best {
                            cut = (e - l / 2) * chunk;
                        }
                    }
                }
            }
            let t = transcribe(&samples[start..cut])?;
            if !t.trim().is_empty() {
                out.push(t.trim().to_string());
            }
            start = cut;
        }
    }
    Ok(out.join(" "))
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace('ё', "е")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn word_errors(reference: &str, hyp: &str) -> (usize, usize) {
    let r = words(reference);
    let h = words(hyp);
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    let mut cur = vec![0usize; h.len() + 1];
    for (i, rw) in r.iter().enumerate() {
        cur[0] = i + 1;
        for (j, hw) in h.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(rw != hw))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    (prev[h.len()], r.len())
}

fn read_wav(path: &Path) -> Vec<f32> {
    let r = hound::WavReader::open(path).expect("wav");
    let max = (1i64 << (r.spec().bits_per_sample - 1)) as f32;
    r.into_samples::<i32>()
        .map(|x| x.unwrap_or(0) as f32 / max)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// CTC-голова GigaAM (один ONNX, greedy: argmax, склейка повторов, без blank).
    struct Ctc {
        sess: ort::session::Session,
        vocab: Vec<String>,
    }

    impl Ctc {
        fn load(dir: &Path, threads: usize) -> Self {
            let sess = ort::session::Session::builder()
                .unwrap()
                .with_intra_threads(threads)
                .unwrap()
                .commit_from_file(dir.join("v3_e2e_ctc.onnx"))
                .unwrap();
            println!(
                "ctc inputs {:?} outputs {:?}",
                sess.inputs()
                    .iter()
                    .map(|i| i.name().to_string())
                    .collect::<Vec<_>>(),
                sess.outputs()
                    .iter()
                    .map(|i| i.name().to_string())
                    .collect::<Vec<_>>()
            );
            let raw = std::fs::read_to_string(dir.join("v3_e2e_ctc_vocab.txt")).unwrap();
            let mut vocab = vec![String::new(); 257];
            for l in raw.lines() {
                if let Some((t, id)) = l.trim_end_matches('\r').rsplit_once(' ') {
                    if let Ok(i) = id.parse::<usize>() {
                        vocab[i] = t.replace('\u{2581}', " ");
                    }
                }
            }
            Ctc { sess, vocab }
        }

        fn transcribe(&mut self, g: &crate::gigaam::GigaAm, samples: &[f32]) -> String {
            if samples.len() < 320 {
                return String::new();
            }
            let (feats, n) = g.log_mel(samples);
            let f = ort::value::Tensor::from_array((vec![1i64, 64, n as i64], feats)).unwrap();
            let l = ort::value::Tensor::from_array((vec![1i64], vec![n as i64])).unwrap();
            let out = self
                .sess
                .run(ort::inputs!["features" => f, "feature_lengths" => l])
                .unwrap();
            let (shape, data) = out[0].try_extract_tensor::<f32>().unwrap();
            let (t, v) = (shape[1] as usize, shape[2] as usize);
            assert_eq!(v, 257, "словарь CTC");
            let mut prev = usize::MAX;
            let mut s = String::new();
            for i in 0..t {
                let row = &data[i * v..(i + 1) * v];
                let best = (0..v).max_by(|&a, &b| row[a].total_cmp(&row[b])).unwrap();
                if best != prev && best != 256 {
                    s.push_str(&self.vocab[best]);
                }
                prev = best;
            }
            crate::gigaam::clean_spaces(&s)
        }
    }

    #[test]
    #[ignore = "requires local models, private dataset and a pseudo-reference directory"]
    fn final_accuracy_eval() {
        let list = std::env::var("VOXFLOW_EVAL_LIST").expect("VOXFLOW_EVAL_LIST");
        let refdir = std::env::var("VOXFLOW_EVAL_REF").expect("VOXFLOW_EVAL_REF");
        let only = std::env::var("VOXFLOW_EVAL_ONLY").ok();
        // VOXFLOW_EVAL_MODEL=int8|fp32, VOXFLOW_EVAL_DIR - каталог весов (по умолчанию боевой).
        let dir = std::env::var("VOXFLOW_EVAL_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| crate::paths::gigaam_dir());
        let files = match std::env::var("VOXFLOW_EVAL_MODEL").as_deref() {
            Ok("fp32") => crate::gigaam::FP32_FILES,
            _ => crate::gigaam::INT8_FILES,
        };
        let ctc_mode = std::env::var("VOXFLOW_EVAL_MODEL").as_deref() == Ok("ctc");
        let mut g = if ctc_mode {
            // фронтенд общий, веса RNNT нужны только ради log_mel
            crate::gigaam::GigaAm::load(&crate::paths::gigaam_dir(), 6).expect("gigaam")
        } else {
            crate::gigaam::GigaAm::load_files(&dir, 6, &files).expect("gigaam")
        };
        let mut ctc = ctc_mode.then(|| Ctc::load(&dir, 6));
        let mut vad = SileroVad::load(&crate::paths::vad_model_path(None)).expect("vad");
        let _ = g.transcribe(&vec![0.0f32; 8000]);

        let b = Params::BASE;
        let variants: Vec<(&str, Params)> = vec![
            ("base", b),
            ("pad150", Params { pad: 2400, ..b }),
            ("pad500", Params { pad: 8000, ..b }),
            ("thr0.25", Params { thr: 0.25, ..b }),
            ("thr0.5", Params { thr: 0.5, ..b }),
            (
                "seg15",
                Params {
                    max_seg: 15 * 16000,
                    ..b
                },
            ),
            (
                "seg20",
                Params {
                    max_seg: 20 * 16000,
                    ..b
                },
            ),
            (
                "seg28",
                Params {
                    max_seg: 28 * 16000,
                    ..b
                },
            ),
            (
                "cut_longest8s",
                Params {
                    cut: Cut::LongestPauseMid(8),
                    ..b
                },
            ),
            (
                "cut_longest4s",
                Params {
                    cut: Cut::LongestPauseMid(4),
                    ..b
                },
            ),
            ("dc", Params { dc: true, ..b }),
            (
                "rms0.06",
                Params {
                    rms_target: Some(0.06),
                    ..b
                },
            ),
            (
                "rms0.10",
                Params {
                    rms_target: Some(0.10),
                    ..b
                },
            ),
        ];

        let files: Vec<String> = std::fs::read_to_string(&list)
            .unwrap()
            .lines()
            .map(str::to_string)
            .filter(|l| !l.is_empty())
            .collect();
        // имя варианта -> (бакет -> (ошибки, слов))
        let mut acc: BTreeMap<&str, BTreeMap<&str, (usize, usize)>> = BTreeMap::new();
        let mut per_file: Vec<(String, f64, f32)> = Vec::new();
        let mut used = 0;
        // RTF: время распознавания (без VAD) / длина звука, только вариант base.
        let (mut asr_ms, mut audio_ms) = (0u128, 0u128);
        // время файлов длиной 9-11 с (мс на ~10-секундной записи)
        let mut ten: Vec<u128> = Vec::new();
        for f in &files {
            let stem = Path::new(f)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string();
            let Ok(reference) = std::fs::read_to_string(format!("{refdir}/{stem}.txt")) else {
                continue;
            };
            if words(&reference).len() < 2 {
                continue;
            }
            let samples = read_wav(Path::new(f));
            let secs = samples.len() as f32 / 16000.0;
            let bucket = if secs > 25.0 { "long" } else { "short" };
            used += 1;
            let mut base_cache: Option<Vec<f32>> = None;
            for (name, p) in &variants {
                if let Some(o) = &only {
                    if !o.split(',').any(|x| x == *name) && *name != "base" {
                        continue;
                    }
                }
                let s = preprocess(&samples, p);
                let probs = if p.dc || p.rms_target.is_some() {
                    vad_probs(&mut vad, &s)
                } else {
                    base_cache
                        .get_or_insert_with(|| vad_probs(&mut vad, &samples))
                        .clone()
                };
                let t0 = std::time::Instant::now();
                let text = long_variant(&probs, &s, p, &mut |seg| match ctc.as_mut() {
                    Some(c) => Ok(c.transcribe(&g, seg)),
                    None => g.transcribe(seg),
                })
                .unwrap();
                if *name == "base" {
                    let ms = t0.elapsed().as_millis();
                    asr_ms += ms;
                    audio_ms += (secs * 1000.0) as u128;
                    if (9.0..=11.0).contains(&secs) {
                        ten.push(ms);
                    }
                }
                let (e, n) = word_errors(&reference, &text);
                for bk in ["all", bucket] {
                    let a = acc.entry(name).or_default().entry(bk).or_default();
                    a.0 += e;
                    a.1 += n;
                }
                if *name == "base" {
                    per_file.push((stem.clone(), e as f64 / n as f64, secs));
                }
            }
        }
        println!("файлов с эталоном: {used}");
        println!(
            "RTF base: {:.4} ({asr_ms} мс на {audio_ms} мс звука); файлов 9-11 с: {}, среднее {:.0} мс",
            asr_ms as f64 / audio_ms.max(1) as f64,
            ten.len(),
            ten.iter().sum::<u128>() as f64 / ten.len().max(1) as f64
        );
        per_file.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        for (s, w, d) in per_file.iter().take(8) {
            println!("  худшие base: {s} WER {:.2} ({d:.0} с)", w);
        }
        println!(
            "{:<16} {:>8} {:>8} {:>8}",
            "вариант", "все", "long", "short"
        );
        for (name, m) in &acc {
            let r = |k: &str| {
                m.get(k)
                    .map(|(e, n)| format!("{:.2}%", 100.0 * *e as f64 / (*n).max(1) as f64))
                    .unwrap_or_default()
            };
            println!(
                "{:<16} {:>8} {:>8} {:>8}",
                name,
                r("all"),
                r("long"),
                r("short")
            );
        }
    }
}
