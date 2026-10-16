#[cfg(target_os = "macos")]
mod imp {
    use crate::engine;
    use std::ffi::c_void;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tauri::AppHandle;

    type CFAllocatorRef = *const c_void;
    type CFBooleanRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFStringRef = *const c_void;

    static INPUT_REQUESTED: AtomicBool = AtomicBool::new(false);
    static POST_EVENT_REQUESTED: AtomicBool = AtomicBool::new(false);
    static ACCESSIBILITY_REQUESTED: AtomicBool = AtomicBool::new(false);
    static MIC_REQUESTED: AtomicBool = AtomicBool::new(false);
    static ONBOARDING_STARTED: AtomicBool = AtomicBool::new(false);
    static ONBOARDING_ACTIVE: AtomicBool = AtomicBool::new(false);

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        static kAXTrustedCheckOptionPrompt: CFStringRef;
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
        fn CGPreflightListenEventAccess() -> bool;
        fn CGRequestListenEventAccess() -> bool;
        fn CGPreflightPostEventAccess() -> bool;
        fn CGRequestPostEventAccess() -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFBooleanTrue: CFBooleanRef;
        fn CFDictionaryCreate(
            allocator: CFAllocatorRef,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFDictionaryRef;
        fn CFRelease(cf: *const c_void);
    }

    pub fn input_monitoring_allowed() -> bool {
        unsafe { CGPreflightListenEventAccess() }
    }

    pub fn post_event_allowed() -> bool {
        unsafe { CGPreflightPostEventAccess() || AXIsProcessTrusted() }
    }

    pub fn request_input_monitoring_once() {
        if input_monitoring_allowed() {
            return;
        }
        if !INPUT_REQUESTED.swap(true, Ordering::SeqCst) {
            engine::dbg_log("permissions: requesting Input Monitoring");
            let _ = unsafe { CGRequestListenEventAccess() };
        }
    }

    pub fn request_post_event_once() {
        if post_event_allowed() {
            return;
        }
        if !POST_EVENT_REQUESTED.swap(true, Ordering::SeqCst) {
            engine::dbg_log("permissions: requesting Post Event/Accessibility");
            let _ = unsafe { CGRequestPostEventAccess() };
        }
        request_accessibility_once();
    }

    pub fn request_accessibility_once() {
        if unsafe { AXIsProcessTrusted() } {
            return;
        }
        if ACCESSIBILITY_REQUESTED.swap(true, Ordering::SeqCst) {
            return;
        }
        engine::dbg_log("permissions: requesting Accessibility");
        unsafe {
            let keys = [kAXTrustedCheckOptionPrompt];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                std::ptr::null(),
                std::ptr::null(),
            );
            if !options.is_null() {
                let _ = AXIsProcessTrustedWithOptions(options);
                CFRelease(options);
            }
        }
    }

    pub fn open_input_monitoring_settings() {
        let _ = Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
            .spawn();
    }

    pub fn open_accessibility_settings() {
        let _ = Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn();
    }

    // Микрофон: AVCaptureDevice через чистый objc-рантайм, без новых крейтов.
    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {
        static AVMediaTypeAudio: *const c_void;
    }

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const std::ffi::c_char) -> *mut c_void;
        fn sel_registerName(name: *const std::ffi::c_char) -> *mut c_void;
        fn objc_msgSend();
        static _NSConcreteGlobalBlock: [*const c_void; 32];
    }

    #[repr(C)]
    struct BlockDesc {
        reserved: usize,
        size: usize,
    }

    /// Блок `void (^)(BOOL)` без захвата.
    #[repr(C)]
    struct GlobalBlock {
        isa: *const c_void,
        flags: i32,
        reserved: i32,
        invoke: extern "C" fn(*const GlobalBlock, bool),
        desc: *const BlockDesc,
    }

    // Только чтение, указатели ведут на статики.
    unsafe impl Sync for GlobalBlock {}
    unsafe impl Sync for BlockDesc {}

    extern "C" fn mic_access_done(_block: *const GlobalBlock, granted: bool) {
        engine::dbg_log(&format!("permissions: microphone answer granted={granted}"));
    }

    pub fn microphone_state() -> super::MicState {
        use super::MicState;
        let status = unsafe {
            let send: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void) -> isize =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let class = objc_getClass(c"AVCaptureDevice".as_ptr());
            let sel = sel_registerName(c"authorizationStatusForMediaType:".as_ptr());
            send(class, sel, AVMediaTypeAudio)
        };
        match status {
            3 => MicState::Granted,
            0 => MicState::NotDetermined,
            _ => MicState::Denied,
        }
    }

    static MIC_DESC: BlockDesc = BlockDesc {
        reserved: 0,
        size: std::mem::size_of::<GlobalBlock>(),
    };

    // BLOCK_IS_GLOBAL: рантайм блок не копирует, статика живёт весь процесс.
    // isa - адрес символа _NSConcreteGlobalBlock, а не его содержимое.
    static MIC_BLOCK: GlobalBlock = GlobalBlock {
        isa: std::ptr::addr_of!(_NSConcreteGlobalBlock) as *const c_void,
        flags: 1 << 28,
        reserved: 0,
        invoke: mic_access_done,
        desc: std::ptr::addr_of!(MIC_DESC),
    };

    fn request_microphone_access() {
        engine::dbg_log("permissions: requesting Microphone");
        unsafe {
            let send: unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *const c_void,
                *const GlobalBlock,
            ) = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let class = objc_getClass(c"AVCaptureDevice".as_ptr());
            let sel = sel_registerName(c"requestAccessForMediaType:completionHandler:".as_ptr());
            send(class, sel, AVMediaTypeAudio, &MIC_BLOCK);
        }
    }

    pub fn open_microphone_settings() {
        let _ = Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            .spawn();
    }

    /// Первый раз - системное окно; после отказа система его уже не покажет,
    /// поэтому ведём в настройки.
    pub fn request_microphone_or_open_settings() {
        // Системное окно - не чаще раза за запуск; дальше только настройки.
        if microphone_state() == super::MicState::NotDetermined
            && !MIC_REQUESTED.swap(true, Ordering::SeqCst)
        {
            request_microphone_access();
        } else {
            open_microphone_settings();
        }
    }

    pub fn onboarding_active() -> bool {
        ONBOARDING_ACTIVE.load(Ordering::SeqCst)
    }

    fn onboarding_needed(post_event: bool, input_monitoring: bool) -> bool {
        !post_event || !input_monitoring
    }

    pub fn onboard_on_launch(_app: AppHandle) {
        if ONBOARDING_STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        // Не создаём искусственное окно без хоткея на каждом обычном запуске.
        // Если оба разрешения уже выданы, listener может стартовать немедленно;
        // 700-мс пауза нужна только реальному first-run onboarding.
        if !onboarding_needed(post_event_allowed(), input_monitoring_allowed()) {
            return;
        }
        ONBOARDING_ACTIVE.store(true, Ordering::SeqCst);
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(700));
            let need_accessibility = !post_event_allowed();
            let need_input = !input_monitoring_allowed();
            if !need_accessibility && !need_input {
                ONBOARDING_ACTIVE.store(false, Ordering::SeqCst);
                return;
            }

            engine::dbg_log(&format!(
                "permissions: onboarding needed accessibility={} input_monitoring={}",
                need_accessibility, need_input
            ));

            // Right Option — первое действие онбординга, поэтому не держим
            // Input Monitoring за 90-секундным ожиданием Accessibility.
            if need_input {
                request_input_monitoring_once();
                for _ in 0..90 {
                    if input_monitoring_allowed() {
                        engine::dbg_log("permissions: Input Monitoring granted during onboarding");
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }

            if need_accessibility && !post_event_allowed() {
                request_post_event_once();
                for _ in 0..90 {
                    if post_event_allowed() {
                        engine::dbg_log("permissions: Accessibility granted during onboarding");
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            ONBOARDING_ACTIVE.store(false, Ordering::SeqCst);
        });
    }

    #[cfg(test)]
    mod tests {
        use super::onboarding_needed;

        #[test]
        fn granted_permissions_do_not_create_a_startup_hotkey_gap() {
            assert!(!onboarding_needed(true, true));
            assert!(onboarding_needed(false, true));
            assert!(onboarding_needed(true, false));
        }
    }
}

use serde::Serialize;

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MicState {
    Granted,
    NotDetermined,
    Denied,
}

/// Чего не хватает, в порядке строк карточки «Доступы».
pub fn missing_permissions(
    mic: MicState,
    accessibility: bool,
    input_monitoring: bool,
) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if mic != MicState::Granted {
        missing.push("microphone");
    }
    if !accessibility {
        missing.push("accessibility");
    }
    if !input_monitoring {
        missing.push("input_monitoring");
    }
    missing
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsReport {
    pub microphone: MicState,
    pub accessibility: bool,
    pub input_monitoring: bool,
    pub missing: Vec<&'static str>,
}

/// На Windows/Linux системных запросов нет: «всё выдано».
pub fn report() -> PermissionsReport {
    #[cfg(target_os = "macos")]
    let (mic, acc, input) = (
        microphone_state(),
        post_event_allowed(),
        input_monitoring_allowed(),
    );
    #[cfg(not(target_os = "macos"))]
    let (mic, acc, input) = (MicState::Granted, true, true);
    PermissionsReport {
        microphone: mic,
        accessibility: acc,
        input_monitoring: input,
        missing: missing_permissions(mic, acc, input),
    }
}

/// Кнопка карточки: системный запрос (один раз за запуск) и настройки.
pub fn request(kind: &str) -> Result<(), String> {
    if !matches!(kind, "microphone" | "accessibility" | "input_monitoring") {
        return Err(format!("неизвестный доступ: {kind}"));
    }
    #[cfg(target_os = "macos")]
    match kind {
        "microphone" => request_microphone_or_open_settings(),
        "accessibility" => {
            request_post_event_once();
            open_accessibility_settings();
        }
        _ => {
            request_input_monitoring_once();
            open_input_monitoring_settings();
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub use imp::*;

#[cfg(not(target_os = "macos"))]
pub fn onboard_on_launch(_app: tauri::AppHandle) {}

#[cfg(test)]
mod missing_tests {
    use super::{missing_permissions, MicState};

    #[test]
    fn nothing_is_missing_when_everything_is_granted() {
        assert!(missing_permissions(MicState::Granted, true, true).is_empty());
    }

    #[test]
    fn undetermined_and_denied_microphone_both_count_as_missing() {
        assert_eq!(
            missing_permissions(MicState::NotDetermined, true, true),
            ["microphone"]
        );
        assert_eq!(
            missing_permissions(MicState::Denied, true, true),
            ["microphone"]
        );
    }

    #[test]
    fn missing_keys_keep_card_row_order() {
        assert_eq!(
            missing_permissions(MicState::Denied, false, false),
            ["microphone", "accessibility", "input_monitoring"]
        );
        assert_eq!(
            missing_permissions(MicState::Granted, true, false),
            ["input_monitoring"]
        );
    }
}
