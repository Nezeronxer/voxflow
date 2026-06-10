//! Silero VAD v6 (ONNX через ort, CPU). Детекция речи: гейт партиал-тиков
//! (не гонять ASR по тишине), отсев пустых диктовок, поиск пауз для сегментации
//! длинного аудио под GigaAM (его не кормят буферами >30 с).
//!
//! Интерфейс модели (v5/v6): input f32 [1,576] = 64 сэмпла контекста (хвост
//! предыдущего чанка, старт — нули) + 512 новых @16кГц; state f32 [2,1,128];
//! sr i64 = 16000. Выходы: output [1,1] = вероятность речи, stateN → обратно.

use std::path::Path;

use anyhow::Result;
use crate::gigaam::OrtCtx;
use ort::session::Session;
use ort::value::Tensor;

pub const CHUNK: usize = 512;
const CONTEXT: usize = 64;
const STATE_LEN: usize = 2 * 128;

pub struct SileroVad {
    session: Session,
    state: Vec<f32>,
    context: [f32; CONTEXT],
}

impl SileroVad {
    pub fn load(model_path: &Path) -> Result<Self> {
        let session = Session::builder()
            .oc("builder")?
            .with_intra_threads(1)
            .oc("intra_threads")? // модель крошечная, потоки только мешают
            .commit_from_file(model_path)
            .oc(&format!("загрузка Silero VAD: {model_path:?}"))?;
        Ok(Self { session, state: vec![0f32; STATE_LEN], context: [0f32; CONTEXT] })
    }

    /// Сброс стрима (новая запись): state и контекст в нули.
    pub fn reset(&mut self) {
        self.state.fill(0.0);
        self.context.fill(0.0);
    }

    /// Вероятность речи для СЛЕДУЮЩИХ 512 сэмплов потока (state/контекст внутри).
    /// Если чанк короче 512 — дополняется нулями (полезно только для хвоста).
    pub fn process_chunk(&mut self, chunk512: &[f32]) -> Result<f32> {
        let mut input = vec![0f32; CONTEXT + CHUNK];
        input[..CONTEXT].copy_from_slice(&self.context);
        let n = chunk512.len().min(CHUNK);
        input[CONTEXT..CONTEXT + n].copy_from_slice(&chunk512[..n]);

        let t_in = Tensor::from_array((vec![1i64, (CONTEXT + CHUNK) as i64], input.clone())).oc("input")?;
        let t_state = Tensor::from_array((vec![2i64, 1, 128], self.state.clone())).oc("state")?;
        let t_sr = Tensor::from_array((vec![1i64], vec![16000i64])).oc("sr")?;
        let out = self
            .session
            .run(ort::inputs!["input" => t_in, "state" => t_state, "sr" => t_sr]).oc("vad.run")?;
        let (_, prob) = out["output"].try_extract_tensor::<f32>().oc("output")?;
        let p = prob.first().copied().unwrap_or(0.0);
        let (_, st) = out["stateN"].try_extract_tensor::<f32>().oc("stateN")?;
        self.state.copy_from_slice(&st[..STATE_LEN]);
        // Контекст = последние 64 сэмпла этого чанка.
        self.context.copy_from_slice(&input[CHUNK..CHUNK + CONTEXT]);
        Ok(p)
    }

    /// Есть ли речь в буфере вообще (reset + скан с early-exit).
    pub fn has_speech(&mut self, samples_16k: &[f32], threshold: f32) -> Result<bool> {
        self.reset();
        for chunk in samples_16k.chunks(CHUNK) {
            if self.process_chunk(chunk)? >= threshold {
                self.reset();
                return Ok(true);
            }
        }
        self.reset();
        Ok(false)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn model_path() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/vad/silero_vad.onnx"))
    }

    #[test]
    fn vad_voice_vs_silence() {
        let mut vad = SileroVad::load(&model_path()).expect("load");

        // Реальный голос → речь есть, prob высокая.
        let r = hound::WavReader::open(
            r"C:\Users\Nezeronxer\AppData\Local\VoxFlow\dataset\20260602_124841_107.wav",
        )
        .expect("wav");
        let max = (1i64 << (r.spec().bits_per_sample - 1)) as f32;
        let voice: Vec<f32> = r.into_samples::<i32>().map(|x| x.unwrap_or(0) as f32 / max).collect();

        vad.reset();
        let mut max_p = 0f32;
        let t0 = std::time::Instant::now();
        let mut chunks = 0u32;
        for chunk in voice.chunks(CHUNK) {
            let p = vad.process_chunk(chunk).unwrap();
            if p > max_p {
                max_p = p;
            }
            chunks += 1;
        }
        let per_chunk_us = t0.elapsed().as_micros() / chunks.max(1) as u128;
        println!("голос: max prob = {max_p:.3}, {per_chunk_us} мкс/чанк, чанков {chunks}");
        assert!(max_p > 0.8, "на реальном голосе ожидалась prob>0.8, got {max_p}");
        assert!(per_chunk_us < 2000, "process_chunk слишком медленный: {per_chunk_us} мкс");
        assert!(vad.has_speech(&voice, 0.5).unwrap());

        // Тишина → речи нет.
        let silence = vec![0f32; 16000];
        assert!(!vad.has_speech(&silence, 0.5).unwrap(), "на нулях речи быть не должно");
    }
}
