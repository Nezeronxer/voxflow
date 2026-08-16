import test from "node:test";
import assert from "node:assert/strict";
import {
  OPENAI_COMPAT_PROVIDERS,
  CUSTOM_PROVIDER,
  providerFromBaseUrl,
} from "../src/aiProviders.ts";

test("known base urls resolve to their own preset", () => {
  for (const provider of OPENAI_COMPAT_PROVIDERS) {
    if (!provider.baseUrl) continue;
    assert.equal(providerFromBaseUrl(provider.baseUrl).value, provider.value);
  }
  // Хвостовой слэш, регистр и пробелы приходят из буфера обмена постоянно.
  assert.equal(
    providerFromBaseUrl("  https://OpenRouter.ai/api/v1/  ").value,
    "openrouter",
  );
});

test("unknown base url is custom, not silently the first preset", () => {
  assert.equal(providerFromBaseUrl("https://api.example.com/v1").value, "custom");
  assert.equal(CUSTOM_PROVIDER.value, "custom");
  // Пустой адрес — ещё не выбор пользователя: показываем первый пресет.
  assert.equal(providerFromBaseUrl("").value, OPENAI_COMPAT_PROVIDERS[0].value);
});

test("every preset carries a usable https endpoint", () => {
  for (const provider of OPENAI_COMPAT_PROVIDERS) {
    if (!provider.baseUrl) continue;
    const url = new URL(provider.baseUrl);
    // Бэкенд (net::ensure_https_or_loopback_base) режет всё, кроме https и loopback.
    const loopback = url.hostname === "localhost" || url.hostname === "127.0.0.1";
    assert.ok(
      url.protocol === "https:" || loopback,
      `${provider.value}: ${provider.baseUrl}`,
    );
    assert.ok(!provider.baseUrl.endsWith("/"), `${provider.value}: лишний слэш`);
    if (provider.keyUrl) assert.ok(provider.keyUrl.startsWith("https://"));
  }
});
