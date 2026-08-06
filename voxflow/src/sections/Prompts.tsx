import { useEffect, useMemo, useState } from "react";
import { promptModels, refreshPromptRules } from "../api";
import type { AiPromptRule, PromptModelView, Settings } from "../types";
import { Field, Icon, PageHead, Select, Switch } from "../ui";

/// Человеческие имена сервисов. Идентификаторы приходят из
/// `app_context::ai_target`, каталог моделей — из `prompt_rules.json`, поэтому
/// здесь только подписи: список моделей и правила фронт не дублирует.
const SERVICE_LABELS: Record<string, string> = {
  claude: "Claude",
  chatgpt: "ChatGPT",
  codex: "Codex",
  gemini: "Gemini",
  perplexity: "Perplexity",
  generic: "Остальные нейросети",
};

type Props = {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
};

function ruleFor(rules: AiPromptRule[], service: string): AiPromptRule | undefined {
  return rules.find((rule) => rule.match.trim().toLowerCase() === service);
}

function checkedLabel(model: PromptModelView): string {
  if (!model.doc) return "Своя документация не отслеживается.";
  if (!model.checked) return "Документацию ещё не проверяли.";
  const when = new Date(model.checked);
  if (Number.isNaN(when.getTime())) return "Документацию ещё не проверяли.";
  const stamp = when.toLocaleDateString();
  return model.refreshed
    ? `Правила пересобраны из документации, проверено ${stamp}.`
    : `Документация без изменений, проверено ${stamp}.`;
}

export default function Prompts({ settings, update }: Props) {
  const [catalog, setCatalog] = useState<PromptModelView[]>([]);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  const rules = settings.ai_prompt_rules ?? [];
  const choices = settings.prompt_models ?? [];
  const backendOff = settings.ai_backend === "off";

  async function load() {
    setCatalog(await promptModels());
  }

  useEffect(() => {
    void load();
  }, []);

  // Порядок сервисов берём из каталога, чтобы новая модель в JSON появлялась в
  // интерфейсе сама, без правки фронта.
  const services = useMemo(() => {
    const seen: string[] = [];
    for (const model of catalog) {
      if (!seen.includes(model.service)) seen.push(model.service);
    }
    return seen;
  }, [catalog]);

  function selectedFor(service: string): PromptModelView | undefined {
    const chosen = choices.find((c) => c.service === service)?.model;
    const ofService = catalog.filter((m) => m.service === service);
    return ofService.find((m) => m.id === chosen) ?? ofService[0];
  }

  function chooseModel(service: string, model: string) {
    update({
      prompt_models: [...choices.filter((c) => c.service !== service), { service, model }],
    });
  }

  // Пустой текст = вернуться на правила из каталога, поэтому правило удаляем,
  // а не сохраняем пустым: пустое бэкенд игнорирует, но оно висело бы мусорной
  // строкой в разделе «Приложения», где живёт тот же список.
  function setOverride(service: string, prompt: string) {
    const rest = rules.filter((rule) => rule.match.trim().toLowerCase() !== service);
    update({
      ai_prompt_rules: prompt.trim() ? [...rest, { match: service, prompt }] : rest,
    });
  }

  async function onRefresh() {
    if (busy) return;
    setBusy(true);
    setStatus("Читаю документацию…");
    try {
      const report = await refreshPromptRules();
      await load();
      if (report.updated.length > 0) {
        setStatus(`Документация изменилась, правила пересобраны: ${report.updated.join(", ")}.`);
      } else if (report.failed.length > 0) {
        setStatus(
          `Проверено ${report.checked}, обновлений нет. Не удалось прочитать: ${report.failed.join(", ")}.`,
        );
      } else {
        setStatus(`Проверено ${report.checked}, документация не менялась.`);
      }
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Не удалось проверить документацию");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="content-inner">
      <PageHead
        title="Промпты"
        desc="Диктовка в чат с нейросетью превращается в структурный промпт по правилам той модели, в которую вы пишете."
      />

      <div className="card">
        <div className="card-head">
          <div className="card-title">Пересборка промпта</div>
          <div className="sub">
            Сервис определяется автоматически по активному окну. Номер модели —
            вручную: ни заголовок окна, ни дерево доступности его не содержат.
          </div>
        </div>

        <Field
          label="Пересобирать диктовку в промпт"
          hint="Работает в чатах нейросетей. В редакторах кода и терминалах диктовка остаётся дословной."
        >
          <Switch
            checked={settings.prompt_rebuild}
            onChange={(v) => update({ prompt_rebuild: v })}
          />
        </Field>

        {settings.prompt_rebuild && backendOff && (
          <p className="hint">
            <Icon.Sparkles className="ico" /> Пересборку делает нейросеть — выберите
            бэкенд в разделе «ИИ», иначе текст вставится как надиктован.
          </p>
        )}
      </div>

      <div className="card">
        <div className="card-head">
          <div className="card-title">Правила по моделям</div>
          <div className="sub">
            Правила составлены по документации вендоров. Приложение само перечитывает
            её и пересобирает правила, когда документ меняется — обновлять приложение
            для этого не нужно.
          </div>
          <button className="btn" type="button" onClick={onRefresh} disabled={busy}>
            <Icon.Clock className="ico" /> {busy ? "Читаю…" : "Проверить документацию"}
          </button>
        </div>

        {status && <p className="hint">{status}</p>}
        {catalog.length === 0 && <p className="hint">Каталог моделей не загрузился.</p>}

        {services.map((service) => {
          const models = catalog.filter((m) => m.service === service);
          const selected = selectedFor(service);
          const override = ruleFor(rules, service);
          return (
            <div className="app-group" key={service}>
              <div className="app-group-title">{SERVICE_LABELS[service] ?? service}</div>

              {models.length > 1 && (
                <Field label="Модель" hint="Правила у Opus и Sonnet разные — выберите ту, в которой пишете.">
                  <Select
                    value={selected?.id ?? ""}
                    onChange={(v) => chooseModel(service, v)}
                    options={models.map((m) => ({ value: m.id, label: m.label }))}
                  />
                </Field>
              )}

              {selected && (
                <>
                  <p className="hint">{selected.rules}</p>
                  <p className="hint">
                    {checkedLabel(selected)}
                    {selected.doc && (
                      <>
                        {" "}
                        <a href={selected.doc} target="_blank" rel="noreferrer">
                          Источник
                        </a>
                      </>
                    )}
                  </p>
                </>
              )}

              <Field
                label="Свои правила"
                hint="Перекрывают правила из документации целиком. Пустое поле возвращает их обратно."
              >
                <textarea
                  value={override?.prompt ?? ""}
                  onChange={(event) => setOverride(service, event.currentTarget.value)}
                  placeholder="Например: всегда добавляй критерии приёмки и список затронутых файлов."
                  rows={3}
                />
              </Field>
            </div>
          );
        })}
      </div>
    </div>
  );
}
