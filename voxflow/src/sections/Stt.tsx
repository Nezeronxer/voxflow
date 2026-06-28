import { useState } from "react";
import { saveSettings, sttTest } from "../api";
import { PageHead, Field, Select, Switch, Icon } from "../ui";
import type { Settings } from "../types";

// Облачный STT (D-022). Локальный роутер GigaAM/Parakeet/Whisper остаётся
// дефолтом и приватен — аудио не покидает устройство. Облачные провайдеры
// подключаются здесь как альтернатива с авто-fallback на локальное распознавание.
//
// Стиль секции — как Ai.tsx: PageHead + карточки с Field/Select/Switch, кнопка
// «Проверить». Все поля живут в Settings (types.ts) и сохраняются тем же update(),
// что и остальные настройки (debounce-save в App.tsx). Перед stt_test делаем явный
// синхронный saveSettings — бэкенд читает провайдера/ключи из БД, и проверять надо
// именно текущее (несохранённое из-за debounce) состояние.

// Готовые пресеты провайдеров: один клик → провайдер/URL/модель заполняются сами.
// Groq · whisper-large-v3 — рекомендуемая сильная модель «уровня Aqua»: флагман по
// точности (8.4% WER), мультиязычный (русский), OpenAI-совместимый, БЕСПЛАТНЫЙ ключ.
type SttPreset = {
  id: string;
  label: string;
  badge?: string;
  patch: Partial<Settings>;
  keyHint?: string;
};
const STT_PRESETS: SttPreset[] = [
  {
    id: "groq-large-v3",
    label: "Groq · whisper-large-v3",
    badge: "рекоменд., беспл.",
    patch: {
      stt_provider: "openai_compat",
      oai_stt_base_url: "https://api.groq.com/openai/v1",
      oai_stt_model: "whisper-large-v3",
    },
    keyHint: "Бесплатный ключ за 1 мин: console.groq.com/keys",
  },
  {
    id: "groq-turbo",
    label: "Groq · large-v3-turbo",
    badge: "быстрее",
    patch: {
      stt_provider: "openai_compat",
      oai_stt_base_url: "https://api.groq.com/openai/v1",
      oai_stt_model: "whisper-large-v3-turbo",
    },
    keyHint: "Бесплатный ключ за 1 мин: console.groq.com/keys",
  },
  {
    id: "avalon",
    label: "Aqua · avalon-1",
    patch: {
      stt_provider: "openai_compat",
      oai_stt_base_url: "https://api.aqua.sh/v1",
      oai_stt_model: "avalon-1",
    },
    keyHint: "Ключ: дашборд Aqua (платно, ~$0.39/час)",
  },
  {
    id: "openai",
    label: "OpenAI · gpt-4o-transcribe",
    patch: {
      stt_provider: "openai_compat",
      oai_stt_base_url: "https://api.openai.com/v1",
      oai_stt_model: "gpt-4o-transcribe",
    },
    keyHint: "Ключ: platform.openai.com/api-keys",
  },
  {
    id: "deepgram",
    label: "Deepgram · nova-3",
    patch: {
      stt_provider: "deepgram",
      deepgram_base: "https://api.deepgram.com",
      deepgram_model: "nova-3",
    },
    keyHint: "Ключ: console.deepgram.com (есть free-tier)",
  },
  {
    id: "local",
    label: "Локально",
    badge: "офлайн",
    patch: { stt_provider: "local" },
  },
];

export default function Stt({
  settings,
  update,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
}) {
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState<string | null>(null);

  const provider = settings.stt_provider;
  const isLocal = provider === "local";

  // Активен пресет, если ВСЕ его поля совпадают с текущими настройками (иначе «Свой»).
  const activePreset = STT_PRESETS.find((p) =>
    Object.entries(p.patch).every(
      ([k, v]) => (settings as unknown as Record<string, unknown>)[k] === v,
    ),
  );
  const providerName =
    provider === "deepgram"
      ? "Deepgram"
      : provider === "openai_compat"
        ? "OpenAI-compatible"
        : "Локально";
  const providerMode = isLocal ? "Офлайн и приватно" : "Облако с фолбэком";
  const providerHint = isLocal
    ? "Аудио остаётся на устройстве"
    : settings.stt_fallback_local
      ? "При ошибке вернётся на локальное распознавание"
      : "Нужны сеть и API-ключ";

  async function onTest() {
    setTesting(true);
    setResult(null);
    try {
      // Бэкенд stt_test читает провайдера/ключи из настроек в БД. update() пишет
      // в БД с debounce (400 мс), поэтому здесь сохраняем синхронно — иначе можно
      // проверить устаревшие значения.
      await saveSettings(settings);
      const r = await sttTest();
      setResult(r);
    } finally {
      setTesting(false);
    }
  }

  return (
    <div className="content-inner">
      <PageHead
        title="Облако"
        desc="Облачный движок распознавания речи. Локальный GigaAM/Parakeet/Whisper остаётся по умолчанию и приватен — аудио не покидает устройство."
      />

      <div className="card cloud-card">
        <div className="cloud-provider-head">
          <div>
            <div className="card-title">Провайдер STT</div>
            <p className="cloud-provider-copy">
              Какой движок распознаёт речь. Облако подключается как BYOK-режим,
              локальное распознавание остаётся приватным запасным вариантом.
            </p>
          </div>
          <div className="cloud-provider-status" aria-live="polite">
            <span>{providerName}</span>
            <strong>{providerMode}</strong>
            <small>{providerHint}</small>
          </div>
        </div>

        <div className="cloud-presets">
          <div className="cloud-presets-head">
            <div>
              <div className="field-label">Готовые пресеты</div>
              <div className="field-hint">
                Один клик заполнит провайдера, адрес и модель. Groq ·
                whisper-large-v3 — сильный бесплатный старт.
              </div>
            </div>
            <div className="cloud-presets-active">
              {activePreset ? activePreset.label : "Свой набор"}
            </div>
          </div>

          <div className="cloud-preset-grid">
            {STT_PRESETS.map((p) => {
              const active = activePreset?.id === p.id;
              return (
                <button
                  key={p.id}
                  type="button"
                  className={active ? "cloud-preset is-active" : "cloud-preset"}
                  onClick={() => {
                    setResult(null);
                    update(p.patch);
                  }}
                  aria-pressed={active}
                >
                  <span className="cloud-preset-main">{p.label}</span>
                  <span className="cloud-preset-meta">
                    {p.badge ?? (p.id === "local" ? "без ключа" : "API-ключ")}
                  </span>
                </button>
              );
            })}
          </div>

          <div className="cloud-preset-note">
            {activePreset?.keyHint ??
              (activePreset
                ? "Локальный режим выбран: ключ и проверка соединения не нужны."
                : "Пресет не выбран: можно вручную указать провайдера, URL и модель ниже.")}
          </div>
          <div className="cloud-preset-note">
            Если диктовка распознаётся неверно, сначала смените язык или модель
            во вкладке «Модель». Cloud STT можно включить для сложных голосов,
            микрофонов и языков, но онлайн-провайдеры зависят от сети, лимитов
            API и доступности сервиса.
          </div>
        </div>

        <Field
          label="Движок распознавания"
          hint="Локальные GigaAM/Parakeet/Whisper работают офлайн и приватно. Облачные провайдеры — быстрее на слабом железе, но требуют сети и ключа."
        >
          <Select
            value={settings.stt_provider}
            onChange={(v) => {
              setResult(null);
              update({ stt_provider: v });
            }}
            options={[
              { value: "local", label: "Локально" },
              {
                value: "openai_compat",
                label: "OpenAI-совместимый (Avalon/OpenAI/Groq)",
              },
              { value: "deepgram", label: "Deepgram" },
            ]}
          />
        </Field>

        {!isLocal && (
          <div
            className="field-hint"
            style={{ marginTop: -6, marginBottom: 14, maxWidth: "none" }}
          >
            Без ключа VoxFlow мгновенно работает локально (умный
            фолбэк) — облако подключится, как только укажете ключ.
          </div>
        )}

        {provider === "openai_compat" && (
          <>
            <Field
              label="Base URL"
              hint="Адрес OpenAI-совместимого API. Пресеты выше заполняют его сами."
            >
              <input
                type="text"
                className="input-mono"
                placeholder="https://api.groq.com/openai/v1"
                value={settings.oai_stt_base_url}
                onChange={(e) =>
                  update({ oai_stt_base_url: e.currentTarget.value })
                }
                style={{ width: 320 }}
              />
            </Field>

            <Field label="Модель" hint="Идентификатор модели у провайдера">
              <input
                type="text"
                placeholder="whisper-large-v3"
                value={settings.oai_stt_model}
                onChange={(e) =>
                  update({ oai_stt_model: e.currentTarget.value })
                }
                style={{ width: 260 }}
              />
            </Field>

            <Field
              label="API-ключ"
              hint="Ключ хранится локально и используется только для запросов к выбранному провайдеру"
            >
              <input
                type="password"
                placeholder="Вставьте ключ"
                value={settings.oai_stt_key}
                onChange={(e) => update({ oai_stt_key: e.currentTarget.value })}
                style={{ width: 260 }}
              />
            </Field>

            <div
              className="field-hint"
              style={{ marginTop: -6, marginBottom: 4, maxWidth: "none" }}
            >
              Рекомендуется Groq · whisper-large-v3 — флагман по точности,
              сильный русский, OpenAI-совместимый, бесплатный ключ
              (console.groq.com/keys). Из РФ — через прокси.
            </div>
          </>
        )}

        {provider === "deepgram" && (
          <>
            <Field label="Base URL" hint="Адрес API Deepgram">
              <input
                type="text"
                className="input-mono"
                placeholder="https://api.deepgram.com"
                value={settings.deepgram_base}
                onChange={(e) =>
                  update({ deepgram_base: e.currentTarget.value })
                }
                style={{ width: 320 }}
              />
            </Field>

            <Field label="Модель" hint="Идентификатор модели Deepgram">
              <input
                type="text"
                placeholder="nova-3"
                value={settings.deepgram_model}
                onChange={(e) =>
                  update({ deepgram_model: e.currentTarget.value })
                }
                style={{ width: 260 }}
              />
            </Field>

            <Field
              label="API-ключ"
              hint="Ключ хранится локально и используется только для запросов к Deepgram"
            >
              <input
                type="password"
                placeholder="Вставьте ключ"
                value={settings.deepgram_key}
                onChange={(e) =>
                  update({ deepgram_key: e.currentTarget.value })
                }
                style={{ width: 260 }}
              />
            </Field>
          </>
        )}

        <div className="add-row stt-test-row">
          <button
            className="btn btn-primary"
            onClick={onTest}
            disabled={testing || isLocal}
          >
            <Icon.Check className="ico" />
            {testing ? "Проверка…" : "Проверить"}
          </button>
          {result && (
            <span className="stt-test-result">{result}</span>
          )}
          {isLocal && !result && (
            <span className="stt-test-local">
              Локальное распознавание не требует проверки соединения
            </span>
          )}
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <div className="card-title">Сеть и отказоустойчивость</div>
        </div>

        <Field
          label="Прокси"
          hint="HTTP/HTTPS-прокси для облачных запросов. Пусто → используется системный HTTPS_PROXY из окружения."
        >
          <input
            type="text"
            className="input-mono"
            placeholder="http://127.0.0.1:10808"
            value={settings.proxy_url}
            onChange={(e) => update({ proxy_url: e.currentTarget.value })}
            style={{ width: 320 }}
          />
        </Field>

        <Field
          label="Откат на локальное распознавание"
          hint="Если облако недоступно (нет сети, ошибка или таймаут) — автоматически распознать локально. В плашке появится метка «офлайн»."
        >
          <Switch
            checked={settings.stt_fallback_local}
            onChange={(v) => update({ stt_fallback_local: v })}
          />
        </Field>

        {!isLocal && (
          <Field
            label="Живой черновик в плашке (через API)"
            hint="Показывать серый текст в плашке во время речи для облачной модели — как у офлайн-моделей, но через API. Периодически отправляет растущий звук в облако (≤4 превью на диктовку). Локальная модель не нужна. Расходует квоту API: на бесплатном тире при активной диктовке лимит можно исчерпать — тогда выключите этот тоггл (распознавание останется, без серого превью)."
          >
            <Switch
              checked={settings.cloud_live_draft}
              onChange={(v) => update({ cloud_live_draft: v })}
            />
          </Field>
        )}
      </div>
    </div>
  );
}
