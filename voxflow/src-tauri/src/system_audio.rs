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

#[cfg(not(windows))]
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
