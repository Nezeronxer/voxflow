import { useEffect, useState } from "react";
import { aiTest, openExternalUrl, saveSettings, type AiModelOption } from "../api";
import { PageHead, Field, Select, Switch, Icon } from "../ui";
import type { Settings } from "../types";
import {
  CUSTOM_PROVIDER,
  OPENAI_COMPAT_PROVIDERS,
  providerFromBaseUrl,
} from "../aiProviders";
import SecretControl from "../components/SecretControl";
import LocalAiCard from "../components/LocalAiCard";

type Option = AiModelOption;

const GEMINI_MODELS: Option[] = [
  { value: "gemini-2.5-flash", label: "Gemini 2.5 Flash" },
  { value: "gemini-2.5-pro", label: "Gemini 2.5 Pro" },
  { value: "gemini-2.0-flash", label: "Gemini 2.0 Flash" },
];

const OLLAMA_MODELS: Option[] = [
  { value: "qwen3:4b", label: "Qwen3 4B" },
  { value: "qwen3:8b", label: "Qwen3 8B" },
  { value: "llama3.1:8b", label: "Llama 3.1 8B" },
  { value: "gemma3:4b", label: "Gemma 3 4B" },
  { value: "voiceflow", label: "VoiceFlow profile" },
];

function withCurrentOption(options: readonly Option[], current: string): Option[] {
  const value = current.trim();
  if (!value || options.some((option) => option.value === value)) return [...options];
  return [{ value, label: `Текущая: ${value}` }, ...options];
}


export default function Ai({
  settings,
  update,
  persist,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  persist?: (settings: Settings) => Promise<boolean>;
}) {
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; message: string } | null>(
    null,
  );
  const [openRouterModels, setOpenRouterModels] = useState<Option[]>([]);

  const backend = settings.ai_backend;
  const aiOff = backend === "off";
  const savedProvider = providerFromBaseUrl(settings.rewrite_base_url);
  // Выбор «Своё» держим в состоянии: адрес там пустой, пока пользователь не
  // впечатал свой, и по одному только rewrite_base_url его не отличить от
  // свежей установки (иначе селект сам прыгал бы обратно на первый пресет).
  const [customPicked, setCustomPicked] = useState(savedProvider.value === "custom");
  const isCustom = customPicked || savedProvider.value === "custom";
  const rewriteProvider = isCustom ? CUSTOM_PROVIDER : savedProvider;
  const isOpenRouter = rewriteProvider.value === "openrouter";
  // Облачный ASR доступен только для Gemini — Qwen3 в Ollama чисто текстовый.
  const cloudAsrDisabled = backend !== "gemini";

  useEffect(() => {
    setOpenRouterModels([]);
  }, [backend, settings.rewrite_base_url, settings.rewrite_key]);

  function applyOpenAiCompatProvider(providerValue: string) {
    const provider =
      OPENAI_COMPAT_PROVIDERS.find((item) => item.value === providerValue) ??
      OPENAI_COMPAT_PROVIDERS[0];
    const providerIsOpenRouter = provider.value === "openrouter";
    const keepModel = provider.models.some(
      (model) => model.value === settings.rewrite_model,
    );
    setResult(null);
    setOpenRouterModels([]);
    setCustomPicked(provider.value === "custom");
    if (provider.value === "custom") {
      // Свой адрес уже введён — не затираем его переключением селекта.
      if (!isCustom) {
        update({ rewrite_base_url: "", rewrite_model: "", rewrite_auth_header: "" });
      }
      return;
    }
    update({
      rewrite_base_url: provider.baseUrl,
      rewrite_model: providerIsOpenRouter
        ? ""
        : keepModel
        ? settings.rewrite_model
        : provider.models[0]?.value ?? "",
      // У пресетов ключ всегда уходит как Authorization: Bearer. Оставшийся
      // от «Своего» заголовок ломал бы авторизацию молча.
      rewrite_auth_header: "",
    });
  }

  async function onTest() {
    setTesting(true);
    setResult(null);
    try {
      const saved = await (persist ? persist(settings) : saveSettings(settings));
      if (!saved) {
        setResult({ ok: false, message: "Не удалось сохранить настройки" });
        return;
      }
      const r = await aiTest();
      setResult(r);
      if (isOpenRouter && r.ok && r.models?.length) {
        setOpenRouterModels(r.models);
        const current = settings.rewrite_model.trim();
        if (!r.models.some((model) => model.value === current)) {
          update({ rewrite_model: r.models[0].value });
        }
      } else if (isOpenRouter) {
        setOpenRouterModels([]);
      }
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

      <LocalAiCard settings={settings} update={update} />

      <div className="card">
        <div className="card-head">
          <div className="card-title">Бэкенд</div>
          <div className="sub">
            Локальное распознавание остаётся по умолчанию и работает офлайн. ИИ
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
              setOpenRouterModels([]);
              if (v === "openai_compat") {
                // Уже введённый адрес (в том числе свой) сохраняем как есть;
                // пустой — первое включение, подставляем первый пресет.
                const baseUrl =
                  settings.rewrite_base_url.trim() ||
                  OPENAI_COMPAT_PROVIDERS[0].baseUrl;
                const provider = providerFromBaseUrl(baseUrl);
                const providerIsOpenRouter = provider.value === "openrouter";
                update({
                  ai_backend: v,
                  ai_backend_behavior_version: 1,
                  cloud_asr: false,
                  rewrite_base_url: baseUrl,
                  rewrite_model: providerIsOpenRouter
                    ? ""
                    : settings.rewrite_model.trim() ||
                      provider.models[0]?.value ||
                      "",
                });
              } else {
                update(
                  v === "gemini"
                    ? { ai_backend: v, ai_backend_behavior_version: 1 }
                    : {
                        ai_backend: v,
                        ai_backend_behavior_version: 1,
                        cloud_asr: false,
                      },
                );
              }
            }}
            options={[
              { value: "off", label: "Выключен" },
              { value: "ollama", label: "Локальный (Ollama / Qwen3)" },
              { value: "gemini", label: "Google Gemini" },
              {
                value: "openai_compat",
                label:
                  "Облачный (OpenRouter / OpenAI-compatible)",
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
              <SecretControl
                kind="ai_api_key"
                value={settings.ai_api_key}
                onChange={(value) => update({ ai_api_key: value })}
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
              hint="Выберите модель Gemini для обработки текста"
            >
              <Select
                value={settings.ai_model}
                onChange={(v) => update({ ai_model: v })}
                options={withCurrentOption(GEMINI_MODELS, settings.ai_model)}
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

            <Field label="Модель" hint="Выберите локальную модель Ollama">
              <Select
                value={settings.ollama_model}
                onChange={(v) => update({ ollama_model: v })}
                options={withCurrentOption(
                  OLLAMA_MODELS,
                  settings.ollama_model,
                )}
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
              label="Провайдер"
              hint={rewriteProvider.hint}
            >
              <Select
                value={rewriteProvider.value}
                onChange={applyOpenAiCompatProvider}
                options={OPENAI_COMPAT_PROVIDERS.map((provider) => ({
                  value: provider.value,
                  label: provider.label,
                }))}
              />
            </Field>

            {isCustom && (
              <Field
                label="Base URL"
                hint="Адрес OpenAI-совместимого API без /chat/completions. Только https (или localhost)"
              >
                <input
                  type="text"
                  placeholder="https://api.example.com/v1"
                  value={settings.rewrite_base_url}
                  onChange={(e) => {
                    setResult(null);
                    update({ rewrite_base_url: e.currentTarget.value });
                  }}
                  style={{ width: 260 }}
                />
              </Field>
            )}

            <Field
              label="API-ключ"
              hint="Ключ хранится локально и используется только для запросов к выбранному провайдеру"
            >
              <SecretControl
                kind="rewrite_key"
                value={settings.rewrite_key}
                onChange={(value) => {
                  setResult(null);
                  setOpenRouterModels([]);
                  update({ rewrite_key: value });
                }}
              />
            </Field>

            <div
              className="field-hint"
              style={{ marginTop: -6, marginBottom: 14, maxWidth: "none" }}
            >
              {rewriteProvider.keyUrl && (
                <>
                  Где взять ключ:{" "}
                  <button
                    type="button"
                    className="link-btn"
                    onClick={() => void openExternalUrl(rewriteProvider.keyUrl)}
                  >
                    {rewriteProvider.keyUrl.replace(/^https:\/\//, "")}
                  </button>
                  {". "}
                </>
              )}
              {isCustom ? (
                <>
                  Для своего адреса ключ берётся только из этого поля: из
                  переменных окружения он подхватывается лишь у известных
                  провайдеров и localhost. В коде/логах не хранится.
                </>
              ) : (
                <>
                  Или переменная окружения{" "}
                  {[
                    ...new Set([
                      "REWRITE_API_KEY",
                      rewriteProvider.keyHint,
                      "OPENAI_API_KEY",
                    ]),
                  ]
                    .filter((name) => /^[A-Z_]+$/.test(name))
                    .map((name, index) => (
                      <span key={name}>
                        {index > 0 && " / "}
                        <code>{name}</code>
                      </span>
                    ))}
                  ; в коде/логах не хранится.
                </>
              )}
            </div>

            {isCustom && (
              <Field
                label="Заголовок ключа"
                hint="Пусто = Authorization: Bearer <ключ>. Для сервисов вне OpenAI-конвенции впишите своё имя заголовка, например x-api-key"
              >
                <input
                  type="text"
                  placeholder="Authorization"
                  value={settings.rewrite_auth_header}
                  onChange={(e) => {
                    setResult(null);
                    update({ rewrite_auth_header: e.currentTarget.value });
                  }}
                  style={{ width: 260 }}
                />
              </Field>
            )}

            {isOpenRouter ? (
              openRouterModels.length > 0 ? (
                <Field
                  label="Бесплатная модель"
                  hint={`Base URL: ${rewriteProvider.baseUrl}`}
                >
                  <Select
                    value={
                      openRouterModels.some(
                        (model) => model.value === settings.rewrite_model,
                      )
                        ? settings.rewrite_model
                        : openRouterModels[0]?.value ?? ""
                    }
                    onChange={(v) => update({ rewrite_model: v })}
                    options={openRouterModels}
                  />
                </Field>
              ) : (
                <Field
                  label="Бесплатная модель"
                  hint="Список появится только после успешной проверки OpenRouter-ключа"
                >
                  <span className="field-hint" style={{ maxWidth: 280 }}>
                    Вставьте ключ и нажмите «Проверить».
                  </span>
                </Field>
              )
            ) : (
              <Field
                label="Модель"
                hint={
                  isCustom
                    ? "Идентификатор модели как его ждёт ваш сервис"
                    : `Base URL: ${rewriteProvider.baseUrl}. Можно вписать любую модель провайдера`
                }
              >
                {/* Не Select: список моделей у провайдеров меняется чаще релизов
                    VoxFlow. datalist даёт подсказки, но не запирает ввод. */}
                <input
                  type="text"
                  list="rewrite-model-options"
                  placeholder={rewriteProvider.models[0]?.value ?? "model-id"}
                  value={settings.rewrite_model}
                  onChange={(e) => {
                    setResult(null);
                    update({ rewrite_model: e.currentTarget.value });
                  }}
                  style={{ width: 260 }}
                />
                <datalist id="rewrite-model-options">
                  {rewriteProvider.models.map((model) => (
                    <option key={model.value} value={model.value}>
                      {model.label}
                    </option>
                  ))}
                </datalist>
              </Field>
            )}
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
              {result.ok ? result.message || "Подключение работает" : result.message}
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
              ? "Только для облачного Gemini (Qwen3 — текстовый). Локальное распознавание остаётся по умолчанию."
              : "Gemini вместо локального распознавания. Локальный GigaAM/Parakeet/Whisper остаётся приватным запасным вариантом — аудио не покидает устройство."
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

        {aiOff && (
          <div className="field-hint" style={{ marginTop: 12, maxWidth: "none" }}>
            Эти функции работают только при включённом бэкенде ИИ.
          </div>
        )}
      </div>

      <div className="card">
        <div className="card-head">
          <div className="card-title">Надёжность рерайта</div>
          <div className="sub">
            Рерайт синхронный: не успел или потерял слова — в поле уходит текст
            после локальных правил.
          </div>
        </div>

        <Field
          label="Таймаут ответа, с"
          hint="Сколько ждать ответа модели. Локальная Ollama получает минимум 60 с — на CPU меньше не хватает"
        >
          <input
            type="number"
            min={5}
            max={300}
            value={settings.ai_timeout_s}
            onChange={(e) => {
              const n = parseInt(e.currentTarget.value, 10);
              update({
                ai_timeout_s: Number.isFinite(n)
                  ? Math.min(300, Math.max(5, n))
                  : 20,
              });
            }}
          />
        </Field>

        <Field
          label="Сохранять слов, %"
          hint="Какая доля слов диктовки обязана остаться в ответе модели. Ниже порога рерайт отклоняется целиком. 100% = запрет любой потери"
        >
          <input
            type="number"
            min={0}
            max={100}
            step={5}
            value={Math.round(settings.rewrite_min_recall * 100)}
            onChange={(e) => {
              const n = parseInt(e.currentTarget.value, 10);
              update({
                rewrite_min_recall: Number.isFinite(n)
                  ? Math.min(100, Math.max(0, n)) / 100
                  : 0.9,
              });
            }}
          />
        </Field>

        <Field
          label="Максимум токенов ответа"
          hint="Верхняя граница; фактический лимит считается от длины диктовки. Обрыв по лимиту не вставляется — отдаётся исходный текст"
        >
          <input
            type="number"
            min={256}
            max={32768}
            step={256}
            value={settings.rewrite_max_output_tokens}
            onChange={(e) => {
              const n = parseInt(e.currentTarget.value, 10);
              update({
                rewrite_max_output_tokens: Number.isFinite(n)
                  ? Math.min(32768, Math.max(256, n))
                  : 4096,
              });
            }}
          />
        </Field>
      </div>
    </div>
  );
}
