import { useEffect, useMemo, useState } from "react";
import { activeAppContext, defaultAppProfilePresets } from "../api";
import type { ActiveAppContext, AiPromptRule, ProfileOverride, Settings } from "../types";
import { Icon, IS_APPLE_PLATFORM, PageHead, Select } from "../ui";

const PROFILE_OPTIONS = [
  { value: "neutral", label: "Нейтральный" },
  { value: "casual", label: "Неформальный" },
  { value: "work", label: "Рабочий" },
  { value: "formal", label: "Формальный" },
  { value: "doc", label: "Документ" },
  { value: "ai", label: "Промпт для ИИ" },
  { value: "code", label: "Код (дословно)" },
  { value: "verbatim", label: "Дословно" },
];
const PROFILE_VALUES = new Set(PROFILE_OPTIONS.map((item) => item.value));

type AppPreset = {
  name: string;
  match: string;
  macMatch?: string;
  profile: string;
  group: string;
  hint: string;
  glyph: string;
};

const APP_GROUPS: { title: string; apps: AppPreset[] }[] = [
  {
    title: "Сообщения",
    apps: [
      { name: "Telegram", match: "telegram", profile: "casual", group: "Сообщения", hint: "Коротко, живо, без лишней формальности.", glyph: "telegram" },
      { name: "WhatsApp", match: "whatsapp", profile: "casual", group: "Сообщения", hint: "Натуральный разговорный тон.", glyph: "whatsapp" },
      { name: "Discord", match: "discord", profile: "casual", group: "Сообщения", hint: "Быстрые реплики и меньше правок.", glyph: "discord" },
    ],
  },
  {
    title: "Почта",
    apps: [
      { name: "Gmail", match: "gmail", profile: "formal", group: "Почта", hint: "Аккуратные абзацы и деловой тон.", glyph: "gmail" },
      { name: "Outlook", match: "outlook", macMatch: "microsoft outlook", profile: "formal", group: "Почта", hint: "Официальные письма без разговорного тона.", glyph: "outlook" },
    ],
  },
  {
    title: "Промты",
    apps: [
      { name: "Codex", match: "codex", profile: "ai", group: "Промты", hint: "Команды, списки и технический контекст.", glyph: "terminal" },
      { name: "ChatGPT", match: "chatgpt", profile: "ai", group: "Промты", hint: "Промты без случайной отправки.", glyph: "spark" },
      { name: "Claude", match: "claude", profile: "ai", group: "Промты", hint: "Чёткие инструкции и длинный контекст.", glyph: "claude" },
      { name: "Gemini", match: "gemini", profile: "ai", group: "Промты", hint: "Структурные запросы без лишней воды.", glyph: "spark" },
      { name: "Perplexity", match: "perplexity", profile: "ai", group: "Промты", hint: "Вопросы с контекстом и критериями поиска.", glyph: "spark" },
      { name: "DeepSeek", match: "deepseek", profile: "ai", group: "Промты", hint: "Чёткие задачи для кода и анализа.", glyph: "spark" },
      { name: "Grok", match: "grok", profile: "ai", group: "Промты", hint: "Короткие постановки с явным результатом.", glyph: "spark" },
      { name: "OpenRouter", match: "openrouter", profile: "ai", group: "Промты", hint: "Единые правила для веб-чата и роутера моделей.", glyph: "spark" },
    ],
  },
  {
    title: "Код",
    apps: [
      { name: "VS Code", match: "code.exe", macMatch: "code", profile: "code", group: "Код", hint: "Бережно к переменным и символам.", glyph: "code" },
      { name: "Cursor", match: "cursor", profile: "code", group: "Код", hint: "Меньше переписывания, больше точности.", glyph: "cursor" },
      { name: "Windsurf", match: "windsurf", profile: "code", group: "Код", hint: "Команды и имена файлов сохраняются.", glyph: "wave" },
    ],
  },
  {
    title: "Документы",
    apps: [
      { name: "Word", match: "word", macMatch: "microsoft word", profile: "doc", group: "Документы", hint: "Полные предложения и мягкая редактура.", glyph: "word" },
      { name: "Google Docs", match: "google docs", profile: "doc", group: "Документы", hint: "Длинный текст без чатового тона.", glyph: "docs" },
    ],
  },
];

function profileLabel(profile: string): string {
  const value = profile.trim() || "neutral";
  return PROFILE_OPTIONS.find((item) => item.value === value)?.label ?? value;
}

function profileOptionsWithCurrent(profile: string) {
  const value = profile.trim() || "neutral";
  if (PROFILE_VALUES.has(value)) return PROFILE_OPTIONS;
  return [{ value, label: `Текущий: ${value}` }, ...PROFILE_OPTIONS];
}

function normalizeRule(rule: ProfileOverride): ProfileOverride | null {
  const match = rule.match.trim();
  if (!match) return null;
  // Профиль не сводим к трём вариантам: backend различает ai/code/doc/
  // verbatim/neutral, и потеря этих значений меняет поведение диктовки.
  return { match, profile: rule.profile.trim() || "neutral" };
}

function normalizePromptRule(rule: AiPromptRule): AiPromptRule | null {
  const match = rule.match.trim();
  if (!match || !rule.prompt.trim()) return null;
  return { match, prompt: rule.prompt };
}

function sameMatch(a: string, b: string): boolean {
  return a.trim().toLowerCase() === b.trim().toLowerCase();
}

function preferredMatch(app: AppPreset): string {
  return IS_APPLE_PLATFORM && app.macMatch ? app.macMatch : app.match;
}

function appMatches(app: AppPreset): string[] {
  return Array.from(
    new Set([preferredMatch(app), app.match, app.macMatch].filter((v): v is string => !!v)),
  );
}

function sameAppMatch(a: string, b: string): boolean {
  if (sameMatch(a, b)) return true;
  return APP_GROUPS.some((group) =>
    group.apps.some((app) => {
      const matches = appMatches(app);
      return matches.some((value) => sameMatch(value, a)) &&
        matches.some((value) => sameMatch(value, b));
    }),
  );
}

function sameRule(a: ProfileOverride, b: ProfileOverride): boolean {
  return sameAppMatch(a.match, b.match);
}

function ruleForApp(rules: ProfileOverride[], app: AppPreset): ProfileOverride | undefined {
  const matches = appMatches(app);
  return rules.find((rule) => matches.some((match) => sameMatch(rule.match, match)));
}

function promptRuleForApp(rules: AiPromptRule[], app: AppPreset): AiPromptRule | undefined {
  const matches = appMatches(app);
  return rules.find((rule) => matches.some((match) => sameMatch(rule.match, match)));
}

function platformPreset(rule: ProfileOverride): ProfileOverride {
  const app = APP_GROUPS.flatMap((group) => group.apps).find((candidate) =>
    appMatches(candidate).some((match) => sameMatch(match, rule.match)),
  );
  return app ? { ...rule, match: preferredMatch(app) } : rule;
}

function promptPlaceholder(appName: string): string {
  if (appName === "Codex") return "Например: превращай диктовку в задачу для Codex: контекст, файлы, что проверить, что не трогать.";
  if (appName === "Claude") return "Например: структурируй как длинный промпт с требованиями, ограничениями и форматом ответа.";
  if (appName === "Perplexity") return "Например: делай исследовательский вопрос с контекстом и критериями источников.";
  return "Например: перепиши диктовку как ясный промпт для этой нейросети.";
}

type Props = {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  /** Внутри страницы хаба заголовок даёт хаб. */
  embedded?: boolean;
};

export default function Applications({ settings, update, embedded }: Props) {
  const [context, setContext] = useState<ActiveAppContext | null>(null);
  const [openPrompts, setOpenPrompts] = useState<Record<string, boolean>>({});
  const [presets, setPresets] = useState<ProfileOverride[]>([]);
  const [match, setMatch] = useState("");
  const [profile, setProfile] = useState("casual");
  const [promptMatch, setPromptMatch] = useState("");
  const [promptText, setPromptText] = useState("");
  const [status, setStatus] = useState("Выберите приложение и задайте стиль диктовки.");

  const rules = settings.app_profile_overrides ?? [];
  const promptRules = settings.ai_prompt_rules ?? [];

  const hasPresets = useMemo(
    () => presets.length > 0 && presets.every((preset) => rules.some((rule) => sameRule(rule, preset))),
    [presets, rules],
  );

  async function refreshContext() {
    try {
      const next = await activeAppContext();
      setContext(next);
      setStatus("Активное приложение обновлено.");
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Не удалось определить активное приложение.");
    }
  }

  function saveRules(nextRules: ProfileOverride[], message = "Профили обновлены.") {
    const normalized = nextRules.map(normalizeRule).filter((item): item is ProfileOverride => item !== null);
    update({ app_profile_overrides: normalized });
    setStatus(message);
  }

  function savePromptRules(nextRules: AiPromptRule[], message = "Промты обновлены.") {
    const normalized = nextRules.map(normalizePromptRule).filter((item): item is AiPromptRule => item !== null);
    update({ ai_prompt_rules: normalized });
    setStatus(message);
  }

  function upsertRule(nextRule: ProfileOverride, message: string) {
    const normalized = normalizeRule(nextRule);
    if (!normalized) return;
    const withoutDuplicate = rules.filter((rule) => !sameRule(rule, normalized));
    saveRules([...withoutDuplicate, normalized], message);
  }

  function upsertPromptRule(nextRule: AiPromptRule, message: string) {
    const normalized = normalizePromptRule(nextRule);
    const withoutDuplicate = promptRules.filter((rule) => !sameMatch(rule.match, nextRule.match));
    if (!normalized) {
      savePromptRules(withoutDuplicate, "Промт очищен.");
      return;
    }
    savePromptRules([...withoutDuplicate, normalized], message);
  }

  function addRule() {
    const next = normalizeRule({ match, profile });
    if (!next) {
      setStatus("Введите exe или часть заголовка окна.");
      return;
    }
    upsertRule(next, "Правило добавлено.");
    setMatch("");
  }

  function addCurrentWindow() {
    if (!context?.exe && !context?.title) {
      setStatus("Сначала обновите активное окно.");
      return;
    }
    const candidate = context.exe || context.title;
    upsertRule({ match: candidate, profile: context.profile || "neutral" }, "Текущее окно добавлено.");
  }

  function editRule(index: number, patch: Partial<ProfileOverride>) {
    const next = rules.map((rule, ruleIndex) => (ruleIndex === index ? { ...rule, ...patch } : rule));
    saveRules(next);
  }

  function deleteRule(index: number) {
    saveRules(
      rules.filter((_, ruleIndex) => ruleIndex !== index),
      "Правило удалено.",
    );
  }

  function addPromptRule() {
    const next = normalizePromptRule({ match: promptMatch, prompt: promptText });
    if (!next) {
      setStatus("Введите match и промт для нейросети.");
      return;
    }
    upsertPromptRule(next, "Промт добавлен.");
    setPromptMatch("");
    setPromptText("");
  }

  function editPromptRule(index: number, patch: Partial<AiPromptRule>) {
    const next = promptRules.map((rule, ruleIndex) => (ruleIndex === index ? { ...rule, ...patch } : rule));
    savePromptRules(next);
  }

  function deletePromptRule(index: number) {
    savePromptRules(
      promptRules.filter((_, ruleIndex) => ruleIndex !== index),
      "Промт удалён.",
    );
  }

  function addPresets() {
    const missing = presets.filter((preset) => !rules.some((rule) => sameRule(rule, preset)));
    if (missing.length === 0) {
      setStatus("Базовые профили уже добавлены.");
      return;
    }
    saveRules([...rules, ...missing], "Базовые профили приложений добавлены.");
  }

  useEffect(() => {
    refreshContext();
    defaultAppProfilePresets()
      .then((items) => setPresets(items.map(platformPreset)))
      .catch(() => setPresets([]));
  }, []);

  return (
    <>
      {!embedded && (
        <PageHead
          title="Приложения"
          desc="Выберите приложение и стиль диктовки. VoxFlow применит профиль к активному окну без ручного копания в правилах."
        />
      )}

      <section className="app-hero card">
        <div className="app-hero-main">
          <span className="app-avatar" aria-hidden>
            {(context?.exe || "?").slice(0, 1)}
          </span>
          <div>
            <h2>{context?.exe || "Активное окно"}</h2>
            <p>{context?.title || "Обновите детектор, чтобы увидеть текущее приложение."}</p>
          </div>
        </div>
        <div className="app-hero-actions">
          <span className="badge accent">{context ? profileLabel(context.profile) : "..."}</span>
          <button className="btn btn-sm btn-ghost" type="button" onClick={addCurrentWindow}>
            <Icon.Plus className="ico" /> Добавить текущее
          </button>
          <button className="btn btn-sm" type="button" onClick={refreshContext}>
            <Icon.Refresh className="ico" /> Обновить
          </button>
        </div>
      </section>

      <section className="app-picker-shell">
        <div className="app-picker-main">
          <div className="app-picker-top">
            <div>
              <h2>Программы</h2>
              <p>Выбор сохраняется сразу.</p>
            </div>
            <button className="btn" type="button" onClick={addPresets} disabled={hasPresets}>
              <Icon.Plus className="ico" /> {hasPresets ? "Все базовые добавлены" : "Добавить базовые"}
            </button>
          </div>
          <p className="hint app-picker-status">{status}</p>

          {APP_GROUPS.map((group) => (
            <div className="app-group" key={group.title}>
              <div className="app-group-title">
                {group.title === "Промты" ? "Нейросети" : group.title}
              </div>
              <div className="app-list">
                {group.apps.map((app) => {
                  const configured = ruleForApp(rules, app);
                  const promptConfigured = promptRuleForApp(promptRules, app);
                  const currentProfile = configured?.profile || app.profile;
                  const targetMatch = configured?.match || preferredMatch(app);
                  const promptTargetMatch =
                    promptConfigured?.match || preferredMatch(app);
                  const isPromptApp = group.title === "Промты";
                  // Поле промпта раскрыто, если его открыли или промпт уже задан.
                  const promptOpen =
                    openPrompts[app.name] ?? Boolean(promptConfigured?.prompt);
                  return (
                    <div className="app-row" key={app.name}>
                      <span className="app-avatar" aria-hidden>
                        {app.name.slice(0, 1)}
                      </span>
                      <span className="app-row-copy">
                        <strong>{app.name}</strong>
                        <small>{app.hint}</small>
                      </span>
                      <span className="app-row-controls">
                        {isPromptApp && (
                          <button
                            type="button"
                            className={`btn btn-sm btn-ghost${promptOpen ? " active" : ""}`}
                            aria-expanded={promptOpen}
                            onClick={() =>
                              setOpenPrompts((prev) => ({ ...prev, [app.name]: !promptOpen }))
                            }
                          >
                            Промпт{promptConfigured?.prompt ? " •" : ""}
                          </button>
                        )}
                        <Select
                          value={currentProfile}
                          onChange={(value) =>
                            upsertRule(
                              { match: targetMatch, profile: value },
                              `${app.name}: ${profileLabel(value)}.`,
                            )
                          }
                          options={profileOptionsWithCurrent(currentProfile)}
                        />
                      </span>
                      {isPromptApp && promptOpen && (
                        <textarea
                          className="app-row-prompt"
                          aria-label={`Промпт для ${app.name}`}
                          value={promptConfigured?.prompt ?? ""}
                          onChange={(event) => upsertPromptRule(
                            {
                              match: promptTargetMatch,
                              prompt: event.currentTarget.value,
                            },
                            `${app.name}: промт обновлён.`,
                          )}
                          placeholder={promptPlaceholder(app.name)}
                          rows={3}
                        />
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          ))}
        </div>
      </section>

      <details className="advanced-rules">
        <summary>Свои правила для окон и промпты</summary>
        <section className="card">
          <div className="add-row">
            <input
              value={match}
              onChange={(event) => setMatch(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") addRule();
              }}
              placeholder="telegram, code (macOS), code.exe (Windows), Codex..."
            />
            <Select value={profile} onChange={setProfile} options={PROFILE_OPTIONS} />
            <button className="btn btn-primary" type="button" onClick={addRule}>
              <Icon.Plus className="ico" /> Добавить
            </button>
          </div>

          {rules.length === 0 ? (
            <div className="empty">Пока нет ручных правил. Выберите приложение выше или добавьте своё правило.</div>
          ) : (
            <table className="table">
              <thead>
                <tr>
                  <th>Приложение, exe или заголовок</th>
                  <th>Профиль</th>
                  <th aria-label="Действия" />
                </tr>
              </thead>
              <tbody>
                {rules.map((rule, index) => (
                  <tr key={`${rule.match}-${index}`}>
                    <td>
                      <input
                        value={rule.match}
                        onChange={(event) => editRule(index, { match: event.target.value })}
                        aria-label="Приложение, exe или заголовок"
                      />
                    </td>
                    <td>
                      <Select
                        value={rule.profile || "neutral"}
                        onChange={(value) => editRule(index, { profile: value })}
                        options={profileOptionsWithCurrent(rule.profile)}
                      />
                    </td>
                    <td className="table-actions">
                      <button className="btn btn-sm btn-danger btn-ghost" type="button" aria-label="Удалить правило" onClick={() => deleteRule(index)}>
                        <Icon.Trash className="ico" />
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>

        <section className="card">
          <div className="card-head">
            <div className="card-title">Дополнительные промты</div>
            <div className="sub">Для нейросетей, которых нет в плитках выше.</div>
          </div>
          <div className="app-prompt-add">
            <input
              value={promptMatch}
              onChange={(event) => setPromptMatch(event.target.value)}
              placeholder="poe, openrouter, mistral..."
            />
            <textarea
              value={promptText}
              onChange={(event) => setPromptText(event.target.value)}
              placeholder="Как переписывать диктовку именно для этого AI-сервиса"
              rows={3}
            />
            <button className="btn btn-primary" type="button" onClick={addPromptRule}>
              <Icon.Plus className="ico" /> Добавить
            </button>
          </div>

          {promptRules.length === 0 ? (
            <div className="empty compact">Промты пока не заданы.</div>
          ) : (
            <table className="table prompt-rules-table">
              <thead>
                <tr>
                  <th>Нейросеть / match</th>
                  <th>Промт</th>
                  <th aria-label="Действия" />
                </tr>
              </thead>
              <tbody>
                {promptRules.map((rule, index) => (
                  <tr key={`${rule.match}-${index}`}>
                    <td>
                      <input
                        value={rule.match}
                        onChange={(event) => editPromptRule(index, { match: event.target.value })}
                        aria-label="Нейросеть или match"
                      />
                    </td>
                    <td>
                      <textarea
                        value={rule.prompt}
                        onChange={(event) => editPromptRule(index, { prompt: event.target.value })}
                        aria-label="Промт"
                        rows={2}
                      />
                    </td>
                    <td className="table-actions">
                      <button className="btn btn-sm btn-danger btn-ghost" type="button" aria-label="Удалить промт" onClick={() => deletePromptRule(index)}>
                        <Icon.Trash className="ico" />
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      </details>
    </>
  );
}
