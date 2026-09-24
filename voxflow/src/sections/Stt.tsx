import { useState } from "react";
import { saveSettings, sttTest } from "../api";
import { PageHead, SectionShell, Field, Select, Switch, Icon } from "../ui";
import type { Settings } from "../types";
import SecretControl from "../components/SecretControl";

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
];

// Языки распознавания. «Авто» — Whisper для всех языков; явный RU/EN включают
// быстрые специализированные GigaAM/Parakeet (если установлены).
const LANGUAGE_OPTIONS = [
  { value: "auto", label: "Все языки (авто)" },
  { value: "ru", label: "Русский" },
  { value: "en", label: "English" },
  { value: "uk", label: "Українська" },
  { value: "de", label: "Deutsch" },
  { value: "fr", label: "Français" },
  { value: "es", label: "Español" },
  { value: "it", label: "Italiano" },
  { value: "pt", label: "Português" },
  { value: "pl", label: "Polski" },
  { value: "tr", label: "Türkçe" },
  { value: "zh", label: "中文" },
  { value: "ja", label: "日本語" },
  { value: "ko", label: "한국어" },
  { value: "ar", label: "العربية" },
  { value: "hi", label: "हिन्दी" },
];

/**
 * `part` делит раздел по странице «Диктовка»: main — язык и где распознавать
 * (то, что меняют), advanced — прокси, откат и черновик через API (в свёрнутом
 * «Дополнительно»). Без `part` рисуется всё — для отдельной страницы.
 */
export default function Stt({
  settings,
  update,
  persist,
  embedded,
  part,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  persist?: (settings: Settings) => Promise<boolean>;
  embedded?: boolean;
  part?: "main" | "advanced";
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

  async function onTest() {
    setTesting(true);
    setResult(null);
    try {
      // Бэкенд stt_test читает провайдера/ключи из настроек в БД. update() пишет
      // в БД с debounce (400 мс), поэтому здесь сохраняем синхронно — иначе можно
      // проверить устаревшие значения.
      const saved = await (persist ? persist(settings) : saveSettings(settings));
      if (!saved) {
        setResult("Не удалось сохранить настройки");
        return;
      }
      const r = await sttTest();
      setResult(r);
    } finally {
      setTesting(false);
    }
  }

  function chooseWhere(cloud: boolean) {
    setResult(null);
    if (!cloud) update({ stt_provider: "local" });
    // Облако без выбранного провайдера — сразу рекомендуемый пресет.
    else if (isLocal) update(STT_PRESETS[0].patch);
  }

  const main = (
    <div className="card">
      <div className="card-head">
        <div className="card-title">Распознавание</div>
      </div>

      <Field
        label="Язык"
        hint="«Авто» понимает все языки и смешанную речь. Русский и English работают быстрее."
      >
        <Select
          value={settings.language}
          onChange={(v) => update({ language: v })}
          options={LANGUAGE_OPTIONS}
        />
      </Field>

      <Field
        label="Где распознавать"
        hint={
          isLocal
            ? "Звук не покидает компьютер. Модели — в разделе «Модели распознавания»."
            : "Облако точнее на слабом железе, но нужен интернет и ключ."
        }
      >
        <div className="seg" role="radiogroup" aria-label="Где распознавать">
          {[
            { cloud: false, label: "На устройстве" },
            { cloud: true, label: "В облаке" },
          ].map((option) => {
            const active = option.cloud !== isLocal;
            return (
              <button
                key={option.label}
                type="button"
                role="radio"
                aria-checked={active}
                className={`seg-btn${active ? " active" : ""}`}
                onClick={() => chooseWhere(option.cloud)}
              >
                {option.label}
              </button>
            );
          })}
        </div>
      </Field>

      {!isLocal && (
        <>
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
                  <span className="cloud-preset-meta">{p.badge ?? "API-ключ"}</span>
                </button>
              );
            })}
          </div>
          {activePreset?.keyHint && (
            <div className="field-hint cloud-key-hint">{activePreset.keyHint}</div>
          )}

          <Field label="Провайдер">
            <Select
              value={settings.stt_provider}
              onChange={(v) => {
                setResult(null);
                update({ stt_provider: v });
              }}
              options={[
                { value: "openai_compat", label: "OpenAI-совместимый" },
                { value: "deepgram", label: "Deepgram" },
              ]}
            />
          </Field>

          {provider === "openai_compat" && (
            <>
              <Field label="Адрес API">
                <input
                  type="text"
                  className="input-mono"
                  placeholder="https://api.groq.com/openai/v1"
                  value={settings.oai_stt_base_url}
                  onChange={(e) => update({ oai_stt_base_url: e.currentTarget.value })}
                  style={{ width: 320 }}
                />
              </Field>
              <Field label="Модель">
                <input
                  type="text"
                  placeholder="whisper-large-v3"
                  value={settings.oai_stt_model}
                  onChange={(e) => update({ oai_stt_model: e.currentTarget.value })}
                  style={{ width: 260 }}
                />
              </Field>
              <Field label="API-ключ" hint="Хранится только на этом компьютере">
                <SecretControl
                  kind="oai_stt_key"
                  value={settings.oai_stt_key}
                  onChange={(value) => update({ oai_stt_key: value })}
                />
              </Field>
            </>
          )}

          {provider === "deepgram" && (
            <>
              <Field label="Адрес API">
                <input
                  type="text"
                  className="input-mono"
                  placeholder="https://api.deepgram.com"
                  value={settings.deepgram_base}
                  onChange={(e) => update({ deepgram_base: e.currentTarget.value })}
                  style={{ width: 320 }}
                />
              </Field>
              <Field label="Модель">
                <input
                  type="text"
                  placeholder="nova-3"
                  value={settings.deepgram_model}
                  onChange={(e) => update({ deepgram_model: e.currentTarget.value })}
                  style={{ width: 260 }}
                />
              </Field>
              <Field label="API-ключ" hint="Хранится только на этом компьютере">
                <SecretControl
                  kind="deepgram_key"
                  value={settings.deepgram_key}
                  onChange={(value) => update({ deepgram_key: value })}
                />
              </Field>
            </>
          )}

          <div className="add-row stt-test-row">
            <button className="btn" onClick={onTest} disabled={testing}>
              <Icon.Check className="ico" />
              {testing ? "Проверка…" : "Проверить"}
            </button>
            {result && <span className="stt-test-result">{result}</span>}
          </div>
        </>
      )}
    </div>
  );

  const advanced = (
    <>
      <Field
        label="Прокси"
        hint="Для облака и загрузки моделей. Пусто — системный прокси."
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
        label="Запасное распознавание на устройстве"
        hint="Если облако не ответило — распознать локально"
      >
        <Switch
          checked={settings.stt_fallback_local}
          onChange={(v) => update({ stt_fallback_local: v })}
        />
      </Field>

      {!isLocal && (
        <Field
          label="Живой текст в плашке через облако"
          hint="До 4 запросов на диктовку — расходует квоту API"
        >
          <Switch
            checked={settings.cloud_live_draft}
            onChange={(v) => update({ cloud_live_draft: v })}
          />
        </Field>
      )}
    </>
  );

  if (part === "main") return main;
  if (part === "advanced") return advanced;
  return (
    <SectionShell embedded={embedded}>
      {!embedded && (
        <PageHead
          title="Облако"
          desc="Облачный движок распознавания речи. Локальный GigaAM/Parakeet/Whisper остаётся по умолчанию и приватен — аудио не покидает устройство."
        />
      )}
      {main}
      <div className="card">{advanced}</div>
    </SectionShell>
  );
}
