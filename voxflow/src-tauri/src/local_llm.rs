//! Встроенный локальный ИИ: llama.cpp (`llama-server`) + GGUF-модели.
//!
//! Зачем. До 2.1 локальная нейросеть для обработки текста требовала чужого
//! ПО (Ollama, LM Studio) — человек уходил в терминал за `ollama pull`. Здесь
//! всё делает само приложение: скачивает runtime llama.cpp закреплённой
//! сборки, скачивает модель с HuggingFace, проверяет контрольные суммы,
//! поднимает сервер на loopback-порту и ходит к нему OpenAI-совместимым
//! протоколом. Наружу уходят только загрузки (GitHub и HuggingFace) —
//! диктовка никогда.
//!
//! Что откуда:
//!
//! - runtime — GitHub Releases `ggml-org/llama.cpp`, тег [`RUNTIME_TAG`]
//!   (v0.4.0, сентябрь 2026). Контрольная сумма берётся из поля `digest`
//!   релизного API того же выпуска и сверяется после загрузки;
//! - модели — HuggingFace, sha256 и размер берутся из LFS-метаданных
//!   (`/api/models/<repo>/tree/main`) и сверяются после загрузки.
//!
//! Распаковка — системным `tar`: на Windows 10+ bsdtar читает zip, на macOS
//! tar.gz родной. Ни zip-крейта, ни C-сборок не требуется.
//!
//! Сервер живёт, пока живёт приложение (как у Ollama): повторная загрузка
//! модели стоит секунды, а диктовка ждать не должна. Смена модели или режима
//! GPU перезапускает процесс.

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

use crate::net;

/// Закреплённая сборка llama.cpp. Меняется вместе с проверкой совместимости
/// (флаги `llama-server`, формат ответа `/v1/chat/completions`).
pub const RUNTIME_TAG: &str = "b10809";
/// Порт локального сервера. 8771 занят whisper-server.
pub const SERVER_PORT: u16 = 8772;
/// Окно контекста: системный промпт VoxFlow ~6k токенов + диктовка + ответ.
const CONTEXT_TOKENS: u32 = 8192;
/// Сколько ждём, пока сервер загрузит модель и ответит на `/health`.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const NO_WINDOW: u32 = 0x0800_0000;

/// Модель из каталога. `size_gb` — вес файла (для интерфейса и ярусов),
/// `min_ram_gb` — с какого объёма памяти модель имеет смысл предлагать (см.
/// [`crate::local_ai`] — правила общие).
#[derive(Serialize, Clone, Debug)]
pub struct LlmModel {
    pub id: &'static str,
    pub label: &'static str,
    pub repo: &'static str,
    pub file: &'static str,
    pub size_gb: f32,
    pub min_ram_gb: u32,
    pub blurb: &'static str,
}

/// Каталог. Порядок — от лёгкой к тяжёлой: так интерфейс подбирает
/// «самую крупную из подходящих» одним проходом.
pub const CATALOG: &[LlmModel] = &[
    LlmModel {
        id: "qwen3-1.7b",
        label: "Qwen3 1.7B",
        repo: "Qwen/Qwen3-1.7B-GGUF",
        file: "Qwen3-1.7B-Q8_0.gguf",
        size_gb: 1.8,
        min_ram_gb: 8,
        blurb: "Самая лёгкая: чистит паразиты и пунктуацию даже на слабом ноутбуке.",
    },
    LlmModel {
        id: "qwen2.5-3b-instruct",
        label: "Qwen2.5 3B Instruct",
        repo: "Qwen/Qwen2.5-3B-Instruct-GGUF",
        file: "qwen2.5-3b-instruct-q4_k_m.gguf",
        size_gb: 2.0,
        min_ram_gb: 8,
        blurb: "Рекомендуемая: хороший русский, быстрая на процессоре.",
    },
    LlmModel {
        id: "qwen3-4b",
        label: "Qwen3 4B",
        repo: "Qwen/Qwen3-4B-GGUF",
        file: "Qwen3-4B-Q4_K_M.gguf",
        size_gb: 2.5,
        min_ram_gb: 16,
        blurb: "Точнее понимает смысл и исправляет ослышки; хочет 16 ГБ памяти.",
    },
    LlmModel {
        id: "gemma-3-4b-it",
        label: "Gemma 3 4B",
        repo: "ggml-org/gemma-3-4b-it-GGUF",
        file: "gemma-3-4b-it-Q4_K_M.gguf",
        size_gb: 2.5,
        min_ram_gb: 16,
        blurb: "Альтернатива от Google, ровный стиль в документах и письмах.",
    },
    LlmModel {
        id: "qwen3-8b",
        label: "Qwen3 8B",
        repo: "Qwen/Qwen3-8B-GGUF",
        file: "Qwen3-8B-Q4_K_M.gguf",
        size_gb: 5.0,
        min_ram_gb: 24,
        blurb: "Лучшее качество; нужна видеокарта или 24 ГБ памяти.",
    },
];

/// Модель по умолчанию для новой установки.
pub const DEFAULT_MODEL: &str = "qwen2.5-3b-instruct";

pub fn catalog_model(id: &str) -> Option<&'static LlmModel> {
    CATALOG.iter().find(|m| m.id == id.trim())
}

// ───────────────────────────── Пути ─────────────────────────────

/// Каталог GGUF-моделей: `<data>/models/llm/`.
pub fn models_dir() -> PathBuf {
    let dir = crate::paths::models_dir().join("llm");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Корень runtime: `<data>/runtime/llama-<tag>/<flavor>/`.
fn runtime_dir(flavor: &str) -> PathBuf {
    crate::paths::data_dir()
        .join("runtime")
        .join(format!("llama-{RUNTIME_TAG}"))
        .join(flavor)
}

fn server_binary_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

/// Сборка runtime под платформу и режим ускорения.
///
/// Windows + NVIDIA/AMD/Intel → Vulkan-сборка: работает на любой карте с
/// драйвером и не тянет 370 МБ CUDA-runtime. Без ускорения — CPU-сборка.
/// macOS Apple Silicon — Metal в единственной arm64-сборке.
pub fn runtime_flavor(gpu: bool) -> &'static str {
    if cfg!(target_os = "macos") {
        "macos-arm64"
    } else if cfg!(windows) {
        if gpu {
            "win-vulkan-x64"
        } else {
            "win-cpu-x64"
        }
    } else {
        "ubuntu-x64"
    }
}

fn runtime_archive_name(flavor: &str) -> String {
    let ext = if flavor.starts_with("win-") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("llama-{RUNTIME_TAG}-bin-{flavor}.{ext}")
}

/// Найти `llama-server` внутри распакованного runtime (архивы разных
/// платформ кладут бинарь на разную глубину: корень, `build/bin/`).
fn find_server_binary(root: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, depth: u8, name: &str) -> Option<PathBuf> {
        let entries = std::fs::read_dir(dir).ok()?;
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.file_name().and_then(|n| n.to_str()) == Some(name) {
                return Some(path);
            }
            if path.is_dir() {
                subdirs.push(path);
            }
        }
        if depth == 0 {
            return None;
        }
        subdirs.iter().find_map(|sub| walk(sub, depth - 1, name))
    }
    walk(root, 4, server_binary_name())
}

pub fn runtime_installed(gpu: bool) -> bool {
    find_server_binary(&runtime_dir(runtime_flavor(gpu))).is_some()
}

fn model_path(model: &LlmModel) -> PathBuf {
    models_dir().join(model.file)
}

fn marker_path(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|v| v.to_str()).unwrap_or("model");
    path.with_file_name(format!("{name}.sha256"))
}

/// Модель установлена: файл на месте и есть маркер проверенной суммы.
pub fn model_installed(id: &str) -> bool {
    let Some(model) = catalog_model(id) else {
        return false;
    };
    let path = model_path(model);
    path.is_file() && marker_path(&path).is_file()
}

/// Готов ли встроенный ИИ к работе с выбранной моделью.
pub fn configured(s: &crate::settings::Settings) -> bool {
    model_installed(&s.builtin_llm_model) && runtime_installed(crate::paths::gpu_active())
}

// ───────────────────────────── Загрузка ─────────────────────────────

static DOWNLOADING: AtomicBool = AtomicBool::new(false);

/// Описание файла для загрузки: адрес, размер, sha256 (hex, нижний регистр).
#[derive(Debug, Clone, PartialEq)]
pub struct Remote {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

fn curl_json(url: &str, proxy: &str, headers: &[&str]) -> Result<serde_json::Value> {
    let mut cmd = net::curl();
    net::apply_proxy(&mut cmd, proxy);
    cmd.arg("-sL")
        .arg("--fail")
        .arg("-m")
        .arg("30")
        .arg("-A")
        .arg("VoxFlow");
    for h in headers {
        cmd.arg("-H").arg(h);
    }
    cmd.arg(url);
    let out = cmd
        .output()
        .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "{url}: curl завершился с ошибкой {} ({})",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| anyhow!("{url}: ответ не JSON: {e}"))
}

/// Разобрать релиз GitHub: найти asset по имени, взять его размер и digest.
pub fn parse_release_asset(release: &serde_json::Value, name: &str) -> Result<Remote> {
    let assets = release
        .get("assets")
        .and_then(|a| a.as_array())
        .ok_or_else(|| anyhow!("в ответе GitHub нет списка файлов релиза"))?;
    let asset = assets
        .iter()
        .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(name))
        .ok_or_else(|| anyhow!("в релизе llama.cpp {RUNTIME_TAG} нет файла {name}"))?;
    let url = asset
        .get("browser_download_url")
        .and_then(|u| u.as_str())
        .ok_or_else(|| anyhow!("у файла {name} нет адреса загрузки"))?
        .to_string();
    let size = asset.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
    let digest = asset
        .get("digest")
        .and_then(|d| d.as_str())
        .and_then(|d| d.strip_prefix("sha256:"))
        .ok_or_else(|| {
            anyhow!("GitHub не сообщил контрольную сумму файла {name} — загрузка отменена")
        })?
        .to_ascii_lowercase();
    Ok(Remote {
        url,
        size,
        sha256: digest,
    })
}

/// Разобрать дерево репозитория HuggingFace: размер и sha256 LFS-файла.
pub fn parse_hf_tree(tree: &serde_json::Value, repo: &str, file: &str) -> Result<Remote> {
    let entries = tree
        .as_array()
        .ok_or_else(|| anyhow!("HuggingFace вернул не список файлов для {repo}"))?;
    let entry = entries
        .iter()
        .find(|e| e.get("path").and_then(|p| p.as_str()) == Some(file))
        .ok_or_else(|| anyhow!("в репозитории {repo} нет файла {file}"))?;
    let lfs = entry
        .get("lfs")
        .ok_or_else(|| anyhow!("{file}: нет LFS-метаданных, контрольную сумму взять неоткуда"))?;
    let sha256 = lfs
        .get("oid")
        .and_then(|o| o.as_str())
        .ok_or_else(|| anyhow!("{file}: нет sha256 в LFS-метаданных"))?
        .to_ascii_lowercase();
    let size = lfs
        .get("size")
        .and_then(|s| s.as_u64())
        .or_else(|| entry.get("size").and_then(|s| s.as_u64()))
        .unwrap_or(0);
    Ok(Remote {
        url: format!("https://huggingface.co/{repo}/resolve/main/{file}"),
        size,
        sha256,
    })
}

fn resolve_runtime(flavor: &str, proxy: &str) -> Result<Remote> {
    let url =
        format!("https://api.github.com/repos/ggml-org/llama.cpp/releases/tags/{RUNTIME_TAG}");
    let release = curl_json(&url, proxy, &["Accept: application/vnd.github+json"])?;
    parse_release_asset(&release, &runtime_archive_name(flavor))
}

fn resolve_model(model: &LlmModel, proxy: &str) -> Result<Remote> {
    let url = format!("https://huggingface.co/api/models/{}/tree/main", model.repo);
    let tree = curl_json(&url, proxy, &[])?;
    parse_hf_tree(&tree, model.repo, model.file)
}

fn sha256_of(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buf = [0_u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Скачать файл с докачкой и прогрессом (`model:progress` с `name = event_name`),
/// затем сверить sha256. Возвращает путь к готовому файлу.
fn download_verified(
    app: &AppHandle,
    event_name: &str,
    remote: &Remote,
    dest: &Path,
    proxy: &str,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let part = dest.with_extension(format!(
        "{}.part",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("bin")
    ));
    let total = remote.size;
    let part_len = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if total > 0 && part_len > total {
        let _ = std::fs::remove_file(&part);
    }

    let mut reset_after_unsupported_resume = false;
    loop {
        let resume = std::fs::metadata(&part)
            .map(|m| m.len() > 0)
            .unwrap_or(false);
        let mut cmd = net::curl();
        net::apply_proxy(&mut cmd, proxy);
        cmd.arg("-L")
            .arg("--fail")
            .arg("--silent")
            .arg("--show-error")
            .arg("-A")
            .arg("VoxFlow")
            .arg("--connect-timeout")
            .arg("15")
            .arg("--speed-limit")
            .arg("1024")
            .arg("--speed-time")
            .arg("30")
            .arg("--retry")
            .arg("3")
            .arg("-o")
            .arg(&part);
        if resume {
            cmd.arg("--continue-at").arg("-");
        }
        cmd.arg(&remote.url);
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;
        let status = loop {
            std::thread::sleep(Duration::from_millis(400));
            let received = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            let _ = app.emit(
                "model:progress",
                serde_json::json!({ "name": event_name, "received": received, "total": total }),
            );
            if let Some(status) = child.try_wait()? {
                break status;
            }
        };
        if status.success() {
            break;
        }
        // 33 = сервер не умеет докачку — начинаем заново один раз.
        if status.code() == Some(33) && resume && !reset_after_unsupported_resume {
            let _ = std::fs::remove_file(&part);
            reset_after_unsupported_resume = true;
            continue;
        }
        return Err(anyhow!(
            "загрузка прервалась ({status}); частичный файл сохранён, повторите позже"
        ));
    }

    let actual_len = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if total > 0 && actual_len != total {
        let _ = std::fs::remove_file(&part);
        return Err(anyhow!(
            "неполная загрузка: ожидалось {total} байт, получено {actual_len}"
        ));
    }
    let actual = sha256_of(&part)?;
    if actual != remote.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(anyhow!(
            "контрольная сумма не совпала: ожидалась {}, получена {actual}",
            remote.sha256
        ));
    }
    let _ = std::fs::remove_file(dest);
    std::fs::rename(&part, dest)?;
    std::fs::write(marker_path(dest), format!("{}\n", remote.sha256))?;
    Ok(())
}

/// Распаковать архив системным `tar` (bsdtar на Windows читает zip).
fn extract_archive(archive: &Path, into: &Path) -> Result<()> {
    std::fs::create_dir_all(into)?;
    let mut cmd = Command::new("tar");
    #[cfg(windows)]
    cmd.creation_flags(NO_WINDOW);
    let out = cmd
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .output()
        .map_err(|e| anyhow!("не удалось запустить tar: {e}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "tar не распаковал {}: {}",
            archive.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn ensure_runtime(app: &AppHandle, gpu: bool, proxy: &str) -> Result<PathBuf> {
    let flavor = runtime_flavor(gpu);
    let root = runtime_dir(flavor);
    if let Some(bin) = find_server_binary(&root) {
        return Ok(bin);
    }
    let remote = resolve_runtime(flavor, proxy)?;
    let archive = root.join(runtime_archive_name(flavor));
    download_verified(app, "llama-runtime", &remote, &archive, proxy)?;
    extract_archive(&archive, &root)?;
    let _ = std::fs::remove_file(&archive);
    let _ = std::fs::remove_file(marker_path(&archive));
    find_server_binary(&root).ok_or_else(|| {
        anyhow!(
            "в архиве {} не нашёлся {}",
            remote.url,
            server_binary_name()
        )
    })
}

/// Скачать runtime (если нет) и модель. Прогресс уходит событиями
/// `model:progress` / `model:done` / `model:error` с `name = id` модели;
/// загрузка runtime идёт под именем `llama-runtime`.
pub fn download(app: AppHandle, id: String, proxy: String) -> Result<()> {
    let model = catalog_model(&id).ok_or_else(|| anyhow!("неизвестная модель: {id}"))?;
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        return Err(anyhow!("другая загрузка ещё идёт"));
    }
    std::thread::Builder::new()
        .name("voxflow-llm-download".into())
        .spawn(move || {
            let result = (|| -> Result<()> {
                ensure_runtime(&app, crate::paths::gpu_active(), &proxy)?;
                let dest = model_path(model);
                if !model_installed(model.id) {
                    let remote = resolve_model(model, &proxy)?;
                    download_verified(&app, model.id, &remote, &dest, &proxy)?;
                }
                Ok(())
            })();
            DOWNLOADING.store(false, Ordering::SeqCst);
            match result {
                Ok(()) => {
                    let _ = app.emit("model:done", serde_json::json!({ "name": model.id }));
                }
                Err(e) => {
                    log::error!("локальный ИИ {}: {e:#}", model.id);
                    let _ = app.emit(
                        "model:error",
                        serde_json::json!({ "name": model.id, "error": e.to_string(), "message": e.to_string() }),
                    );
                }
            }
        })
        .map_err(|e| anyhow!("не удалось запустить поток загрузки: {e}"))?;
    Ok(())
}

pub fn is_downloading() -> bool {
    DOWNLOADING.load(Ordering::SeqCst)
}

/// Удалить файл модели (сервер с этой моделью останавливается).
pub fn delete(id: &str) -> Result<()> {
    let model = catalog_model(id).ok_or_else(|| anyhow!("неизвестная модель: {id}"))?;
    if server_status().model_id.as_deref() == Some(model.id) {
        stop();
    }
    let path = model_path(model);
    let _ = std::fs::remove_file(marker_path(&path));
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

// ───────────────────────────── Сервер ─────────────────────────────

struct Running {
    child: Child,
    model_id: String,
    gpu: bool,
}

static SERVER: Mutex<Option<Running>> = Mutex::new(None);

#[derive(Serialize, Clone, Debug, Default)]
pub struct ServerStatus {
    pub running: bool,
    /// Сервер как раз поднимается (модель грузится в память).
    pub starting: bool,
    pub model_id: Option<String>,
    pub gpu: bool,
}

/// Состояние сервера без ожидания: пока [`ensure_server`] держит замок на
/// время загрузки модели, синхронная команда интерфейса не должна висеть
/// вместе с ним — отвечаем «поднимается».
pub fn server_status() -> ServerStatus {
    let Some(mut guard) = SERVER.try_lock() else {
        return ServerStatus {
            starting: true,
            ..ServerStatus::default()
        };
    };
    if let Some(run) = guard.as_mut() {
        // Процесс мог умереть (нехватка памяти) — не врём, что он работает.
        if run.child.try_wait().ok().flatten().is_some() {
            *guard = None;
            return ServerStatus::default();
        }
        return ServerStatus {
            running: true,
            starting: false,
            model_id: Some(run.model_id.clone()),
            gpu: run.gpu,
        };
    }
    ServerStatus::default()
}

fn health_ok() -> bool {
    let mut cmd = net::curl();
    let out = cmd
        .arg("--noproxy")
        .arg("*")
        .arg("-s")
        .arg("-m")
        .arg("2")
        .arg(format!("http://127.0.0.1:{SERVER_PORT}/health"))
        .output();
    match out {
        Ok(o) if o.status.success() => serde_json::from_slice::<serde_json::Value>(&o.stdout)
            .ok()
            .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(|s| s == "ok"))
            .unwrap_or(false),
        _ => false,
    }
}

fn server_log_path() -> PathBuf {
    crate::paths::data_dir().join("llama-server.log")
}

/// Аргументы запуска сервера — вынесены ради теста и читаемости.
pub fn server_args(model: &Path, gpu: bool, threads: u32) -> Vec<String> {
    vec![
        "-m".into(),
        model.display().to_string(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        SERVER_PORT.to_string(),
        "-c".into(),
        CONTEXT_TOKENS.to_string(),
        "-ngl".into(),
        if gpu { "99".into() } else { "0".into() },
        "-t".into(),
        threads.to_string(),
        "--no-webui".into(),
        "--jinja".into(),
    ]
}

/// Поднять сервер с нужной моделью (или убедиться, что он уже поднят).
pub fn ensure_server(model_id: &str, gpu: bool, threads: u32) -> Result<()> {
    let model = catalog_model(model_id).ok_or_else(|| anyhow!("неизвестная модель: {model_id}"))?;
    let path = model_path(model);
    if !model_installed(model.id) {
        return Err(anyhow!(
            "модель «{}» не скачана — откройте «Локальный ИИ» и нажмите «Скачать»",
            model.label
        ));
    }
    let bin = find_server_binary(&runtime_dir(runtime_flavor(gpu))).ok_or_else(|| {
        anyhow!("движок llama.cpp не установлен — скачайте модель заново в «Локальном ИИ»")
    })?;

    let mut guard = SERVER.lock();
    if let Some(run) = guard.as_mut() {
        let alive = run.child.try_wait().ok().flatten().is_none();
        if alive && run.model_id == model.id && run.gpu == gpu && health_ok() {
            return Ok(());
        }
        let _ = run.child.kill();
        let _ = run.child.wait();
        *guard = None;
    }

    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(server_log_path())
        .ok();
    let (out, err) = match log {
        Some(f) => (Stdio::from(f.try_clone().unwrap_or(f)), Stdio::null()),
        None => (Stdio::null(), Stdio::null()),
    };
    let mut cmd = Command::new(&bin);
    cmd.args(server_args(&path, gpu, threads))
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    if let Some(dir) = bin.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    cmd.creation_flags(NO_WINDOW);
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("не удалось запустить llama-server: {e}"))?;

    let started = Instant::now();
    loop {
        if health_ok() {
            break;
        }
        if let Some(status) = child.try_wait()? {
            return Err(anyhow!(
                "llama-server завершился при запуске ({status}); подробности в {}",
                server_log_path().display()
            ));
        }
        if started.elapsed() > STARTUP_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(
                "llama-server не ответил за {} с — модель слишком тяжёлая для этой машины?",
                STARTUP_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    log::info!(
        "llama-server поднят: модель {} gpu={gpu} за {} мс",
        model.id,
        started.elapsed().as_millis()
    );
    *guard = Some(Running {
        child,
        model_id: model.id.to_string(),
        gpu,
    });
    Ok(())
}

/// Остановить сервер (выход из приложения, удаление модели, выключение ИИ).
pub fn stop() {
    // Во время старта замок занят загрузкой модели; ждать её из синхронной
    // команды нельзя — лучше оставить процесс, чем подвесить интерфейс.
    let Some(mut guard) = SERVER.try_lock_for(Duration::from_secs(2)) else {
        log::warn!("llama-server ещё поднимается — остановка отложена");
        return;
    };
    if let Some(mut run) = guard.take() {
        let _ = run.child.kill();
        let _ = run.child.wait();
    }
}

// ───────────────────────────── Запросы ─────────────────────────────

/// Тело запроса — OpenAI-совместимый chat. `chat_template_kwargs` глушит
/// режим размышления Qwen3; остальным моделям поле безвредно.
pub fn chat_body(system: &str, user: &str, max_tokens: u32) -> serde_json::Value {
    serde_json::json!({
        "model": "voxflow-local",
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user }
        ],
        "stream": false,
        "temperature": 0.2,
        "top_p": 0.8,
        "max_tokens": max_tokens,
        "chat_template_kwargs": { "enable_thinking": false }
    })
}

/// Разобрать ответ: текст первого choice; `finish_reason == "length"` —
/// обрыв, такой ответ вставлять нельзя.
pub fn parse_chat_completion(v: &serde_json::Value) -> Result<String> {
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("неизвестная ошибка");
        return Err(anyhow!("llama-server: {msg}"));
    }
    let choice = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .ok_or_else(|| anyhow!("llama-server вернул ответ без choices"))?;
    if choice.get("finish_reason").and_then(|f| f.as_str()) == Some("length") {
        return Err(anyhow!(
            "ответ модели оборван по лимиту токенов — вставлять неполный текст нельзя"
        ));
    }
    let content = choice
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| anyhow!("llama-server вернул ответ без текста"))?;
    Ok(content.to_string())
}

fn chat(system: &str, user: &str, timeout_s: u64, max_tokens: u32) -> Result<String> {
    let body = chat_body(system, user, max_tokens);
    let payload = serde_json::to_vec(&body).map_err(|e| anyhow!("сериализация тела: {e}"))?;
    let req = net::TempPayload::write_json("llm_req", &payload)?;
    let data_arg = req.curl_data_arg();
    let mut cmd = net::curl();
    cmd.arg("--noproxy")
        .arg("*")
        .arg("-s")
        .arg("-m")
        .arg(timeout_s.to_string())
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("-X")
        .arg("POST")
        .arg("--data-binary")
        .arg(&data_arg)
        .arg(format!(
            "http://127.0.0.1:{SERVER_PORT}/v1/chat/completions"
        ));
    let out = cmd
        .output()
        .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;
    if !out.status.success() && out.stdout.is_empty() {
        if net::curl_timed_out(&out.status) {
            return Err(anyhow!(
                "локальная модель не ответила за {timeout_s} с. Увеличьте таймаут ИИ или возьмите модель полегче"
            ));
        }
        return Err(anyhow!(
            "локальный сервер ИИ недоступен: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| anyhow!("ответ llama-server — не JSON: {e}"))?;
    let content = parse_chat_completion(&v)?;
    let cleaned = crate::ollama::strip_think(&content);
    if cleaned.is_empty() {
        return Err(anyhow!("локальная модель вернула пустой ответ"));
    }
    if crate::ollama::looks_like_reasoning(&cleaned, user) {
        return Err(anyhow!(
            "локальная модель вернула рассуждение вместо текста (рефайн пропущен)"
        ));
    }
    Ok(cleaned)
}

/// Отрефайнить текст встроенной моделью: поднять сервер при необходимости и
/// спросить. Сигнатура повторяет [`crate::ollama::refine`] по смыслу.
pub fn refine(s: &crate::settings::Settings, system: &str, user: &str) -> Result<String> {
    let gpu = crate::paths::gpu_active();
    ensure_server(&s.builtin_llm_model, gpu, s.effective_threads())?;
    let input = net::estimate_tokens(system) + net::estimate_tokens(user);
    let max_tokens = net::output_token_budget(input, s.rewrite_max_output_tokens.min(4096));
    chat(system, user, s.backend_timeout_s(true), max_tokens)
}

/// Короткая проба для кнопки «Проверить».
pub fn ping(s: &crate::settings::Settings) -> Result<String> {
    let gpu = crate::paths::gpu_active();
    ensure_server(&s.builtin_llm_model, gpu, s.effective_threads())?;
    let text = chat(
        "Ответь ровно одним словом.",
        "Напиши: ОК",
        s.backend_timeout_s(true),
        16,
    )?;
    Ok(text.trim().to_string())
}

/// Фоновый прогрев: если встроенный ИИ выбран и готов — поднять сервер сразу,
/// чтобы первая диктовка не ждала загрузку модели.
pub fn warmup(s: &crate::settings::Settings) {
    if s.ai_backend != "builtin" || !configured(s) {
        return;
    }
    let model = s.builtin_llm_model.clone();
    let threads = s.effective_threads();
    let _ = std::thread::Builder::new()
        .name("voxflow-llm-warmup".into())
        .spawn(move || {
            if let Err(e) = ensure_server(&model, crate::paths::gpu_active(), threads) {
                log::warn!("прогрев локального ИИ: {e:#}");
            }
        });
}

// ───────────────────────────── Состояние для UI ─────────────────────────────

#[derive(Serialize, Clone, Debug)]
pub struct ModelView {
    pub id: String,
    pub label: String,
    pub size_gb: f32,
    pub min_ram_gb: u32,
    pub blurb: String,
    pub installed: bool,
    /// Потянет ли эта машина модель (по тем же ярусам, что у Ollama-каталога).
    pub fits: bool,
    /// Самая крупная из подходящих — её и предлагаем.
    pub recommended: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct LocalLlmState {
    pub runtime_tag: String,
    pub runtime_installed: bool,
    pub gpu: bool,
    pub downloading: bool,
    pub machine: crate::local_ai::Machine,
    pub models: Vec<ModelView>,
    pub server: ServerStatus,
}

pub fn state() -> LocalLlmState {
    let machine = crate::local_ai::probe_machine();
    let gpu = crate::paths::gpu_active();
    let fits: Vec<bool> = CATALOG
        .iter()
        .map(|m| crate::local_ai::fits_spec(m.size_gb, m.min_ram_gb, &machine))
        .collect();
    let recommended = fits.iter().rposition(|f| *f);
    let models = CATALOG
        .iter()
        .enumerate()
        .map(|(i, m)| ModelView {
            id: m.id.to_string(),
            label: m.label.to_string(),
            size_gb: m.size_gb,
            min_ram_gb: m.min_ram_gb,
            blurb: m.blurb.to_string(),
            installed: model_installed(m.id),
            fits: fits[i],
            recommended: recommended == Some(i),
        })
        .collect();
    LocalLlmState {
        runtime_tag: RUNTIME_TAG.to_string(),
        runtime_installed: runtime_installed(gpu),
        gpu,
        downloading: is_downloading(),
        machine,
        models,
        server: server_status(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique_and_default_exists() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len());
        assert!(catalog_model(DEFAULT_MODEL).is_some());
        // Порядок — по весу: интерфейс берёт «самую крупную из подходящих».
        for pair in CATALOG.windows(2) {
            assert!(pair[0].size_gb <= pair[1].size_gb);
        }
    }

    #[test]
    fn runtime_archive_names_follow_llama_cpp_release_layout() {
        assert_eq!(
            runtime_archive_name("win-cpu-x64"),
            format!("llama-{RUNTIME_TAG}-bin-win-cpu-x64.zip")
        );
        assert_eq!(
            runtime_archive_name("macos-arm64"),
            format!("llama-{RUNTIME_TAG}-bin-macos-arm64.tar.gz")
        );
        assert_eq!(
            runtime_archive_name("win-vulkan-x64"),
            format!("llama-{RUNTIME_TAG}-bin-win-vulkan-x64.zip")
        );
    }

    #[test]
    fn github_release_asset_requires_digest() {
        let release = serde_json::json!({
            "assets": [
                { "name": "llama-b1-bin-win-cpu-x64.zip", "size": 10,
                  "digest": "sha256:ABCD", "browser_download_url": "https://x/y.zip" },
                { "name": "no-digest.zip", "size": 1, "browser_download_url": "https://x/n.zip" }
            ]
        });
        let r = parse_release_asset(&release, "llama-b1-bin-win-cpu-x64.zip").unwrap();
        assert_eq!(
            r,
            Remote {
                url: "https://x/y.zip".into(),
                size: 10,
                sha256: "abcd".into()
            }
        );
        assert!(parse_release_asset(&release, "no-digest.zip").is_err());
        assert!(parse_release_asset(&release, "missing.zip").is_err());
    }

    #[test]
    fn hf_tree_gives_lfs_sha256_and_resolve_url() {
        let tree = serde_json::json!([
            { "type": "file", "path": "README.md", "size": 5 },
            { "type": "file", "path": "m.gguf", "size": 7,
              "lfs": { "oid": "DEAD", "size": 7 } }
        ]);
        let r = parse_hf_tree(&tree, "org/repo", "m.gguf").unwrap();
        assert_eq!(r.url, "https://huggingface.co/org/repo/resolve/main/m.gguf");
        assert_eq!(r.sha256, "dead");
        assert_eq!(r.size, 7);
        assert!(parse_hf_tree(&tree, "org/repo", "README.md").is_err());
    }

    #[test]
    fn chat_response_rejects_truncation_and_errors() {
        let ok = serde_json::json!({
            "choices": [{ "finish_reason": "stop", "message": { "content": "Привет" } }]
        });
        assert_eq!(parse_chat_completion(&ok).unwrap(), "Привет");
        let cut = serde_json::json!({
            "choices": [{ "finish_reason": "length", "message": { "content": "Прив" } }]
        });
        assert!(parse_chat_completion(&cut).is_err());
        let err = serde_json::json!({ "error": { "message": "no slot" } });
        assert!(parse_chat_completion(&err).is_err());
    }

    #[test]
    fn server_args_switch_gpu_layers() {
        let cpu = server_args(Path::new("m.gguf"), false, 4);
        let gpu = server_args(Path::new("m.gguf"), true, 4);
        let ngl = |args: &[String]| {
            args.iter()
                .position(|a| a == "-ngl")
                .map(|i| args[i + 1].clone())
        };
        assert_eq!(ngl(&cpu).as_deref(), Some("0"));
        assert_eq!(ngl(&gpu).as_deref(), Some("99"));
        assert!(cpu.contains(&"--no-webui".to_string()));
        assert!(chat_body("s", "u", 10)["chat_template_kwargs"]["enable_thinking"] == false);
    }

    #[test]
    fn finds_server_binary_in_nested_layout() {
        let root = std::env::temp_dir().join(format!("voxflow-llm-{}", std::process::id()));
        let nested = root.join("build").join("bin");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join(server_binary_name()), b"x").unwrap();
        assert_eq!(
            find_server_binary(&root).unwrap(),
            nested.join(server_binary_name())
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
