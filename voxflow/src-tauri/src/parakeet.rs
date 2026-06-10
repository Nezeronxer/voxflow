//! NVIDIA Parakeet TDT 0.6B v3 — мультиязычный ASR (EN + 24 языка, включая RU).
//! ONNX через ort (CPU, int8), модель istupakov/parakeet-tdt-0.6b-v3-onnx (CC-BY-4.0).
//! Используется для language="en" и автодетекта языка (language="auto").
//!
//! ЗАПОЛНЯЕТСЯ агентом P (волна 1): сейчас здесь каркас с константами каталога,
//! чтобы крейт компилировался у параллельных агентов.

use std::path::Path;

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
