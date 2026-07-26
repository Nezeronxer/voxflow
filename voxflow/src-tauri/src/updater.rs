//! GitHub Releases updater.
//!
//! Asset discovery is platform-specific: Windows x64 gets the Inno Setup
//! installer, macOS gets the matching DMG. Both platforms install
//! automatically; the platform-specific part is only *how* the verified
//! download is applied (spawn the installer vs. swap the app bundle).

use anyhow::{anyhow, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const OWNER: &str = "Nezeronxer";
const REPO: &str = "voxflow";
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/Nezeronxer/voxflow/releases/latest";
const RELEASE_DOWNLOAD_PREFIX: &str = "https://github.com/Nezeronxer/voxflow/releases/download/";
const USER_AGENT: &str = "VoxFlow-Updater";

#[cfg(target_os = "macos")]
const MACOS_BUNDLE_IDS: [&str; 2] = [
    "com.nezeronxer.voxflow.macos",
    // Releases through 1.0.7 used this identifier. Keeping it in the exact
    // allow-list lets a current install remove those stale copies as well.
    "com.voxflow.app",
];

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateTarget {
    WindowsX64,
    MacosArm64,
    MacosX64,
    Unsupported,
}

impl UpdateTarget {
    fn asset_pattern(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::WindowsX64 => Some(("VoxFlow-Setup-", ".exe")),
            Self::MacosArm64 => Some(("VoxFlow-macOS-", "-arm64.dmg")),
            Self::MacosX64 => Some(("VoxFlow-macOS-", "-x64.dmg")),
            Self::Unsupported => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::WindowsX64 => "Windows x64",
            Self::MacosArm64 => "macOS ARM64",
            Self::MacosX64 => "macOS x64",
            Self::Unsupported => "этой платформы",
        }
    }
}

fn current_update_target() -> UpdateTarget {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return UpdateTarget::WindowsX64;
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return UpdateTarget::MacosArm64;
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return UpdateTarget::MacosX64;
    }
    #[allow(unreachable_code)]
    UpdateTarget::Unsupported
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub available: bool,
    pub auto_install: bool,
    pub current_version: String,
    pub latest_version: String,
    pub release_name: String,
    pub release_url: String,
    pub asset_name: String,
    pub asset_url: String,
    pub asset_size: u64,
    pub asset_digest: String,
    pub published_at: String,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInstallResult {
    pub launched: bool,
    pub path: String,
    pub message: String,
}

pub fn check(proxy_url: &str) -> Result<UpdateInfo> {
    let mut cmd = crate::net::curl();
    cmd.arg("-sSfL")
        .arg("-m")
        .arg("25")
        .arg("-H")
        .arg("Accept: application/vnd.github+json")
        .arg("-H")
        .arg("X-GitHub-Api-Version: 2022-11-28")
        .arg("-A")
        .arg(USER_AGENT);
    crate::net::apply_proxy(&mut cmd, proxy_url);
    cmd.arg(LATEST_RELEASE_URL);

    let out = cmd
        .output()
        .map_err(|e| anyhow!("не удалось запустить curl для GitHub Releases: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!(
            "GitHub Releases недоступен: {} {}",
            out.status,
            stderr.trim()
        ));
    }

    let release: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| anyhow!("GitHub Releases вернул не-JSON: {e}"))?;
    update_info_from_release_for(&release, current_update_target())
}

pub fn download_and_launch(
    asset_url: &str,
    asset_name: &str,
    expected_size: u64,
    expected_digest: &str,
    proxy_url: &str,
) -> Result<UpdateInstallResult> {
    validate_asset_url(asset_url)?;
    validate_asset_name_for(asset_name, current_update_target())?;

    let dest = installer_download_path(asset_name);
    let mut cmd = crate::net::curl();
    cmd.arg("-L")
        .arg("--fail")
        .arg("--silent")
        .arg("--show-error")
        .arg("-m")
        .arg("900")
        .arg("-A")
        .arg(USER_AGENT)
        .arg("-o")
        .arg(&dest);
    crate::net::apply_proxy(&mut cmd, proxy_url);
    cmd.arg(asset_url);

    let out = cmd
        .output()
        .map_err(|e| anyhow!("не удалось скачать установщик обновления: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&dest);
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!(
            "скачивание обновления не удалось: {} {}",
            out.status,
            stderr.trim()
        ));
    }
    verify_downloaded_asset(&dest, expected_size, expected_digest).inspect_err(|_| {
        let _ = std::fs::remove_file(&dest);
    })?;

    apply_downloaded_update(&dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&dest);
    })
}

/// Windows: hand the verified Inno Setup package over to a *detached* process.
///
/// The previous version spawned a plain child and treated a successful
/// `CreateProcess` as "installer running", then hard-killed VoxFlow 2.7 s
/// later. Both halves of that were wrong:
///
/// - a plain child stays in the parent's process group and job object, so a job
///   with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` takes the installer down together
///   with VoxFlow — the app vanished and nothing was installed;
/// - nothing ever checked that the installer was still alive, so *any* early
///   failure (killed, blocked, crashed on start) still reported success and the
///   app quit into a silent no-op.
///
/// Now the installer is created detached, in its own process group and broken
/// out of any inherited job, and we only report success once it has survived a
/// short observation window. If it died, VoxFlow stays open and shows why.
#[cfg(windows)]
fn apply_downloaded_update(dest: &Path) -> Result<UpdateInstallResult> {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    use std::os::windows::process::CommandExt;

    let spawn_with = |flags: u32| {
        let mut cmd = Command::new(dest);
        cmd.creation_flags(flags);
        // Never inherit the install directory as the working directory: a
        // process CWD pins that directory and the installer writes into it.
        if let Some(parent) = dest.parent() {
            cmd.current_dir(parent);
        }
        cmd.spawn()
    };

    // Breakaway is refused (ERROR_ACCESS_DENIED) by jobs without
    // JOB_OBJECT_LIMIT_BREAKAWAY_OK, so fall back instead of failing the update.
    let mut child =
        spawn_with(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB)
            .or_else(|_| spawn_with(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP))
            .map_err(|e| anyhow!("не удалось запустить установщик обновления: {e}"))?;

    std::thread::sleep(INSTALLER_LIVENESS_WINDOW);
    if let Ok(Some(status)) = child.try_wait() {
        return Err(anyhow!(
            "установщик обновления завершился сразу после запуска ({status}) — обновление не установлено. \
             Проверьте антивирус и запустите пакет вручную: {}",
            dest.display()
        ));
    }

    Ok(UpdateInstallResult {
        launched: true,
        path: dest.display().to_string(),
        message: "Установщик обновления запущен. VoxFlow сейчас закроется.".into(),
    })
}

/// macOS: install the DMG in place instead of dumping the user on a web page.
///
/// Everything that can fail — mounting, identity checks, copying, the swap —
/// happens while VoxFlow is still alive, so a failure is reported in the UI and
/// the installed app is left untouched. Only the throwaway cleanup/relaunch step
/// runs after we exit.
#[cfg(target_os = "macos")]
fn apply_downloaded_update(dest: &Path) -> Result<UpdateInstallResult> {
    let exe = std::env::current_exe().map_err(|e| anyhow!("current executable: {e}"))?;
    let current = enclosing_macos_app_bundle(&exe)
        .ok_or_else(|| anyhow!("автообновление доступно только для установленного VoxFlow.app"))?;
    let parent = current
        .parent()
        .ok_or_else(|| anyhow!("не удалось определить папку установки VoxFlow.app"))?
        .to_path_buf();

    let mount = hdiutil_attach(dest)?;
    let staged = parent.join(format!(".voxflow-update-{}.app", std::process::id()));
    let _ = std::fs::remove_dir_all(&staged);
    let copied = ditto(&mount.join("VoxFlow.app"), &staged)
        .and_then(|()| verify_staged_macos_bundle(&staged));
    hdiutil_detach(&mount);
    if let Err(e) = copied {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(e);
    }

    let backup = swap_macos_bundle(&current, &staged)?;
    spawn_macos_relaunch(&current, &backup);
    let _ = std::fs::remove_file(dest);

    Ok(UpdateInstallResult {
        launched: true,
        path: current.display().to_string(),
        message: "Обновление установлено. VoxFlow перезапустится.".into(),
    })
}

#[cfg(not(any(windows, target_os = "macos")))]
fn apply_downloaded_update(_dest: &Path) -> Result<UpdateInstallResult> {
    Err(anyhow!(
        "автоустановка обновлений для {} не поддерживается: откройте релиз вручную",
        current_update_target().label()
    ))
}

/// How long a freshly spawned Windows installer must stay alive before we are
/// willing to shut VoxFlow down behind it.
#[cfg(windows)]
const INSTALLER_LIVENESS_WINDOW: std::time::Duration = std::time::Duration::from_millis(1500);

#[cfg(target_os = "macos")]
fn hdiutil_attach(dmg: &Path) -> Result<PathBuf> {
    let out = Command::new("/usr/bin/hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-plist"])
        .arg(dmg)
        .output()
        .map_err(|e| anyhow!("не удалось запустить hdiutil: {e}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "не удалось смонтировать образ обновления: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let value = plist::Value::from_reader_xml(std::io::Cursor::new(&out.stdout))
        .map_err(|e| anyhow!("hdiutil вернул неожиданный ответ: {e}"))?;
    value
        .as_dictionary()
        .and_then(|dict| dict.get("system-entities"))
        .and_then(plist::Value::as_array)
        .and_then(|entities| {
            entities.iter().find_map(|entity| {
                entity
                    .as_dictionary()
                    .and_then(|dict| dict.get("mount-point"))
                    .and_then(plist::Value::as_string)
                    .filter(|point| !point.is_empty())
                    .map(PathBuf::from)
            })
        })
        .ok_or_else(|| anyhow!("образ обновления смонтирован без точки монтирования"))
}

#[cfg(target_os = "macos")]
fn hdiutil_detach(mount: &Path) {
    let _ = Command::new("/usr/bin/hdiutil")
        .arg("detach")
        .arg(mount)
        .arg("-quiet")
        .status();
}

/// `ditto` (not `fs::copy`) — it is the only stock tool that preserves the
/// symlinks, permissions and extended attributes an app bundle's signature
/// depends on.
#[cfg(target_os = "macos")]
fn ditto(src: &Path, dst: &Path) -> Result<()> {
    let out = Command::new("/usr/bin/ditto")
        .arg(src)
        .arg(dst)
        .output()
        .map_err(|e| anyhow!("не удалось запустить ditto: {e}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "не удалось скопировать VoxFlow.app из образа: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Refuse to install anything that is not a strictly newer, genuine VoxFlow
/// bundle — the same identity gate the stale-copy cleanup uses.
#[cfg(target_os = "macos")]
fn verify_staged_macos_bundle(staged: &Path) -> Result<()> {
    let identity = macos_bundle_identity(staged)
        .map_err(|e| anyhow!("образ обновления не содержит корректный VoxFlow.app: {e}"))?;
    if !is_known_voxflow_bundle(&identity) {
        return Err(anyhow!("образ обновления содержит чужое приложение"));
    }
    if version_cmp(&identity.version, env!("CARGO_PKG_VERSION"))? != std::cmp::Ordering::Greater {
        return Err(anyhow!(
            "в образе обновления версия {} не новее установленной {}",
            identity.version,
            env!("CARGO_PKG_VERSION")
        ));
    }
    Ok(())
}

/// Move the running bundle aside and put the new one in its place, rolling the
/// old bundle back if the second rename fails. Renaming (not deleting) the
/// running bundle keeps this process alive until it exits on its own.
#[cfg(target_os = "macos")]
fn swap_macos_bundle(current: &Path, staged: &Path) -> Result<PathBuf> {
    let parent = current
        .parent()
        .ok_or_else(|| anyhow!("не удалось определить папку установки VoxFlow.app"))?;
    let backup = parent.join(format!(".voxflow-old-{}.app", std::process::id()));
    let _ = std::fs::remove_dir_all(&backup);
    std::fs::rename(current, &backup).map_err(|e| {
        anyhow!(
            "нет прав заменить {}: {e}. Установите обновление вручную из образа.",
            current.display()
        )
    })?;
    if let Err(e) = std::fs::rename(staged, current) {
        let _ = std::fs::rename(&backup, current);
        let _ = std::fs::remove_dir_all(staged);
        return Err(anyhow!("не удалось установить новую версию: {e}"));
    }
    Ok(backup)
}

/// Drop the old bundle and relaunch once this process is gone. Best effort by
/// construction: the update itself is already applied when this runs.
#[cfg(target_os = "macos")]
fn spawn_macos_relaunch(app: &Path, backup: &Path) {
    let script = format!(
        "i=0; while kill -0 {pid} 2>/dev/null && [ $i -lt 300 ]; do sleep 0.2; i=$((i+1)); done; \
         rm -rf {backup}; open {app}",
        pid = std::process::id(),
        backup = sh_quote(backup),
        app = sh_quote(app),
    );
    if let Err(e) = Command::new("/bin/sh").arg("-c").arg(script).spawn() {
        log::warn!("не удалось запланировать перезапуск VoxFlow: {e}");
    }
}

#[cfg(target_os = "macos")]
fn sh_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

/// Remove older VoxFlow application bundles from normal macOS install roots.
///
/// Safety boundaries are intentionally strict:
/// - cleanup runs only when the current executable itself lives directly in
///   `/Applications` or `~/Applications`;
/// - only direct child `.app` directories are considered (no recursive scan);
/// - bundle id, name, executable and a strictly older semantic version must all
///   match;
/// - the current bundle and symlinks are never removed;
/// - Application Support / models / database are outside the scanned roots.
#[cfg(target_os = "macos")]
pub fn cleanup_old_macos_app_bundles() -> Result<usize> {
    let exe = std::env::current_exe().map_err(|e| anyhow!("current executable: {e}"))?;
    let Some(current_bundle) = enclosing_macos_app_bundle(&exe) else {
        // `cargo run`, tests and standalone tools are not installed app bundles.
        return Ok(0);
    };

    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Applications"));
    }

    let Some(current_parent) = current_bundle.parent() else {
        return Ok(0);
    };
    if !roots
        .iter()
        .any(|root| paths_refer_to_same_location(root, current_parent))
    {
        // Never clean Downloads, mounted DMGs, build output or arbitrary trees.
        return Ok(0);
    }

    cleanup_old_macos_app_bundles_in(&current_bundle, &roots, env!("CARGO_PKG_VERSION"))
}

#[cfg(not(target_os = "macos"))]
pub fn cleanup_old_macos_app_bundles() -> Result<usize> {
    Ok(0)
}

#[cfg(target_os = "macos")]
#[derive(Debug, PartialEq, Eq)]
struct MacosBundleIdentity {
    identifier: String,
    name: String,
    executable: String,
    version: String,
}

#[cfg(target_os = "macos")]
fn enclosing_macos_app_bundle(executable: &Path) -> Option<PathBuf> {
    executable
        .ancestors()
        .find(|ancestor| {
            ancestor
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
                && ancestor.join("Contents/MacOS").is_dir()
        })
        .map(Path::to_path_buf)
}

#[cfg(target_os = "macos")]
fn paths_refer_to_same_location(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(target_os = "macos")]
fn macos_plist_value(bundle: &Path, key: &str) -> Result<String> {
    let plist_path = bundle.join("Contents/Info.plist");
    let plist = plist::Value::from_file(&plist_path)
        .map_err(|e| anyhow!("cannot inspect {}: {e}", plist_path.display()))?;
    let dictionary = plist
        .as_dictionary()
        .ok_or_else(|| anyhow!("{} is not a plist dictionary", plist_path.display()))?;
    dictionary
        .get(key)
        .and_then(plist::Value::as_string)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("cannot read string {key} from {}", plist_path.display()))
}

#[cfg(target_os = "macos")]
fn macos_bundle_identity(bundle: &Path) -> Result<MacosBundleIdentity> {
    let metadata = std::fs::symlink_metadata(bundle)
        .map_err(|e| anyhow!("cannot stat {}: {e}", bundle.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(anyhow!("bundle is not a real directory"));
    }
    let executable = bundle.join("Contents/MacOS/voxflow");
    let executable_meta = std::fs::symlink_metadata(&executable)
        .map_err(|e| anyhow!("missing VoxFlow executable: {e}"))?;
    if !executable_meta.file_type().is_file() {
        return Err(anyhow!("VoxFlow executable is not a regular file"));
    }
    Ok(MacosBundleIdentity {
        identifier: macos_plist_value(bundle, "CFBundleIdentifier")?,
        name: macos_plist_value(bundle, "CFBundleName")?,
        executable: macos_plist_value(bundle, "CFBundleExecutable")?,
        version: macos_plist_value(bundle, "CFBundleShortVersionString")?,
    })
}

#[cfg(target_os = "macos")]
fn is_known_voxflow_bundle(identity: &MacosBundleIdentity) -> bool {
    identity.name == "VoxFlow"
        && identity.executable == "voxflow"
        && MACOS_BUNDLE_IDS
            .iter()
            .any(|known| identity.identifier == *known)
}

#[cfg(target_os = "macos")]
fn cleanup_old_macos_app_bundles_in(
    current_bundle: &Path,
    roots: &[PathBuf],
    current_version: &str,
) -> Result<usize> {
    let current_identity = macos_bundle_identity(current_bundle)?;
    if !is_known_voxflow_bundle(&current_identity)
        || version_cmp(&current_identity.version, current_version)? != std::cmp::Ordering::Equal
    {
        return Err(anyhow!(
            "current app bundle identity/version does not match this VoxFlow build"
        ));
    }
    let current_canonical = current_bundle
        .canonicalize()
        .map_err(|e| anyhow!("cannot resolve current app bundle: {e}"))?;
    let mut removed = 0usize;

    for root in roots {
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                log::warn!("cannot scan app directory {}: {e}", root.display());
                continue;
            }
        };
        for entry in entries.flatten() {
            let candidate = entry.path();
            if !candidate
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
            {
                continue;
            }
            let Ok(metadata) = std::fs::symlink_metadata(&candidate) else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            let Ok(candidate_canonical) = candidate.canonicalize() else {
                continue;
            };
            if candidate_canonical == current_canonical {
                continue;
            }
            let Ok(identity) = macos_bundle_identity(&candidate) else {
                continue;
            };
            if !is_known_voxflow_bundle(&identity)
                || !matches!(
                    version_cmp(&identity.version, current_version),
                    Ok(std::cmp::Ordering::Less)
                )
            {
                continue;
            }

            match std::fs::remove_dir_all(&candidate) {
                Ok(()) => {
                    removed += 1;
                    log::info!(
                        "removed stale VoxFlow app bundle version {} from {}",
                        identity.version,
                        candidate.display()
                    );
                }
                Err(e) => log::warn!(
                    "cannot remove stale VoxFlow app bundle {}: {e}",
                    candidate.display()
                ),
            }
        }
    }
    Ok(removed)
}

fn update_info_from_release_for(
    release: &serde_json::Value,
    target: UpdateTarget,
) -> Result<UpdateInfo> {
    let tag = release
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("GitHub release без tag_name"))?;
    let latest_version = normalize_version_tag(tag);
    if latest_version.is_empty() {
        return Err(anyhow!("GitHub release tag пустой"));
    }

    let pattern = target
        .asset_pattern()
        .map(|(prefix, suffix)| format!("{prefix}*{suffix}"))
        .unwrap_or_else(|| "нет поддерживаемого пакета".to_string());
    let asset =
        find_installer_asset_for(release, target, tag, &latest_version).ok_or_else(|| {
            anyhow!(
                "в latest release {tag} нет пакета {pattern} для {}",
                target.label()
            )
        })?;
    let asset_name = asset
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let asset_url = asset
        .get("browser_download_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    validate_asset_url(&asset_url)?;
    validate_asset_name_for(&asset_name, target)?;
    let asset_size = asset.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
    if asset_size == 0 {
        return Err(anyhow!("GitHub release содержит пустой пакет обновления"));
    }
    let asset_digest = asset
        .get("digest")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    validate_asset_digest(&asset_digest)?;

    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let available = version_cmp(&latest_version, &current_version)
        .map(|ord| ord == std::cmp::Ordering::Greater)
        .unwrap_or(false);

    Ok(UpdateInfo {
        available,
        auto_install: !matches!(target, UpdateTarget::Unsupported),
        current_version,
        latest_version,
        release_name: release
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(tag)
            .to_string(),
        release_url: release
            .get("html_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        asset_name,
        asset_url,
        asset_size,
        asset_digest,
        published_at: release
            .get("published_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        notes: clamp_notes(release.get("body").and_then(|v| v.as_str()).unwrap_or("")),
    })
}

fn find_installer_asset_for<'a>(
    release: &'a serde_json::Value,
    target: UpdateTarget,
    tag: &str,
    version: &str,
) -> Option<&'a serde_json::Value> {
    let (prefix, suffix) = target.asset_pattern()?;
    release
        .get("assets")
        .and_then(|v| v.as_array())
        .and_then(|assets| {
            assets.iter().find(|asset| {
                let name = asset.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let url = asset
                    .get("browser_download_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                is_installer_asset_name_for(name, prefix, suffix)
                    && asset_name_matches_version(name, target, version)
                    && url == format!("{RELEASE_DOWNLOAD_PREFIX}{tag}/{name}")
            })
        })
}

fn asset_name_matches_version(name: &str, target: UpdateTarget, version: &str) -> bool {
    match target {
        UpdateTarget::WindowsX64 => name == format!("VoxFlow-Setup-{version}.exe"),
        UpdateTarget::MacosArm64 => [
            format!("VoxFlow-macOS-{version}-arm64.dmg"),
            format!("VoxFlow-macOS-{version}-arm64-adhoc.dmg"),
            format!("VoxFlow-macOS-{version}-arm64-unsigned.dmg"),
            format!("VoxFlow-macOS-{version}-arm64-signed-unnotarized.dmg"),
            format!("VoxFlow-macOS-{version}-arm64-developer-id-notarized.dmg"),
        ]
        .iter()
        .any(|expected| expected == name),
        UpdateTarget::MacosX64 => [
            format!("VoxFlow-macOS-{version}-x64.dmg"),
            format!("VoxFlow-macOS-{version}-x64-adhoc.dmg"),
            format!("VoxFlow-macOS-{version}-x64-unsigned.dmg"),
            format!("VoxFlow-macOS-{version}-x64-signed-unnotarized.dmg"),
            format!("VoxFlow-macOS-{version}-x64-developer-id-notarized.dmg"),
        ]
        .iter()
        .any(|expected| expected == name),
        UpdateTarget::Unsupported => false,
    }
}

fn validate_asset_url(url: &str) -> Result<()> {
    if !url.starts_with(RELEASE_DOWNLOAD_PREFIX) {
        return Err(anyhow!(
            "недоверенный URL обновления: ожидается GitHub Releases {OWNER}/{REPO}"
        ));
    }
    if url.contains('\n') || url.contains('\r') || url.contains('"') {
        return Err(anyhow!("URL обновления содержит недопустимые символы"));
    }
    Ok(())
}

fn validate_asset_name_for(name: &str, target: UpdateTarget) -> Result<()> {
    let (prefix, suffix) = target
        .asset_pattern()
        .ok_or_else(|| anyhow!("обновления для {} не поддерживаются", target.label()))?;
    if is_installer_asset_name_for(name, prefix, suffix)
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains(':')
    {
        Ok(())
    } else {
        Err(anyhow!(
            "недоверенное имя пакета обновления: ожидалось {prefix}*{suffix}"
        ))
    }
}

fn validate_asset_digest(digest: &str) -> Result<()> {
    if digest.is_empty() {
        return Ok(());
    }
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(anyhow!(
            "GitHub release содержит неподдерживаемый digest пакета"
        ));
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "GitHub release содержит некорректный SHA-256 пакета"
        ));
    }
    Ok(())
}

fn verify_downloaded_asset(path: &Path, expected_size: u64, expected_digest: &str) -> Result<()> {
    let actual_size = std::fs::metadata(path)
        .map_err(|e| anyhow!("скачанный установщик не найден: {e}"))?
        .len();
    if expected_size == 0 || actual_size != expected_size {
        return Err(anyhow!(
            "размер скачанного установщика не совпадает: ожидалось {expected_size}, получено {actual_size}"
        ));
    }
    validate_asset_digest(expected_digest)?;
    let Some(expected_hex) = expected_digest.strip_prefix("sha256:") else {
        return Ok(());
    };

    let mut file = std::fs::File::open(path)
        .map_err(|e| anyhow!("не удалось открыть скачанный установщик: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| anyhow!("не удалось проверить SHA-256 установщика: {e}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual_hex = format!("{:x}", hasher.finalize());
    if !actual_hex.eq_ignore_ascii_case(expected_hex) {
        return Err(anyhow!(
            "SHA-256 скачанного установщика не совпадает с GitHub Release"
        ));
    }
    Ok(())
}

fn is_installer_asset_name_for(name: &str, prefix: &str, suffix: &str) -> bool {
    if !name.starts_with(prefix) {
        return false;
    }
    name.ends_with(suffix)
        || (suffix == "-arm64.dmg"
            && (name.ends_with("-arm64-adhoc.dmg")
                || name.ends_with("-arm64-unsigned.dmg")
                || name.ends_with("-arm64-signed-unnotarized.dmg")
                || name.ends_with("-arm64-developer-id-notarized.dmg")))
        || (suffix == "-x64.dmg"
            && (name.ends_with("-x64-adhoc.dmg")
                || name.ends_with("-x64-unsigned.dmg")
                || name.ends_with("-x64-signed-unnotarized.dmg")
                || name.ends_with("-x64-developer-id-notarized.dmg")))
}

fn installer_download_path(asset_name: &str) -> PathBuf {
    let safe_name: String = asset_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let (stem, ext) = match safe_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => (safe_name.as_str(), "bin"),
    };
    crate::paths::unique_tmp_path(stem, ext)
}

fn normalize_version_tag(tag: &str) -> String {
    tag.trim()
        .trim_start_matches('v')
        .trim_start_matches('V')
        .to_string()
}

fn parse_version_tuple(version: &str) -> Result<(u64, u64, u64)> {
    let core = version.trim().split(['-', '+']).next().unwrap_or("").trim();
    let mut parts = core.split('.');
    let major = parse_version_part(parts.next())?;
    let minor = parse_version_part(parts.next())?;
    let patch = parse_version_part(parts.next())?;
    Ok((major, minor, patch))
}

fn parse_version_part(part: Option<&str>) -> Result<u64> {
    part.filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("неполная версия"))?
        .parse::<u64>()
        .map_err(|_| anyhow!("некорректная версия"))
}

fn version_cmp(a: &str, b: &str) -> Result<std::cmp::Ordering> {
    Ok(parse_version_tuple(a)?.cmp(&parse_version_tuple(b)?))
}

fn clamp_notes(notes: &str) -> String {
    const MAX_CHARS: usize = 1800;
    let mut out = String::new();
    for ch in notes.chars().take(MAX_CHARS) {
        out.push(ch);
    }
    out
}

#[cfg(windows)]
#[allow(dead_code)]
fn _is_exe_path(path: &Path) -> bool {
    path.extension()
        .and_then(|v| v.to_str())
        .map(|v| v.eq_ignore_ascii_case("exe"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_dotted_versions_numerically() {
        assert_eq!(
            version_cmp("1.0.10", "1.0.2").unwrap(),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            version_cmp("1.2.0", "1.2.0").unwrap(),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            version_cmp("1.2.0-beta", "1.1.9").unwrap(),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn validates_only_repo_release_installers() {
        assert!(
            validate_asset_name_for("VoxFlow-Setup-1.0.3.exe", UpdateTarget::WindowsX64).is_ok()
        );
        assert!(validate_asset_name_for("..\\evil.exe", UpdateTarget::WindowsX64).is_err());
        assert!(
            validate_asset_name_for("VoxFlow-macOS-1.0.8-arm64.dmg", UpdateTarget::MacosArm64)
                .is_ok()
        );
        assert!(validate_asset_name_for(
            "VoxFlow-macOS-2.0.1-arm64-adhoc.dmg",
            UpdateTarget::MacosArm64
        )
        .is_ok());
        assert!(validate_asset_name_for(
            "VoxFlow-macOS-2.0.5-arm64-developer-id-notarized.dmg",
            UpdateTarget::MacosArm64
        )
        .is_ok());
        assert!(asset_name_matches_version(
            "VoxFlow-macOS-2.0.5-arm64-developer-id-notarized.dmg",
            UpdateTarget::MacosArm64,
            "2.0.5"
        ));
        assert!(validate_asset_name_for(
            "VoxFlow-macOS-2.0.1-x64-adhoc.dmg",
            UpdateTarget::MacosArm64
        )
        .is_err());
        assert!(
            validate_asset_name_for("VoxFlow-Setup-1.0.8.exe", UpdateTarget::MacosArm64).is_err()
        );
        assert!(
            validate_asset_url(
                "https://github.com/Nezeronxer/voxflow/releases/download/v1.0.3/VoxFlow-Setup-1.0.3.exe"
            )
            .is_ok()
        );
        assert!(validate_asset_url(
            "https://github.com/other/voxflow/releases/download/v1.0.3/VoxFlow-Setup-1.0.3.exe"
        )
        .is_err());
        assert!(validate_asset_digest("").is_ok());
        assert!(validate_asset_digest(&format!("sha256:{}", "a".repeat(64))).is_ok());
        assert!(validate_asset_digest("sha512:abcd").is_err());
        assert!(validate_asset_digest("sha256:abcd").is_err());
    }

    #[test]
    fn selects_platform_specific_asset_from_release_json() {
        let release = serde_json::json!({
            "tag_name": "v99.0.0",
            "name": "VoxFlow v99.0.0",
            "html_url": "https://github.com/Nezeronxer/voxflow/releases/tag/v99.0.0",
            "published_at": "2026-07-02T00:00:00Z",
            "body": "notes",
            "assets": [
                {
                    "name": "VoxFlow-Setup-99.0.0.exe",
                    "size": 123,
                    "browser_download_url": "https://github.com/Nezeronxer/voxflow/releases/download/v99.0.0/VoxFlow-Setup-99.0.0.exe"
                },
                {
                    "name": "VoxFlow-macOS-99.0.0-arm64-adhoc.dmg",
                    "size": 456,
                    "browser_download_url": "https://github.com/Nezeronxer/voxflow/releases/download/v99.0.0/VoxFlow-macOS-99.0.0-arm64-adhoc.dmg"
                },
                {
                    "name": "VoxFlow-macOS-99.0.0-x64-adhoc.dmg",
                    "size": 789,
                    "browser_download_url": "https://github.com/Nezeronxer/voxflow/releases/download/v99.0.0/VoxFlow-macOS-99.0.0-x64-adhoc.dmg"
                }
            ]
        });
        let windows = update_info_from_release_for(&release, UpdateTarget::WindowsX64).unwrap();
        let mac_arm = update_info_from_release_for(&release, UpdateTarget::MacosArm64).unwrap();
        let mac_x64 = update_info_from_release_for(&release, UpdateTarget::MacosX64).unwrap();

        assert!(windows.available);
        assert!(windows.auto_install);
        assert_eq!(windows.latest_version, "99.0.0");
        assert_eq!(windows.asset_name, "VoxFlow-Setup-99.0.0.exe");
        assert_eq!(mac_arm.asset_name, "VoxFlow-macOS-99.0.0-arm64-adhoc.dmg");
        assert!(mac_arm.auto_install);
        assert_eq!(mac_x64.asset_name, "VoxFlow-macOS-99.0.0-x64-adhoc.dmg");
    }

    #[test]
    fn rejects_installer_whose_filename_does_not_match_release_tag() {
        let release = serde_json::json!({
            "tag_name": "v2.0.0",
            "assets": [{
                "name": "VoxFlow-Setup-1.0.8.exe",
                "size": 123,
                "browser_download_url": "https://github.com/Nezeronxer/voxflow/releases/download/v2.0.0/VoxFlow-Setup-1.0.8.exe"
            }]
        });

        assert!(update_info_from_release_for(&release, UpdateTarget::WindowsX64).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_never_selects_or_launches_windows_installer() {
        assert!(matches!(
            current_update_target(),
            UpdateTarget::MacosArm64 | UpdateTarget::MacosX64
        ));
        assert!(
            validate_asset_name_for("VoxFlow-Setup-99.0.0.exe", current_update_target()).is_err()
        );

        // A Windows package must be rejected before anything is downloaded or
        // executed, whatever the release JSON claims.
        let err = download_and_launch(
            "https://github.com/Nezeronxer/voxflow/releases/download/v99.0.0/VoxFlow-Setup-99.0.0.exe",
            "VoxFlow-Setup-99.0.0.exe",
            123,
            "",
            "",
        )
        .unwrap_err();
        assert!(err.to_string().contains("недоверенное имя пакета"));
    }

    #[test]
    fn keeps_the_package_extension_when_naming_the_download() {
        assert_eq!(
            installer_download_path("VoxFlow-Setup-2.0.11.exe")
                .extension()
                .and_then(|ext| ext.to_str()),
            Some("exe")
        );
        assert_eq!(
            installer_download_path("VoxFlow-macOS-2.0.11-arm64-adhoc.dmg")
                .extension()
                .and_then(|ext| ext.to_str()),
            Some("dmg")
        );
    }

    #[test]
    fn every_supported_platform_installs_automatically() {
        for target in [
            UpdateTarget::WindowsX64,
            UpdateTarget::MacosArm64,
            UpdateTarget::MacosX64,
        ] {
            let release = serde_json::json!({
                "tag_name": "v99.0.0",
                "assets": [
                    {
                        "name": "VoxFlow-Setup-99.0.0.exe",
                        "size": 1,
                        "browser_download_url": "https://github.com/Nezeronxer/voxflow/releases/download/v99.0.0/VoxFlow-Setup-99.0.0.exe"
                    },
                    {
                        "name": "VoxFlow-macOS-99.0.0-arm64-adhoc.dmg",
                        "size": 2,
                        "browser_download_url": "https://github.com/Nezeronxer/voxflow/releases/download/v99.0.0/VoxFlow-macOS-99.0.0-arm64-adhoc.dmg"
                    },
                    {
                        "name": "VoxFlow-macOS-99.0.0-x64-adhoc.dmg",
                        "size": 3,
                        "browser_download_url": "https://github.com/Nezeronxer/voxflow/releases/download/v99.0.0/VoxFlow-macOS-99.0.0-x64-adhoc.dmg"
                    }
                ]
            });
            assert!(
                update_info_from_release_for(&release, target)
                    .unwrap()
                    .auto_install,
                "{target:?} must install without sending the user to a web page"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_staged_bundle_must_be_a_strictly_newer_voxflow() {
        let root = macos_cleanup_test_root("staged");
        std::fs::create_dir_all(&root).unwrap();

        let foreign =
            write_test_macos_bundle(&root, "Foreign.app", "example.foreign", "VoxFlow", "99.0.0");
        assert!(verify_staged_macos_bundle(&foreign)
            .unwrap_err()
            .to_string()
            .contains("чужое приложение"));

        let older = write_test_macos_bundle(
            &root,
            "Older.app",
            "com.nezeronxer.voxflow.macos",
            "VoxFlow",
            "0.0.1",
        );
        assert!(verify_staged_macos_bundle(&older)
            .unwrap_err()
            .to_string()
            .contains("не новее"));

        let newer = write_test_macos_bundle(
            &root,
            "Newer.app",
            "com.nezeronxer.voxflow.macos",
            "VoxFlow",
            "999.0.0",
        );
        assert!(verify_staged_macos_bundle(&newer).is_ok());

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_swap_replaces_the_bundle_and_keeps_the_old_one_as_backup() {
        let root = macos_cleanup_test_root("swap-ok");
        std::fs::create_dir_all(&root).unwrap();
        let current = write_test_macos_bundle(
            &root,
            "VoxFlow.app",
            "com.nezeronxer.voxflow.macos",
            "VoxFlow",
            "1.0.0",
        );
        let staged = write_test_macos_bundle(
            &root,
            ".staged.app",
            "com.nezeronxer.voxflow.macos",
            "VoxFlow",
            "2.0.0",
        );

        let backup = swap_macos_bundle(&current, &staged).unwrap();

        assert_eq!(
            macos_bundle_identity(&current).unwrap().version,
            "2.0.0",
            "the installed path must now hold the new version"
        );
        assert_eq!(
            macos_bundle_identity(&backup).unwrap().version,
            "1.0.0",
            "the running bundle is moved aside, never deleted under our feet"
        );
        assert!(!staged.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_swap_restores_the_old_bundle_when_the_new_one_cannot_be_moved_in() {
        let root = macos_cleanup_test_root("swap-fail");
        std::fs::create_dir_all(&root).unwrap();
        let current = write_test_macos_bundle(
            &root,
            "VoxFlow.app",
            "com.nezeronxer.voxflow.macos",
            "VoxFlow",
            "1.0.0",
        );
        let missing = root.join(".never-staged.app");

        let err = swap_macos_bundle(&current, &missing).unwrap_err();

        assert!(err
            .to_string()
            .contains("не удалось установить новую версию"));
        assert_eq!(
            macos_bundle_identity(&current).unwrap().version,
            "1.0.0",
            "a failed swap must leave the working install exactly where it was"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shell_quoting_survives_paths_with_spaces_and_quotes() {
        assert_eq!(
            sh_quote(Path::new("/Applications/Vox Flow.app")),
            "'/Applications/Vox Flow.app'"
        );
        assert_eq!(sh_quote(Path::new("/tmp/it's.app")), r"'/tmp/it'\''s.app'");
    }

    /// Per-test directory. The label keeps concurrently running tests apart:
    /// a wall-clock suffix alone can collide, and the loser then has its
    /// bundles deleted by the winner's teardown.
    #[cfg(target_os = "macos")]
    fn macos_cleanup_test_root(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "voxflow-app-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[cfg(target_os = "macos")]
    fn write_test_macos_bundle(
        root: &Path,
        file_name: &str,
        identifier: &str,
        name: &str,
        version: &str,
    ) -> PathBuf {
        let bundle = root.join(file_name);
        let macos = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("voxflow"), b"test binary").unwrap();
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{identifier}</string>
<key>CFBundleName</key><string>{name}</string>
<key>CFBundleExecutable</key><string>voxflow</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
</dict></plist>"#
        );
        std::fs::write(bundle.join("Contents/Info.plist"), plist).unwrap();
        bundle
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cleanup_removes_only_strictly_older_verified_bundles() {
        use std::os::unix::fs::symlink;

        let root = macos_cleanup_test_root("cleanup");
        std::fs::create_dir_all(&root).unwrap();
        let current = write_test_macos_bundle(
            &root,
            "VoxFlow.app",
            "com.nezeronxer.voxflow.macos",
            "VoxFlow",
            "2.0.1",
        );
        let old_current_id = write_test_macos_bundle(
            &root,
            "VoxFlow 1.0.8.app",
            "com.nezeronxer.voxflow.macos",
            "VoxFlow",
            "1.0.8",
        );
        let old_legacy_id = write_test_macos_bundle(
            &root,
            "VoxFlow 1.0.7.app",
            "com.voxflow.app",
            "VoxFlow",
            "1.0.7",
        );
        let same_version = write_test_macos_bundle(
            &root,
            "VoxFlow Copy.app",
            "com.nezeronxer.voxflow.macos",
            "VoxFlow",
            "2.0.1",
        );
        let newer = write_test_macos_bundle(
            &root,
            "VoxFlow Beta.app",
            "com.nezeronxer.voxflow.macos",
            "VoxFlow",
            "3.0.0",
        );
        let foreign =
            write_test_macos_bundle(&root, "Foreign.app", "example.foreign", "VoxFlow", "1.0.0");
        let wrong_name = write_test_macos_bundle(
            &root,
            "Lookalike.app",
            "com.nezeronxer.voxflow.macos",
            "Not VoxFlow",
            "1.0.0",
        );
        let link = root.join("VoxFlow Linked Old.app");
        symlink(&old_current_id, &link).unwrap();

        let removed =
            cleanup_old_macos_app_bundles_in(&current, std::slice::from_ref(&root), "2.0.1")
                .unwrap();

        assert_eq!(removed, 2);
        assert!(
            current.exists(),
            "the running/current bundle is never removed"
        );
        assert!(!old_current_id.exists());
        assert!(!old_legacy_id.exists());
        assert!(same_version.exists(), "same-version copies are not stale");
        assert!(
            newer.exists(),
            "a newer bundle must never be downgraded away"
        );
        assert!(foreign.exists());
        assert!(wrong_name.exists());
        assert!(
            link.symlink_metadata().is_ok(),
            "symlinks are never followed"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finds_enclosing_app_bundle_but_not_standalone_binary() {
        let root = macos_cleanup_test_root("enclosing");
        std::fs::create_dir_all(&root).unwrap();
        let app = write_test_macos_bundle(
            &root,
            "VoxFlow.app",
            "com.nezeronxer.voxflow.macos",
            "VoxFlow",
            "2.0.2",
        );
        let app_exe = app.join("Contents/MacOS/voxflow");
        assert_eq!(enclosing_macos_app_bundle(&app_exe), Some(app.clone()));
        assert_eq!(enclosing_macos_app_bundle(&root.join("voxflow")), None);
        let _ = std::fs::remove_dir_all(root);
    }
}
