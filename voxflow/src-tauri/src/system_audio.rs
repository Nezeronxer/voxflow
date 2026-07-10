//! Управление системным output-mute на время диктовки.
//!
//! Меняем именно mute-флаг default render endpoint, а не громкость: так можно
//! вернуть предыдущее состояние без риска испортить пользовательский volume.

#[cfg(windows)]
mod imp {
    use anyhow::{Context, Result};
    use windows::core::GUID;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    struct ComApartment {
        initialized: bool,
    }

    impl ComApartment {
        fn init() -> Self {
            // Если COM уже инициализирован в другом режиме, CoreAudio обычно всё равно
            // доступен на текущем потоке. В этом случае не зовём CoUninitialize.
            let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
            Self { initialized }
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            if self.initialized {
                unsafe { CoUninitialize() };
            }
        }
    }

    fn endpoint_volume() -> Result<IAudioEndpointVolume> {
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .context("CoreAudio: device enumerator")?;
        let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
            .context("CoreAudio: default render endpoint")?;
        let volume: IAudioEndpointVolume =
            unsafe { device.Activate(CLSCTX_ALL, None) }.context("CoreAudio: endpoint volume")?;
        Ok(volume)
    }

    fn current_mute() -> Result<bool> {
        let _com = ComApartment::init();
        let endpoint = endpoint_volume()?;
        let muted = unsafe { endpoint.GetMute() }.context("CoreAudio: GetMute")?;
        Ok(muted.as_bool())
    }

    fn set_mute(mute: bool) -> Result<()> {
        let _com = ComApartment::init();
        let endpoint = endpoint_volume()?;
        unsafe { endpoint.SetMute(mute, &GUID::zeroed()) }.context("CoreAudio: SetMute")?;
        Ok(())
    }

    #[derive(Debug)]
    pub struct AutoMuteGuard {
        was_muted: bool,
        active: bool,
    }

    impl AutoMuteGuard {
        pub fn engage() -> Result<Self> {
            let was_muted = current_mute()?;
            if !was_muted {
                set_mute(true)?;
            }
            Ok(Self {
                was_muted,
                active: true,
            })
        }

        pub fn restore(&mut self) {
            if !self.active {
                return;
            }
            if let Err(e) = set_mute(self.was_muted) {
                log::warn!("auto-mute restore failed: {e:#}");
            }
            self.active = false;
        }
    }

    impl Drop for AutoMuteGuard {
        fn drop(&mut self) {
            self.restore();
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use anyhow::{anyhow, Context, Result};
    use std::process::Command;

    fn run_osascript(script: &str) -> Result<String> {
        let out = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
            .context("macOS audio mute: osascript")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!("macOS audio mute: {}", stderr.trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn parse_apple_bool(value: &str) -> Result<bool> {
        match value.trim().to_ascii_lowercase().as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            other => Err(anyhow!("macOS audio mute: unexpected bool {other:?}")),
        }
    }

    fn current_mute() -> Result<bool> {
        let out = run_osascript("output muted of (get volume settings)")?;
        parse_apple_bool(&out)
    }

    fn set_mute(mute: bool) -> Result<()> {
        let value = if mute { "true" } else { "false" };
        run_osascript(&format!("set volume output muted {value}"))?;
        Ok(())
    }

    #[derive(Debug)]
    pub struct AutoMuteGuard {
        was_muted: bool,
        active: bool,
    }

    impl AutoMuteGuard {
        pub fn engage() -> Result<Self> {
            let was_muted = current_mute().context("macOS audio mute: read current state")?;
            if !was_muted {
                set_mute(true).context("macOS audio mute: set muted")?;
            }
            Ok(Self {
                was_muted,
                active: true,
            })
        }

        pub fn restore(&mut self) {
            if !self.active {
                return;
            }
            if let Err(e) = set_mute(self.was_muted) {
                log::warn!("auto-mute restore failed: {e:#}");
            }
            self.active = false;
        }
    }

    impl Drop for AutoMuteGuard {
        fn drop(&mut self) {
            self.restore();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::parse_apple_bool;

        #[test]
        fn parses_osascript_booleans() {
            assert!(parse_apple_bool("true\n").unwrap());
            assert!(!parse_apple_bool(" false ").unwrap());
            assert!(parse_apple_bool("missing value").is_err());
        }
    }
}

#[cfg(all(not(windows), not(target_os = "macos")))]
mod imp {
    use anyhow::Result;

    #[derive(Debug)]
    pub struct AutoMuteGuard;

    impl AutoMuteGuard {
        pub fn engage() -> Result<Self> {
            Ok(Self)
        }

        pub fn restore(&mut self) {}
    }
}

pub use imp::AutoMuteGuard;
