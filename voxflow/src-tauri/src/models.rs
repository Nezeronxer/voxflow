//! Каталог и загрузка моделей с HuggingFace в %LOCALAPPDATA%\VoxFlow\models:
//! whisper (одиночные GGML .bin), GigaAM-v3 (набор ONNX-файлов в models/gigaam/)
//! и Parakeet TDT v3 (набор ONNX-файлов в models/parakeet/).
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
    /// "gigaam"/"parakeet" — каталог ONNX-файлов, "whisper" — одиночный ggml-*.bin.
    /// Зеркалится в types.ts (ModelInfo.kind) — фронт по нему рисует hero-карточку.
    pub kind: String,
}

/// Имя каталожного пункта GigaAM (это НЕ файл — набор из 4 файлов в models/gigaam/).
pub const GIGAAM_NAME: &str = "gigaam-v3";
/// Имя каталожного пункта Parakeet TDT v3 (набор из 4 файлов в models/parakeet/).
pub const PARAKEET_NAME: &str = "parakeet-v3";

/// Защита от двойного запуска загрузки каталожной модели: автозагрузка при первом
/// старте (ensure_default_models) и клик «Скачать» в UI могут прилететь
/// одновременно — два параллельных curl в один .part порвали бы файл. У
/// whisper-моделей такой гонки нет (качаются только по клику).
static GIGAAM_DOWNLOADING: AtomicBool = AtomicBool::new(false);
static PARAKEET_DOWNLOADING: AtomicBool = AtomicBool::new(false);

/// Многофайловая ONNX-модель: каталог в models/<dir> вместо одиночного .bin.
/// Механизм скачивания/докачки/удаления общий (run_download_dir), записи
/// различаются только данными.
struct DirModel {
    name: &'static str,
    label: &'static str,
    /// Суммарный размер файлов для каталога (как видит пользователь).
    size_mb: u32,
    kind: &'static str,
    base_url: &'static str,
    files: &'static [(&'static str, u64)],
    dir: fn() -> std::path::PathBuf,
    ready: fn(&std::path::Path) -> bool,
    downloading: &'static AtomicBool,
}

const DIR_MODELS: &[DirModel] = &[
    // GigaAM — ПЕРВОЙ записью: дефолтный движок, фронт ждёт её первой строкой list().
    DirModel {
        name: GIGAAM_NAME,
        label: "GigaAM-v3 — русский (рекомендуется)",
        size_mb: 217,
        kind: "gigaam",
        base_url: "https://huggingface.co/istupakov/gigaam-v3-onnx/resolve/main/",
        files: crate::gigaam::GIGAAM_FILES,
        dir: gigaam_dir,
        ready: crate::gigaam::dir_ready,
        downloading: &GIGAAM_DOWNLOADING,
    },
    DirModel {
        name: PARAKEET_NAME,
        label: "Parakeet TDT v3 — английский + автоопределение языка",
        size_mb: 640,
        kind: "parakeet",
        base_url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/",
        files: crate::parakeet::PARAKEET_FILES,
        dir: crate::paths::parakeet_dir,
        ready: crate::parakeet::dir_ready,
        downloading: &PARAKEET_DOWNLOADING,
    },
];

fn find_dir_model(name: &str) -> Option<&'static DirModel> {
    DIR_MODELS.iter().find(|m| m.name == name)
}

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
    let mut out = Vec::with_capacity(CATALOG.len() + DIR_MODELS.len());
    // Каталожные ONNX-модели — первыми строками (GigaAM — самой первой:
    // дефолтный движок), фронт рисует их hero-карточками.
    out.extend(DIR_MODELS.iter().map(|m| ModelInfo {
        name: m.name.to_string(),
        label: m.label.to_string(),
        size_mb: m.size_mb,
        installed: (m.ready)(&(m.dir)()),
        kind: m.kind.to_string(),
    }));
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
    // Каталожная ONNX-модель — это каталог, а не одиночный файл: сносим целиком
    // (вместе с .part).
    if let Some(m) = find_dir_model(name) {
        let dir = (m.dir)();
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
/// Parakeet (~640 МБ) сюда сознательно НЕ входит — качается только по кнопке в UI.
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
    if let Some(m) = find_dir_model(&name) {
        // Уже качается → молча выходим: события прогресса и так летят от первого
        // потока, второй curl поверх того же .part устроил бы кашу.
        if m.downloading
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(());
        }
        std::thread::spawn(move || {
            let r = run_download_dir(&app, m);
            m.downloading.store(false, Ordering::SeqCst);
            if let Err(e) = r {
                log::error!("download {}: {e:#}", m.name);
                let _ = app.emit(
                    "model:error",
                    // "error" читает фронт (types.ts), "message" — для обратной совместимости.
                    serde_json::json!({ "name": m.name, "error": e.to_string(), "message": e.to_string() }),
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

/// Скачать файлы каталожной ONNX-модели последовательно в её каталог. Прогресс —
/// СУММАРНЫЙ (готовые байты всех файлов / общий размер) под единым именем модели.
/// Файл с уже правильным размером — скип: докачка после обрыва сети/закрытия
/// приложения продолжает с первого недокачанного файла.
fn run_download_dir(app: &AppHandle, m: &DirModel) -> Result<()> {
    let dir = (m.dir)();
    std::fs::create_dir_all(&dir)?;
    let total: u64 = m.files.iter().map(|(_, s)| *s).sum();
    let mut base: u64 = 0; // байты уже завершённых файлов (вклад в суммарный прогресс)

    for (fname, fsize) in m.files {
        let dest = dir.join(fname);
        // Критерий готовности как в gigaam/parakeet::dir_ready: бинарь — точный
        // размер, vocab (.txt) — просто непустой (текст может прилететь с CRLF).
        let done = std::fs::metadata(&dest)
            .map(|m| {
                if fname.ends_with(".txt") {
                    m.len() > 0
                } else {
                    m.len() == *fsize
                }
            })
            .unwrap_or(false);
        if done {
            base += fsize;
            continue;
        }

        // .part уникален: расширения файлов различны, with_extension не конфликтует.
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&part);
        let url = format!("{}{fname}", m.base_url);

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
                serde_json::json!({ "name": m.name, "received": received, "total": total }),
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
        serde_json::json!({ "name": m.name, "received": total, "total": total }),
    );
    let _ = app.emit("model:done", serde_json::json!({ "name": m.name }));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Регрессия: запись Parakeet в каталоге согласована с константами parakeet.rs
    /// (имя, kind, URL, размер каталога соответствует сумме файлов).
    #[test]
    fn parakeet_dir_model_consistent() {
        let m = find_dir_model(PARAKEET_NAME).expect("parakeet-v3 должен быть в DIR_MODELS");
        assert_eq!(m.kind, "parakeet");
        assert!(
            m.base_url
                .starts_with("https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx")
                && m.base_url.ends_with('/'),
            "base_url: {}",
            m.base_url
        );
        assert_eq!(m.files.len(), crate::parakeet::PARAKEET_FILES.len());
        // size_mb каталога = сумма байтов файлов в МиБ (округление ±1).
        let mib = (m.files.iter().map(|(_, s)| *s).sum::<u64>() / 1_048_576) as i64;
        assert!(
            (m.size_mb as i64 - mib).abs() <= 1,
            "size_mb={} а файлы={mib} МиБ",
            m.size_mb
        );
    }

    /// Регрессия: GigaAM остаётся ПЕРВОЙ строкой list() (фронт рисует hero именно
    /// по ней), Parakeet присутствует, installed отражает реальное состояние диска.
    #[test]
    fn list_keeps_gigaam_first_and_reports_parakeet() {
        let l = list();
        assert_eq!(l[0].name, GIGAAM_NAME);
        let p = l
            .iter()
            .find(|m| m.name == PARAKEET_NAME)
            .expect("parakeet-v3 в list()");
        assert_eq!(p.kind, "parakeet");
        assert_eq!(
            p.installed,
            crate::parakeet::dir_ready(&crate::paths::parakeet_dir())
        );
    }

    /// Регрессия: имена каталожных ONNX-моделей не пересекаются с whisper-каталогом —
    /// иначе delete/start_download свернули бы не туда.
    #[test]
    fn dir_model_names_do_not_collide_with_whisper() {
        for m in DIR_MODELS {
            assert!(
                !CATALOG.iter().any(|e| e.name == m.name),
                "коллизия имени {}",
                m.name
            );
        }
    }
}
