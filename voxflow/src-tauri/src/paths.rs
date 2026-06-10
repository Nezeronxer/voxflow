//! Resolution of data/model/tmp/resource paths.
//!
//! ВАЖНО (R5): whisper.cpp открывает файлы через ANSI-codepage, поэтому пути к
//! МОДЕЛИ и временным WAV должны быть ASCII. Их кладём в %LOCALAPPDATA%\VoxFlow
//! (юзернейм ASCII). Сами exe/DLL грузятся ОС по wide-пути — им кириллица ок.

use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// %LOCALAPPDATA%\VoxFlow (ASCII), создаётся при первом обращении.
pub fn data_dir() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
    let d = base.join("VoxFlow");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn models_dir() -> PathBuf {
    let d = data_dir().join("models");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn tmp_dir() -> PathBuf {
    let d = data_dir().join("tmp");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Каталог датасета персонализации (аудио-сэмплы пользователя).
pub fn dataset_dir() -> PathBuf {
    let d = data_dir().join("dataset");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn db_path() -> PathBuf {
    data_dir().join("voxflow.db")
}

pub fn model_path(name: &str) -> PathBuf {
    models_dir().join(name)
}

/// Каталог моделей GigaAM (models/gigaam, ASCII-путь — ort открывает по wide,
/// но единообразие с whisper-моделями дешевле, чем особые случаи).
pub fn gigaam_dir() -> PathBuf {
    let d = models_dir().join(crate::gigaam::GIGAAM_DIR);
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Dev-копия Silero VAD (вшита на этапе компиляции).
const DEV_VAD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/vad/silero_vad.onnx");

/// silero_vad.onnx: в проде — из resource_dir, в dev/standalone — из dev-копии.
pub fn vad_model_path(app: Option<&AppHandle>) -> PathBuf {
    if let Some(app) = app {
        if let Ok(r) = app.path().resource_dir() {
            for c in [r.join("resources").join("vad").join("silero_vad.onnx"), r.join("vad").join("silero_vad.onnx")] {
                if c.exists() {
                    return c;
                }
            }
        }
    }
    // Прод-фолбэк без Tauri-контекста: рядом с exe (так раскладывает инсталлятор).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let c = dir.join("resources").join("vad").join("silero_vad.onnx");
            if c.exists() {
                return c;
            }
        }
    }
    PathBuf::from(DEV_VAD)
}

/// Dev-копии бинарей whisper (вшиты на этапе компиляции): CPU и CUDA.
const DEV_WHISPER_CPU: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/whisper/Release");
const DEV_WHISPER_CUDA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/whisper-cuda/Release");

/// Есть ли NVIDIA-GPU с драйвером (наличие nvcuda.dll в System32).
pub fn has_nvidia() -> bool {
    let sys = std::env::var("SystemRoot").unwrap_or_else(|_| "C:/Windows".into());
    std::path::Path::new(&sys)
        .join("System32")
        .join("nvcuda.dll")
        .exists()
}

/// Каталог с whisper-cli.exe / whisper-server.exe + DLL.
/// При наличии NVIDIA приоритет у CUDA-сборки (whisper-cuda), иначе CPU (whisper).
/// В проде — из resource_dir, в dev — из dev-копий.
pub fn whisper_dir(app: &AppHandle) -> PathBuf {
    let cli = whisper_cli_name();
    let gpu = has_nvidia();
    let res = app.path().resource_dir().ok();
    let mut candidates: Vec<PathBuf> = Vec::new();

    // GPU-сборки имеют приоритет (и resource, и dev) над CPU.
    if gpu {
        if let Some(r) = &res {
            candidates.push(r.join("resources").join("whisper-cuda").join("Release"));
            candidates.push(r.join("whisper-cuda"));
        }
        candidates.push(PathBuf::from(DEV_WHISPER_CUDA));
    }
    // CPU-сборки.
    if let Some(r) = &res {
        candidates.push(r.join("resources").join("whisper").join("Release"));
        candidates.push(r.join("whisper"));
    }
    candidates.push(PathBuf::from(DEV_WHISPER_CPU));

    for c in candidates {
        if c.join(cli).exists() {
            return c;
        }
    }
    PathBuf::from(DEV_WHISPER_CPU)
}

/// Dev-каталог whisper (для `--selftest` без Tauri-контекста): CUDA при наличии GPU.
pub fn whisper_dir_standalone() -> PathBuf {
    if has_nvidia() {
        let p = PathBuf::from(DEV_WHISPER_CUDA);
        if p.join(whisper_cli_name()).exists() {
            return p;
        }
    }
    PathBuf::from(DEV_WHISPER_CPU)
}

pub fn whisper_cli_name() -> &'static str {
    if cfg!(windows) { "whisper-cli.exe" } else { "whisper-cli" }
}

#[allow(dead_code)] // задел под whisper-server (persistent), пока используем cli one-shot
pub fn whisper_server_name() -> &'static str {
    if cfg!(windows) { "whisper-server.exe" } else { "whisper-server" }
}
