import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [engineSource, permissionsSource, tauriSource, entitlementSource, infoSource] =
  await Promise.all([
    readFile(new URL("../src-tauri/src/engine.rs", import.meta.url), "utf8"),
    readFile(new URL("../src-tauri/src/macos_permissions.rs", import.meta.url), "utf8"),
    readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
    readFile(new URL("../src-tauri/Entitlements.plist", import.meta.url), "utf8"),
    readFile(new URL("../src-tauri/Info.plist", import.meta.url), "utf8"),
  ]);

function rustFunction(source, signature, nextMarker) {
  const start = source.indexOf(signature);
  assert.notEqual(start, -1, `missing ${signature}`);
  const end = source.indexOf(nextMarker, start);
  assert.notEqual(end, -1, `missing end marker for ${signature}`);
  return source.slice(start, end);
}

test("the packaged macOS app declares both microphone privacy contracts", () => {
  const config = JSON.parse(tauriSource);
  assert.equal(config.bundle?.macOS?.entitlements, "Entitlements.plist");
  assert.match(entitlementSource, /<key>com\.apple\.security\.device\.audio-input<\/key>\s*<true\/>/);
  assert.match(infoSource, /<key>NSMicrophoneUsageDescription<\/key>\s*<string>[^<]+<\/string>/);
});

test("a dictation action reaches CoreAudio before a missing-model warning", () => {
  const body = rustFunction(
    engineSource,
    "fn start_capture_into(",
    "\n/// Поднять петлю",
  );
  const microphoneOpen = body.indexOf("audio::start_capture(&device)");
  const modelGuard = body.indexOf("no_model_installed(&s)");
  assert.notEqual(microphoneOpen, -1, "missing CoreAudio capture start");
  assert.notEqual(modelGuard, -1, "missing local-model guard");
  assert.ok(
    microphoneOpen < modelGuard,
    "the local-model guard must not prevent macOS from asking for microphone access",
  );
});

test("first-launch onboarding offers Right Option permission before text insertion", () => {
  const body = rustFunction(
    permissionsSource,
    "pub fn onboard_on_launch(",
    "\n}\n\n#[cfg(target_os = \"macos\")]",
  );
  const inputStep = body.indexOf("if need_input {");
  const accessibilityStep = body.indexOf("if need_accessibility && !post_event_allowed() {");
  assert.notEqual(inputStep, -1, "missing Input Monitoring onboarding step");
  assert.notEqual(accessibilityStep, -1, "missing Accessibility onboarding step");
  assert.ok(
    inputStep < accessibilityStep,
    "Right Option must not wait behind the Accessibility timeout",
  );
});
