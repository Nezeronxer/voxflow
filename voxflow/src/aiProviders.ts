/**
 * Каталог OpenAI-совместимых провайдеров LLM-постобработки.
 *
 * Отдельный .ts (не .tsx) — как settingsSync.ts/hotkeyCapture.ts: чистая
 * логика без JSX импортируется напрямую в node --test.
 */

export type ModelOption = { value: string; label: string };

export type CompatProvider = {
  value: string;
  label: string;
  baseUrl: string;
  hint: string;
  /** Страница, где выдают ключ. Пусто — ключ не нужен (локальный сервер). */
  keyUrl: string;
  keyHint: string;
  /** Подсказки для поля модели; ввести можно любую (datalist, не Select). */
  models: readonly ModelOption[];
};

// Пресеты OpenAI-совместимых сервисов. Различие только в Base URL и моделях —
// протокол один (/chat/completions), поэтому список расширяется строкой.
// Последний пункт «Своё» — любой другой сервис: адрес, модель и заголовок
// ключа вводит пользователь.
export const OPENAI_COMPAT_PROVIDERS: readonly CompatProvider[] = [
  {
    value: "openrouter",
    label: "OpenRouter",
    baseUrl: "https://openrouter.ai/api/v1",
    hint: "Много моделей через один OpenAI-compatible API, есть бесплатные.",
    keyUrl: "https://openrouter.ai/keys",
    keyHint: "OPENROUTER_API_KEY",
    models: [],
  },
  {
    value: "groq",
    label: "Groq",
    baseUrl: "https://api.groq.com/openai/v1",
    hint: "Быстрые OpenAI-compatible модели Groq.",
    keyUrl: "https://console.groq.com/keys",
    keyHint: "REWRITE_API_KEY",
    models: [
      { value: "llama-3.3-70b-versatile", label: "Llama 3.3 70B Versatile" },
      { value: "llama-3.1-8b-instant", label: "Llama 3.1 8B Instant" },
    ],
  },
  {
    value: "openai",
    label: "OpenAI",
    baseUrl: "https://api.openai.com/v1",
    hint: "Официальный OpenAI API.",
    keyUrl: "https://platform.openai.com/api-keys",
    keyHint: "OPENAI_API_KEY",
    models: [
      { value: "gpt-4o-mini", label: "GPT-4o mini" },
      { value: "gpt-4o", label: "GPT-4o" },
      { value: "gpt-4.1-mini", label: "GPT-4.1 mini" },
    ],
  },
  {
    value: "deepseek",
    label: "DeepSeek",
    baseUrl: "https://api.deepseek.com/v1",
    hint: "Дёшево и хорошо знает русский.",
    keyUrl: "https://platform.deepseek.com/api_keys",
    keyHint: "REWRITE_API_KEY",
    models: [
      { value: "deepseek-chat", label: "DeepSeek Chat" },
      { value: "deepseek-reasoner", label: "DeepSeek Reasoner" },
    ],
  },
  {
    value: "mistral",
    label: "Mistral",
    baseUrl: "https://api.mistral.ai/v1",
    hint: "Европейский провайдер, есть бесплатный тир.",
    keyUrl: "https://console.mistral.ai/api-keys",
    keyHint: "REWRITE_API_KEY",
    models: [
      { value: "mistral-small-latest", label: "Mistral Small" },
      { value: "mistral-large-latest", label: "Mistral Large" },
    ],
  },
  {
    value: "together",
    label: "Together AI",
    baseUrl: "https://api.together.xyz/v1",
    hint: "Open-source модели через один ключ.",
    keyUrl: "https://api.together.ai/settings/api-keys",
    keyHint: "REWRITE_API_KEY",
    models: [
      {
        value: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        label: "Llama 3.3 70B Turbo",
      },
      { value: "Qwen/Qwen2.5-72B-Instruct-Turbo", label: "Qwen2.5 72B Turbo" },
    ],
  },
  {
    value: "cerebras",
    label: "Cerebras",
    baseUrl: "https://api.cerebras.ai/v1",
    hint: "Очень быстрый инференс — рерайт успевает до вставки.",
    keyUrl: "https://cloud.cerebras.ai",
    keyHint: "REWRITE_API_KEY",
    models: [
      { value: "llama-3.3-70b", label: "Llama 3.3 70B" },
      { value: "qwen-3-32b", label: "Qwen3 32B" },
    ],
  },
  {
    value: "xai",
    label: "xAI (Grok)",
    baseUrl: "https://api.x.ai/v1",
    hint: "OpenAI-совместимый эндпоинт xAI.",
    keyUrl: "https://console.x.ai",
    keyHint: "REWRITE_API_KEY",
    models: [
      { value: "grok-3-mini", label: "Grok 3 mini" },
      { value: "grok-3", label: "Grok 3" },
    ],
  },
  {
    value: "gemini_compat",
    label: "Google Gemini (OpenAI-режим)",
    baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
    hint: "Тот же ключ AI Studio, но через OpenAI-протокол.",
    keyUrl: "https://aistudio.google.com/apikey",
    keyHint: "REWRITE_API_KEY",
    models: [
      { value: "gemini-2.5-flash", label: "Gemini 2.5 Flash" },
      { value: "gemini-2.5-pro", label: "Gemini 2.5 Pro" },
    ],
  },
  {
    value: "anthropic",
    label: "Anthropic (OpenAI-режим)",
    baseUrl: "https://api.anthropic.com/v1",
    hint: "Claude через OpenAI-совместимый эндпоинт.",
    keyUrl: "https://console.anthropic.com/settings/keys",
    keyHint: "REWRITE_API_KEY",
    models: [
      { value: "claude-haiku-4-5-20251001", label: "Claude Haiku 4.5" },
      { value: "claude-sonnet-4-5", label: "Claude Sonnet 4.5" },
    ],
  },
  {
    value: "aqua",
    label: "Aqua / Avalon",
    baseUrl: "https://api.aqua.sh/v1",
    hint: "Aqua OpenAI-compatible endpoint.",
    keyUrl: "",
    keyHint: "REWRITE_API_KEY",
    models: [
      { value: "claude-3-5-haiku", label: "Claude 3.5 Haiku" },
      { value: "gpt-4o-mini", label: "GPT-4o mini" },
    ],
  },
  {
    value: "lmstudio",
    label: "LM Studio (локально)",
    baseUrl: "http://localhost:1234/v1",
    hint: "Локальный сервер LM Studio: офлайн, ключ не нужен.",
    keyUrl: "",
    keyHint: "—",
    models: [],
  },
  {
    value: "custom",
    label: "Своё (любой сервис)",
    baseUrl: "",
    hint: "Свой Base URL, модель и заголовок ключа — подойдёт любой OpenAI-совместимый API.",
    keyUrl: "",
    keyHint: "REWRITE_API_KEY",
    models: [],
  },
];

export const CUSTOM_PROVIDER = OPENAI_COMPAT_PROVIDERS[OPENAI_COMPAT_PROVIDERS.length - 1];
/**
 * Пресет по сохранённому адресу. Незнакомый адрес — это «Своё», а НЕ первый
 * пресет: раньше свой Base URL молча подписывался OpenRouter'ом и уходил в его
 * ветку «бесплатные модели».
 */
export function providerFromBaseUrl(baseUrl: string): CompatProvider {
  const normalized = baseUrl.trim().replace(/\/+$/, "").toLowerCase();
  if (!normalized) return OPENAI_COMPAT_PROVIDERS[0];
  return (
    OPENAI_COMPAT_PROVIDERS.find(
      (provider) => provider.baseUrl.toLowerCase() === normalized,
    ) ?? CUSTOM_PROVIDER
  );
}
