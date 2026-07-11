//! Resolution of data/model/tmp/resource paths.
//!
//! ВАЖНО (R5): whisper.cpp открывает файлы через ANSI-codepage, поэтому пути к
//! МОДЕЛИ и временным WAV должны быть ASCII. Их кладём в %LOCALAPPDATA%\VoxFlow
//! (юзернейм ASCII). Сами exe/DLL грузятся ОС по wide-пути — им кириллица ок.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Manager};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

const STALE_TEMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    // Windows privacy is inherited from the per-user LocalAppData ACL. Unix
    // mode bits do not have a faithful Windows equivalent in std.
    Ok(())
}

fn ensure_private_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    set_mode(path, 0o700)
}

fn private_dir(path: PathBuf) -> PathBuf {
    if let Err(e) = ensure_private_dir(&path) {
        log::warn!("не удалось защитить каталог {}: {e}", path.display());
    }
    path
}

/// Restrict an existing user-data file to the current Unix user. On Windows
/// the file keeps the ACL inherited from LocalAppData.
pub fn set_private_file_permissions(path: &Path) -> io::Result<()> {
    set_mode(path, 0o600)
}

/// Create or truncate a sensitive file without an umask-dependent exposure
/// window on Unix. Existing files are tightened as well.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    set_private_file_permissions(path)?;
    Ok(file)
}

/// Open a sensitive append-only diagnostic file with private Unix mode bits.
pub fn open_private_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    set_private_file_permissions(path)?;
    Ok(file)
}

#[cfg(not(test))]
fn data_dir_path() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
    base.join("VoxFlow")
}

#[cfg(test)]
fn data_dir_path() -> PathBuf {
    // `dirs::data_local_dir()` resolves FOLDERID_LocalAppData through WinAPI
    // on Windows, so overriding LOCALAPPDATA does not isolate unit tests. Keep
    // every test binary out of the real user profile by construction instead.
    std::env::var_os("VOXFLOW_TEST_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("voxflow-test-data-{}", std::process::id()))
        })
}

/// %LOCALAPPDATA%\VoxFlow (ASCII), создаётся при первом обращении.
pub fn data_dir() -> PathBuf {
    private_dir(data_dir_path())
}

pub fn models_dir() -> PathBuf {
    private_dir(data_dir().join("models"))
}

pub fn tmp_dir() -> PathBuf {
    private_dir(data_dir().join("tmp"))
}

pub fn unique_tmp_path(prefix: &str, ext: &str) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let clean_prefix: String = prefix
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean_ext = ext.trim_start_matches('.');
    tmp_dir().join(format!(
        "{clean_prefix}-{}-{nanos}-{seq}.{clean_ext}",
        std::process::id()
    ))
}

fn cleanup_stale_temp_files_in(
    dir: &Path,
    now: SystemTime,
    max_age: Duration,
) -> io::Result<usize> {
    let mut removed = 0;
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        // Never follow or delete symlinks and never recurse. Cleanup is limited
        // to regular files created by VoxFlow in its own tmp directory.
        if !metadata.file_type().is_file() {
            continue;
        }
        let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
        let sensitive_temp =
            extension.eq_ignore_ascii_case("wav") || extension.eq_ignore_ascii_case("json");
        // Custom updater downloads into this private tmp directory. Interrupted
        // or already-launched installers used to survive forever (hundreds of
        // megabytes each). Match only our fixed updater prefix; never treat an
        // arbitrary .exe in the directory as disposable.
        let stale_windows_installer = extension.eq_ignore_ascii_case("exe")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("VoxFlow-Setup-"));
        if !sensitive_temp && !stale_windows_installer {
            continue;
        }
        let Some(age) = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
        else {
            continue;
        };
        if age >= max_age && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Remove crash leftovers containing speech/request payloads and completed or
/// interrupted updater installers. Files newer than 24 hours are preserved to
/// avoid racing another process, a long job, or an installer still in use.
pub fn cleanup_stale_temp_files() -> io::Result<usize> {
    cleanup_stale_temp_files_in(&tmp_dir(), SystemTime::now(), STALE_TEMP_MAX_AGE)
}

pub struct TempFileGuard {
    path: PathBuf,
}

impl TempFileGuard {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Каталог датасета персонализации (аудио-сэмплы пользователя).
pub fn dataset_dir() -> PathBuf {
    private_dir(data_dir().join("dataset"))
}

pub fn db_path() -> PathBuf {
    data_dir().join("voxflow.db")
}

pub fn model_path(name: &str) -> PathBuf {
    models_dir().join(name)
}

#[cfg(target_os = "macos")]
fn macos_whisper_resource_dir() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "whisper-darwin-arm64"
    } else {
        "whisper-darwin-x64"
    }
}

/// Каталог моделей GigaAM (models/gigaam, ASCII-путь — ort открывает по wide,
/// но единообразие с whisper-моделями дешевле, чем особые случаи).
pub fn gigaam_dir() -> PathBuf {
    private_dir(models_dir().join(crate::gigaam::GIGAAM_DIR))
}

/// Каталог моделей Parakeet TDT v3 (models/parakeet). Без create_dir_all:
/// installed-статус читается по metadata, каталог создаёт сам загрузчик.
pub fn parakeet_dir() -> PathBuf {
    models_dir().join(crate::parakeet::PARAKEET_DIR)
}

/// Dev-копия Silero VAD (вшита на этапе компиляции).
const DEV_VAD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/vad/silero_vad.onnx");

/// silero_vad.onnx: в проде — из resource_dir, в dev/standalone — из dev-копии.
pub fn vad_model_path(app: Option<&AppHandle>) -> PathBuf {
    if let Some(app) = app {
        if let Ok(r) = app.path().resource_dir() {
            for c in [
                r.join("resources").join("vad").join("silero_vad.onnx"),
                r.join("vad").join("silero_vad.onnx"),
            ] {
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
const DEV_WHISPER_CUDA: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/whisper-cuda/Release"
);

/// Есть ли NVIDIA-GPU с драйвером (наличие nvcuda.dll в System32).
pub fn has_nvidia() -> bool {
    #[cfg(not(windows))]
    {
        false
    }
    #[cfg(windows)]
    {
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| "C:/Windows".into());
        std::path::Path::new(&sys)
            .join("System32")
            .join("nvcuda.dll")
            .exists()
    }
}

/// Каталог с whisper-cli.exe / whisper-server.exe + DLL.
/// При наличии NVIDIA приоритет у CUDA-сборки (whisper-cuda), иначе CPU (whisper).
/// В проде — из resource_dir, в dev — из dev-копий.
pub fn whisper_dir(app: &AppHandle) -> PathBuf {
    let cli = whisper_cli_name();
    let res = app.path().resource_dir().ok();
    let mut candidates: Vec<PathBuf> = Vec::new();

    // GPU-сборки имеют приоритет (и resource, и dev) над CPU.
    if has_nvidia() {
        if let Some(r) = &res {
            candidates.push(r.join("resources").join("whisper-cuda").join("Release"));
            candidates.push(r.join("whisper-cuda"));
        }
        candidates.push(PathBuf::from(DEV_WHISPER_CUDA));
    }
    for candidate in candidates {
        if candidate.join(cli).exists() {
            return candidate;
        }
    }
    whisper_cpu_dir(app)
}

/// Заведомо не-CUDA runtime. Используется как рабочий fallback, когда Windows
/// содержит nvcuda.dll, но установленный драйвер несовместим с bundled CUDA DLL.
pub fn whisper_cpu_dir(app: &AppHandle) -> PathBuf {
    let cli = whisper_cli_name();
    let res = app.path().resource_dir().ok();
    let mut candidates: Vec<PathBuf> = Vec::new();

    #[cfg(target_os = "macos")]
    {
        let dir = macos_whisper_resource_dir();
        if let Some(r) = &res {
            candidates.push(r.join("resources").join(dir));
            candidates.push(r.join(dir));
        }
        candidates.push(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("resources")
                .join(dir),
        );
    }

    if let Some(r) = &res {
        candidates.push(r.join("resources").join("whisper").join("Release"));
        candidates.push(r.join("whisper"));
    }
    candidates.push(PathBuf::from(DEV_WHISPER_CPU));

    for candidate in candidates {
        if candidate.join(cli).exists() {
            return candidate;
        }
    }
    PathBuf::from(DEV_WHISPER_CPU)
}

/// Dev-каталог whisper (для `--selftest` без Tauri-контекста): CUDA при наличии GPU.
pub fn whisper_dir_standalone() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(macos_whisper_resource_dir());
        if p.join(whisper_cli_name()).exists() {
            return p;
        }
    }
    if has_nvidia() {
        let p = PathBuf::from(DEV_WHISPER_CUDA);
        if p.join(whisper_cli_name()).exists() {
            return p;
        }
    }
    PathBuf::from(DEV_WHISPER_CPU)
}

pub fn whisper_cli_name() -> &'static str {
    if cfg!(windows) {
        "whisper-cli.exe"
    } else {
        "whisper-cli"
    }
}

#[allow(dead_code)] // задел под whisper-server (persistent), пока используем cli one-shot
pub fn whisper_server_name() -> &'static str {
    if cfg!(windows) {
        "whisper-server.exe"
    } else {
        "whisper-server"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_never_uses_the_production_profile() {
        let path = data_dir_path();
        assert_eq!(path, data_dir());
        assert_ne!(path, dirs::data_local_dir().unwrap().join("VoxFlow"));
    }

    fn test_dir(name: &str) -> PathBuf {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "voxflow-paths-{name}-{}-{nanos}-{seq}",
            std::process::id()
        ))
    }

    #[test]
    fn cleanup_only_removes_old_regular_sensitive_temp_files() {
        let dir = test_dir("cleanup");
        ensure_private_dir(&dir).unwrap();
        let wav = dir.join("utterance.wav");
        let json = dir.join("payload.json");
        let installer = dir.join("VoxFlow-Setup-1_0_8-123.exe");
        let foreign_exe = dir.join("keep-me.exe");
        let keep = dir.join("model.part");
        std::fs::write(&wav, b"speech").unwrap();
        std::fs::write(&json, b"private text").unwrap();
        std::fs::write(&installer, b"old installer").unwrap();
        std::fs::write(&foreign_exe, b"not owned by updater").unwrap();
        std::fs::write(&keep, b"download").unwrap();
        let child = dir.join("nested.wav");
        std::fs::create_dir(&child).unwrap();

        let removed = cleanup_stale_temp_files_in(
            &dir,
            SystemTime::now() + Duration::from_secs(2),
            Duration::from_secs(1),
        )
        .unwrap();

        assert_eq!(removed, 3);
        assert!(!wav.exists());
        assert!(!json.exists());
        assert!(!installer.exists());
        assert!(foreign_exe.exists());
        assert!(keep.exists());
        assert!(child.is_dir());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn private_helpers_tighten_directories_and_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = test_dir("permissions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let file = dir.join("secret.json");
        std::fs::write(&file, b"secret").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
        drop(open_private_append(&file).unwrap());
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_does_not_follow_or_delete_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = test_dir("symlink");
        ensure_private_dir(&dir).unwrap();
        let target = dir.join("target.txt");
        let link = dir.join("linked.wav");
        std::fs::write(&target, b"keep").unwrap();
        symlink(&target, &link).unwrap();

        let removed = cleanup_stale_temp_files_in(
            &dir,
            SystemTime::now() + Duration::from_secs(2),
            Duration::from_secs(1),
        )
        .unwrap();

        assert_eq!(removed, 0);
        assert!(link.symlink_metadata().is_ok());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(dir);
    }
}
