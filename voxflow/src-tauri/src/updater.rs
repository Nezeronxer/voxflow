//! GitHub Releases updater for the custom Inno Setup installer.
//!
//! The project ships `VoxFlow-Setup-<version>.exe` from GitHub Releases. We keep
//! the updater intentionally small: query the latest release, pick that installer
//! asset, download it to VoxFlow's temp directory, then launch it visibly.

use anyhow::{anyhow, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

const OWNER: &str = "Nezeronxer";
const REPO: &str = "voxflow";
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/Nezeronxer/voxflow/releases/latest";
const RELEASE_DOWNLOAD_PREFIX: &str = "https://github.com/Nezeronxer/voxflow/releases/download/";
const INSTALLER_PREFIX: &str = "VoxFlow-Setup-";
const INSTALLER_SUFFIX: &str = ".exe";
const USER_AGENT: &str = "VoxFlow-Updater";

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub available: bool,
    pub current_version: String,
    pub latest_version: String,
    pub release_name: String,
    pub release_url: String,
    pub asset_name: String,
    pub asset_url: String,
    pub asset_size: u64,
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
    cmd.arg(LATEST_RELEASE_URL);

    let out = crate::net::curl_secret_with_proxy(cmd, &[], proxy_url)
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
    update_info_from_release(&release)
}

pub fn download_and_launch(
    asset_url: &str,
    asset_name: &str,
    proxy_url: &str,
) -> Result<UpdateInstallResult> {
    validate_asset_url(asset_url)?;
    validate_asset_name(asset_name)?;

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
    cmd.arg(asset_url);

    let out = crate::net::curl_secret_with_proxy(cmd, &[], proxy_url)
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
    if !dest.exists() || std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) == 0 {
        return Err(anyhow!("скачанный установщик пустой или не найден"));
    }

    Command::new(&dest)
        .spawn()
        .map_err(|e| anyhow!("не удалось запустить установщик обновления: {e}"))?;

    Ok(UpdateInstallResult {
        launched: true,
        path: dest.display().to_string(),
        message: "Установщик обновления запущен".into(),
    })
}

fn update_info_from_release(release: &serde_json::Value) -> Result<UpdateInfo> {
    let tag = release
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("GitHub release без tag_name"))?;
    let latest_version = normalize_version_tag(tag);
    if latest_version.is_empty() {
        return Err(anyhow!("GitHub release tag пустой"));
    }

    let asset = find_installer_asset(release).ok_or_else(|| {
        anyhow!("в latest release {tag} нет установщика {INSTALLER_PREFIX}*{INSTALLER_SUFFIX}")
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
    validate_asset_name(&asset_name)?;

    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let available = version_cmp(&latest_version, &current_version)
        .map(|ord| ord == std::cmp::Ordering::Greater)
        .unwrap_or(false);

    Ok(UpdateInfo {
        available,
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
        asset_size: asset.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
        published_at: release
            .get("published_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        notes: clamp_notes(release.get("body").and_then(|v| v.as_str()).unwrap_or("")),
    })
}

fn find_installer_asset(release: &serde_json::Value) -> Option<&serde_json::Value> {
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
                is_installer_asset_name(name) && url.starts_with(RELEASE_DOWNLOAD_PREFIX)
            })
        })
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

fn validate_asset_name(name: &str) -> Result<()> {
    if is_installer_asset_name(name)
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains(':')
    {
        Ok(())
    } else {
        Err(anyhow!(
            "недоверенное имя установщика обновления: ожидалось {INSTALLER_PREFIX}*{INSTALLER_SUFFIX}"
        ))
    }
}

fn is_installer_asset_name(name: &str) -> bool {
    name.starts_with(INSTALLER_PREFIX) && name.ends_with(INSTALLER_SUFFIX)
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
    let stem = safe_name.trim_end_matches(INSTALLER_SUFFIX);
    crate::paths::unique_tmp_path(stem, "exe")
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
        assert!(validate_asset_name("VoxFlow-Setup-1.0.3.exe").is_ok());
        assert!(validate_asset_name("..\\evil.exe").is_err());
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
    }

    #[test]
    fn extracts_update_info_from_release_json() {
        let release = serde_json::json!({
            "tag_name": "v99.0.0",
            "name": "VoxFlow v99.0.0",
            "html_url": "https://github.com/Nezeronxer/voxflow/releases/tag/v99.0.0",
            "published_at": "2026-07-02T00:00:00Z",
            "body": "notes",
            "assets": [{
                "name": "VoxFlow-Setup-99.0.0.exe",
                "size": 123,
                "browser_download_url": "https://github.com/Nezeronxer/voxflow/releases/download/v99.0.0/VoxFlow-Setup-99.0.0.exe"
            }]
        });
        let info = update_info_from_release(&release).unwrap();
        assert!(info.available);
        assert_eq!(info.latest_version, "99.0.0");
        assert_eq!(info.asset_name, "VoxFlow-Setup-99.0.0.exe");
    }
}
