import test from "node:test";
import assert from "node:assert/strict";
import {
  mergeRendererSettings,
  settingsFingerprint,
  stableSerialize,
} from "../src/settingsSync.ts";

test("stableSerialize ignores object key order", () => {
  assert.equal(stableSerialize({ b: 2, a: 1 }), stableSerialize({ a: 1, b: 2 }));
});

test("settingsFingerprint redacts every renderer secret", () => {
  const first = settingsFingerprint({
    theme: "dark",
    ai_api_key: "top-secret",
    oai_stt_key: "stt-secret",
    deepgram_key: "deep-secret",
    rewrite_key: "rewrite-secret",
  });
  const redacted = settingsFingerprint({
    theme: "dark",
    ai_api_key: "",
    oai_stt_key: "",
    deepgram_key: "",
    rewrite_key: "",
  });
  assert.equal(first, redacted);
  assert.equal(first.includes("top-secret"), false);
});

test("three-way merge preserves a newer local edit", () => {
  const base = { language: "auto", theme: "system", ai_api_key: "" };
  const current = { language: "ru", theme: "system", ai_api_key: "typed-key" };
  const incoming = { language: "en", theme: "dark", ai_api_key: "" };

  assert.deepEqual(mergeRendererSettings(base, current, incoming), {
    language: "ru",
    theme: "dark",
    ai_api_key: "typed-key",
  });
});

test("three-way merge accepts a tray change for an untouched field", () => {
  const base = { language: "auto", theme: "system" };
  const current = { language: "auto", theme: "dark" };
  const incoming = { language: "ru", theme: "system" };

  assert.deepEqual(mergeRendererSettings(base, current, incoming), {
    language: "ru",
    theme: "dark",
  });
});
