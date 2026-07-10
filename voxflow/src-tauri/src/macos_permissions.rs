#[cfg(target_os = "macos")]
mod imp {
    use crate::engine;
    use std::ffi::c_void;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tauri::{AppHandle, Emitter};

    type CFAllocatorRef = *const c_void;
    type CFBooleanRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFStringRef = *const c_void;

    static INPUT_REQUESTED: AtomicBool = AtomicBool::new(false);
    static POST_EVENT_REQUESTED: AtomicBool = AtomicBool::new(false);
    static ACCESSIBILITY_REQUESTED: AtomicBool = AtomicBool::new(false);
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

    pub fn onboarding_active() -> bool {
        ONBOARDING_ACTIVE.load(Ordering::SeqCst)
    }

    pub fn onboard_on_launch(app: AppHandle) {
        if ONBOARDING_STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        ONBOARDING_ACTIVE.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            let need_accessibility = !post_event_allowed();
            let need_input = !input_monitoring_allowed();
            if !need_accessibility && !need_input {
                ONBOARDING_ACTIVE.store(false, Ordering::SeqCst);
                return;
            }

            let _ = app.emit(
                "error",
                serde_json::json!({
                    "message": "Разрешите VoxFlow в macOS Privacy & Security: сначала Accessibility для вставки текста, затем Input Monitoring для Right Option"
                }),
            );
            engine::dbg_log(&format!(
                "permissions: onboarding needed accessibility={} input_monitoring={}",
                need_accessibility, need_input
            ));

            if need_accessibility {
                request_post_event_once();
                open_accessibility_settings();
                for _ in 0..90 {
                    if post_event_allowed() {
                        engine::dbg_log("permissions: Accessibility granted during onboarding");
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }

            if !input_monitoring_allowed() {
                request_input_monitoring_once();
                open_input_monitoring_settings();
                for _ in 0..90 {
                    if input_monitoring_allowed() {
                        engine::dbg_log("permissions: Input Monitoring granted during onboarding");
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            ONBOARDING_ACTIVE.store(false, Ordering::SeqCst);
        });
    }
}

#[cfg(target_os = "macos")]
pub use imp::*;

#[cfg(not(target_os = "macos"))]
pub fn onboard_on_launch(_app: tauri::AppHandle) {}
