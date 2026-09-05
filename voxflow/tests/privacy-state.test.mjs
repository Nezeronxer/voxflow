import test from "node:test";
import assert from "node:assert/strict";
import { egressState, isLoopbackUrl } from "../src/privacyState.ts";
import { DEFAULT_SETTINGS } from "../src/types.ts";

const base = (patch) => ({ ...DEFAULT_SETTINGS, ...patch });

test("чистая установка честно называется локальной", () => {
  const state = egressState(base({}));
  assert.equal(state.local, true);
  assert.match(state.summary, /Локально/);
  assert.deepEqual(state.audioServices, []);
  assert.deepEqual(state.textServices, []);
});

test("облачный ключ постобработки снимает обещание локальности", () => {
  // Ровно случай, из-за которого подвал врал: ключ настроен, текст уходит.
  const state = egressState(
    base({
      ai_backend: "openai_compat",
      rewrite_base_url: "https://api.openai.com/v1",
      rewrite_model: "gpt-4o-mini",
    }),
  );
  assert.equal(state.local, false);
  assert.deepEqual(state.textServices, ["OpenAI"]);
  assert.deepEqual(state.audioServices, []);
  assert.match(state.summary, /Текст диктовки уходит в OpenAI/);
});

test("локальные движки остаются локальными, несмотря на выбранный бэкенд", () => {
  // Петля — это та же машина: обещание локальности остаётся честным.
  for (const patch of [
    { ai_backend: "ollama", ollama_url: "http://localhost:11434" },
    { ai_backend: "ollama", ollama_url: "http://127.0.0.1:11434" },
    {
      ai_backend: "openai_compat",
      rewrite_base_url: "http://localhost:1234/v1",
      rewrite_model: "qwen",
    },
    // Бэкенд выбран, но адрес пустой — запросов ещё нет.
    { ai_backend: "openai_compat", rewrite_base_url: "" },
  ]) {
    const state = egressState(base(patch));
    assert.equal(state.local, true, JSON.stringify(patch));
  }
});

test("удалённая Ollama перестаёт быть локальной", () => {
  const state = egressState(
    base({ ai_backend: "ollama", ollama_url: "http://195.96.132.160:11434" }),
  );
  assert.equal(state.local, false);
  assert.deepEqual(state.textServices, ["195.96.132.160"]);
});

test("облачное распознавание уводит аудио, а не только текст", () => {
  const state = egressState(
    base({ stt_provider: "deepgram", deepgram_base: "https://api.deepgram.com" }),
  );
  assert.equal(state.local, false);
  assert.deepEqual(state.audioServices, ["Deepgram"]);
  assert.match(state.summary, /Аудио диктовки уходит в Deepgram/);
});

test("один сервис на оба канала не перечисляется дважды", () => {
  const state = egressState(
    base({
      ai_backend: "gemini",
      ai_api_key: "k",
      cloud_asr: true,
    }),
  );
  assert.equal(state.summary, "Аудио и текст уходят в Google Gemini");
});

test("разные сервисы на аудио и текст названы оба", () => {
  const state = egressState(
    base({
      stt_provider: "openai_compat",
      oai_stt_base_url: "https://api.groq.com/openai/v1",
      ai_backend: "openai_compat",
      rewrite_base_url: "https://api.deepseek.com/v1",
      rewrite_model: "deepseek-chat",
    }),
  );
  assert.match(state.summary, /Аудио → Groq/);
  assert.match(state.summary, /текст → DeepSeek/);
});

test("cloud_asr без Gemini-бэкенда аудио никуда не уводит", () => {
  // Флаг стейл-значение: движок его игнорирует, подвал обязан игнорировать тоже.
  const state = egressState(base({ cloud_asr: true, ai_backend: "off" }));
  assert.equal(state.local, true);
});

test("isLoopbackUrl не обманывается похожим хостом", () => {
  assert.equal(isLoopbackUrl("http://localhost:11434"), true);
  assert.equal(isLoopbackUrl("https://localhost.evil.test/v1"), false);
  assert.equal(isLoopbackUrl("не-адрес"), false);
});
