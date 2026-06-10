import { useState } from "react";
import { aiTest } from "../api";
import { PageHead, Field, Select, Switch, Icon } from "../ui";
import type { Settings } from "../types";

export default function Ai({
  settings,
  update,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
}) {
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; message: string } | null>(
    null,
  );

  const backend = settings.ai_backend;
  const aiOff = backend === "off";
  // Облачный ASR доступен только для Gemini — Qwen3 в Ollama чисто текстовый.
  const cloudAsrDisabled = backend !== "gemini";

  async function onTest() {
    setTesting(true);
    setResult(null);
    try {
      const r = await aiTest();
      setResult(r);
    } finally {
      setTesting(false);
    }
  }

  return (
    <div className="content-inner">
      <PageHead
        title="ИИ"
        desc="Подключите нейросеть для умной обработки текста и облачного распознавания."
      />

      <div className="card">
        <div className="card-head">
          <div className="card-title">Бэкенд</div>
          <div className="sub">
            Локальный whisper остаётся по умолчанию и работает офлайн. ИИ
            подключается отдельно.
          </div>
        </div>

        <Field
          label="Бэкенд ИИ"
          hint="Какую нейросеть использовать для умных функций"
        >
          <Select
            value={settings.ai_backend}
            onChange={(v) => {
              // Сбрасываем прошлый результат проверки и стейл-флаг cloud_asr
              // (он только для Gemini) — UI и хранилище не должны расходиться.
              setResult(null);
              update(
                v === "gemini"
                  ? { ai_backend: v }
                  : { ai_backend: v, cloud_asr: false },
              );
            }}
            options={[
              { value: "off", label: "Выключен" },
              { value: "ollama", label: "Локальный (Ollama / Qwen3)" },
              { value: "gemini", label: "Google Gemini" },
              {
                value: "openai_compat",
                label:
                  "Облачный (OpenAI-совместимый: Claude Haiku / OpenAI / Groq)",
              },
            ]}
          />
        </Field>

        {backend === "gemini" && (
          <>
            <Field
              label="API-ключ"
              hint="Ключ хранится локально и используется только для запросов к выбранному бэкенду"
            >
              <input
                type="password"
                placeholder="Вставьте ключ"
                value={settings.ai_api_key}
                onChange={(e) => update({ ai_api_key: e.currentTarget.value })}
                style={{ width: 260 }}
              />
            </Field>

            <div
              className="field-hint"
              style={{ marginTop: -6, marginBottom: 14, maxWidth: "none" }}
            >
              Бесплатный ключ:{" "}
              <a
                href="https://aistudio.google.com/apikey"
                target="_blank"
                rel="noreferrer"
                style={{ color: "var(--accent-hover)" }}
              >
                aistudio.google.com/apikey
              </a>
            </div>

            <Field
              label="Модель"
              hint="Идентификатор модели у выбранного бэкенда"
            >
              <input
                type="text"
                placeholder="gemini-2.5-flash"
                value={settings.ai_model}
                onChange={(e) => update({ ai_model: e.currentTarget.value })}
                style={{ width: 260 }}
              />
            </Field>
          </>
        )}

        {backend === "ollama" && (
          <>
            <Field
              label="Адрес Ollama"
              hint="Локальный сервер Ollama. По умолчанию работает на этом адресе"
            >
              <input
                type="text"
                placeholder="http://localhost:11434"
                value={settings.ollama_url}
                onChange={(e) => update({ ollama_url: e.currentTarget.value })}
                style={{ width: 260 }}
              />
            </Field>

            <Field label="Модель" hint="Идентификатор модели в Ollama">
              <input
                type="text"
                placeholder="qwen3:4b"
                value={settings.ollama_model}
                onChange={(e) => update({ ollama_model: e.currentTarget.value })}
                style={{ width: 260 }}
              />
            </Field>

            <div
              className="field-hint"
              style={{ marginTop: -6, marginBottom: 14, maxWidth: "none" }}
            >
              Установите Ollama (
              <a
                href="https://ollama.com/download"
                target="_blank"
                rel="noreferrer"
                style={{ color: "var(--accent-hover)" }}
              >
                ollama.com/download
              </a>
              ), затем: <code>ollama pull qwen3:4b</code>. Опционально — соберите
              профиль: <code>ollama create voiceflow -f voxflow/ollama/Modelfile</code>.
              Всё работает офлайн.
            </div>
          </>
        )}

        {backend === "openai_compat" && (
          <>
            <Field
              label="Base URL"
              hint="Адрес OpenAI-совместимого API. Avalon: https://api.aqua.sh/v1 · OpenAI: https://api.openai.com/v1 · Groq: https://api.groq.com/openai/v1"
            >
              <input
                type="text"
                className="input-mono"
                placeholder="https://api.groq.com/openai/v1"
                value={settings.rewrite_base_url}
                onChange={(e) =>
                  update({ rewrite_base_url: e.currentTarget.value })
                }
                style={{ width: 320 }}
              />
            </Field>

            <Field
              label="Модель"
              hint="Идентификатор chat-модели у провайдера"
            >
              <input
                type="text"
                placeholder="llama-3.3-70b-versatile / claude-3-5-haiku"
                value={settings.rewrite_model}
                onChange={(e) => update({ rewrite_model: e.currentTarget.value })}
                style={{ width: 320 }}
              />
            </Field>

            <Field
              label="API-ключ"
              hint="Ключ хранится локально и используется только для запросов к выбранному провайдеру"
            >
              <input
                type="password"
                placeholder="Вставьте ключ"
                value={settings.rewrite_key}
                onChange={(e) => update({ rewrite_key: e.currentTarget.value })}
                style={{ width: 260 }}
              />
            </Field>

            <div
              className="field-hint"
              style={{ marginTop: -6, marginBottom: 14, maxWidth: "none" }}
            >
              Или переменная окружения <code>REWRITE_API_KEY</code> /{" "}
              <code>OPENAI_API_KEY</code>; в коде/логах не хранится.
            </div>
          </>
        )}

        <div className="add-row" style={{ alignItems: "center" }}>
          <button
            className="btn btn-primary"
            onClick={onTest}
            disabled={testing || aiOff}
          >
            <Icon.Check className="ico" />
            {testing ? "Проверка…" : "Проверить"}
          </button>
          {result && (
            <span
              style={{
                fontSize: 13,
                color: result.ok ? "var(--green)" : "var(--red)",
              }}
            >
              {result.ok ? "Подключение работает" : result.message}
            </span>
          )}
          {aiOff && !result && (
            <span style={{ fontSize: 12.5, color: "var(--amber)" }}>
              Сначала выберите бэкенд ИИ
            </span>
          )}
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <div className="card-title">Умные функции</div>
          {aiOff && (
            <div className="sub" style={{ color: "var(--amber)" }}>
              Для функций ниже нужно включить бэкенд ИИ
            </div>
          )}
        </div>

        <Field
          label="Облачное распознавание"
          hint={
            backend === "ollama"
              ? "Только для облачного Gemini (Qwen3 — текстовый). Локальный whisper остаётся по умолчанию."
              : "Gemini вместо локального whisper. Локальный whisper остаётся по умолчанию и приватен — аудио не покидает устройство."
          }
        >
          <span
            style={
              cloudAsrDisabled
                ? { opacity: 0.4, pointerEvents: "none" }
                : undefined
            }
          >
            <Switch
              checked={cloudAsrDisabled ? false : settings.cloud_asr}
              onChange={(v) => update({ cloud_asr: v })}
            />
          </span>
        </Field>

        <Field
          label="Авто-стиль по приложению"
          hint="Gmail → официально, мессенджеры → неформально, нейросети → чёткий стиль"
        >
          <Switch
            checked={settings.tone_by_app}
            onChange={(v) => update({ tone_by_app: v })}
          />
        </Field>

        {aiOff && (
          <div className="field-hint" style={{ marginTop: 12, maxWidth: "none" }}>
            Эти функции работают только при включённом бэкенде ИИ.
          </div>
        )}
      </div>
    </div>
  );
}
