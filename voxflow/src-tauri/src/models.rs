//! Каталог и загрузка моделей с HuggingFace в %LOCALAPPDATA%\VoxFlow\models:
//! GigaAM-v3 (набор ONNX-файлов в models/gigaam/) и Parakeet TDT v3
//! (models/parakeet/). Whisper убран (D-065).
//!
//! Качаем системным `curl` (HTTPS через SChannel на Windows) — без тяжёлого
//! HTTP-стека и без C-сборок TLS. Прогресс — поллингом размера .part-файла.

use anyhow::{anyhow, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Clone)]
pub struct ModelInfo {
    pub name: String,
    pub label: String,
    pub size_mb: u32,
    pub installed: bool,
    /// "gigaam"/"parakeet" — каталог ONNX-файлов.
    /// Зеркалится в types.ts (ModelInfo.kind) — фронт по нему рисует hero-карточку.
    pub kind: String,
    /// Сколько модель занимает на диске сейчас (МБ, по настоящим файлам);
    /// `None` - не скачана.
    pub disk_mb: Option<u64>,
    /// Нужна ли модель при текущих настройках (считает `list_models`, D-053).
    pub in_use: bool,
}

/// Размер файлов на диске в МБ (вверх до целого), каталоги - рекурсивно.
/// `None`, если ни одного пути нет. Ссылки не раскрываются.
pub(crate) fn disk_mb(paths: &[std::path::PathBuf]) -> Option<u64> {
    fn bytes(p: &std::path::Path) -> Option<u64> {
        let meta = std::fs::symlink_metadata(p).ok()?;
        if meta.is_dir() {
            let total = std::fs::read_dir(p)
                .ok()?
                .flatten()
                .filter_map(|e| bytes(&e.path()))
                .sum();
            Some(total)
        } else {
            Some(meta.len())
        }
    }
    let found: Vec<u64> = paths.iter().filter_map(|p| bytes(p)).collect();
    if found.is_empty() {
        return None;
    }
    let total: u64 = found.iter().sum();
    Some(total.div_ceil(1024 * 1024))
}

/// Файлы модели на диске - то, что освобождает `delete`.
fn model_paths(name: &str) -> Vec<std::path::PathBuf> {
    find_dir_model(name)
        .map(|m| vec![(m.dir)()])
        .unwrap_or_default()
}

/// Имя каталожного пункта GigaAM (это НЕ файл — набор из 4 файлов в models/gigaam/).
pub const GIGAAM_NAME: &str = "gigaam-v3";
/// Полная (fp32) GigaAM-v3: точнее int8 (D-038, D-040), своя папка рядом с int8.
pub const GIGAAM_FP32_NAME: &str = "gigaam-v3-fp32";
/// Имя каталожного пункта Parakeet TDT v3 (набор из 4 файлов в models/parakeet/).
pub const PARAKEET_NAME: &str = "parakeet-v3";
/// Единственный рекомендованный Whisper-пресет: multilingual, пригоден для RU/EN
/// и смешанной речи, но не требует 1.6–3.1 ГБ на диске.
static GIGAAM_DOWNLOADING: AtomicBool = AtomicBool::new(false);
static GIGAAM_FP32_DOWNLOADING: AtomicBool = AtomicBool::new(false);
static PARAKEET_DOWNLOADING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct Artifact {
    name: &'static str,
    size_bytes: u64,
    sha256: &'static str,
}

const GIGAAM_ARTIFACTS: &[Artifact] = &[
    Artifact {
        name: "v3_e2e_rnnt_encoder.int8.onnx",
        size_bytes: 224_570_477,
        sha256: "4e0e076a6076cd110277e529b8ac8f32cd5297f7fbebad5341ae8ddb7d00817b",
    },
    Artifact {
        name: "v3_e2e_rnnt_decoder.int8.onnx",
        size_bytes: 1_159_170,
        sha256: "89014e134865615b91e037157e46e389b1271e6072460efc010ea08e61e23146",
    },
    Artifact {
        name: "v3_e2e_rnnt_joint.int8.onnx",
        size_bytes: 687_791,
        sha256: "ade116563dbf66e503b0994efab6b5861412743e52bf31c39fc3fffa3783d5d1",
    },
    Artifact {
        name: "v3_e2e_rnnt_vocab.txt",
        size_bytes: 13_354,
        sha256: "39abae20e692998290c574e606f11a9edef2902a1995463fcff63d1490cf22b7",
    },
];

const GIGAAM_FP32_ARTIFACTS: &[Artifact] = &[
    Artifact {
        name: "v3_e2e_rnnt_encoder.onnx",
        size_bytes: 885_084_534,
        sha256: "cd60b3764a832e8560ae6d3ad0b10adc1a42ffae412b9476f25620aae4f4a508",
    },
    Artifact {
        name: "v3_e2e_rnnt_decoder.onnx",
        size_bytes: 4_599_910,
        sha256: "7b0a16d67fd2cb37061decc93c69e364a9ab27afee3c57495d55b1c974cf7231",
    },
    Artifact {
        name: "v3_e2e_rnnt_joint.onnx",
        size_bytes: 2_712_896,
        sha256: "602ff7017a93311aad34df1437c8d7f49911353c13d6eae7a6ee7b041339465c",
    },
    Artifact {
        name: "v3_e2e_rnnt_vocab.txt",
        size_bytes: 13_354,
        sha256: "39abae20e692998290c574e606f11a9edef2902a1995463fcff63d1490cf22b7",
    },
];

const PARAKEET_ARTIFACTS: &[Artifact] = &[
    Artifact {
        name: "encoder-model.int8.onnx",
        size_bytes: 652_183_999,
        sha256: "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09",
    },
    Artifact {
        name: "decoder_joint-model.int8.onnx",
        size_bytes: 18_202_004,
        sha256: "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70",
    },
    Artifact {
        name: "nemo128.onnx",
        size_bytes: 139_764,
        sha256: "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f",
    },
    Artifact {
        name: "vocab.txt",
        size_bytes: 93_939,
        sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
    },
];

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
    files: &'static [Artifact],
    dir: fn() -> std::path::PathBuf,
    downloading: &'static AtomicBool,
}

const DIR_MODELS: &[DirModel] = &[
    // GigaAM — первой ONNX-записью: фронт рисует hero именно по порядку каталога.
    DirModel {
        name: GIGAAM_NAME,
        label: "GigaAM-v3 — быстрый русский",
        size_mb: 217,
        kind: "gigaam",
        base_url: "https://huggingface.co/istupakov/gigaam-v3-onnx/resolve/322c3b29492673eb7d0b434bfa9dfb8653e34d02/",
        files: GIGAAM_ARTIFACTS,
        dir: gigaam_dir,
        downloading: &GIGAAM_DOWNLOADING,
    },
    DirModel {
        name: GIGAAM_FP32_NAME,
        label: "GigaAM-v3 полная - точнее (885 МБ)",
        size_mb: 885,
        kind: "gigaam",
        base_url: "https://huggingface.co/istupakov/gigaam-v3-onnx/resolve/322c3b29492673eb7d0b434bfa9dfb8653e34d02/",
        files: GIGAAM_FP32_ARTIFACTS,
        dir: gigaam_fp32_dir,
        downloading: &GIGAAM_FP32_DOWNLOADING,
    },
    DirModel {
        name: PARAKEET_NAME,
        label: "Parakeet TDT v3 — быстрый English (EN и Авто)",
        size_mb: 640,
        kind: "parakeet",
        base_url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce/",
        files: PARAKEET_ARTIFACTS,
        dir: crate::paths::parakeet_dir,
        downloading: &PARAKEET_DOWNLOADING,
    },
];

fn find_dir_model(name: &str) -> Option<&'static DirModel> {
    DIR_MODELS.iter().find(|m| m.name == name)
}

/// Каталог models/gigaam/ (4 ONNX/vocab файла GigaAM-v3).
fn gigaam_dir() -> std::path::PathBuf {
    crate::paths::models_dir().join(crate::gigaam::GIGAAM_DIR)
}

/// Каталог models/gigaam-fp32/ (полная модель; int8 лежит отдельно).
pub fn gigaam_fp32_dir() -> std::path::PathBuf {
    crate::paths::models_dir().join(crate::gigaam::GIGAAM_FP32_DIR)
}

pub fn list() -> Vec<ModelInfo> {
    // Порядок каталога важен: фронт рисует hero-карточку по первой GigaAM.
    let info = |name: &str, label: &str, size_mb: u32, installed: bool, kind: &str| ModelInfo {
        name: name.to_string(),
        label: label.to_string(),
        size_mb,
        installed,
        kind: kind.to_string(),
        disk_mb: if installed {
            disk_mb(&model_paths(name))
        } else {
            None
        },
        in_use: false,
    };
    DIR_MODELS
        .iter()
        .map(|m| info(m.name, m.label, m.size_mb, dir_model_ready(m.name), m.kind))
        .collect()
}

fn verify_sha256(path: &std::path::Path, expected: &str) -> Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != expected {
        return Err(anyhow!(
            "контрольная сумма модели не совпала: ожидалась {expected}, получена {actual}"
        ));
    }
    Ok(())
}

fn marker_path(path: &std::path::Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("model");
    path.with_file_name(format!("{name}.sha256"))
}

fn marker_is_current(path: &std::path::Path, expected: &str) -> bool {
    let marker = marker_path(path);
    if !std::fs::read_to_string(&marker)
        .map(|value| value.trim() == expected)
        .unwrap_or(false)
    {
        return false;
    }
    let Ok(model_modified) = std::fs::metadata(path).and_then(|meta| meta.modified()) else {
        return false;
    };
    std::fs::metadata(marker)
        .and_then(|meta| meta.modified())
        .map(|marker_modified| marker_modified >= model_modified)
        .unwrap_or(false)
}

fn write_marker(path: &std::path::Path, sha256: &str) -> Result<()> {
    std::fs::write(marker_path(path), format!("{sha256}\n"))?;
    Ok(())
}

fn artifact_fast_ready(path: &std::path::Path, artifact: Artifact) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() == artifact.size_bytes)
        .unwrap_or(false)
}

fn verify_artifact(path: &std::path::Path, artifact: Artifact) -> Result<()> {
    let meta =
        std::fs::metadata(path).map_err(|error| anyhow!("{} не найден: {error}", artifact.name))?;
    if !meta.is_file() || meta.len() != artifact.size_bytes {
        return Err(anyhow!(
            "неверный размер {}: ожидалось {}, получено {}",
            artifact.name,
            artifact.size_bytes,
            meta.len()
        ));
    }
    if marker_is_current(path, artifact.sha256) {
        return Ok(());
    }
    verify_sha256(path, artifact.sha256)?;
    write_marker(path, artifact.sha256)
}

fn remove_artifact(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(marker_path(path));
}

fn finalize_part(part: &std::path::Path, dest: &std::path::Path, artifact: Artifact) -> Result<()> {
    let actual = std::fs::metadata(part).map(|meta| meta.len()).unwrap_or(0);
    if actual != artifact.size_bytes {
        if actual > artifact.size_bytes {
            let _ = std::fs::remove_file(part);
        }
        return Err(anyhow!(
            "неполная загрузка {}: ожидалось {}, получено {}",
            artifact.name,
            artifact.size_bytes,
            actual
        ));
    }
    if let Err(error) = verify_sha256(part, artifact.sha256) {
        let _ = std::fs::remove_file(part);
        return Err(error);
    }
    std::fs::OpenOptions::new()
        .write(true)
        .open(part)?
        .sync_all()?;
    remove_artifact(dest);
    std::fs::rename(part, dest)?;
    write_marker(dest, artifact.sha256)
}

fn configure_resumable_curl(cmd: &mut Command, resume: bool) {
    if resume {
        cmd.arg("--continue-at").arg("-");
    }
    cmd.arg("--connect-timeout")
        .arg("15")
        .arg("--speed-limit")
        .arg("1024")
        .arg("--speed-time")
        .arg("30")
        .arg("--retry")
        .arg("3");
}

pub fn dir_model_ready(name: &str) -> bool {
    let Some(model) = find_dir_model(name) else {
        return false;
    };
    let dir = (model.dir)();
    model
        .files
        .iter()
        .all(|artifact| artifact_fast_ready(&dir.join(artifact.name), *artifact))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GigaVariant {
    Fp32,
    Int8,
}

/// fp32 не прошла проверку или не загрузилась: до перезапуска идём на int8,
/// а не мучаем каждую диктовку повторной загрузкой.
static FP32_BROKEN: AtomicBool = AtomicBool::new(false);

/// Есть чем распознавать русский: int8 или fp32.
pub fn gigaam_available() -> bool {
    gigaam_variant().is_some()
}

pub fn mark_gigaam_fp32_broken() {
    FP32_BROKEN.store(true, Ordering::SeqCst);
}

fn files_present(dir: &std::path::Path, files: &[Artifact]) -> bool {
    files
        .iter()
        .all(|a| artifact_fast_ready(&dir.join(a.name), *a))
}

/// Какая GigaAM нужна сейчас: готовая и не битая fp32, иначе int8, иначе ничего.
fn gigaam_variant_in(
    fp32_dir: &std::path::Path,
    int8_dir: &std::path::Path,
    fp32_broken: bool,
) -> Option<GigaVariant> {
    if !fp32_broken && files_present(fp32_dir, GIGAAM_FP32_ARTIFACTS) {
        Some(GigaVariant::Fp32)
    } else if files_present(int8_dir, GIGAAM_ARTIFACTS) {
        Some(GigaVariant::Int8)
    } else {
        None
    }
}

pub fn gigaam_variant() -> Option<GigaVariant> {
    gigaam_variant_in(
        &gigaam_fp32_dir(),
        &gigaam_dir(),
        FP32_BROKEN.load(Ordering::SeqCst),
    )
}

/// Нужна ли фоновая загрузка полной модели: только тем, кто реально использует
/// GigaAM (язык русский или авто), если не удалял её сам.
fn wants_fp32_download(uses_gigaam: bool, opted_out: bool) -> bool {
    uses_gigaam && !opted_out
}

/// Метка «полную модель удалил сам пользователь»: автозагрузка её не возвращает,
/// пока он не нажмёт «Скачать» (это и есть способ остаться на быстрой int8).
fn fp32_opt_out_path() -> std::path::PathBuf {
    crate::paths::models_dir().join("gigaam-fp32.skip")
}

/// То же для Parakeet: в «авто» он качается сам (D-065), удалённый - не возвращаем.
fn parakeet_opt_out_path() -> std::path::PathBuf {
    crate::paths::models_dir().join("parakeet.skip")
}

pub fn verify_dir_model(name: &str) -> Result<()> {
    let model = find_dir_model(name).ok_or_else(|| anyhow!("Неизвестная модель: {name}"))?;
    let dir = (model.dir)();
    for artifact in model.files {
        verify_artifact(&dir.join(artifact.name), *artifact)?;
    }
    Ok(())
}

pub fn delete(name: &str) -> Result<()> {
    // Каталожная ONNX-модель — это каталог, а не одиночный файл: сносим целиком
    // (вместе с .part).
    if let Some(m) = find_dir_model(name) {
        if name == GIGAAM_FP32_NAME {
            FP32_BROKEN.store(false, Ordering::SeqCst);
            let _ = std::fs::write(fp32_opt_out_path(), b"");
        }
        if name == PARAKEET_NAME {
            let _ = std::fs::write(parakeet_opt_out_path(), b"");
        }
        let dir = (m.dir)();
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        return Ok(());
    }
    Err(anyhow!("Неизвестная модель: {name}"))
}

/// Что уже лежит на диске и прошло проверку - вход для `plan_local_models`.
#[derive(Clone, Debug, Default)]
pub(crate) struct InstalledModels {
    pub gigaam_fp32: bool,
    pub gigaam_int8: bool,
    pub parakeet: bool,
    /// Пользователь сам удалил полную GigaAM (метка `gigaam-fp32.skip`).
    pub fp32_opted_out: bool,
    /// Пользователь сам удалил Parakeet (метка `parakeet.skip`).
    pub parakeet_opted_out: bool,
}

/// Решение автоподготовки моделей: что качать.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ModelPlan {
    /// Модели, которых нет, а маршрут их ждёт.
    pub download: Vec<String>,
    /// Фоновая загрузка полной GigaAM (D-040).
    pub download_fp32: bool,
}

/// Чистый выбор без ФС (D-042, D-065): уже установленная годная модель важнее
/// загрузки. GigaAM нужна русскому и авто (и английскому без Parakeet - тот
/// же маршрут авто); Parakeet для авто качается сам - он вместо Whisper
/// распознаёт английский. Явный `en` Parakeet сам не качает (как и раньше).
pub(crate) fn plan_local_models(language: &str, installed: &InstalledModels) -> ModelPlan {
    let language = language.trim().to_ascii_lowercase();
    let english = language == "en" || language == "english";
    let russian = language == "ru" || language == "russian";
    let gigaam_any = installed.gigaam_fp32 || installed.gigaam_int8;
    let mut download = Vec::new();
    let parakeet_covers = english && installed.parakeet;
    if !gigaam_any && !parakeet_covers {
        download.push(GIGAAM_NAME.to_string());
    }
    if !russian && !english && !installed.parakeet && !installed.parakeet_opted_out {
        download.push(PARAKEET_NAME.to_string());
    }
    ModelPlan {
        download,
        download_fp32: wants_fp32_download(!english, installed.fp32_opted_out)
            && !installed.gigaam_fp32,
    }
}

/// Файлы от Whisper (до 2.5.0) в папке моделей: веса `ggml-*.bin`, их
/// `.part`/`.sha256` и метка `whisper.skip`. Только эти имена - чужое не трогаем.
fn is_legacy_whisper_file(name: &str) -> bool {
    name == "whisper.skip"
        || (name.starts_with("ggml-")
            && [".bin", ".part", ".bin.sha256"]
                .iter()
                .any(|ext| name.ends_with(ext)))
}

/// Удалить остатки Whisper (D-065): гигабайты весов больше ничего не делают.
fn remove_legacy_whisper_files() {
    let Ok(rd) = std::fs::read_dir(crate::paths::models_dir()) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if entry.file_type().is_ok_and(|t| t.is_file()) && is_legacy_whisper_file(name) {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => log::info!("удалён старый файл Whisper: {name}"),
                Err(e) => log::warn!("не удалил старый файл Whisper {name}: {e}"),
            }
        }
    }
}

pub fn ensure_default_models(app: AppHandle, settings: &crate::settings::Settings) {
    let settings = settings.clone();
    // Проверка legacy-файла может включать однократный SHA больших весов, поэтому
    // не блокируем setup/UI. Успешная проверка пишет marker и следующие старты быстры.
    std::thread::spawn(move || {
        remove_legacy_whisper_files();
        let installed = InstalledModels {
            gigaam_fp32: verify_dir_model(GIGAAM_FP32_NAME).is_ok(),
            gigaam_int8: verify_dir_model(GIGAAM_NAME).is_ok(),
            parakeet: verify_dir_model(PARAKEET_NAME).is_ok(),
            fp32_opted_out: fp32_opt_out_path().exists(),
            parakeet_opted_out: parakeet_opt_out_path().exists(),
        };
        let plan = plan_local_models(&settings.language, &installed);
        let proxy = settings.proxy_url.clone();
        for name in plan.download {
            if let Err(e) = start_download(app.clone(), name, proxy.clone()) {
                log::error!("ensure_default_models: {e:#}");
            }
        }
        // Полная GigaAM - в фоне и после int8: пока она не готова, работает int8.
        if plan.download_fp32 {
            while GIGAAM_DOWNLOADING.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_secs(2));
            }
            if let Err(e) = start_download(app, GIGAAM_FP32_NAME.to_string(), proxy) {
                log::error!("ensure_default_models fp32: {e:#}");
            }
        }
    });
}

/// Имена моделей, чью загрузку попросили отменить. Цикл curl проверяет флаг
/// раз в 400 мс, убивает curl и оставляет .part — следующий «Скачать» докачает.
static CANCEL_REQUESTS: parking_lot::Mutex<Vec<String>> = parking_lot::Mutex::new(Vec::new());
pub fn cancel_download(name: &str) {
    let mut requests = CANCEL_REQUESTS.lock();
    if !requests.iter().any(|n| n == name) {
        requests.push(name.to_string());
    }
}

fn cancel_requested(name: &str) -> bool {
    CANCEL_REQUESTS.lock().iter().any(|n| n == name)
}

fn clear_cancel(name: &str) -> bool {
    let mut requests = CANCEL_REQUESTS.lock();
    let before = requests.len();
    requests.retain(|n| n != name);
    requests.len() != before
}

/// Итог фонового потока загрузки → событие для UI. Отмена — не ошибка:
/// `model:cancelled`, частичный файл остаётся для докачки.
fn report(app: &AppHandle, name: &str, result: Result<()>) {
    let cancelled = clear_cancel(name);
    match result {
        Ok(()) => {}
        Err(_) if cancelled => {
            let _ = app.emit("model:cancelled", serde_json::json!({ "name": name }));
        }
        Err(e) => {
            log::error!("download {name}: {e:#}");
            let _ = app.emit(
                "model:error",
                // "error" читает фронт (types.ts), "message" — для обратной совместимости.
                serde_json::json!({ "name": name, "error": e.to_string(), "message": e.to_string() }),
            );
        }
    }
}

/// Запустить загрузку в фоновом потоке. События: `model:progress` / `model:done` /
/// `model:error` / `model:cancelled`. Любой отказ тоже уходит `model:error`,
/// иначе UI, уже нарисовавший прогресс, висел бы на «скачивание…».
pub fn start_download(app: AppHandle, name: String, proxy: String) -> Result<()> {
    let result = spawn_download(app.clone(), name.clone(), proxy);
    if let Err(e) = &result {
        let _ = app.emit(
            "model:error",
            serde_json::json!({ "name": name, "error": e.to_string(), "message": e.to_string() }),
        );
    }
    result
}

fn spawn_download(app: AppHandle, name: String, proxy: String) -> Result<()> {
    if name == GIGAAM_FP32_NAME {
        let _ = std::fs::remove_file(fp32_opt_out_path());
    }
    if name == PARAKEET_NAME {
        let _ = std::fs::remove_file(parakeet_opt_out_path());
    }
    if let Some(m) = find_dir_model(&name) {
        // Уже качается → молча выходим: события прогресса и так летят от первого
        // потока, второй curl поверх того же .part устроил бы кашу.
        if m.downloading
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(());
        }
        clear_cancel(m.name);
        std::thread::spawn(move || {
            let r = run_download_dir(&app, m, &proxy);
            m.downloading.store(false, Ordering::SeqCst);
            report(&app, m.name, r);
        });
        return Ok(());
    }
    Err(anyhow!("Неизвестная модель: {name}"))
}

/// Понятная причина вместо «exit status: 6»: чаще всего из РФ это недоступный
/// huggingface.co, и лечится прокси в настройках.
fn curl_failure(artifact: &str, code: Option<i32>, stderr: &str) -> anyhow::Error {
    let hint = match code {
        Some(5) => "не удалось подключиться к прокси — проверьте адрес прокси в настройках",
        Some(6) | Some(7) | Some(35) | Some(52) | Some(56) => {
            "нет связи с huggingface.co — проверьте интернет или укажите прокси в настройках «Облако»"
        }
        Some(28) => "сеть слишком медленная или соединение зависло — повторите, загрузка продолжится",
        Some(22) => "сервер отказал в выдаче файла (HTTP-ошибка) — повторите позже",
        Some(23) => "не удалось записать файл — проверьте свободное место на диске",
        _ => "загрузка прервалась — повторите, она продолжится с места обрыва",
    };
    let detail = stderr.trim();
    if detail.is_empty() {
        anyhow!("{artifact}: {hint}")
    } else {
        anyhow!("{artifact}: {hint} ({detail})")
    }
}

/// Скачать один файл в `<dest>.part` с докачкой и прогрессом, проверить SHA и
/// атомарно переименовать в `dest`. `(base, total)` — для суммарного прогресса
/// многофайловой модели (`received = base + размер .part`).
fn download_artifact<R: tauri::Runtime>(
    app: &AppHandle<R>,
    event_name: &str,
    url: &str,
    dest: &std::path::Path,
    artifact: Artifact,
    (base, total): (u64, u64),
    proxy: &str,
) -> Result<()> {
    // .part уникален: расширения файлов различны, with_extension не конфликтует.
    let part = dest.with_extension("part");
    let part_len = std::fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
    if part_len > artifact.size_bytes {
        let _ = std::fs::remove_file(&part);
    } else if part_len == artifact.size_bytes && finalize_part(&part, dest, artifact).is_ok() {
        return Ok(());
    }

    let mut reset_after_unsupported_resume = false;
    loop {
        let resume = std::fs::metadata(&part)
            .map(|meta| meta.len() > 0)
            .unwrap_or(false);
        let mut cmd = crate::net::curl();
        crate::net::apply_proxy(&mut cmd, proxy);
        cmd.arg("-L")
            .arg("--fail")
            .arg("--silent")
            .arg("--show-error")
            .arg("-o")
            .arg(&part)
            .arg(url)
            .stderr(std::process::Stdio::piped());
        configure_resumable_curl(&mut cmd, resume);

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;

        let status = loop {
            std::thread::sleep(Duration::from_millis(400));
            let received = base + std::fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
            let _ = app.emit(
                "model:progress",
                serde_json::json!({ "name": event_name, "received": received, "total": total }),
            );
            if cancel_requested(event_name) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow!("загрузка отменена"));
            }
            if let Some(status) = child.try_wait()? {
                break status;
            }
        };
        // --silent --show-error: в stderr одна строка, pipe не переполнится.
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }

        if status.success() {
            return finalize_part(&part, dest, artifact);
        }
        // Crash мог случиться после последнего байта, но до rename; не теряем
        // уже валидный файл из-за HTTP 416/ошибки возобновления.
        if finalize_part(&part, dest, artifact).is_ok() {
            return Ok(());
        }
        // 33 = сервер не умеет докачку — начинаем заново один раз.
        if status.code() == Some(33) && resume && !reset_after_unsupported_resume {
            let _ = std::fs::remove_file(&part);
            reset_after_unsupported_resume = true;
            continue;
        }
        return Err(curl_failure(artifact.name, status.code(), &stderr));
    }
}

/// Скачать файлы каталожной ONNX-модели последовательно в её каталог. Прогресс —
/// СУММАРНЫЙ (готовые байты всех файлов / общий размер) под единым именем модели.
/// Файл с уже правильным размером — скип: докачка после обрыва сети/закрытия
/// приложения продолжает с первого недокачанного файла.
fn run_download_dir(app: &AppHandle, m: &DirModel, proxy: &str) -> Result<()> {
    let dir = (m.dir)();
    std::fs::create_dir_all(&dir)?;
    let total: u64 = m.files.iter().map(|artifact| artifact.size_bytes).sum();
    let mut base: u64 = 0; // байты уже завершённых файлов (вклад в суммарный прогресс)

    for artifact in m.files {
        let dest = dir.join(artifact.name);
        if verify_artifact(&dest, *artifact).is_err() {
            remove_artifact(&dest);
            let url = format!("{}{}", m.base_url, artifact.name);
            download_artifact(app, m.name, &url, &dest, *artifact, (base, total), proxy)?;
        }
        base += artifact.size_bytes;
    }

    let _ = app.emit(
        "model:progress",
        serde_json::json!({ "name": m.name, "received": total, "total": total }),
    );
    if m.name == GIGAAM_FP32_NAME {
        FP32_BROKEN.store(false, Ordering::SeqCst);
    }
    let _ = app.emit("model:done", serde_json::json!({ "name": m.name }));
    Ok(())
}

#[cfg(test)]
fn curl_args_for_test(resume: bool) -> Vec<String> {
    let mut command = Command::new("curl");
    configure_resumable_curl(&mut command, resume);
    command
        .get_args()
        .map(|value| value.to_string_lossy().into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_mb_counts_real_files_and_is_none_when_missing() {
        let dir = std::env::temp_dir().join(format!("vf-disk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.onnx"), vec![0u8; 3 * 1024 * 1024]).unwrap();
        std::fs::write(dir.join("sub").join("b.bin"), vec![0u8; 1024 * 1024]).unwrap();
        let single = dir.join("w.bin");
        assert_eq!(disk_mb(std::slice::from_ref(&dir)), Some(4));
        // Отдельный файл и отсутствующий путь рядом с ним.
        let missing = dir.join("nope");
        assert_eq!(disk_mb(&[dir.join("a.onnx"), missing.clone()]), Some(3));
        assert_eq!(disk_mb(&[missing]), None);
        // Крошечный файл - не «0 МБ».
        std::fs::write(&single, b"x").unwrap();
        assert_eq!(disk_mb(&[single]), Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Регрессия: запись Parakeet в каталоге согласована с константами parakeet.rs
    /// (имя, kind, URL, размер каталога соответствует сумме файлов).
    #[test]
    fn parakeet_dir_model_consistent() {
        let m = find_dir_model(PARAKEET_NAME).expect("parakeet-v3 должен быть в DIR_MODELS");
        assert_eq!(m.kind, "parakeet");
        assert!(
            m.base_url
                .starts_with("https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx")
                && m.base_url
                    .contains("8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce")
                && m.base_url.ends_with('/'),
            "base_url: {}",
            m.base_url
        );
        assert_eq!(m.files.len(), crate::parakeet::PARAKEET_FILES.len());
        // size_mb каталога = сумма байтов файлов в МиБ (округление ±1).
        let manifest: Vec<(&str, u64)> = m
            .files
            .iter()
            .map(|artifact| (artifact.name, artifact.size_bytes))
            .collect();
        assert_eq!(manifest, crate::parakeet::PARAKEET_FILES);
        let mib = (m
            .files
            .iter()
            .map(|artifact| artifact.size_bytes)
            .sum::<u64>()
            / 1_048_576) as i64;
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
        assert_eq!(p.installed, dir_model_ready(PARAKEET_NAME));
    }

    #[test]
    fn gigaam_manifest_matches_runtime_files_and_is_pinned() {
        let model = find_dir_model(GIGAAM_NAME).expect("gigaam manifest");
        let manifest: Vec<(&str, u64)> = model
            .files
            .iter()
            .map(|artifact| (artifact.name, artifact.size_bytes))
            .collect();
        assert_eq!(manifest, crate::gigaam::GIGAAM_FILES);
        assert!(model
            .base_url
            .contains("322c3b29492673eb7d0b434bfa9dfb8653e34d02"));
    }

    #[test]
    fn gigaam_fp32_is_a_separate_pinned_catalog_entry() {
        let fp32 = find_dir_model(GIGAAM_FP32_NAME).expect("fp32 в каталоге");
        let int8 = find_dir_model(GIGAAM_NAME).expect("int8 в каталоге");
        // Размеры и SHA-256 взяты из HF API (lfs.oid) для ревизии 322c3b29.
        let expected = [
            (
                "v3_e2e_rnnt_encoder.onnx",
                885_084_534_u64,
                "cd60b3764a832e8560ae6d3ad0b10adc1a42ffae412b9476f25620aae4f4a508",
            ),
            (
                "v3_e2e_rnnt_decoder.onnx",
                4_599_910,
                "7b0a16d67fd2cb37061decc93c69e364a9ab27afee3c57495d55b1c974cf7231",
            ),
            (
                "v3_e2e_rnnt_joint.onnx",
                2_712_896,
                "602ff7017a93311aad34df1437c8d7f49911353c13d6eae7a6ee7b041339465c",
            ),
            (
                "v3_e2e_rnnt_vocab.txt",
                13_354,
                "39abae20e692998290c574e606f11a9edef2902a1995463fcff63d1490cf22b7",
            ),
        ];
        let actual: Vec<_> = fp32
            .files
            .iter()
            .map(|a| (a.name, a.size_bytes, a.sha256))
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(fp32.base_url, int8.base_url);
        assert_eq!(fp32.size_mb, 885);
        assert_ne!((fp32.dir)(), (int8.dir)(), "папки не смешиваются");
        assert_eq!(list()[1].name, GIGAAM_FP32_NAME);
    }

    /// Файлы нужного размера без содержимого (sparse): для проверки выбора
    /// хватает размеров, как и у `dir_model_ready`.
    fn fake_dir(dir: &std::path::Path, files: &[Artifact], skip_last: bool) {
        std::fs::create_dir_all(dir).unwrap();
        let n = if skip_last {
            files.len() - 1
        } else {
            files.len()
        };
        for a in &files[..n] {
            std::fs::File::create(dir.join(a.name))
                .unwrap()
                .set_len(a.size_bytes)
                .unwrap();
        }
    }

    #[test]
    fn gigaam_variant_prefers_ready_fp32_and_falls_back_to_int8() {
        let tmp = std::env::temp_dir().join(format!("vf-variant-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (fp32, int8) = (tmp.join("fp32"), tmp.join("int8"));
        assert_eq!(gigaam_variant_in(&fp32, &int8, false), None);

        fake_dir(&int8, GIGAAM_ARTIFACTS, false);
        // fp32 не скачана -> int8.
        assert_eq!(
            gigaam_variant_in(&fp32, &int8, false),
            Some(GigaVariant::Int8)
        );
        // fp32 недокачана (нет последнего файла) -> int8.
        fake_dir(&fp32, GIGAAM_FP32_ARTIFACTS, true);
        assert_eq!(
            gigaam_variant_in(&fp32, &int8, false),
            Some(GigaVariant::Int8)
        );
        // fp32 готова -> fp32.
        fake_dir(&fp32, GIGAAM_FP32_ARTIFACTS, false);
        assert_eq!(
            gigaam_variant_in(&fp32, &int8, false),
            Some(GigaVariant::Fp32)
        );
        // fp32 не загрузилась (битая) -> int8 несмотря на размеры.
        assert_eq!(
            gigaam_variant_in(&fp32, &int8, true),
            Some(GigaVariant::Int8)
        );
        // int8 нет, fp32 не битая -> fp32 одна.
        std::fs::remove_dir_all(&int8).unwrap();
        assert_eq!(
            gigaam_variant_in(&fp32, &int8, false),
            Some(GigaVariant::Fp32)
        );
        assert_eq!(gigaam_variant_in(&fp32, &int8, true), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fp32_background_download_follows_russian_use_and_user_opt_out() {
        // использует GigaAM (ru/авто, движок gigaam) - качаем
        assert!(wants_fp32_download(true, false));
        // не использует (другой движок или язык), даже если int8 лежит - нет
        assert!(!wants_fp32_download(false, false));
        // пользователь удалил полную сам - не навязываем
        assert!(!wants_fp32_download(true, true));
    }

    #[test]
    fn curl_resume_is_compatible_with_older_apple_curl() {
        let fresh = curl_args_for_test(false);
        let resumed = curl_args_for_test(true);
        assert!(!fresh.iter().any(|arg| arg == "--continue-at"));
        assert!(resumed.iter().any(|arg| arg == "--continue-at"));
        assert!(!resumed.iter().any(|arg| arg == "--retry-all-errors"));
        assert!(!resumed.iter().any(|arg| arg == "--max-time"));
    }

    #[test]
    fn finalize_is_atomic_and_preserves_only_resumable_partials() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "voxflow-model-finalize-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("fixture dir");
        let dest = dir.join("fixture.bin");
        let part = dir.join("fixture.part");
        let artifact = Artifact {
            name: "fixture.bin",
            size_bytes: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        };

        std::fs::write(&part, b"ab").expect("short part");
        assert!(finalize_part(&part, &dest, artifact).is_err());
        assert!(part.exists(), "короткий part нужен для resume");

        std::fs::write(&part, b"abd").expect("corrupt part");
        assert!(finalize_part(&part, &dest, artifact).is_err());
        assert!(!part.exists(), "полный файл с плохим SHA нужно удалить");

        std::fs::write(&part, b"abc").expect("valid part");
        finalize_part(&part, &dest, artifact).expect("finalize valid part");
        assert_eq!(std::fs::read(&dest).expect("read dest"), b"abc");
        assert!(!part.exists());
        assert!(marker_is_current(&dest, artifact.sha256));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Веса читаются прямо из проверенного файла (D-053, отображение в память),
    /// поэтому подмена файла той же длины после проверки должна ловиться снова.
    #[test]
    fn verify_artifact_catches_same_size_swap_after_marker() {
        let dir = std::env::temp_dir().join(format!("voxflow-model-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("fixture dir");
        let path = dir.join("fixture.bin");
        let artifact = Artifact {
            name: "fixture.bin",
            size_bytes: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        };
        std::fs::write(&path, b"abc").expect("valid");
        verify_artifact(&path, artifact).expect("valid file passes");
        std::fs::write(&path, b"abd").expect("swap");
        // Подмена позже маркера, даже если часы ФС грубые.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_modified(later))
            .expect("mtime");
        assert!(verify_artifact(&path, artifact).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sha256_verifier_accepts_known_payload_and_rejects_mismatch() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "voxflow-model-sha-{}-{}.bin",
            std::process::id(),
            nonce
        ));
        std::fs::write(&path, b"abc").expect("write fixture");
        assert!(verify_sha256(
            &path,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        )
        .is_ok());
        assert!(verify_sha256(&path, &"0".repeat(64)).is_err());
        let _ = std::fs::remove_file(path);
    }

    /// Живая загрузка маленького файла GigaAM с HF: докачка с середины .part,
    /// отмена и внятная ошибка при мёртвом прокси.
    #[cfg(not(windows))]
    #[test]
    #[ignore = "requires network access to huggingface.co"]
    fn download_artifact_resumes_cancels_and_explains_proxy_failure() {
        let app = tauri::test::mock_app();
        let handle = app.handle();
        let artifact = GIGAAM_ARTIFACTS[3]; // vocab.txt, 13 КБ
        let url = format!("{}{}", DIR_MODELS[0].base_url, artifact.name);
        let dir = std::env::temp_dir().join(format!("voxflow-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join(artifact.name);
        let span = (0, artifact.size_bytes);

        download_artifact(handle, "t-fresh", &url, &dest, artifact, span, "").expect("fresh");
        let full = std::fs::read(&dest).unwrap();

        // Докачка: .part с первой половиной файла → curl продолжает, SHA сходится.
        let resumed = dir.join("resumed.txt");
        std::fs::write(resumed.with_extension("part"), &full[..full.len() / 2]).unwrap();
        download_artifact(handle, "t-resume", &url, &resumed, artifact, span, "").expect("resume");
        assert_eq!(std::fs::read(&resumed).unwrap(), full);

        let cancelled = dir.join("cancelled.txt");
        cancel_download("t-cancel");
        let err = download_artifact(handle, "t-cancel", &url, &cancelled, artifact, span, "")
            .unwrap_err();
        assert!(err.to_string().contains("отменена"), "{err}");
        assert!(clear_cancel("t-cancel"));
        assert!(!cancelled.exists());

        let proxied = dir.join("proxied.txt");
        let err = download_artifact(
            handle,
            "t-proxy",
            &url,
            &proxied,
            artifact,
            span,
            "http://127.0.0.1:9",
        )
        .unwrap_err();
        assert!(err.to_string().contains("прокси"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn delete_rejects_path_traversal_names() {
        assert!(delete("..\\voxflow.db").is_err());
        assert!(delete("../voxflow.db").is_err());
        assert!(delete("C:\\Users\\Public\\victim.bin").is_err());
        assert!(delete("ggml-large-v3.bin").is_err());
    }

    fn installed(fp32: bool, int8: bool, parakeet: bool) -> InstalledModels {
        InstalledModels {
            gigaam_fp32: fp32,
            gigaam_int8: int8,
            parakeet,
            ..InstalledModels::default()
        }
    }

    #[test]
    fn plan_uses_installed_models_without_download() {
        let all = installed(true, false, true);
        for lang in ["ru", "en", "auto"] {
            assert_eq!(
                plan_local_models(lang, &all),
                ModelPlan::default(),
                "{lang}"
            );
        }
        // Только int8: ничего не качаем, кроме фоновой полной (D-040).
        let int8 = plan_local_models("ru", &installed(false, true, false));
        assert!(int8.download.is_empty());
        assert!(int8.download_fp32);
    }

    #[test]
    fn plan_fetches_what_the_route_needs() {
        let none = installed(false, false, false);
        assert_eq!(plan_local_models("ru", &none).download, [GIGAAM_NAME]);
        // Авто вместо Whisper: GigaAM + Parakeet для английского (D-065).
        assert_eq!(
            plan_local_models("auto", &none).download,
            [GIGAAM_NAME, PARAKEET_NAME]
        );
        // Явный en с Parakeet - GigaAM не нужна; без Parakeet en идёт как авто на GigaAM.
        let parakeet_only = installed(false, false, true);
        assert!(plan_local_models("en", &parakeet_only).download.is_empty());
        assert!(!plan_local_models("en", &parakeet_only).download_fp32);
        assert_eq!(plan_local_models("en", &none).download, [GIGAAM_NAME]);
    }

    #[test]
    fn plan_respects_user_deletions() {
        let mut inst = installed(false, true, false);
        inst.fp32_opted_out = true;
        inst.parakeet_opted_out = true;
        let plan = plan_local_models("auto", &inst);
        assert!(plan.download.is_empty());
        assert!(!plan.download_fp32);
    }

    #[test]
    fn only_whisper_leftovers_are_cleaned() {
        for name in [
            "ggml-large-v3.bin",
            "ggml-large-v3-turbo-q5_0.part",
            "ggml-large-v3.bin.sha256",
            "whisper.skip",
        ] {
            assert!(is_legacy_whisper_file(name), "{name}");
        }
        for name in [
            "gigaam-fp32.skip",
            "parakeet.skip",
            "ggml-notes.txt",
            "voxflow.db",
            "gigaam",
        ] {
            assert!(!is_legacy_whisper_file(name), "{name}");
        }
    }
}
