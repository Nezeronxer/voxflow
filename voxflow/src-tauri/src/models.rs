//! Каталог и загрузка моделей с HuggingFace в %LOCALAPPDATA%\VoxFlow\models:
//! whisper (одиночные GGML .bin), GigaAM-v3 (набор ONNX-файлов в models/gigaam/)
//! и Parakeet TDT v3 (набор ONNX-файлов в models/parakeet/).
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
/// Единственный рекомендованный Whisper-пресет: multilingual, пригоден для RU/EN
/// и смешанной речи, но не требует 1.6–3.1 ГБ на диске.
pub const RECOMMENDED_WHISPER_NAME: &str = "ggml-large-v3-turbo-q5_0.bin";
/// Слабые legacy-пресеты не предлагаем к новой загрузке, но сохраняем в
/// каталоге, чтобы уже установленную/активную модель можно было увидеть и удалить.
const WEAK_LEGACY_WHISPER_NAMES: &[&str] = &["ggml-tiny.bin", "ggml-base.bin", "ggml-small.bin"];

/// Защита от двойного запуска загрузки каталожной модели: автозагрузка при первом
/// старте (ensure_default_models) и клик «Скачать» в UI могут прилететь
/// одновременно — два параллельных curl в один .part порвали бы файл. У
/// whisper-моделей такой гонки нет (качаются только по клику).
static GIGAAM_DOWNLOADING: AtomicBool = AtomicBool::new(false);
static PARAKEET_DOWNLOADING: AtomicBool = AtomicBool::new(false);
static WHISPER_DOWNLOADING: AtomicBool = AtomicBool::new(false);

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
        name: PARAKEET_NAME,
        label: "Parakeet TDT v3 — быстрый English (явный EN)",
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

struct Entry {
    name: &'static str,
    label: &'static str,
    size_mb: u32,
    size_bytes: u64,
    sha256: &'static str,
}

const CATALOG: &[Entry] = &[
    Entry {
        name: "ggml-tiny.bin",
        label: "Tiny — legacy, низкая точность (78 МБ)",
        size_mb: 78,
        size_bytes: 77_691_713,
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    },
    Entry {
        name: "ggml-large-v3-turbo-q5_0.bin",
        label: "Large v3 Turbo Q5 — рекомендуется (все языки, 574 МБ)",
        size_mb: 574,
        size_bytes: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    },
    Entry {
        name: "ggml-large-v3-turbo-q8_0.bin",
        label: "Large v3 Turbo Q8 — точнее Q5, всё ещё быстрый (874 МБ)",
        size_mb: 874,
        size_bytes: 874_188_075,
        sha256: "317eb69c11673c9de1e1f0d459b253999804ec71ac4c23c17ecf5fbe24e259a1",
    },
    Entry {
        name: "ggml-large-v3-turbo.bin",
        label: "Large v3 Turbo — мощная, тяжелее (1.6 ГБ)",
        size_mb: 1620,
        size_bytes: 1_624_555_275,
        sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
    },
    Entry {
        name: "ggml-large-v3.bin",
        label: "Large v3 — максимальная точность, медленнее (3.1 ГБ)",
        size_mb: 3100,
        size_bytes: 3_095_033_483,
        sha256: "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
    },
    Entry {
        name: "ggml-base.bin",
        label: "Base — legacy, низкая точность (148 МБ)",
        size_mb: 148,
        size_bytes: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    },
    Entry {
        name: "ggml-small.bin",
        label: "Small — legacy, уступает Turbo (488 МБ)",
        size_mb: 488,
        size_bytes: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    },
    Entry {
        name: "ggml-medium.bin",
        label: "Medium — повышенная точность без Large (1.53 ГБ)",
        size_mb: 1530,
        size_bytes: 1_533_763_059,
        sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
    },
];

/// Каталог models/gigaam/ (4 ONNX/vocab файла GigaAM-v3).
fn gigaam_dir() -> std::path::PathBuf {
    crate::paths::models_dir().join(crate::gigaam::GIGAAM_DIR)
}

pub fn list() -> Vec<ModelInfo> {
    let mut out = Vec::with_capacity(CATALOG.len() + DIR_MODELS.len());
    // Каталожные ONNX-модели — первыми строками, фронт рисует их hero-карточками.
    out.extend(DIR_MODELS.iter().map(|m| ModelInfo {
        name: m.name.to_string(),
        label: m.label.to_string(),
        size_mb: m.size_mb,
        installed: dir_model_ready(m.name),
        kind: m.kind.to_string(),
    }));
    out.extend(CATALOG.iter().map(|e| ModelInfo {
        name: e.name.to_string(),
        label: e.label.to_string(),
        size_mb: e.size_mb,
        installed: whisper_model_ready(e.name),
        kind: "whisper".to_string(),
    }));
    out
}

fn url_for(name: &str) -> String {
    format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/{name}"
    )
}

fn is_whisper_catalog_name(name: &str) -> bool {
    CATALOG.iter().any(|e| e.name == name)
}

fn is_weak_legacy_whisper(name: &str) -> bool {
    WEAK_LEGACY_WHISPER_NAMES.contains(&name)
}

fn catalog_entry(name: &str) -> Option<&'static Entry> {
    CATALOG.iter().find(|entry| entry.name == name)
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

fn whisper_artifact(entry: &Entry) -> Artifact {
    Artifact {
        name: entry.name,
        size_bytes: entry.size_bytes,
        sha256: entry.sha256,
    }
}

pub fn whisper_model_ready(name: &str) -> bool {
    let Some(entry) = catalog_entry(name) else {
        return false;
    };
    artifact_fast_ready(&crate::paths::model_path(name), whisper_artifact(entry))
}

pub fn verify_whisper_model_path(path: &std::path::Path) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("некорректное имя модели"))?;
    if let Some(entry) = catalog_entry(name) {
        verify_artifact(path, whisper_artifact(entry))
    } else if std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() > 0)
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(anyhow!("модель {name} пуста или недоступна"))
    }
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
        let dir = (m.dir)();
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        return Ok(());
    }
    if !is_whisper_catalog_name(name) {
        return Err(anyhow!("Неизвестная модель: {name}"));
    }
    let p = crate::paths::model_path(name);
    remove_artifact(&p);
    let _ = std::fs::remove_file(p.with_extension("part"));
    Ok(())
}

/// Автозагрузка дефолтной модели при первом запуске: свежая установка должна
/// сразу получить multilingual Whisper large-v3-turbo для language=auto.
/// GigaAM/Parakeet остаются выбираемыми спец-маршрутами, но не являются
/// стартовым default: пользователь просил out-of-the-box мультиязычность.
pub fn ensure_default_models(app: AppHandle, settings: &crate::settings::Settings) {
    let language = settings.language.trim().to_ascii_lowercase();
    let wants_multilingual = matches!(
        language.as_str(),
        "auto" | "all" | "any" | "multi" | "multilingual" | "*"
    ) || settings.engine != "gigaam";

    let configured_model_is_usable = is_whisper_catalog_name(&settings.model)
        && (!is_weak_legacy_whisper(&settings.model)
            || crate::paths::model_path(&settings.model).exists());
    let name = if wants_multilingual && configured_model_is_usable {
        settings.model.clone()
    } else if wants_multilingual {
        RECOMMENDED_WHISPER_NAME.to_string()
    } else if language == "ru" || language == "russian" {
        GIGAAM_NAME.to_string()
    } else {
        return;
    };

    // Проверка legacy-файла может включать однократный SHA больших весов, поэтому
    // не блокируем setup/UI. Успешная проверка пишет marker и следующие старты быстры.
    std::thread::spawn(move || {
        let ready = if find_dir_model(&name).is_some() {
            verify_dir_model(&name).is_ok()
        } else {
            verify_whisper_model_path(&crate::paths::model_path(&name)).is_ok()
        };
        if !ready {
            if let Err(e) = start_download(app, name) {
                log::error!("ensure_default_models: {e:#}");
            }
        }
    });
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
    // Уже скачанные legacy-веса остаются управляемыми и удаляемыми. Новую
    // загрузку слабого пресета не начинаем: предлагаем один понятный default.
    if is_weak_legacy_whisper(&name) && !crate::paths::model_path(&name).exists() {
        return Err(anyhow!(
            "Эта legacy-модель больше не предлагается; выберите Large v3 Turbo Q5"
        ));
    }
    if WHISPER_DOWNLOADING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        let message = "Уже загружается другая Whisper-модель";
        let _ = app.emit(
            "model:error",
            serde_json::json!({ "name": name, "error": message, "message": message }),
        );
        return Err(anyhow!(message));
    }
    std::thread::spawn(move || {
        if let Err(e) = run_download(&app, &name) {
            log::error!("download {name}: {e:#}");
            let _ = app.emit(
                "model:error",
                serde_json::json!({ "name": name, "error": e.to_string(), "message": e.to_string() }),
            );
        }
        WHISPER_DOWNLOADING.store(false, Ordering::SeqCst);
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
    let total: u64 = m.files.iter().map(|artifact| artifact.size_bytes).sum();
    let mut base: u64 = 0; // байты уже завершённых файлов (вклад в суммарный прогресс)

    for artifact in m.files {
        let dest = dir.join(artifact.name);
        if verify_artifact(&dest, *artifact).is_ok() {
            base += artifact.size_bytes;
            continue;
        }
        remove_artifact(&dest);

        // .part уникален: расширения файлов различны, with_extension не конфликтует.
        let part = dest.with_extension("part");
        let part_len = std::fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
        if part_len > artifact.size_bytes {
            let _ = std::fs::remove_file(&part);
        } else if part_len == artifact.size_bytes && finalize_part(&part, &dest, *artifact).is_ok()
        {
            base += artifact.size_bytes;
            continue;
        }

        let url = format!("{}{}", m.base_url, artifact.name);
        let mut reset_after_unsupported_resume = false;
        loop {
            let resume = std::fs::metadata(&part)
                .map(|meta| meta.len() > 0)
                .unwrap_or(false);
            let mut cmd = Command::new("curl");
            cmd.arg("-L")
                .arg("--fail")
                .arg("--silent")
                .arg("--show-error")
                .arg("-o")
                .arg(&part)
                .arg(&url);
            configure_resumable_curl(&mut cmd, resume);
            #[cfg(windows)]
            cmd.creation_flags(NO_WINDOW);

            let mut child = cmd
                .spawn()
                .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;

            let status = loop {
                std::thread::sleep(Duration::from_millis(400));
                let received = base + std::fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
                let _ = app.emit(
                    "model:progress",
                    serde_json::json!({ "name": m.name, "received": received, "total": total }),
                );
                if let Some(status) = child.try_wait()? {
                    break status;
                }
            };

            if status.success() {
                finalize_part(&part, &dest, *artifact)?;
                break;
            }
            // Crash мог случиться после последнего байта, но до rename; не теряем
            // уже валидный файл из-за HTTP 416/ошибки возобновления.
            if finalize_part(&part, &dest, *artifact).is_ok() {
                break;
            }
            if status.code() == Some(33) && resume && !reset_after_unsupported_resume {
                let _ = std::fs::remove_file(&part);
                reset_after_unsupported_resume = true;
                continue;
            }
            return Err(anyhow!(
                "curl ({}) завершился с ошибкой: {status}; частичная загрузка сохранена",
                artifact.name
            ));
        }
        base += artifact.size_bytes;
    }

    let _ = app.emit(
        "model:progress",
        serde_json::json!({ "name": m.name, "received": total, "total": total }),
    );
    let _ = app.emit("model:done", serde_json::json!({ "name": m.name }));
    Ok(())
}

fn run_download(app: &AppHandle, name: &str) -> Result<()> {
    let entry = catalog_entry(name).ok_or_else(|| anyhow!("Неизвестная модель: {name}"))?;
    let artifact = whisper_artifact(entry);
    let dest = crate::paths::model_path(name);
    if verify_artifact(&dest, artifact).is_ok() {
        let _ = app.emit("model:done", serde_json::json!({ "name": name }));
        return Ok(());
    }
    remove_artifact(&dest);
    let part = dest.with_extension("part");
    let url = url_for(name);
    let total = entry.size_bytes;
    let part_len = std::fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
    if part_len > total {
        let _ = std::fs::remove_file(&part);
    } else if part_len == total && finalize_part(&part, &dest, artifact).is_ok() {
        let _ = app.emit(
            "model:progress",
            serde_json::json!({ "name": name, "received": total, "total": total }),
        );
        let _ = app.emit("model:done", serde_json::json!({ "name": name }));
        return Ok(());
    }

    let mut reset_after_unsupported_resume = false;
    loop {
        let resume = std::fs::metadata(&part)
            .map(|meta| meta.len() > 0)
            .unwrap_or(false);
        let mut cmd = Command::new("curl");
        cmd.arg("-L")
            .arg("--fail")
            .arg("--silent")
            .arg("--show-error")
            .arg("-o")
            .arg(&part)
            .arg(&url);
        configure_resumable_curl(&mut cmd, resume);
        #[cfg(windows)]
        cmd.creation_flags(NO_WINDOW);

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;

        let status = loop {
            std::thread::sleep(Duration::from_millis(400));
            let received = std::fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
            let _ = app.emit(
                "model:progress",
                serde_json::json!({ "name": name, "received": received, "total": total }),
            );
            if let Some(status) = child.try_wait()? {
                break status;
            }
        };
        if status.success() || finalize_part(&part, &dest, artifact).is_ok() {
            if !dest.exists() {
                finalize_part(&part, &dest, artifact)?;
            }
            let _ = app.emit(
                "model:progress",
                serde_json::json!({ "name": name, "received": total, "total": total }),
            );
            let _ = app.emit("model:done", serde_json::json!({ "name": name }));
            return Ok(());
        }
        if status.code() == Some(33) && resume && !reset_after_unsupported_resume {
            let _ = std::fs::remove_file(&part);
            reset_after_unsupported_resume = true;
            continue;
        }
        return Err(anyhow!(
            "curl завершился с ошибкой: {status}; частичная загрузка сохранена"
        ));
    }
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
    fn whisper_catalog_has_fast_and_quality_v2_profiles() {
        assert!(is_whisper_catalog_name("ggml-tiny.bin"));
        assert!(is_whisper_catalog_name("ggml-large-v3-turbo-q8_0.bin"));
        assert!(is_whisper_catalog_name("ggml-medium.bin"));
        for entry in CATALOG {
            assert!(entry.size_bytes > 0, "{} size", entry.name);
            assert_eq!(entry.sha256.len(), 64, "{} sha256", entry.name);
            assert!(entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert!(url_for(entry.name).contains("5359861c739e955e79d9a303bcbc70fb988958b1"));
        }
    }

    #[test]
    fn catalog_has_one_recommendation_and_marks_weak_legacy_models() {
        let recommended = CATALOG
            .iter()
            .filter(|entry| entry.label.contains("рекомендуется"))
            .collect::<Vec<_>>();
        assert_eq!(recommended.len(), 1);
        assert_eq!(recommended[0].name, RECOMMENDED_WHISPER_NAME);

        for name in WEAK_LEGACY_WHISPER_NAMES {
            let entry = CATALOG
                .iter()
                .find(|entry| entry.name == *name)
                .expect("legacy-модель остаётся управляемой");
            assert!(entry.label.contains("legacy"), "{}", entry.label);
            assert!(is_weak_legacy_whisper(name));
        }
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

    #[test]
    fn delete_rejects_path_traversal_names() {
        assert!(delete("..\\voxflow.db").is_err());
        assert!(delete("../voxflow.db").is_err());
        assert!(delete("C:\\Users\\Public\\victim.bin").is_err());
        assert!(is_whisper_catalog_name("ggml-base.bin"));
    }
}
