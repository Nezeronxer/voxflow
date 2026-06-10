//! Каталог и загрузка моделей с HuggingFace в %LOCALAPPDATA%\VoxFlow\models:
//! whisper (одиночные GGML .bin) и GigaAM-v3 (набор ONNX-файлов в models/gigaam/).
//!
//! Качаем системным `curl` (HTTPS через SChannel на Windows) — без тяжёлого
//! HTTP-стека и без C-сборок TLS. Прогресс — поллингом размера .part-файла.

use anyhow::{anyhow, Result};
use serde::Serialize;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const NO_WINDOW: u32 = 0x08000000;

#[derive(Serialize, Clone)]
pub struct ModelInfo {
    pub name: String,
    pub label: String,
    pub size_mb: u32,
    pub installed: bool,
    /// "gigaam" — каталог ONNX-файлов (models/gigaam/), "whisper" — одиночный ggml-*.bin.
    /// Зеркалится в types.ts (ModelInfo.kind) — фронт по нему рисует hero-карточку.
    pub kind: String,
}

/// Имя каталожного пункта GigaAM (это НЕ файл — набор из 4 файлов в models/gigaam/).
pub const GIGAAM_NAME: &str = "gigaam-v3";
const GIGAAM_BASE_URL: &str = "https://huggingface.co/istupakov/gigaam-v3-onnx/resolve/main/";
/// Суммарный размер 4 файлов GigaAM (~226 МБ десятичных) для каталога.
const GIGAAM_SIZE_MB: u32 = 217;

/// Защита от двойного запуска загрузки GigaAM: автозагрузка при первом старте
/// (ensure_default_models) и клик «Скачать» в UI могут прилететь одновременно —
/// два параллельных curl в один .part порвали бы файл. У whisper-моделей такой
/// гонки нет (качаются только по клику), поэтому атомик заведён только тут.
static GIGAAM_DOWNLOADING: AtomicBool = AtomicBool::new(false);

struct Entry {
    name: &'static str,
    label: &'static str,
    size_mb: u32,
}

const CATALOG: &[Entry] = &[
    Entry {
        name: "ggml-large-v3-turbo-q5_0.bin",
        label: "Large v3 Turbo Q5 — рекомендуется (сильный русский, 574 МБ)",
        size_mb: 574,
    },
    Entry {
        name: "ggml-large-v3-turbo.bin",
        label: "Large v3 Turbo — мощная, тяжелее (1.6 ГБ)",
        size_mb: 1620,
    },
    Entry {
        name: "ggml-large-v3.bin",
        label: "Large v3 — максимальная точность, медленнее (3.1 ГБ)",
        size_mb: 3100,
    },
    Entry {
        name: "ggml-base.bin",
        label: "Base — быстрая, для слабых ПК (148 МБ)",
        size_mb: 148,
    },
    Entry {
        name: "ggml-small.bin",
        label: "Small — компромисс качество/скорость (488 МБ)",
        size_mb: 488,
    },
];

/// Каталог models/gigaam/ (4 ONNX/vocab файла GigaAM-v3).
fn gigaam_dir() -> std::path::PathBuf {
    crate::paths::models_dir().join(crate::gigaam::GIGAAM_DIR)
}

pub fn list() -> Vec<ModelInfo> {
    let mut out = Vec::with_capacity(CATALOG.len() + 1);
    // GigaAM — ПЕРВОЙ строкой: дефолтный движок, фронт рисует её hero-карточкой.
    out.push(ModelInfo {
        name: GIGAAM_NAME.to_string(),
        label: "GigaAM-v3 — русский (рекомендуется)".to_string(),
        size_mb: GIGAAM_SIZE_MB,
        installed: crate::gigaam::dir_ready(&gigaam_dir()),
        kind: "gigaam".to_string(),
    });
    out.extend(CATALOG.iter().map(|e| ModelInfo {
        name: e.name.to_string(),
        label: e.label.to_string(),
        size_mb: e.size_mb,
        installed: crate::paths::model_path(e.name).exists(),
        kind: "whisper".to_string(),
    }));
    out
}

fn url_for(name: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{name}")
}

fn catalog_size(name: &str) -> u64 {
    CATALOG
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.size_mb as u64 * 1_000_000)
        .unwrap_or(0)
}

pub fn delete(name: &str) -> Result<()> {
    // GigaAM — это каталог, а не одиночный файл: сносим целиком (вместе с .part).
    if name == GIGAAM_NAME {
        let dir = gigaam_dir();
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        return Ok(());
    }
    let p = crate::paths::model_path(name);
    if p.exists() {
        std::fs::remove_file(p)?;
    }
    Ok(())
}

/// Автозагрузка дефолтной модели при первом запуске: GigaAM не на месте и не
/// качается → стартуем скачивание в фоне. Зовётся интегратором из setup (lib.rs);
/// прогресс фронт увидит обычными событиями model:progress под именем "gigaam-v3".
pub fn ensure_default_models(app: AppHandle) {
    if crate::gigaam::dir_ready(&gigaam_dir()) {
        return;
    }
    // Дубликат (гонка с кликом «Скачать» в UI) глушится атомиком в start_download.
    if let Err(e) = start_download(app, GIGAAM_NAME.to_string()) {
        log::error!("ensure_default_models: {e:#}");
    }
}

/// Запустить загрузку в фоновом потоке. События: `model:progress` / `model:done` / `model:error`.
pub fn start_download(app: AppHandle, name: String) -> Result<()> {
    if name == GIGAAM_NAME {
        // Уже качается → молча выходим: события прогресса и так летят от первого
        // потока, второй curl поверх того же .part устроил бы кашу.
        if GIGAAM_DOWNLOADING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(());
        }
        std::thread::spawn(move || {
            let r = run_download_gigaam(&app);
            GIGAAM_DOWNLOADING.store(false, Ordering::SeqCst);
            if let Err(e) = r {
                log::error!("download {GIGAAM_NAME}: {e:#}");
                let _ = app.emit(
                    "model:error",
                    // "error" читает фронт (types.ts), "message" — для обратной совместимости.
                    serde_json::json!({ "name": GIGAAM_NAME, "error": e.to_string(), "message": e.to_string() }),
                );
            }
        });
        return Ok(());
    }
    if !CATALOG.iter().any(|e| e.name == name) {
        return Err(anyhow!("Неизвестная модель: {name}"));
    }
    std::thread::spawn(move || {
        if let Err(e) = run_download(&app, &name) {
            log::error!("download {name}: {e:#}");
            let _ = app.emit(
                "model:error",
                serde_json::json!({ "name": name, "error": e.to_string(), "message": e.to_string() }),
            );
        }
    });
    Ok(())
}

/// Скачать 4 файла GigaAM последовательно в models/gigaam/. Прогресс — СУММАРНЫЙ
/// (готовые байты всех файлов / общий размер) под единым именем "gigaam-v3".
/// Файл с уже правильным размером — скип: докачка после обрыва сети/закрытия
/// приложения продолжает с первого недокачанного файла.
fn run_download_gigaam(app: &AppHandle) -> Result<()> {
    let dir = gigaam_dir();
    std::fs::create_dir_all(&dir)?;
    let total: u64 = crate::gigaam::GIGAAM_FILES.iter().map(|(_, s)| *s).sum();
    let mut base: u64 = 0; // байты уже завершённых файлов (вклад в суммарный прогресс)

    for (fname, fsize) in crate::gigaam::GIGAAM_FILES {
        let dest = dir.join(fname);
        // Критерий готовности как в gigaam::dir_ready: бинарь — точный размер,
        // vocab (.txt) — просто непустой (текст может прилететь с CRLF).
        let done = std::fs::metadata(&dest)
            .map(|m| if fname.ends_with(".txt") { m.len() > 0 } else { m.len() == *fsize })
            .unwrap_or(false);
        if done {
            base += fsize;
            continue;
        }

        // .part уникален: расширения файлов различны, with_extension не конфликтует.
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&part);
        let url = format!("{GIGAAM_BASE_URL}{fname}");

        let mut cmd = Command::new("curl");
        cmd.arg("-L")
            .arg("--fail")
            .arg("--silent")
            .arg("--show-error")
            .arg("-o")
            .arg(&part)
            .arg(&url);
        #[cfg(windows)]
        cmd.creation_flags(NO_WINDOW);

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;

        loop {
            std::thread::sleep(Duration::from_millis(400));
            let received = base + std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            let _ = app.emit(
                "model:progress",
                serde_json::json!({ "name": GIGAAM_NAME, "received": received, "total": total }),
            );
            if let Some(status) = child.try_wait()? {
                if status.success() {
                    std::fs::rename(&part, &dest)?;
                    base += fsize;
                    break;
                } else {
                    let _ = std::fs::remove_file(&part);
                    return Err(anyhow!("curl ({fname}) завершился с ошибкой: {status}"));
                }
            }
        }
    }

    let _ = app.emit(
        "model:progress",
        serde_json::json!({ "name": GIGAAM_NAME, "received": total, "total": total }),
    );
    let _ = app.emit("model:done", serde_json::json!({ "name": GIGAAM_NAME }));
    Ok(())
}

fn run_download(app: &AppHandle, name: &str) -> Result<()> {
    let dest = crate::paths::model_path(name);
    let part = dest.with_extension("part");
    let _ = std::fs::remove_file(&part);
    let url = url_for(name);
    let total = content_length(&url).unwrap_or_else(|| catalog_size(name));

    let mut cmd = Command::new("curl");
    cmd.arg("-L")
        .arg("--fail")
        .arg("--silent")
        .arg("--show-error")
        .arg("-o")
        .arg(&part)
        .arg(&url);
    #[cfg(windows)]
    cmd.creation_flags(NO_WINDOW);

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;

    loop {
        std::thread::sleep(Duration::from_millis(400));
        let received = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let _ = app.emit(
            "model:progress",
            serde_json::json!({ "name": name, "received": received, "total": total }),
        );
        if let Some(status) = child.try_wait()? {
            if status.success() {
                std::fs::rename(&part, &dest)?;
                let _ = app.emit(
                    "model:progress",
                    serde_json::json!({ "name": name, "received": total, "total": total }),
                );
                let _ = app.emit("model:done", serde_json::json!({ "name": name }));
                return Ok(());
            } else {
                let _ = std::fs::remove_file(&part);
                return Err(anyhow!("curl завершился с ошибкой: {status}"));
            }
        }
    }
}

/// Узнать размер файла через HEAD (для точного прогресс-бара). Best-effort.
fn content_length(url: &str) -> Option<u64> {
    let mut cmd = Command::new("curl");
    cmd.arg("-sIL").arg(url);
    #[cfg(windows)]
    cmd.creation_flags(NO_WINDOW);
    let out = cmd.output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut last = None;
    for line in text.lines() {
        let l = line.to_ascii_lowercase();
        if let Some(rest) = l.strip_prefix("content-length:") {
            if let Ok(n) = rest.trim().parse::<u64>() {
                last = Some(n);
            }
        }
    }
    last
}
